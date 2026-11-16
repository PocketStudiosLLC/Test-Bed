//! The share service's local control channel: a Unix socket on Linux, a
//! named pipe on Windows. Only this user can use either.

use std::io;

use tokio::io::{AsyncRead, AsyncWrite};

pub type Reader = Box<dyn AsyncRead + Unpin + Send>;
pub type Writer = Box<dyn AsyncWrite + Unpin + Send>;

fn split<T: AsyncRead + AsyncWrite + Send + 'static>(s: T) -> (Reader, Writer) {
    let (r, w) = tokio::io::split(s);
    (Box::new(r), Box::new(w))
}

/// Connects to the running share service.
pub async fn connect() -> io::Result<(Reader, Writer)> {
    #[cfg(unix)]
    {
        Ok(split(tokio::net::UnixStream::connect(crate::paths::host_control_socket()).await?))
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        let name = crate::paths::host_control_socket();
        // All instances busy: the service is answering someone else; wait a bit.
        for _ in 0..20 {
            match ClientOptions::new().open(&name) {
                Ok(c) => return Ok(split(c)),
                Err(e) if e.raw_os_error() == Some(231 /* ERROR_PIPE_BUSY */) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, "the share service is busy"))
    }
}

/// The service's side: accepts one connection at a time, each handled by the caller.
pub struct Listener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    next: tokio::net::windows::named_pipe::NamedPipeServer,
}

impl Listener {
    pub fn bind() -> io::Result<Self> {
        let path = crate::paths::host_control_socket();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = path.parent().expect("socket path has a directory");
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            let _ = std::fs::remove_file(&path);
            let inner = tokio::net::UnixListener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self { inner })
        }
        #[cfg(windows)]
        {
            // The first instance claims the name: a second service fails here.
            let next = pipe::create(&path, true)?;
            Ok(Self { next })
        }
    }

    pub async fn accept(&mut self) -> io::Result<(Reader, Writer)> {
        #[cfg(unix)]
        {
            let (s, _) = self.inner.accept().await?;
            Ok(split(s))
        }
        #[cfg(windows)]
        {
            self.next.connect().await?;
            let fresh = pipe::create(&crate::paths::host_control_socket(), false)?;
            Ok(split(std::mem::replace(&mut self.next, fresh)))
        }
    }
}

/// The pipe's security. The service may run as administrator (to control
/// administrator windows), and then the pipe's default security would let in
/// administrator processes only: Test Bed, which isn't one, couldn't reach
/// it. So it names this user (and SYSTEM and administrators) outright.
#[cfg(windows)]
mod pipe {
    use std::ffi::c_void;
    use std::io;
    use std::path::Path;

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// This process's user, as an SID string (`S-1-5-21-…`).
    fn user_sid() -> windows::core::Result<String> {
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
            let mut len = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
            let mut buf = vec![0u8; len as usize];
            let r = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len);
            let _ = CloseHandle(token);
            r?;
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut s = PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut s)?;
            let sid = s.to_string().unwrap_or_default();
            let _ = LocalFree(Some(HLOCAL(s.0.cast())));
            Ok(sid)
        }
    }

    pub fn create(path: &Path, first: bool) -> io::Result<NamedPipeServer> {
        let mut opts = ServerOptions::new();
        opts.first_pipe_instance(first).reject_remote_clients(true);
        let sid = match user_sid() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("control pipe: can't read this user's SID ({e}); using default security");
                return opts.create(path);
            }
        };
        // Full access for this user, SYSTEM and administrators; medium
        // integrity (an ordinary app) may write to it.
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)(A;;GA;;;BA)S:(ML;;NW;;;ME)").encode_utf16().chain([0]).collect();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        if let Err(e) = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut sd, None) } {
            tracing::warn!("control pipe: bad security descriptor ({e}); using default security");
            return opts.create(path);
        }
        let mut attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let r = unsafe { opts.create_with_security_attributes_raw(path, (&mut attrs as *mut SECURITY_ATTRIBUTES).cast::<c_void>()) };
        unsafe {
            let _ = LocalFree(Some(HLOCAL(sd.0)));
        }
        r
    }
}

/// Removes the socket file when the service stops (nothing to do for a pipe).
pub fn cleanup() {
    #[cfg(unix)]
    let _ = std::fs::remove_file(crate::paths::host_control_socket());
}
