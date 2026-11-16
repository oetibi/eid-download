//! Supervised child processes (P4): the only way the product starts an external tool such as
//! yt-dlp.
//!
//! * no shell, ever: the program is an absolute path to a regular file and the arguments are a
//!   vector; `.bat`/`.cmd` are refused (on Windows `CreateProcess` would run them through
//!   `cmd.exe`, whose own parsing is an injection surface);
//! * the environment is cleared and replaced by exactly what the caller passes (an allowlist
//!   built by the caller), the working directory is explicit, stdin is null;
//! * the whole process **tree** is owned: Windows starts the child `CREATE_SUSPENDED |
//!   CREATE_NO_WINDOW`, assigns it to a fresh Job object with `KILL_ON_JOB_CLOSE`, and only then
//!   resumes it, so not even a grandchild spawned in the first instruction escapes the job;
//!   Unix puts the child in its own process group and kills the group;
//! * stdout and stderr are drained concurrently by two tasks, each keeping at most a caller-set
//!   number of bytes (the rest is counted and discarded), so a flooding tool can neither block
//!   on a full pipe nor exhaust our memory;
//! * dropping a [`Supervised`] (cancellation) kills the tree; when the direct child exits the
//!   rest of the tree is killed too (nothing may outlive the job it belongs to).

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Names of environment variables the OS itself needs in a child, to be copied from our own
/// environment by an allowlist builder (everything else is the caller's explicit choice).
#[cfg(windows)]
pub const ENV_PASSTHROUGH: &[&str] = &["SystemRoot", "TEMP", "TMP"];
/// Names of environment variables the OS itself needs in a child (see the Windows variant).
#[cfg(not(windows))]
pub const ENV_PASSTHROUGH: &[&str] = &["TMPDIR"];

/// Directories that hold the operating system's own programs (`%SystemRoot%\System32`; on
/// Unix `/usr/bin` and `/bin`), for a child's `PATH` next to the tools directory.
pub fn system_program_dirs() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|p| vec![p.join("System32")])
            .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
    }
}

/// Which part of an over-long stream is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// The first `cap` bytes (e.g. a JSON document: a cut one is refused anyway).
    Head,
    /// The last `cap` bytes (e.g. an error log: the final lines carry the error).
    Tail,
}

/// How one output stream is handled.
#[derive(Clone, Debug)]
pub struct StreamSpec {
    /// Maximum bytes retained in memory.
    pub cap: usize,
    pub keep: Keep,
    /// Optional per-line forwarding (lines split at `\n` or `\r`, each cut to `max_line` bytes).
    /// Lossy by design: when the receiver is slow, lines are dropped, never queued unboundedly.
    pub lines: Option<mpsc::Sender<Vec<u8>>>,
    pub max_line: usize,
}

impl StreamSpec {
    pub fn capture(cap: usize, keep: Keep) -> StreamSpec {
        StreamSpec {
            cap,
            keep,
            lines: None,
            max_line: 4096,
        }
    }
}

/// Everything needed to start one supervised child.
#[derive(Clone, Debug)]
pub struct ChildSpec {
    /// Absolute path of a regular file; never looked up in `PATH`.
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Absolute path of an existing directory.
    pub cwd: PathBuf,
    /// The child's complete environment (ours is not inherited).
    pub env: Vec<(OsString, OsString)>,
    pub stdout: StreamSpec,
    pub stderr: StreamSpec,
}

/// Bytes kept from one stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Captured {
    pub data: Vec<u8>,
    /// Bytes the child wrote in total (saturating).
    pub total: u64,
    /// `total > data.len()`: part of the stream was discarded.
    pub truncated: bool,
}

/// Result of a child that ran to its end.
#[derive(Debug)]
pub struct Finished {
    pub status: ExitStatus,
    pub stdout: Captured,
    pub stderr: Captured,
}

/// A running child that owns its process tree.
pub struct Supervised {
    child: tokio::process::Child,
    tree: Tree,
    out: Option<JoinHandle<io::Result<Captured>>>,
    err: Option<JoinHandle<io::Result<Captured>>>,
    /// The direct child has been reaped: from here on its id may belong to someone else, so
    /// the tree must never be signalled again (P4 review M5).
    reaped: bool,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// Refuses what must never be started (see the module docs). Pure, so it is tested directly.
pub fn validate_program(program: &Path) -> io::Result<()> {
    if !program.is_absolute() {
        return Err(invalid(format!(
            "refusing to start a program that is not an absolute path (it would be looked up \
             in PATH): {}",
            program.display()
        )));
    }
    let ext = program
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if matches!(ext.as_deref(), Some("bat" | "cmd")) {
        return Err(invalid(format!(
            "refusing to start a batch file (it would run through cmd.exe): {}",
            program.display()
        )));
    }
    Ok(())
}

/// Start `spec` under supervision. Must be called inside a tokio runtime.
pub fn spawn(spec: ChildSpec) -> io::Result<Supervised> {
    validate_program(&spec.program)?;
    let meta = std::fs::metadata(&spec.program)?;
    if !meta.is_file() {
        return Err(invalid(format!(
            "not a regular file: {}",
            spec.program.display()
        )));
    }
    if !spec.cwd.is_absolute() || !spec.cwd.is_dir() {
        return Err(invalid(format!(
            "working directory must be an existing absolute directory: {}",
            spec.cwd.display()
        )));
    }
    let mut c = tokio::process::Command::new(&spec.program);
    c.args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let pending = Tree::start(&mut c)?;
    let mut child = spawn_retrying_busy(&mut c)?;
    let tree = match pending.adopt(&child) {
        Ok(t) => t,
        Err(e) => {
            // never leave a suspended or unowned child behind
            let _ = child.start_kill();
            return Err(e);
        }
    };
    let out = child
        .stdout
        .take()
        .map(|s| tokio::spawn(drain(s, spec.stdout)));
    let err = child
        .stderr
        .take()
        .map(|s| tokio::spawn(drain(s, spec.stderr)));
    Ok(Supervised {
        child,
        tree,
        out,
        err,
        reaped: false,
    })
}

/// Linux refuses to exec a file that some process holds open for writing (`ETXTBSY`). A tool
/// we have just installed and closed can still be "busy" for a moment when another thread of
/// ours forked in between (the child holds a copy of the descriptor until its own exec closes
/// it). Retry up to 5 times (pauses of 10..50 ms, 150 ms in all: a short blocking pause of the
/// calling thread, like the process creation itself); any other error is returned at once.
fn spawn_retrying_busy(c: &mut tokio::process::Command) -> io::Result<tokio::process::Child> {
    const ETXTBSY: i32 = 26;
    let mut tries = 0;
    loop {
        match c.spawn() {
            Err(e) if cfg!(unix) && e.raw_os_error() == Some(ETXTBSY) && tries < 5 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(10 * tries));
            }
            r => return r,
        }
    }
}

impl Supervised {
    /// Process id of the direct child.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Whether `pid` belongs to this child's tree (Windows: the Job object; Unix: the process
    /// group). Used by tests and diagnostics.
    pub fn tree_contains(&self, pid: u32) -> io::Result<bool> {
        self.tree.contains(pid)
    }

    /// Kill every process of the tree now (idempotent).
    pub fn kill_tree(&self) {
        self.tree.kill();
    }

    /// Wait for the direct child to exit, kill whatever is left of its tree, and collect the
    /// output once both pipes reached end-of-file. Dropping this future kills the tree.
    ///
    /// The rest of the tree is killed while the direct child still holds its id (on Unix: it
    /// has exited but is not reaped yet, so its process-group id cannot be reused); only then
    /// is it reaped, and after that the tree is never signalled again.
    pub async fn wait(mut self) -> io::Result<Finished> {
        let status = self.tree.finish(&mut self.child).await?;
        self.reaped = true;
        let stdout = join(self.out.take()).await?;
        let stderr = join(self.err.take()).await?;
        Ok(Finished {
            status,
            stdout,
            stderr,
        })
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        // after a reap the id is free (Unix: a new process group may already use it); the
        // tree was killed before the reap in `wait`
        if !self.reaped {
            self.tree.kill();
            let _ = self.child.start_kill();
        }
    }
}

async fn join(h: Option<JoinHandle<io::Result<Captured>>>) -> io::Result<Captured> {
    match h {
        None => Ok(Captured::default()),
        Some(h) => h.await.map_err(io::Error::other)?,
    }
}

async fn drain(mut r: impl tokio::io::AsyncRead + Unpin, spec: StreamSpec) -> io::Result<Captured> {
    let mut cap = Captured::default();
    let mut buf = vec![0u8; 64 * 1024];
    let mut line: Vec<u8> = Vec::new();
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        cap.total = cap.total.saturating_add(n as u64);
        keep_bytes(&mut cap.data, chunk, spec.cap, spec.keep);
        if let Some(tx) = &spec.lines {
            for &b in chunk {
                if b == b'\n' || b == b'\r' {
                    if !line.is_empty() {
                        let _ = tx.try_send(std::mem::take(&mut line));
                    }
                } else if line.len() < spec.max_line {
                    line.push(b);
                }
            }
        }
    }
    if let Some(tx) = &spec.lines {
        if !line.is_empty() {
            let _ = tx.try_send(line);
        }
    }
    cap.truncated = cap.total > cap.data.len() as u64;
    Ok(cap)
}

fn keep_bytes(data: &mut Vec<u8>, chunk: &[u8], cap: usize, keep: Keep) {
    match keep {
        Keep::Head => {
            let room = cap.saturating_sub(data.len());
            data.extend_from_slice(&chunk[..chunk.len().min(room)]);
        }
        Keep::Tail => {
            if chunk.len() >= cap {
                data.clear();
                data.extend_from_slice(&chunk[chunk.len() - cap..]);
            } else {
                data.extend_from_slice(chunk);
                if data.len() > cap {
                    let cut = data.len() - cap;
                    data.drain(..cut);
                }
            }
        }
    }
}

/// Whether a process with this id is still running (not exited; on Linux zombies count as
/// exited). For tests and diagnostics; pids can be reused, so only meaningful shortly after
/// the pid was observed.
pub fn process_is_alive(pid: u32) -> bool {
    imp::process_is_alive(pid)
}

use imp::Tree;

#[cfg(windows)]
mod imp {
    use std::io;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenThread, ResumeThread, WaitForSingleObject, CREATE_SUSPENDED,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, THREAD_SUSPEND_RESUME,
    };

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: we own this handle and close it exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }
    // SAFETY: a kernel handle value may be used from any thread.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    /// The Job object that owns the child's tree. Closing it kills the tree
    /// (`KILL_ON_JOB_CLOSE`), so even a crash of our process cannot leave tools running.
    pub struct Tree {
        job: Handle,
    }

    pub struct Pending {
        job: Handle,
    }

    impl Tree {
        pub fn start(c: &mut tokio::process::Command) -> io::Result<Pending> {
            // SAFETY: plain FFI; a null name and null attributes create an anonymous,
            // non-inheritable job.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Handle(job);
            // SAFETY: zero is a valid bit pattern for this plain-data struct.
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `info` is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION of the size passed.
            let ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            c.creation_flags(super::super::CREATE_NO_WINDOW | CREATE_SUSPENDED);
            Ok(Pending { job })
        }

        pub fn contains(&self, pid: u32) -> io::Result<bool> {
            // SAFETY: plain FFI; the returned handle is owned by `Handle`.
            let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if h.is_null() {
                return Err(io::Error::last_os_error());
            }
            let h = Handle(h);
            let mut r = 0;
            // SAFETY: both handles are valid; `r` is a valid out pointer.
            if unsafe { IsProcessInJob(h.0, self.job.0, &mut r) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(r != 0)
        }

        pub fn kill(&self) {
            // SAFETY: valid job handle; terminating an empty or finished job is harmless.
            unsafe { TerminateJobObject(self.job.0, 1) };
        }

        /// Wait for the direct child, then kill what is left of the job. The job handle is
        /// ours alone, so killing after the child is reaped cannot reach anyone else.
        pub async fn finish(
            &self,
            child: &mut tokio::process::Child,
        ) -> io::Result<std::process::ExitStatus> {
            let status = child.wait().await?;
            self.kill();
            Ok(status)
        }
    }

    impl Pending {
        /// Assign the suspended child to the job, then resume its threads. Fails closed: the
        /// caller kills the child on error.
        pub fn adopt(self, child: &tokio::process::Child) -> io::Result<Tree> {
            let (Some(h), Some(pid)) = (child.raw_handle(), child.id()) else {
                return Err(io::Error::other("child has no handle"));
            };
            // SAFETY: both handles are valid for the duration of the call.
            if unsafe { AssignProcessToJobObject(self.job.0, h as HANDLE) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let resumed = resume_threads_of(pid)?;
            if resumed == 0 {
                return Err(io::Error::other(
                    "no thread of the new child could be resumed",
                ));
            }
            Ok(Tree { job: self.job })
        }
    }

    /// Resume every thread of `pid` (documented Toolhelp32 enumeration + `ResumeThread`).
    fn resume_threads_of(pid: u32) -> io::Result<usize> {
        // SAFETY: plain FFI; the snapshot handle is owned by `Handle`.
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snap == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snap = Handle(snap);
        // SAFETY: zero is a valid bit pattern; dwSize is set as the API requires.
        let mut te: THREADENTRY32 = unsafe { std::mem::zeroed() };
        te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut n = 0;
        // SAFETY: valid snapshot handle and a correctly sized entry.
        let mut more = unsafe { Thread32First(snap.0, &mut te) } != 0;
        while more {
            if te.th32OwnerProcessID == pid {
                // SAFETY: plain FFI; the thread handle is owned by `Handle`.
                let th = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, te.th32ThreadID) };
                if th.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let th = Handle(th);
                // SAFETY: valid thread handle with THREAD_SUSPEND_RESUME access.
                if unsafe { ResumeThread(th.0) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                n += 1;
            }
            // SAFETY: as above.
            more = unsafe { Thread32Next(snap.0, &mut te) } != 0;
        }
        Ok(n)
    }

    pub fn process_is_alive(pid: u32) -> bool {
        // SAFETY: plain FFI; the handle is owned by `Handle`.
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if h.is_null() {
            return false;
        }
        let h = Handle(h);
        // SAFETY: valid process handle with SYNCHRONIZE access; zero timeout never blocks.
        unsafe { WaitForSingleObject(h.0, 0) == WAIT_TIMEOUT }
    }
}

#[cfg(unix)]
mod imp {
    use std::io;

    /// The child's process group (the child is its leader).
    ///
    /// A process-group id is the leader's pid. While the leader exists, even as an exited but
    /// unreaped zombie, that id cannot be reused, so `killpg` reaches only our group. Once the
    /// leader is reaped and the group is empty, the id is free and a new, unrelated group may
    /// take it: the group is therefore signalled only while the leader is still our unreaped
    /// child (P4 review M5).
    pub struct Tree {
        pgid: i32,
        #[cfg(test)]
        pub(super) stats: std::sync::Arc<KillStats>,
    }

    /// How often the group was signalled, and how often a signal was withheld because the
    /// leader was already reaped (unit tests only).
    #[cfg(test)]
    #[derive(Default, Debug)]
    pub struct KillStats {
        pub sent: std::sync::atomic::AtomicUsize,
        pub withheld_after_reap: std::sync::atomic::AtomicUsize,
    }

    pub struct Pending;

    /// `waitid(P_PID, pid, WEXITED | WNOHANG | WNOWAIT)`: never reaps. `Ok(Some(true))` = the
    /// child has exited (zombie), `Ok(Some(false))` = still running, `Ok(None)` = not our
    /// child any more (already reaped).
    pub(super) fn peek_exit(pid: i32) -> io::Result<Option<bool>> {
        let id = libc::id_t::try_from(pid).map_err(io::Error::other)?;
        // SAFETY: zero is a valid bit pattern for siginfo_t; `info` is a valid out pointer.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: plain libc call with a valid out pointer.
        let r = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if r != 0 {
            let e = io::Error::last_os_error();
            return if e.raw_os_error() == Some(libc::ECHILD) {
                Ok(None)
            } else {
                Err(e)
            };
        }
        // SAFETY: waitid succeeded, so `info` was filled in; with WNOHANG and nothing to report
        // si_pid stays 0.
        Ok(Some(unsafe { info.si_pid() } != 0))
    }

    impl Tree {
        pub fn start(c: &mut tokio::process::Command) -> io::Result<Pending> {
            c.process_group(0);
            Ok(Pending)
        }

        /// Wait until the leader has exited **without reaping it**, kill the rest of its group
        /// while its id is still held, then reap it.
        pub async fn finish(
            &self,
            child: &mut tokio::process::Child,
        ) -> io::Result<std::process::ExitStatus> {
            use tokio::signal::unix::{signal, SignalKind};
            // registered before the first check, so a SIGCHLD in between is not lost; the
            // timeout is only a safety net
            let mut sigchld = signal(SignalKind::child())?;
            loop {
                match peek_exit(self.pgid)? {
                    Some(true) => break,
                    Some(false) => {
                        tokio::select! {
                            _ = sigchld.recv() => {}
                            _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
                        }
                    }
                    // someone else reaped it: its id may already be reused, do not signal
                    None => return child.wait().await,
                }
            }
            self.kill();
            child.wait().await
        }

        pub fn contains(&self, pid: u32) -> io::Result<bool> {
            let pid = i32::try_from(pid).map_err(io::Error::other)?;
            // SAFETY: plain libc call.
            let g = unsafe { libc::getpgid(pid) };
            if g < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(g == self.pgid)
        }

        /// Kill the group, but only while the leader is still our unreaped child (see the type
        /// docs). Only this object reaps the leader, and never at the same time as this call,
        /// so the check cannot go stale before `killpg`.
        pub fn kill(&self) {
            if !matches!(peek_exit(self.pgid), Ok(Some(_))) {
                #[cfg(test)]
                self.stats
                    .withheld_after_reap
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return;
            }
            // SAFETY: plain libc call; ESRCH (group already gone) is fine.
            unsafe { libc::killpg(self.pgid, libc::SIGKILL) };
            #[cfg(test)]
            self.stats
                .sent
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl Pending {
        pub fn adopt(self, child: &tokio::process::Child) -> io::Result<Tree> {
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("child has no pid"))?;
            Ok(Tree {
                pgid: i32::try_from(pid).map_err(io::Error::other)?,
                #[cfg(test)]
                stats: Default::default(),
            })
        }
    }

    pub fn process_is_alive(pid: u32) -> bool {
        let Ok(p) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 only checks existence/permission.
        if unsafe { libc::kill(p, 0) } != 0 {
            return false;
        }
        // a zombie has exited; Linux exposes the state in /proc
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(s) => s
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .is_some_and(|state| state != "Z" && state != "X"),
            Err(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_programs_and_batch_files_are_refused() {
        assert!(validate_program(Path::new("yt-dlp")).is_err());
        assert!(validate_program(Path::new("tools/yt-dlp.exe")).is_err());
        let abs = std::env::temp_dir();
        for bad in ["x.bat", "x.CMD", "x.Bat"] {
            let e = validate_program(&abs.join(bad)).unwrap_err();
            assert!(e.to_string().contains("batch"), "{e}");
        }
        assert!(validate_program(&abs.join("yt-dlp.exe")).is_ok());
    }

    /// `/bin/sh -c <script>` with a minimal environment, stdout lines forwarded.
    #[cfg(unix)]
    fn sh(script: &str, lines: mpsc::Sender<Vec<u8>>) -> ChildSpec {
        let mut stdout = StreamSpec::capture(4096, Keep::Head);
        stdout.lines = Some(lines);
        ChildSpec {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            stdout,
            stderr: StreamSpec::capture(4096, Keep::Tail),
        }
    }

    #[cfg(unix)]
    async fn first_pid(rx: &mut mpsc::Receiver<Vec<u8>>) -> u32 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(20), rx.recv())
            .await
            .expect("the child never printed its grandchild's pid")
            .unwrap();
        String::from_utf8(line).unwrap().trim().parse().unwrap()
    }

    #[cfg(unix)]
    async fn wait_dead(pid: u32, what: &str) {
        let t = std::time::Instant::now();
        while process_is_alive(pid) {
            assert!(
                t.elapsed() < std::time::Duration::from_secs(15),
                "{what} {pid} is still alive"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    // P4 review M5: the leader exits at once and leaves a grandchild in its group. The group
    // must be killed while the leader is still an unreaped zombie (its id cannot be reused
    // then), and after the reap nothing may signal that id again, not even Drop.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_process_group_is_killed_before_its_leader_is_reaped() {
        use std::sync::atomic::Ordering::SeqCst;
        let (tx, mut rx) = mpsc::channel(8);
        let sup = spawn(sh("sleep 600 & echo $!; exit 0", tx)).unwrap();
        let leader = sup.id().unwrap();
        let stats = sup.tree.stats.clone();
        let grandchild = first_pid(&mut rx).await;
        // the grandchild holds the stdout pipe: `wait` only returns once the group was killed
        let f = tokio::time::timeout(std::time::Duration::from_secs(30), sup.wait())
            .await
            .expect("wait never returned: the leftover grandchild was not killed")
            .unwrap();
        assert!(f.status.success(), "{:?}", f.status);
        assert_eq!(stats.sent.load(SeqCst), 1, "the group was signalled once");
        assert_eq!(
            stats.withheld_after_reap.load(SeqCst),
            0,
            "something tried to signal the group after the leader was reaped"
        );
        assert!(
            matches!(imp::peek_exit(leader as i32), Ok(None)),
            "the leader was not reaped"
        );
        wait_dead(grandchild, "grandchild").await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_a_running_child_signals_its_group_once_while_the_leader_exists() {
        use std::sync::atomic::Ordering::SeqCst;
        let (tx, mut rx) = mpsc::channel(8);
        let sup = spawn(sh("sleep 600 & echo $!; sleep 600", tx)).unwrap();
        let leader = sup.id().unwrap();
        let stats = sup.tree.stats.clone();
        let grandchild = first_pid(&mut rx).await;
        drop(sup);
        assert_eq!(stats.sent.load(SeqCst), 1);
        assert_eq!(stats.withheld_after_reap.load(SeqCst), 0);
        wait_dead(leader, "leader").await;
        wait_dead(grandchild, "grandchild").await;
    }

    #[test]
    fn head_and_tail_keep_exactly_the_cap() {
        let mut h = vec![];
        let mut t = vec![];
        for i in 0..1000u32 {
            let chunk = i.to_le_bytes();
            keep_bytes(&mut h, &chunk, 10, Keep::Head);
            keep_bytes(&mut t, &chunk, 10, Keep::Tail);
        }
        assert_eq!(h, [0, 0, 0, 0, 1, 0, 0, 0, 2, 0]);
        let mut want = vec![];
        for i in 997..1000u32 {
            want.extend_from_slice(&i.to_le_bytes());
        }
        assert_eq!(t, want[2..]);
        let mut big = vec![];
        keep_bytes(&mut big, &[7u8; 100], 10, Keep::Tail);
        assert_eq!(big, [7u8; 10]);
    }
}
