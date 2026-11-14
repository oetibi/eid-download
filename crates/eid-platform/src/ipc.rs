//! Per-user local IPC endpoint (ADR-0002): a Windows **named pipe** (protected DACL for the
//! current user only, remote clients rejected) or a **Unix socket** (0600 inside a 0700 directory,
//! peer uid checked). There is no TCP port anywhere. Also holds the handshake token helpers
//! (256-bit random token in a user-private file).
//!
//! The first listener claims the name (`first_pipe_instance` / refusing to replace a live socket),
//! which is also the single-instance mechanism: a second `bind` fails with `AddrInUse`.
//!
//! NOT verified on real Windows with a second user account (MANUAL-TESTS #9): the DACL is built
//! and the pipe works for the owner, but "another user is refused" is untested.

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Longest endpoint name accepted (keeps socket paths well below `sun_path`).
pub const MAX_NAME_LEN: usize = 32;

fn validate_name(name: &str) -> io::Result<()> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid IPC endpoint name (use 1..=32 of A-Z a-z 0-9 - _)",
        ))
    }
}

/// True if the error means "nobody is listening there" (no app running).
pub fn is_absent(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

// ---------------------------------------------------------------- stream

/// A connected IPC stream (either side).
pub struct Stream {
    inner: StreamInner,
}

enum StreamInner {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    #[cfg(windows)]
    PipeServer(tokio::net::windows::named_pipe::NamedPipeServer),
    #[cfg(windows)]
    PipeClient(tokio::net::windows::named_pipe::NamedPipeClient),
}

macro_rules! delegate {
    ($self:ident, $s:ident => $e:expr) => {
        match &mut $self.get_mut().inner {
            #[cfg(unix)]
            StreamInner::Unix($s) => $e,
            #[cfg(windows)]
            StreamInner::PipeServer($s) => $e,
            #[cfg(windows)]
            StreamInner::PipeClient($s) => $e,
        }
    };
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        delegate!(self, s => Pin::new(s).poll_read(cx, buf))
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        delegate!(self, s => Pin::new(s).poll_write(cx, buf))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, s => Pin::new(s).poll_flush(cx))
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, s => Pin::new(s).poll_shutdown(cx))
    }
}

// ---------------------------------------------------------------- Unix

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    fn uid() -> u32 {
        // SAFETY: getuid has no preconditions and cannot fail.
        unsafe { libc::getuid() }
    }

    /// Directory for sockets: `$XDG_RUNTIME_DIR`, else `<tmp>/eid-<uid>`. It must be a real
    /// directory (not a symlink) owned by us with no group/other access.
    fn socket_dir() -> io::Result<PathBuf> {
        let dir = match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
            Some(d) if d.is_absolute() => d,
            _ => {
                let d = std::env::temp_dir().join(format!("eid-{}", uid()));
                match std::fs::DirBuilder::new().mode(0o700).create(&d) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e),
                }
                d
            }
        };
        let md = std::fs::symlink_metadata(&dir)?;
        if !md.is_dir() || md.uid() != uid() || md.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "IPC directory is not a private directory owned by this user",
            ));
        }
        Ok(dir)
    }

    fn socket_path(name: &str) -> io::Result<PathBuf> {
        validate_name(name)?;
        Ok(socket_dir()?.join(format!("eid-{name}.sock")))
    }

    pub struct Listener {
        l: tokio::net::UnixListener,
        path: PathBuf,
    }

    impl Listener {
        pub fn bind(name: &str) -> io::Result<Listener> {
            let path = socket_path(name)?;
            if std::fs::symlink_metadata(&path).is_ok() {
                // Something is there: a live peer means "already running"; a dead socket file
                // left by a crash is removed.
                match std::os::unix::net::UnixStream::connect(&path) {
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            "another instance is already listening",
                        ))
                    }
                    Err(e) if is_absent(&e) => std::fs::remove_file(&path)?,
                    Err(e) => return Err(e),
                }
            }
            let l = tokio::net::UnixListener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Listener { l, path })
        }

        pub async fn accept(&mut self) -> io::Result<Stream> {
            loop {
                let (s, _) = self.l.accept().await?;
                match s.peer_cred() {
                    Ok(c) if c.uid() == uid() => {
                        return Ok(Stream {
                            inner: StreamInner::Unix(s),
                        })
                    }
                    _ => continue, // another user (or unknown): drop it
                }
            }
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    pub async fn connect(name: &str) -> io::Result<Stream> {
        let path = socket_path(name)?;
        let s = tokio::net::UnixStream::connect(&path).await?;
        Ok(Stream {
            inner: StreamInner::Unix(s),
        })
    }
}

// ---------------------------------------------------------------- Windows

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const ERROR_PIPE_BUSY: i32 = 231;
    const ERROR_ACCESS_DENIED: i32 = 5;

    /// String SID of the current user (e.g. `S-1-5-21-...`).
    pub fn current_user_sid() -> io::Result<String> {
        // SAFETY: plain Win32 calls with valid out-pointers; every allocation is freed below.
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let result = (|| {
                let mut len = 0u32;
                GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
                if len == 0 {
                    return Err(io::Error::last_os_error());
                }
                // u64 elements keep the buffer 8-byte aligned for TOKEN_USER.
                let mut buf = vec![0u64; (len as usize).div_ceil(8)];
                if GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
                    == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let tu = &*(buf.as_ptr() as *const TOKEN_USER);
                let mut w: *mut u16 = null_mut();
                if ConvertSidToStringSidW(tu.User.Sid, &mut w) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut n = 0;
                while *w.add(n) != 0 {
                    n += 1;
                }
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(w, n));
                LocalFree(w.cast());
                Ok(s)
            })();
            CloseHandle(token);
            result
        }
    }

    pub fn pipe_path(name: &str) -> io::Result<String> {
        validate_name(name)?;
        Ok(format!(
            "\\\\.\\pipe\\EidDownload-{}-{name}",
            current_user_sid()?
        ))
    }

    /// Owns a self-relative security descriptor allocated by Windows.
    pub(super) struct SecDesc(*mut c_void);
    impl SecDesc {
        #[cfg(test)]
        pub(super) fn raw(&self) -> *mut c_void {
            self.0
        }
    }
    // SAFETY: the descriptor is immutable after creation and only freed on drop.
    unsafe impl Send for SecDesc {}
    unsafe impl Sync for SecDesc {}
    impl Drop for SecDesc {
        fn drop(&mut self) {
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe { LocalFree(self.0) };
        }
    }

    /// Protected DACL with a single ACE: generic-all for the current user. No other principal
    /// (not even Administrators or SYSTEM) is granted access; inheritance is blocked (`P`).
    fn user_only_descriptor() -> io::Result<SecDesc> {
        descriptor_from_sddl(&format!("D:P(A;;GA;;;{})", current_user_sid()?))
    }

    pub(super) fn descriptor_from_sddl(sddl: &str) -> io::Result<SecDesc> {
        let w: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut sd: *mut c_void = null_mut();
        // SAFETY: `w` is NUL-terminated; `sd` receives a LocalAlloc'd descriptor.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(SecDesc(sd))
    }

    pub(super) fn create_instance(
        name: &str,
        first: bool,
        sd: &SecDesc,
    ) -> io::Result<NamedPipeServer> {
        let mut sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        let mut o = ServerOptions::new();
        o.first_pipe_instance(first).reject_remote_clients(true);
        // SAFETY: `sa` and the descriptor it points to outlive the call.
        let r = unsafe {
            o.create_with_security_attributes_raw(
                name,
                (&mut sa as *mut SECURITY_ATTRIBUTES).cast(),
            )
        };
        match r {
            // The name already exists (a running instance, or a squatter): refuse.
            Err(e) if first && e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another instance is already listening",
                ))
            }
            other => other,
        }
    }

    pub struct Listener {
        path: String,
        pub(super) next: NamedPipeServer,
        sd: SecDesc,
    }

    impl Listener {
        pub fn bind(name: &str) -> io::Result<Listener> {
            let path = pipe_path(name)?;
            let sd = user_only_descriptor()?;
            let next = create_instance(&path, true, &sd)?;
            Ok(Listener { path, next, sd })
        }

        pub async fn accept(&mut self) -> io::Result<Stream> {
            self.next.connect().await?;
            let fresh = create_instance(&self.path, false, &self.sd)?;
            let connected = std::mem::replace(&mut self.next, fresh);
            Ok(Stream {
                inner: StreamInner::PipeServer(connected),
            })
        }
    }

    pub async fn connect(name: &str) -> io::Result<Stream> {
        let path = pipe_path(name)?;
        let mut last = io::Error::from(io::ErrorKind::TimedOut);
        for _ in 0..100 {
            match ClientOptions::new().open(&path) {
                Ok(c) => {
                    return Ok(Stream {
                        inner: StreamInner::PipeClient(c),
                    })
                }
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    last = e;
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }
}

/// Accepts connections on the per-user endpoint.
pub struct Listener(imp::Listener);

impl Listener {
    /// Claim the endpoint `name`. `AddrInUse` = another instance is running.
    pub fn bind(name: &str) -> io::Result<Listener> {
        imp::Listener::bind(name).map(Listener)
    }
    pub async fn accept(&mut self) -> io::Result<Stream> {
        self.0.accept().await
    }
}

/// Connect to the endpoint `name` of the current user. `is_absent(&err)` = no instance running.
pub async fn connect(name: &str) -> io::Result<Stream> {
    imp::connect(name).await
}

// ---------------------------------------------------------------- token

/// Handshake token: 256 random bits, lowercase hex (64 chars).
pub fn new_token() -> io::Result<String> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Constant-time equality for tokens of equal length (false on any length difference).
pub fn token_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn is_valid_token(t: &str) -> bool {
    t.len() == 64
        && t.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Write `token` to `path` atomically (private temp file, then replace). Unix: mode 0600.
/// Windows: the file inherits the ACL of its directory (the user's profile).
pub fn write_token_file(path: &Path, token: &str) -> io::Result<()> {
    use std::io::Write;
    if !is_valid_token(token) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "malformed token",
        ));
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".eid-token-{}.tmp", new_token()?[..16].to_owned()));
    let mut f = crate::fs::create_new_private(&tmp)?;
    let r = f.write_all(token.as_bytes()).and_then(|_| f.sync_all());
    drop(f);
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    crate::fs::replace_atomic(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Read the token (never follows a symlink; bounded; must be well formed).
pub fn read_token_file(path: &Path) -> io::Result<String> {
    use std::io::Read;
    let mut f = crate::fs::open_regular_nofollow_read(path)?;
    let mut s = String::new();
    (&mut f).take(256).read_to_string(&mut s)?;
    let t = s.trim();
    if is_valid_token(t) {
        Ok(t.to_owned())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "malformed token file",
        ))
    }
}

/// Per-user configuration directory: `%APPDATA%\EidDownload` on Windows,
/// `$XDG_CONFIG_HOME/eid-download` (or `~/.config/eid-download`) elsewhere.
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .filter(|v| !v.is_empty())
            .map(|v| PathBuf::from(v).join("EidDownload"))
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|h| PathBuf::from(h).join(".config"))
            })?;
        Some(base.join("eid-download"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn unique(tag: &str) -> String {
        format!(
            "t{}-{}-{}",
            std::process::id(),
            tag,
            new_token().unwrap()[..8].to_owned()
        )
    }

    #[test]
    fn endpoint_names_are_validated() {
        for bad in [
            "",
            "a b",
            "../x",
            "a\\b",
            "x".repeat(33).as_str(),
            "é",
            "a/b",
            "a:b",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        for ok in ["main", "A-b_9", &"x".repeat(32)] {
            assert!(validate_name(ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn tokens_are_random_hex_and_compared_exactly() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(is_valid_token(&a));
        assert!(token_eq(&a, &a.clone()));
        assert!(!token_eq(&a, &b));
        assert!(!token_eq(&a, &a[..63]));
        assert!(!token_eq("", &a));
    }

    #[test]
    fn token_file_roundtrip_is_private_and_rejects_garbage() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("token");
        let t = new_token().unwrap();
        write_token_file(&p, &t).unwrap();
        assert_eq!(read_token_file(&p).unwrap(), t);
        if let Some(only) = crate::fs::owner_only(&p).unwrap() {
            assert!(only, "token file must be owner-only");
        }
        // replacing works and leaves no temp file behind
        let t2 = new_token().unwrap();
        write_token_file(&p, &t2).unwrap();
        assert_eq!(read_token_file(&p).unwrap(), t2);
        let leftovers: Vec<_> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, ["token"]);
        // garbage / malformed / refused
        assert!(write_token_file(&p, "short").is_err());
        std::fs::write(&p, "not a token").unwrap();
        assert!(read_token_file(&p).is_err());
        std::fs::write(&p, "x".repeat(10_000)).unwrap();
        assert!(read_token_file(&p).is_err());
        assert!(read_token_file(&d.path().join("missing")).is_err());
    }

    #[tokio::test]
    async fn listener_accepts_and_streams_bytes_both_ways() {
        let name = unique("echo");
        let mut l = Listener::bind(&name).unwrap();
        let n2 = name.clone();
        let client = tokio::spawn(async move {
            let mut c = connect(&n2).await.unwrap();
            c.write_all(b"ping").await.unwrap();
            let mut b = [0u8; 4];
            c.read_exact(&mut b).await.unwrap();
            b
        });
        let mut s = l.accept().await.unwrap();
        let mut b = [0u8; 4];
        s.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"ping");
        s.write_all(b"pong").await.unwrap();
        assert_eq!(&client.await.unwrap(), b"pong");
        // a second client works too (new instance / next accept)
        let n3 = name.clone();
        let c2 = tokio::spawn(async move { connect(&n3).await.unwrap() });
        let _s2 = l.accept().await.unwrap();
        let _ = c2.await.unwrap();
    }

    #[tokio::test]
    async fn second_bind_is_refused_while_the_first_is_alive_and_allowed_after() {
        let name = unique("single");
        let l = Listener::bind(&name).unwrap();
        let e = Listener::bind(&name).err().expect("second bind must fail");
        assert_eq!(e.kind(), io::ErrorKind::AddrInUse, "{e}");
        drop(l);
        let _again = Listener::bind(&name).expect("name is free after the first listener dropped");
    }

    #[tokio::test]
    async fn connecting_to_nothing_is_reported_as_absent() {
        let e = connect(&unique("none")).await.err().unwrap();
        assert!(is_absent(&e), "{e:?}");
        assert!(connect("../evil").await.is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_pipe_name_contains_the_user_sid_and_sid_looks_right() {
        let sid = imp::current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
        let p = imp::pipe_path("main").unwrap();
        assert_eq!(p, format!("\\\\.\\pipe\\EidDownload-{sid}-main"));
    }

    /// What the live DACL of a pipe handle contains, read structurally (never as SDDL text).
    #[cfg(windows)]
    struct DaclInfo {
        protected: bool,
        /// (ACE type, access mask, SID == the current user's SID) for every ACE.
        aces: Vec<(u8, u32, bool)>,
        sddl: String,
    }

    #[cfg(windows)]
    fn sddl_text(sd: *mut std::ffi::c_void) -> String {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
        // SAFETY: `sd` is a valid descriptor; the returned string is copied and freed.
        unsafe {
            let mut text: *mut u16 = std::ptr::null_mut();
            assert_ne!(
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    sd,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &mut text,
                    std::ptr::null_mut(),
                ),
                0
            );
            let mut n = 0;
            while *text.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
            LocalFree(text.cast());
            s
        }
    }

    #[cfg(windows)]
    fn dacl_of(handle: std::os::windows::io::RawHandle) -> DaclInfo {
        use std::ffi::c_void;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSidToSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
        };
        use windows_sys::Win32::Security::{
            EqualSid, GetAce, GetSecurityDescriptorControl, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
            DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
        };
        let mine: Vec<u16> = imp::current_user_sid()
            .unwrap()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: out-pointers are valid; every allocation is freed before returning; the ACE
        // pointers are only read while `sd` (which owns them) is alive.
        unsafe {
            let mut my_sid: *mut c_void = std::ptr::null_mut();
            assert_ne!(ConvertStringSidToSidW(mine.as_ptr(), &mut my_sid), 0);
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let rc = GetSecurityInfo(
                handle as _,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            );
            assert_eq!(rc, 0, "GetSecurityInfo failed: {rc}");
            assert!(!dacl.is_null(), "a NULL DACL means everyone has access");
            let (mut ctrl, mut rev) = (0u16, 0u32);
            assert_ne!(GetSecurityDescriptorControl(sd, &mut ctrl, &mut rev), 0);
            let mut aces = vec![];
            for i in 0..(*dacl).AceCount as u32 {
                let mut ace: *mut c_void = std::ptr::null_mut();
                assert_ne!(GetAce(dacl, i, &mut ace), 0);
                let hdr = &*(ace as *const ACE_HEADER);
                // allowed and denied ACEs share this layout (mask, then the SID)
                let a = &*(ace as *const ACCESS_ALLOWED_ACE);
                let sid = std::ptr::addr_of!(a.SidStart) as *mut c_void;
                aces.push((hdr.AceType, a.Mask, EqualSid(sid, my_sid) != 0));
            }
            let sddl = sddl_text(sd);
            let protected = ctrl & SE_DACL_PROTECTED != 0;
            LocalFree(sd);
            LocalFree(my_sid);
            DaclInfo {
                protected,
                aces,
                sddl,
            }
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_pipe_has_a_protected_dacl_with_only_the_current_user() {
        // Read the DACL back from the live pipe and check its STRUCTURE: protected, exactly one
        // ACE, an *allow* ACE whose SID is the current user's SID (EqualSid), non-empty mask.
        // This does not depend on how SDDL text spells the SID: for the built-in Administrator
        // (the CI runner's account) SDDL prints the alias `LA`, and `GA` as `FA`.
        // (A second-user attempt is MANUAL-TESTS #9 and is not claimed here.)
        use std::os::windows::io::AsRawHandle;
        let name = unique("dacl");
        let mut l = Listener::bind(&name).unwrap();
        let d = dacl_of(l.0.next.as_raw_handle());
        assert!(d.protected, "DACL must not inherit anything: {}", d.sddl);
        assert_eq!(d.aces.len(), 1, "exactly one ACE expected: {}", d.sddl);
        let (ace_type, mask, is_me) = d.aces[0];
        assert_eq!(ace_type, 0, "must be an ACCESS_ALLOWED ACE: {}", d.sddl);
        assert!(is_me, "the only ACE must be the current user: {}", d.sddl);
        assert_ne!(mask, 0, "{}", d.sddl);
        // the instance created for the NEXT client carries the same descriptor
        let n2 = name.clone();
        let c = tokio::spawn(async move { connect(&n2).await.unwrap() });
        let _s = l.accept().await.unwrap();
        let _c = c.await.unwrap();
        let d2 = dacl_of(l.0.next.as_raw_handle());
        assert!(
            d2.protected && d2.aces.len() == 1 && d2.aces[0].0 == 0 && d2.aces[0].2,
            "{}",
            d2.sddl
        );
    }

    /// Guards the premises of the test above: the checker must really tell "the current user"
    /// from Everyone and from other principals, and SDDL text really hides the account SID
    /// behind aliases (which is what broke the old string comparison on the CI runner).
    #[cfg(windows)]
    #[tokio::test]
    async fn the_dacl_checker_rejects_everyone_and_other_principals_and_sddl_text_uses_aliases() {
        use std::os::windows::io::AsRawHandle;
        for (tag, sddl) in [
            ("everyone", "D:P(A;;GA;;;WD)"),
            ("system", "D:P(A;;GA;;;SY)"),
        ] {
            let name = unique(tag);
            let path = imp::pipe_path(&name).unwrap();
            let sd = imp::descriptor_from_sddl(sddl).unwrap();
            let pipe = imp::create_instance(&path, true, &sd).unwrap();
            let d = dacl_of(pipe.as_raw_handle());
            assert_eq!(d.aces.len(), 1, "{}", d.sddl);
            assert!(
                !d.aces[0].2,
                "{tag}: must NOT look like the current user: {}",
                d.sddl
            );
        }
        // a deny-everything-but-me style DACL is not "one allow ACE for me" either
        let name = unique("mixed");
        let path = imp::pipe_path(&name).unwrap();
        let me = imp::current_user_sid().unwrap();
        let sd = imp::descriptor_from_sddl(&format!("D:P(A;;GA;;;{me})(A;;GA;;;WD)")).unwrap();
        let pipe = imp::create_instance(&path, true, &sd).unwrap();
        let d = dacl_of(pipe.as_raw_handle());
        assert_eq!(
            d.aces.len(),
            2,
            "an extra Everyone ACE must be visible: {}",
            d.sddl
        );

        // The SDDL spelling of this machine's RID-500 account is the alias LA (and the system maps GA to FA once the object exists):
        // exactly what the CI runner printed (D:P(A;;FA;;;LA)).
        let prefix = me.rsplit_once('-').unwrap().0; // S-1-5-21-a-b-c
        assert!(
            prefix.starts_with("S-1-5-21-"),
            "unexpected account SID {me}"
        );
        let admin = imp::descriptor_from_sddl(&format!("D:P(A;;GA;;;{prefix}-500)")).unwrap();
        let text = sddl_text(admin.raw());
        assert!(text.contains(";LA)"), "{text}");
        assert!(!text.contains("-500"), "{text}");
    }
}
