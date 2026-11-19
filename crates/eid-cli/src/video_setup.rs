//! v0.1: the one-time video setup for the desktop WINDOW only (never reachable from IPC or
//! Native Messaging: `eid serve` does not use this module, see crates/eid-media/tests/g2_tools.rs).
//! The window shows the agreement text, and only when the user accepts it there does it call
//! [`VideoSetup::install`], which runs `eid video setup --yes-i-agree` (records the agreement and
//! installs the pinned tools, the same tested path as the CLI).

use crate::media_jobs::VideoJobs;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    running: bool,
    error: Option<String>,
    stdin: Option<tokio::process::ChildStdin>,
}

#[derive(Clone)]
pub struct VideoSetup {
    jobs: VideoJobs,
    st: Arc<Mutex<State>>,
}

fn lock(m: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl VideoSetup {
    /// Runs its child through the same executable and environment as `jobs`.
    pub fn new(jobs: VideoJobs) -> VideoSetup {
        VideoSetup {
            jobs,
            st: Arc::new(Mutex::new(State::default())),
        }
    }

    /// `{installed, consent, installing, error, ytdlp_version, ffmpeg_version, deno_version}`
    /// (the versions are the pinned ones, present when installed).
    pub fn status(&self) -> Value {
        let mut versions = (None, None, None);
        let (consent, installed) = match (crate::video::manifest(), crate::video::stores()) {
            (Ok(m), Ok((s, t))) => {
                let installed = t.current(&m).is_ok();
                if installed {
                    let v = |n: &str| {
                        m.tools
                            .iter()
                            .find(|t| t.name == n)
                            .map(|t| t.version.clone())
                    };
                    versions = (v("yt-dlp"), v("ffmpeg"), v("deno"));
                }
                (
                    eid_media::consent::is_granted(&s, &m).unwrap_or(false),
                    installed,
                )
            }
            _ => (false, false),
        };
        let g = lock(&self.st);
        json!({"installed": installed, "consent": consent,
               "installing": g.running, "error": g.error,
               "ytdlp_version": versions.0, "ffmpeg_version": versions.1,
               "deno_version": versions.2})
    }

    /// The agreement text the window shows before [`Self::install`] (`ar` or `en`).
    pub fn consent_text(&self, lang: &str) -> Result<String, String> {
        let m = crate::video::manifest()?;
        let l = if lang == "ar" {
            eid_media::consent::Lang::Ar
        } else {
            eid_media::consent::Lang::En
        };
        Ok(eid_media::consent::consent_text(l, &m).text)
    }

    /// The user accepted the text in the window: record it and install the tools in the
    /// background. `accept` must be true (the window passes the user's explicit choice).
    pub fn install(&self, accept: bool) -> Result<(), String> {
        if !accept {
            return Err("the agreement is required".into());
        }
        let mut g = lock(&self.st);
        if g.running {
            return Ok(());
        }
        let args: Vec<OsString> = [
            "video",
            "setup",
            "--yes-i-agree",
            "--events",
            "--stop-on-stdin-eof",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        let mut child = self
            .jobs
            .command(&args)
            .spawn()
            .map_err(|e| e.to_string())?;
        g.running = true;
        g.error = None;
        g.stdin = child.stdin.take();
        drop(g);
        let me = self.clone();
        // never fired: setup is stopped by closing its input (app exit)
        let (_keep, krx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _keep = _keep;
            let (code, err) = me.jobs.drive(None, child, krx).await;
            let mut g = lock(&me.st);
            g.running = false;
            g.stdin = None;
            g.error = if code == Some(0) {
                None
            } else {
                Some(err.unwrap_or_else(|| "the video tools were not installed".into()))
            };
        });
        Ok(())
    }

    /// App exit: ask a running setup to stop (its download stays resumable).
    pub fn stop(&self) {
        lock(&self.st).stdin.take();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ipc_and_media_quality_lists_are_the_same() {
        assert_eq!(eid_ipc::VIDEO_QUALITIES, eid_media::args::QUALITY_CHOICES);
    }
}
