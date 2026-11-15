//! Running yt-dlp under supervision with the rules of this crate.

use crate::args::{self, Action, MediaUrl, Quality, ToolPaths};
use crate::info::{self, MediaInfo, PlaylistInfo};
use crate::progress::{self, Progress};
use crate::{hints, redact, MediaError};
use eid_platform::process::{self as pp, ChildSpec, Finished, Keep, StreamSpec};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

/// Largest `-J` document of a single video we read.
pub const INFO_STDOUT_LIMIT: usize = 16 * 1024 * 1024;
/// Largest `--flat-playlist -J` document we read.
pub const PLAYLIST_STDOUT_LIMIT: usize = 32 * 1024 * 1024;
/// stdout kept from a download run (progress lines are streamed, not kept).
pub const DOWNLOAD_STDOUT_LIMIT: usize = 64 * 1024;
/// Tail of stderr kept for error messages.
pub const STDERR_LIMIT: usize = 64 * 1024;
/// Most formats accepted for one video.
pub const MAX_FORMATS: usize = 2000;
/// Most entries accepted for one playlist.
pub const MAX_ENTRIES: usize = 20_000;
/// Longest id/title/url kept from the tool (bytes).
pub const MAX_TEXT: usize = 2048;
/// Length of the redacted error excerpt in a [`MediaError::ToolFailed`].
pub const ERROR_EXCERPT: usize = 2000;

/// A configured yt-dlp: tool paths and the child environment.
#[derive(Clone, Debug)]
pub struct Runner {
    pub tools: ToolPaths,
    pub env: Vec<(OsString, OsString)>,
}

impl Runner {
    /// `lookup` reads our own environment (see [`crate::env::child_env`]). The tools
    /// directory on the child's `PATH` is the directory of `yt-dlp`.
    pub fn new(
        tools: ToolPaths,
        lookup: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Runner, MediaError> {
        tools.validate()?;
        let dir = tools
            .ytdlp
            .parent()
            .ok_or_else(|| MediaError::BadToolPath("yt-dlp has no parent folder".into()))?
            .to_path_buf();
        Ok(Runner {
            env: crate::env::child_env(&dir, lookup),
            tools,
        })
    }

    /// The exact child specification for `args` in `cwd` (exposed for tests).
    pub fn spec(&self, cwd: &Path, args: Vec<OsString>, stdout_limit: usize) -> ChildSpec {
        ChildSpec {
            program: self.tools.ytdlp.clone(),
            args,
            cwd: cwd.to_path_buf(),
            env: self.env.clone(),
            stdout: StreamSpec::capture(stdout_limit, Keep::Head),
            stderr: StreamSpec::capture(STDERR_LIMIT, Keep::Tail),
        }
    }

    async fn run(&self, spec: ChildSpec) -> Result<Finished, MediaError> {
        let f = pp::spawn(spec)?.wait().await?;
        if !f.status.success() {
            return Err(failure(&f));
        }
        Ok(f)
    }

    /// `-J` of one video. `cwd` must be a directory we own (a staging directory).
    pub async fn info(&self, url: &MediaUrl, cwd: &Path) -> Result<MediaInfo, MediaError> {
        let a = args::build(&self.tools, url, &Action::Info)?;
        let f = self.run(self.spec(cwd, a, INFO_STDOUT_LIMIT)).await?;
        info::parse_video(&f.stdout, INFO_STDOUT_LIMIT)
    }

    /// `--flat-playlist -J` of a playlist.
    pub async fn playlist(&self, url: &MediaUrl, cwd: &Path) -> Result<PlaylistInfo, MediaError> {
        let a = args::build(&self.tools, url, &Action::PlaylistInfo)?;
        let f = self.run(self.spec(cwd, a, PLAYLIST_STDOUT_LIMIT)).await?;
        info::parse_playlist(&f.stdout, PLAYLIST_STDOUT_LIMIT)
    }

    /// Download one item into `staging` (the working directory). Progress samples are sent
    /// to `progress` (lossy). Returns the finished run; publishing is the caller's job.
    pub async fn download(
        &self,
        url: &MediaUrl,
        staging: &Path,
        quality: Quality,
        limit_rate: Option<u64>,
        archive: bool,
        progress: Option<mpsc::Sender<Progress>>,
    ) -> Result<Finished, MediaError> {
        let a = args::build(
            &self.tools,
            url,
            &Action::Download {
                quality,
                limit_rate,
                archive,
            },
        )?;
        let mut spec = self.spec(staging, a, DOWNLOAD_STDOUT_LIMIT);
        let forward = progress.map(|out| {
            let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
            spec.stdout.lines = Some(tx);
            spec.stdout.max_line = 512;
            tokio::spawn(async move {
                while let Some(line) = rx.recv().await {
                    if let Some(p) = progress::parse_line(&line) {
                        let _ = out.try_send(p);
                    }
                }
            })
        });
        let r = self.run(spec).await;
        if let Some(t) = forward {
            let _ = t.await;
        }
        r
    }
}

/// The error of a failed run: redacted tail of stderr plus a hint.
pub fn failure(f: &Finished) -> MediaError {
    let text = redact::redact_tool_text(&String::from_utf8_lossy(&f.stderr.data));
    let hint = hints::classify(&text);
    let mut start = text.len().saturating_sub(ERROR_EXCERPT);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    MediaError::ToolFailed {
        hint,
        message: text[start..].trim().to_string(),
    }
}

/// Directory of the tools (first entry of the child's `PATH`).
pub fn tools_dir(tools: &ToolPaths) -> Option<PathBuf> {
    tools.ytdlp.parent().map(Path::to_path_buf)
}
