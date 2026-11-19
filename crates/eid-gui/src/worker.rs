//! v0.2: the desktop app's own video worker. Video jobs, the video probe and the tool setup run
//! as a CHILD process (separate so a stop kills the whole yt-dlp/FFmpeg tree, exactly as
//! before), but that child is now `EidDownload.exe --eid-video-worker video ...` instead of a
//! second program `eid.exe` next to it: a window started from inside the ZIP (Windows extracts
//! only the clicked file to a temporary folder) or a removed `eid.exe` failed with "The system
//! cannot find the file specified (os error 2)" (owner's test, 2026-11-19).
//!
//! Least privilege: only `video ...` is accepted here; `eid.exe` keeps the whole command line.

/// The private first argument that turns `EidDownload.exe` into the video worker.
pub const VIDEO_WORKER_FLAG: &str = "--eid-video-worker";

/// What the worker may run: `video <anything eid video accepts>`, nothing else.
pub fn worker_command(args: &[String]) -> Result<&[String], String> {
    match args.split_first() {
        Some((v, rest)) if v == "video" => Ok(rest),
        _ => Err("the video worker only runs `video ...` commands".into()),
    }
}

/// Run the worker and return its exit code (no window, no single-instance check).
pub fn run(args: &[String]) -> i32 {
    let rest = match worker_command(args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    rt.block_on(eid_cli::video::main_args(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_runs_video_commands_only() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            worker_command(&v(&["video", "probe", "--", "u"])).unwrap(),
            &v(&["probe", "--", "u"])[..]
        );
        for bad in [
            v(&[]),
            v(&["get", "u"]),
            v(&["serve"]),
            v(&["add", "u"]),
            v(&["queue", "run"]),
            v(&["--", "video"]),
        ] {
            assert!(worker_command(&bad).is_err(), "{bad:?}");
        }
    }
}
