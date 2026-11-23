//! Files to and from a shared PC: one stream per file, inside the session.
//! Sending (see [`FileOffer`]): the host saves to Downloads, or to the folder
//! the file was dropped on when it can tell. Taking (see [`FetchRequest`]):
//! the viewer browses the host's folders and saves what it picks in its own
//! Downloads.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::net::{Conn, RecvStream, SendStream};
use crate::proto::{
    read_file_head, read_msg, write_fetch, write_msg, DirEntry, FetchReply, FetchRequest, FileHead, FileOffer, FileReply, Listing, Place,
};

/// A host that hasn't answered an offer by then can't receive files (an
/// older Test Bed never reads the stream).
pub const OFFER_TIMEOUT: Duration = Duration::from_secs(10);
const CHUNK: usize = 256 * 1024;
/// Longest file name kept, in characters.
const MAX_NAME: usize = 200;
/// A listing stops growing past about this many bytes (control messages
/// are capped at 1 MiB).
const LISTING_BUDGET: usize = 768 * 1024;

/// This user's Downloads folder.
pub fn downloads_dir() -> PathBuf {
    directories::UserDirs::new()
        .map(|d| d.download_dir().map(Path::to_path_buf).unwrap_or_else(|| d.home_dir().join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("Downloads"))
}

/// A file name that is safe on Linux and Windows: the last path component
/// only, without characters or names Windows reserves.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String =
        base.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).take(MAX_NAME).collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        return "file".into();
    }
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
    if reserved { format!("_{cleaned}") } else { cleaned }
}

/// `dir/name`, or `dir/stem (2).ext` and so on if that is taken.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.into());
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

/// Sends `path`; `progress(sent, total)` is called as bytes go out. Returns
/// where the host saved it.
pub async fn send(conn: &Conn, path: &Path, drop_at: Option<(f32, f32)>, mut progress: impl FnMut(u64, u64)) -> Result<String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let mut file = tokio::fs::File::open(path).await.with_context(|| format!("opening {}", path.display()))?;
    let meta = file.metadata().await?;
    if !meta.is_file() {
        bail!("{name} is a folder: only files can be sent");
    }
    let size = meta.len();
    let (mut send, mut recv) = conn.open_bi().await?;
    write_msg(&mut send, &FileOffer { name: name.clone(), size, drop_at }).await?;
    match tokio::time::timeout(OFFER_TIMEOUT, read_msg::<FileReply>(&mut recv)).await {
        Err(_) => bail!("the other PC didn't answer: update Test Bed there to receive files"),
        Ok(reply) => match reply? {
            Some(FileReply::Ready { .. }) => {}
            Some(FileReply::Refused { text }) => bail!(text),
            other => bail!("unexpected answer {other:?}"),
        },
    }
    let mut buf = vec![0u8; CHUNK];
    let mut sent = 0u64;
    progress(0, size);
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        send.write_all(&buf[..n]).await?;
        sent += n as u64;
        progress(sent, size);
    }
    if sent != size {
        bail!("{name} changed while it was being sent");
    }
    send.finish();
    match read_msg::<FileReply>(&mut recv).await? {
        Some(FileReply::Saved { path }) => Ok(path),
        Some(FileReply::Refused { text }) => bail!(text),
        other => bail!("unexpected answer {other:?}"),
    }
}

/// What the host did with one file stream.
#[derive(Debug)]
pub enum Served {
    /// Saved a file the viewer sent, here.
    Received(PathBuf),
    /// Sent the viewer this file.
    Sent(PathBuf),
    /// Listed a folder for the viewer.
    Listed,
}

/// The host's side of one file stream. `folder` picks the folder for a
/// file sent to it, from the drop point; it runs on a blocking thread.
pub async fn serve(
    send: SendStream,
    mut recv: RecvStream,
    folder: impl FnOnce(Option<(f32, f32)>) -> PathBuf + Send + 'static,
) -> Result<Served> {
    match read_file_head(&mut recv).await? {
        None => bail!("empty file stream"),
        Some(head) => serve_head(send, recv, head, folder).await,
    }
}

/// [`serve`], for a stream whose first message the caller read already.
/// A [`FileHead::Link`] is the caller's to answer: it is refused here.
pub async fn serve_head(
    mut send: SendStream,
    recv: RecvStream,
    head: FileHead,
    folder: impl FnOnce(Option<(f32, f32)>) -> PathBuf + Send + 'static,
) -> Result<Served> {
    match head {
        FileHead::Offer(offer) => receive(send, recv, offer, folder).await.map(Served::Received),
        FileHead::Fetch(req) => answer_fetch(send, req).await,
        FileHead::Link(_) => {
            let text = "this PC can't link here".to_string();
            write_msg(&mut send, &crate::proto::LinkReply::Refused { text: text.clone() }).await?;
            send.finish();
            bail!(text)
        }
    }
}

async fn receive(
    mut send: SendStream,
    mut recv: RecvStream,
    offer: FileOffer,
    folder: impl FnOnce(Option<(f32, f32)>) -> PathBuf + Send + 'static,
) -> Result<PathBuf> {
    let dir = tokio::task::spawn_blocking(move || folder(offer.drop_at)).await?;
    let name = safe_name(&offer.name);
    let opened = async {
        tokio::fs::create_dir_all(&dir).await?;
        let part = unique_path(&dir, &format!("{name}.part"));
        let f = tokio::fs::File::create(&part).await?;
        anyhow::Ok((part, f))
    }
    .await;
    let (part, mut f) = match opened {
        Ok(o) => o,
        Err(e) => {
            let text = format!("can't save in {}: {e:#}", dir.display());
            let _ = write_msg(&mut send, &FileReply::Refused { text: text.clone() }).await;
            bail!(text);
        }
    };
    write_msg(&mut send, &FileReply::Ready { folder: dir.display().to_string() }).await?;

    let copied = async {
        let mut buf = vec![0u8; CHUNK];
        let mut got = 0u64;
        while let Some(n) = recv.read(&mut buf).await? {
            got += n as u64;
            if got > offer.size {
                bail!("more data than offered");
            }
            f.write_all(&buf[..n]).await?;
        }
        if got != offer.size {
            bail!("the transfer stopped after {got} of {} bytes", offer.size);
        }
        f.flush().await?;
        anyhow::Ok(())
    }
    .await;
    drop(f);
    let saved = match copied {
        Ok(()) => {
            let dest = unique_path(&dir, &name);
            tokio::fs::rename(&part, &dest).await.map(|()| dest).map_err(anyhow::Error::from)
        }
        Err(e) => Err(e),
    };
    match saved {
        Ok(dest) => {
            write_msg(&mut send, &FileReply::Saved { path: dest.display().to_string() }).await?;
            send.finish();
            let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
            Ok(dest)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            let _ = write_msg(&mut send, &FileReply::Refused { text: format!("{e:#}") }).await;
            Err(e)
        }
    }
}

// ---- Taking files from the host ------------------------------------------

/// The host can't answer fetches (an older Test Bed, or RemoteLinux).
const FETCH_UNSUPPORTED: &str = "the other PC can't share its files: update Test Bed there";

async fn fetch(conn: &Conn, req: &FetchRequest) -> Result<(FetchReply, RecvStream)> {
    let (mut send, mut recv) = conn.open_bi().await?;
    write_fetch(&mut send, req).await?;
    send.finish();
    match tokio::time::timeout(OFFER_TIMEOUT, read_msg::<FetchReply>(&mut recv)).await {
        Err(_) | Ok(Ok(None)) => bail!(FETCH_UNSUPPORTED),
        Ok(Err(e)) => Err(e.context(FETCH_UNSUPPORTED)),
        Ok(Ok(Some(FetchReply::Refused { text }))) => bail!(text),
        Ok(Ok(Some(reply))) => Ok((reply, recv)),
    }
}

/// The host's folder `dir` (its home if empty), or its subfolder `name`.
pub async fn list(conn: &Conn, dir: &str, name: Option<&str>) -> Result<Listing> {
    match fetch(conn, &FetchRequest::List { dir: dir.into(), name: name.map(Into::into) }).await? {
        (FetchReply::Listing(l), _) => Ok(l),
        (other, _) => bail!("unexpected answer {other:?}"),
    }
}

/// Copies the host's file `name` in `dir` into `dest` (under a free name);
/// `progress(got, total)` is called as bytes come in. Returns the saved file.
pub async fn take(conn: &Conn, dir: &str, name: &str, dest: &Path, mut progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    let (reply, mut recv) = fetch(conn, &FetchRequest::Get { dir: dir.into(), name: name.into() }).await?;
    let FetchReply::Sending { name, size } = reply else { bail!("unexpected answer {reply:?}") };
    let name = safe_name(&name);
    tokio::fs::create_dir_all(dest).await.with_context(|| format!("can't save in {}", dest.display()))?;
    let part = unique_path(dest, &format!("{name}.part"));
    let mut f = tokio::fs::File::create(&part).await.with_context(|| format!("can't save in {}", dest.display()))?;
    // Removes the partial file however this ends, the session dropping included.
    let _cleanup = RemoveOnDrop(part.clone());
    let copied = async {
        let mut buf = vec![0u8; CHUNK];
        let mut got = 0u64;
        progress(0, size);
        while let Some(n) = recv.read(&mut buf).await? {
            got += n as u64;
            if got > size {
                bail!("more data than announced");
            }
            f.write_all(&buf[..n]).await?;
            progress(got, size);
        }
        if got != size {
            bail!("the transfer stopped after {got} of {size} bytes");
        }
        f.flush().await?;
        anyhow::Ok(())
    }
    .await;
    drop(f);
    copied?;
    let to = unique_path(dest, &name);
    tokio::fs::rename(&part, &to).await?;
    Ok(to)
}

/// How [`take_folder`] went.
#[derive(Debug, Default)]
pub struct FolderTaken {
    /// The new folder on this PC.
    pub path: PathBuf,
    pub files: u32,
    pub bytes: u64,
    /// Files that couldn't be copied, and why.
    pub failed: Vec<(String, String)>,
    /// Entries the host left out of a big folder's listing.
    pub omitted: u32,
}

impl FolderTaken {
    /// "Photos (120 files, 2 couldn't be copied)".
    pub fn summary(&self, name: &str) -> String {
        let mut parts = vec![format!("{} file{}", self.files, if self.files == 1 { "" } else { "s" })];
        match self.failed.as_slice() {
            [] => {}
            [(file, why)] => parts.push(format!("{file} couldn't be copied: {why}")),
            many => parts.push(format!("{} couldn't be copied", many.len())),
        }
        if self.omitted > 0 {
            parts.push(format!("{} left out (folder too big to list)", self.omitted));
        }
        format!("{name} ({})", parts.join(", "))
    }
}

/// Folders nested deeper than this aren't followed (a linked folder may loop).
const MAX_FOLDER_DEPTH: usize = 32;

/// Copies the host's folder `name` in `dir`, with everything in it, into a
/// new folder in `dest` (under a free name). `progress(file, got, total)`
/// follows each file, `file` being its path inside the folder. Works with
/// any host that can share files: the folder is walked through listings.
pub async fn take_folder(conn: &Conn, dir: &str, name: &str, dest: &Path, mut progress: impl FnMut(&str, u64, u64)) -> Result<FolderTaken> {
    let root = list(conn, dir, Some(name)).await?;
    tokio::fs::create_dir_all(dest).await.with_context(|| format!("can't save in {}", dest.display()))?;
    let local_root = unique_path(dest, &safe_name(name));
    let mut out = FolderTaken { path: local_root.clone(), ..FolderTaken::default() };
    // (listing, local folder, path inside the taken folder, depth)
    let mut todo = vec![(root, local_root, String::new(), 0usize)];
    while let Some((listing, local, rel, depth)) = todo.pop() {
        tokio::fs::create_dir_all(&local).await.with_context(|| format!("can't create {}", local.display()))?;
        out.omitted += listing.omitted;
        for e in &listing.entries {
            let inner = if rel.is_empty() { e.name.clone() } else { format!("{rel}/{}", e.name) };
            if e.dir {
                if depth + 1 >= MAX_FOLDER_DEPTH {
                    out.failed.push((inner, "nested too deep".into()));
                    continue;
                }
                match list(conn, &listing.path, Some(&e.name)).await {
                    Ok(sub) => todo.push((sub, local.join(safe_name(&e.name)), inner, depth + 1)),
                    Err(err) => out.failed.push((inner, format!("{err:#}"))),
                }
                continue;
            }
            match take(conn, &listing.path, &e.name, &local, |got, total| progress(&inner, got, total)).await {
                Ok(_) => {
                    out.files += 1;
                    out.bytes += e.size;
                }
                // The connection is gone: stop rather than fail every file.
                Err(err) if conn.is_closed() => return Err(err.context(format!("stopped after {} files", out.files))),
                Err(err) => out.failed.push((inner, format!("{err:#}"))),
            }
        }
    }
    Ok(out)
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        // Gone already once renamed.
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn answer_fetch(mut send: SendStream, req: FetchRequest) -> Result<Served> {
    let refuse = |text: String| FetchReply::Refused { text };
    let served = match req {
        FetchRequest::List { dir, name } => {
            let reply = match tokio::task::spawn_blocking(move || list_dir(&dir, name.as_deref())).await? {
                Ok(l) => FetchReply::Listing(l),
                Err(e) => refuse(format!("{e:#}")),
            };
            write_msg(&mut send, &reply).await?;
            Served::Listed
        }
        FetchRequest::Get { dir, name } => {
            let opened = async {
                let path = resolve(&dir, Some(&name))?;
                let file = tokio::fs::File::open(&path).await.with_context(|| format!("can't open {}", path.display()))?;
                let meta = file.metadata().await?;
                if !meta.is_file() {
                    bail!("{name} is a folder: only files can be taken");
                }
                anyhow::Ok((path, file, meta.len()))
            }
            .await;
            let (path, file, size) = match opened {
                Ok(o) => o,
                Err(e) => {
                    let text = format!("{e:#}");
                    write_msg(&mut send, &refuse(text.clone())).await?;
                    send.finish();
                    bail!(text);
                }
            };
            write_msg(&mut send, &FetchReply::Sending { name, size }).await?;
            // Exactly `size` bytes: a file that grows meanwhile is cut, one
            // that shrinks ends short and the viewer throws it away.
            let mut file = file.take(size);
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                send.write_all(&buf[..n]).await?;
            }
            Served::Sent(path)
        }
    };
    send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    Ok(served)
}

/// `dir` (home if empty) and, if given, its child `name`, which must be a
/// plain name.
fn resolve(dir: &str, name: Option<&str>) -> Result<PathBuf> {
    let base = if dir.is_empty() { home_dir() } else { PathBuf::from(dir) };
    if !base.is_absolute() {
        bail!("{dir} isn't a full path");
    }
    match name {
        None => Ok(base),
        Some(n) if n.is_empty() || n == "." || n == ".." || n.contains(['/', '\\']) => bail!("{n:?} isn't a file name"),
        Some(n) => Ok(base.join(n)),
    }
}

fn home_dir() -> PathBuf {
    directories::UserDirs::new().map(|d| d.home_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("/"))
}

fn list_dir(dir: &str, name: Option<&str>) -> Result<Listing> {
    let path = resolve(dir, name)?;
    let read = std::fs::read_dir(&path).with_context(|| format!("can't open {}", path.display()))?;
    let mut entries: Vec<DirEntry> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            // A name that isn't UTF-8 couldn't be asked for again.
            let name = e.file_name().into_string().ok()?;
            // Through symlinks, so a linked folder opens like a folder.
            let meta = std::fs::metadata(e.path()).or_else(|_| e.metadata()).ok()?;
            #[cfg(windows)]
            let hidden = {
                use std::os::windows::fs::MetadataExt;
                name.starts_with('.') || meta.file_attributes() & 0x2 != 0
            };
            #[cfg(not(windows))]
            let hidden = name.starts_with('.');
            let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
            Some(DirEntry { dir: meta.is_dir(), hidden, size: if meta.is_dir() { 0 } else { meta.len() }, modified, name })
        })
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let mut budget = LISTING_BUDGET;
    let total = entries.len();
    entries.retain(|e| {
        let cost = e.name.len() + 32;
        if cost > budget {
            budget = 0;
            return false;
        }
        budget -= cost;
        true
    });
    Ok(Listing {
        path: path.display().to_string(),
        parent: path.parent().map(|p| p.display().to_string()),
        omitted: (total - entries.len()) as u32,
        entries,
        places: places(),
    })
}

/// Folders to start from on this PC.
fn places() -> Vec<Place> {
    let mut out = Vec::new();
    let mut add = |name: &str, p: Option<&Path>| {
        if let Some(p) = p.filter(|p| p.is_dir()) {
            let path = p.display().to_string();
            if !out.iter().any(|o: &Place| o.path == path) {
                out.push(Place { name: name.into(), path });
            }
        }
    };
    if let Some(d) = directories::UserDirs::new() {
        add("Home", Some(d.home_dir()));
        add("Desktop", d.desktop_dir());
        add("Documents", d.document_dir());
        add("Downloads", d.download_dir());
        add("Pictures", d.picture_dir());
        add("Videos", d.video_dir());
        add("Music", d.audio_dir());
    }
    #[cfg(windows)]
    for letter in b'C'..=b'Z' {
        let root = format!("{}:\\", letter as char);
        add(&format!("Drive {}:", letter as char), Some(Path::new(&root)));
    }
    #[cfg(not(windows))]
    add("Computer", Some(Path::new("/")));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name("report.pdf"), "report.pdf");
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name(r"C:\Users\me\a.txt"), "a.txt");
        assert_eq!(safe_name("a<b>c?.txt"), "a_b_c_.txt");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name(""), "file");
        assert_eq!(safe_name("CON.txt"), "_CON.txt");
        assert_eq!(safe_name("com1"), "_com1");
        assert_eq!(safe_name("notes. "), "notes");
    }

    #[test]
    fn fetched_names_stay_in_their_folder() {
        let root = if cfg!(windows) { r"C:\Users\me" } else { "/home/me" };
        assert_eq!(resolve(root, Some("a.txt")).unwrap(), Path::new(root).join("a.txt"));
        assert_eq!(resolve(root, None).unwrap(), Path::new(root));
        for bad in ["..", ".", "", "x/y", r"x\y"] {
            assert!(resolve(root, Some(bad)).is_err(), "{bad:?}");
        }
        assert!(resolve("relative/dir", None).is_err());
        assert!(resolve("", None).unwrap().is_absolute());
    }

    #[test]
    fn listings_put_folders_first() {
        let dir = std::env::temp_dir().join(format!("tb-list-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("zeta")).unwrap();
        std::fs::write(dir.join("Alpha.txt"), b"abc").unwrap();
        std::fs::write(dir.join(".hidden"), b"").unwrap();
        let l = list_dir(&dir.display().to_string(), None).unwrap();
        let names: Vec<_> = l.entries.iter().map(|e| (e.name.as_str(), e.dir, e.hidden, e.size)).collect();
        assert_eq!(names, [("zeta", true, false, 0), (".hidden", false, true, 0), ("Alpha.txt", false, false, 3)]);
        assert_eq!(l.parent.as_deref(), dir.parent().map(|p| p.display().to_string()).as_deref());
        let sub = list_dir(&dir.display().to_string(), Some("zeta")).unwrap();
        assert!(sub.entries.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn taken_names_get_a_number() {
        let dir = std::env::temp_dir().join(format!("tb-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), b"").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (2).txt"));
        std::fs::write(dir.join("a (2).txt"), b"").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (3).txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
