//! ADB SYNC protocol (`sync:` service).
//!
//! After the service is opened, every message is `[u8; 4] id` + `u32
//! little-endian length` + payload.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::AdbError;

/// Largest `DATA` payload adbd accepts (`SYNC_DATA_MAX`).
pub const SYNC_DATA_MAX: usize = 64 * 1024;
/// Longest remote path adbd accepts.
pub const SYNC_PATH_MAX: usize = 1024;
/// `S_IFREG | 0644`.
pub const MODE_REGULAR_644: u32 = 0o100644;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub mode: u32,
    pub size: u32,
    pub mtime: u32,
}

impl Stat {
    /// adbd reports all-zero fields for missing paths.
    pub fn exists(&self) -> bool {
        self.mode != 0
    }
}

pub fn encode(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + payload.len());
    buf.extend_from_slice(id);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

fn check_path(path: &str) -> Result<(), AdbError> {
    if path.len() > SYNC_PATH_MAX {
        return Err(AdbError::Protocol(format!("remote path longer than {SYNC_PATH_MAX} bytes")));
    }
    // Commas are fine: adbd splits the SEND header on the *last* comma.
    if path.contains('\0') {
        return Err(AdbError::Protocol("NUL byte in remote path".into()));
    }
    Ok(())
}

/// `S_IFMT` and the file types [`DirEntry::mode`] can carry.
pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;

/// One entry of a `LIST` reply. `size` is 32-bit in this protocol version:
/// files of 4 GiB or more report their size modulo 2³².
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub mode: u32,
    pub size: u32,
    pub mtime: u32,
}

impl DirEntry {
    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }

    pub fn is_file(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }

    pub fn is_symlink(&self) -> bool {
        self.mode & S_IFMT == S_IFLNK
    }
}

/// An open `sync:` stream.
pub struct SyncSession<S> {
    stream: S,
}

impl<S: AsyncRead + AsyncWrite + Unpin> SyncSession<S> {
    pub fn new(stream: S) -> Self {
        Self { stream }
    }

    pub async fn stat(&mut self, path: &str) -> Result<Stat, AdbError> {
        check_path(path)?;
        self.stream.write_all(&encode(b"STAT", path.as_bytes())).await?;
        let mut resp = [0u8; 16];
        self.stream.read_exact(&mut resp).await?;
        if &resp[..4] != b"STAT" {
            return Err(AdbError::Protocol(format!(
                "expected STAT, got {:?}",
                String::from_utf8_lossy(&resp[..4])
            )));
        }
        let word = |i: usize| u32::from_le_bytes(resp[i..i + 4].try_into().unwrap());
        Ok(Stat { mode: word(4), size: word(8), mtime: word(12) })
    }

    /// Streams `reader` to `path`. `on_progress` receives the running byte count
    /// after each `DATA` packet is written. Returns the number of bytes sent.
    pub async fn send<R, F>(
        &mut self,
        path: &str,
        mode: u32,
        mtime: u32,
        mut reader: R,
        mut on_progress: F,
    ) -> Result<u64, AdbError>
    where
        R: AsyncRead + Unpin,
        F: FnMut(u64),
    {
        check_path(path)?;
        let header = format!("{path},{mode}");
        self.stream.write_all(&encode(b"SEND", header.as_bytes())).await?;

        let mut buf = vec![0u8; SYNC_DATA_MAX];
        let mut sent = 0u64;
        loop {
            let n = read_full(&mut reader, &mut buf).await?;
            if n == 0 {
                break;
            }
            self.stream.write_all(&encode(b"DATA", &buf[..n])).await?;
            sent += n as u64;
            on_progress(sent);
        }

        let mut done = Vec::with_capacity(8);
        done.extend_from_slice(b"DONE");
        done.extend_from_slice(&mtime.to_le_bytes());
        self.stream.write_all(&done).await?;
        self.stream.flush().await?;
        self.read_okay().await?;
        Ok(sent)
    }

    /// Starts a `SEND`; follow with [`Self::send_data`] and [`Self::send_done`].
    pub async fn send_start(&mut self, path: &str, mode: u32) -> Result<(), AdbError> {
        check_path(path)?;
        let header = format!("{path},{mode}");
        self.stream.write_all(&encode(b"SEND", header.as_bytes())).await?;
        Ok(())
    }

    /// Writes `data` as `DATA` packets of at most [`SYNC_DATA_MAX`] bytes.
    pub async fn send_data(&mut self, data: &[u8]) -> Result<(), AdbError> {
        for chunk in data.chunks(SYNC_DATA_MAX) {
            let mut hdr = [0u8; 8];
            hdr[..4].copy_from_slice(b"DATA");
            hdr[4..].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
            self.stream.write_all(&hdr).await?;
            self.stream.write_all(chunk).await?;
        }
        Ok(())
    }

    /// Ends a `SEND` and waits for adbd's verdict.
    pub async fn send_done(&mut self, mtime: u32) -> Result<(), AdbError> {
        let mut done = [0u8; 8];
        done[..4].copy_from_slice(b"DONE");
        done[4..].copy_from_slice(&mtime.to_le_bytes());
        self.stream.write_all(&done).await?;
        self.stream.flush().await?;
        self.read_okay().await
    }

    /// Lists a folder (without `.` and `..`). A folder that is missing or
    /// can't be read lists as empty: check it with [`Self::stat`] first.
    pub async fn list(&mut self, path: &str) -> Result<Vec<DirEntry>, AdbError> {
        check_path(path)?;
        self.stream.write_all(&encode(b"LIST", path.as_bytes())).await?;
        let mut out = Vec::new();
        loop {
            let mut h = [0u8; 20];
            self.stream.read_exact(&mut h).await?;
            let word = |i: usize| u32::from_le_bytes(h[i..i + 4].try_into().unwrap());
            match &h[..4] {
                b"DENT" => {
                    let len = word(16) as usize;
                    if len > SYNC_PATH_MAX {
                        return Err(AdbError::Protocol(format!("LIST name of {len} bytes")));
                    }
                    let mut name = vec![0u8; len];
                    self.stream.read_exact(&mut name).await?;
                    let name = String::from_utf8_lossy(&name).into_owned();
                    if name != "." && name != ".." {
                        out.push(DirEntry { name, mode: word(4), size: word(8), mtime: word(12) });
                    }
                }
                b"DONE" => return Ok(out),
                other => return Err(AdbError::Protocol(format!("expected DENT/DONE, got {:?}", String::from_utf8_lossy(other)))),
            }
        }
    }

    /// Streams the phone's file `path` into `writer`. `on_progress` receives
    /// the running byte count after each `DATA` packet. Returns the size.
    pub async fn recv<W, F>(&mut self, path: &str, mut writer: W, mut on_progress: F) -> Result<u64, AdbError>
    where
        W: AsyncWrite + Unpin,
        F: FnMut(u64),
    {
        check_path(path)?;
        self.stream.write_all(&encode(b"RECV", path.as_bytes())).await?;
        let mut buf = vec![0u8; SYNC_DATA_MAX];
        let mut got = 0u64;
        loop {
            let mut h = [0u8; 8];
            self.stream.read_exact(&mut h).await?;
            let len = u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize;
            match &h[..4] {
                b"DATA" => {
                    if len > SYNC_DATA_MAX {
                        return Err(AdbError::Protocol(format!("DATA of {len} bytes")));
                    }
                    self.stream.read_exact(&mut buf[..len]).await?;
                    writer.write_all(&buf[..len]).await?;
                    got += len as u64;
                    on_progress(got);
                }
                b"DONE" => {
                    writer.flush().await?;
                    return Ok(got);
                }
                b"FAIL" => {
                    let mut msg = vec![0u8; len.min(SYNC_DATA_MAX)];
                    self.stream.read_exact(&mut msg).await?;
                    return Err(AdbError::Fail(String::from_utf8_lossy(&msg).into_owned()));
                }
                other => return Err(AdbError::Protocol(format!("expected DATA/DONE, got {:?}", String::from_utf8_lossy(other)))),
            }
        }
    }

    async fn read_okay(&mut self) -> Result<(), AdbError> {
        let mut header = [0u8; 8];
        self.stream.read_exact(&mut header).await?;
        let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        match &header[..4] {
            b"OKAY" => Ok(()),
            b"FAIL" => {
                let mut msg = vec![0u8; len.min(SYNC_DATA_MAX)];
                self.stream.read_exact(&mut msg).await?;
                Err(AdbError::Fail(String::from_utf8_lossy(&msg).into_owned()))
            }
            other => Err(AdbError::Protocol(format!(
                "expected OKAY/FAIL, got {:?}",
                String::from_utf8_lossy(other)
            ))),
        }
    }

    pub async fn quit(mut self) -> Result<(), AdbError> {
        self.stream.write_all(&encode(b"QUIT", &[])).await?;
        self.stream.shutdown().await.ok();
        Ok(())
    }
}

/// Fills `buf` unless EOF comes first, so DATA packets are full-sized.
async fn read_full<R: AsyncRead + Unpin>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_little_endian_frames() {
        assert_eq!(encode(b"STAT", b"/x"), b"STAT\x02\x00\x00\x00/x");
        assert_eq!(MODE_REGULAR_644, 33188);
    }

    #[test]
    fn rejects_long_paths() {
        assert!(check_path(&"a".repeat(SYNC_PATH_MAX + 1)).is_err());
        assert!(check_path("/sdcard/a\0b").is_err());
        assert!(check_path("/sdcard/Download/ok.jpg").is_ok());
    }
}
