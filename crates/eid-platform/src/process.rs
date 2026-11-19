//! The only sanctioned way to start a child process.
//!
//! * never goes through a shell: program and arguments are passed as a vector;
//! * on Windows sets `CREATE_NO_WINDOW`, so no console window flashes (the Zajil video
//!   post-mortem: a black window per video). Grandchildren (e.g. ffmpeg started by yt-dlp)
//!   inherit the hidden console; that is verified by a Windows CI test in P4.

use std::ffi::OsStr;
use std::process::Command;

mod supervised;
pub use supervised::{
    process_is_alive, spawn, system_program_dirs, validate_program, Captured, ChildSpec, Finished,
    Keep, StreamSpec, Supervised, ENV_PASSTHROUGH,
};

/// `CREATE_NO_WINDOW` process-creation flag value (winbase.h).
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Build a [`Command`] for `program` with `args`. No shell is involved, ever.
pub fn command<I, S>(program: impl AsRef<OsStr>, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut c = Command::new(program);
    c.args(args);
    apply_no_window(&mut c);
    c
}

/// The async child type the app's own job processes use (see [`async_command`]).
pub use tokio::process::{
    Child as AsyncChild, ChildStdin as AsyncChildStdin, Command as AsyncCommand,
};

/// An async [`tokio::process::Command`] for `program` with `args`: no shell, no console window
/// on Windows, stdin/stdout/stderr piped, killed when dropped. Used by the desktop app's video
/// job layer to run its own `eid` executable.
pub fn async_command<I, S>(program: impl AsRef<OsStr>, args: I) -> AsyncCommand
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut c = tokio::process::Command::new(program);
    c.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// Apply the "no console window" policy to an existing command.
pub fn apply_no_window(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = c;
    }
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{
        GetHandleInformation, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    /// Clears `HANDLE_FLAG_INHERIT` on this process's standard handles and restores it on drop.
    pub struct NoInheritStdio(Vec<HANDLE>);

    impl NoInheritStdio {
        pub fn new() -> NoInheritStdio {
            let mut changed = vec![];
            for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                // SAFETY: plain handle queries; a handle we do not own is only flag-toggled
                // and restored.
                unsafe {
                    let h = GetStdHandle(which);
                    if h.is_null() || h == INVALID_HANDLE_VALUE {
                        continue;
                    }
                    let mut flags = 0u32;
                    if GetHandleInformation(h, &mut flags) != 0
                        && flags & HANDLE_FLAG_INHERIT != 0
                        && SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) != 0
                    {
                        changed.push(h);
                    }
                }
            }
            NoInheritStdio(changed)
        }
    }

    impl Drop for NoInheritStdio {
        fn drop(&mut self) {
            for h in &self.0 {
                // SAFETY: restoring the flag we cleared on a handle that is still open.
                unsafe { SetHandleInformation(*h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
            }
        }
    }
}

/// Start `program` fully detached from this process: no console window, no inherited stdio, in
/// its own process group, and (Windows) outside the parent's job object when the job allows
/// breakaway, so a browser that kills its native-messaging host does not take the app with it.
/// Never goes through a shell. Returns once the child has been created (it is not waited for).
pub fn spawn_detached<I, S>(program: impl AsRef<OsStr>, args: I) -> std::io::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    use std::process::Stdio;
    let mut c = Command::new(program);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Our own stdio handles (the browser's pipes, when we are a native-messaging host) are
    // inheritable; `CreateProcess` would hand them to the app, which would then keep the
    // browser's pipes open forever. Make them non-inheritable until the child exists (this
    // guard must outlive BOTH spawn attempts below).
    #[cfg(windows)]
    let _no_inherit = win::NoInheritStdio::new();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let base = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
        c.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB);
        match c.spawn() {
            Ok(_) => return Ok(()),
            // the parent's job forbids breakaway: start without it
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                c.creation_flags(base);
            }
            Err(e) => return Err(e),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    c.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_flag_value_is_create_no_window() {
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
    }

    #[test]
    fn security_args_are_passed_literally_without_shell() {
        // A hostile "filename" must reach the child as one literal argument.
        let hostile = "$(touch PWNED)`id`;|&.zip";
        let out = command("printf", ["%s", hostile]);
        #[cfg(unix)]
        {
            let mut out = out;
            let o = out.output().unwrap();
            assert_eq!(String::from_utf8_lossy(&o.stdout), hostile);
        }
        #[cfg(not(unix))]
        {
            let _ = out;
        }
    }
}
