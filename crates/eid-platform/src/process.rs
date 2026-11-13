//! The only sanctioned way to start a child process.
//!
//! * never goes through a shell: program and arguments are passed as a vector;
//! * on Windows sets `CREATE_NO_WINDOW`, so no console window flashes (the Zajil video
//!   post-mortem: a black window per video). Grandchildren (e.g. ffmpeg started by yt-dlp)
//!   inherit the hidden console; that is verified by a Windows CI test in P4.

use std::ffi::OsStr;
use std::process::Command;

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
