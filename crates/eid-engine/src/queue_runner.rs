//! The production [`JobRunner`]: each queue item is one engine download, resumed from its sidecar
//! when one exists.

use crate::config::Config;
use crate::download::{self, Event, JobSpec, STATE_SUFFIX};
use crate::error::Error;
use crate::scheduler::{JobRunner, RunJob, RunResult};
use crate::state;
use futures_util::future::BoxFuture;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Byte progress of one queue item: `(item, done, total)`. Called often (per received chunk), so it
/// must be cheap and must not block.
pub type ProgressSink = Arc<dyn Fn(u64, u64, Option<u64>) + Send + Sync>;

pub struct EngineRunner {
    pub cfg: Config,
    /// Where byte progress goes (the desktop app's progress bars); `None` drops it.
    pub progress: Option<ProgressSink>,
    /// v0.1: connections per download changed at run time (the desktop app's settings); read
    /// when each download starts. `None` = `cfg.connections`.
    pub connections_live: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

impl EngineRunner {
    pub fn new(cfg: Config) -> EngineRunner {
        EngineRunner {
            cfg,
            progress: None,
            connections_live: None,
        }
    }
}

/// `base/subdir`, created if needed. `subdir` is one validated plain component; an existing entry
/// of that name must be a real directory (a planted symlink is refused), so a queue can never be
/// redirected out of its download directory through its sub-folders.
pub fn ensure_job_dir(base: &Path, subdir: Option<&str>) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(base).map_err(Error::io("create download directory"))?;
    let Some(sub) = subdir else {
        return Ok(base.to_path_buf());
    };
    if !crate::sanitize::is_safe_stored_name(sub) {
        return Err(Error::InvalidInput("unsafe sub-folder name".into()));
    }
    let dir = base.join(sub);
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.file_type().is_dir() => Ok(dir),
        Ok(_) => Err(Error::InvalidInput(
            "the sub-folder name is taken by something that is not a plain directory".into(),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&dir).map_err(Error::io("create sub-folder"))?;
            Ok(dir)
        }
        Err(e) => Err(Error::io("inspect sub-folder")(e)),
    }
}

/// Retry later, or give up now? Network trouble and server hiccups are worth another go; a
/// refusal, a bad response, a checksum mismatch or a corrupt sidecar will not fix itself.
pub fn is_retryable(e: &Error) -> bool {
    match e {
        Error::Network(_)
        | Error::NoProgress(_)
        | Error::ShortBody
        | Error::ServerChanged
        | Error::RangeIgnored
        | Error::Busy
        | Error::Io { .. } => true,
        Error::Status { status, .. } => *status >= 500 || *status == 408 || *status == 429,
        Error::Forbidden(_)
        | Error::BadResponse(_)
        | Error::InvalidInput(_)
        | Error::BadState(_)
        | Error::Checksum { .. }
        | Error::Cancelled => false,
    }
}

impl JobRunner for EngineRunner {
    fn run(
        &self,
        job: RunJob,
        started: Arc<dyn Fn(String) + Send + Sync>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, RunResult> {
        let mut cfg = self.cfg.clone();
        if let Some(c) = &self.connections_live {
            let n = c.load(std::sync::atomic::Ordering::Relaxed);
            if n > 0 {
                cfg.connections = n.min(crate::config::MAX_CONNECTIONS);
            }
        }
        if let Some(b) = job.limit_bps {
            // v0.2 per-download limit, a child of the global limiter when there is one
            cfg.job_limit_bps = Some(b.max(1));
        }
        let progress = self.progress.clone();
        Box::pin(async move {
            let dir = match ensure_job_dir(&job.base_dir, job.subdir.as_deref()) {
                Ok(d) => d,
                Err(e) => {
                    return RunResult::Failed {
                        retryable: is_retryable(&e),
                        message: crate::urlsafe::redact_text(&e.to_string()),
                    }
                }
            };
            let sink: download::EventSink = {
                let started = started.clone();
                let item = job.item;
                Arc::new(move |e: Event| match e {
                    Event::Started { name, total, .. } => {
                        if let Some(p) = &progress {
                            p(item, 0, total);
                        }
                        started(name);
                    }
                    Event::Progress { done, total } => {
                        if let Some(p) = &progress {
                            p(item, done, total);
                        }
                    }
                    Event::Finished { size } => {
                        if let Some(p) = &progress {
                            p(item, size, Some(size));
                        }
                    }
                    _ => {}
                })
            };
            // resume from a sidecar when there is one (by recorded name, else by URL)
            let (url, wanted) = (job.url.clone(), job.file_name.clone());
            let d2 = dir.clone();
            let sidecar = tokio::task::spawn_blocking(move || {
                wanted
                    .map(|n| d2.join(format!("{n}{STATE_SUFFIX}")))
                    .filter(|p| p.is_file())
                    .or_else(|| state::find_for_url(&d2, &url))
            })
            .await
            .ok()
            .flatten();
            let result = match sidecar {
                Some(sp) => download::resume(&sp, cfg, sink, cancel).await,
                None => {
                    // credentials are re-attached in memory only
                    let mut u = match url::Url::parse(&job.url) {
                        Ok(u) => u,
                        Err(_) => {
                            return RunResult::Failed {
                                retryable: false,
                                message: "invalid URL".into(),
                            }
                        }
                    };
                    if let Some((user, pass)) = &job.auth {
                        let _ = u.set_username(user);
                        let _ = u.set_password(Some(pass));
                    }
                    download::download(
                        JobSpec {
                            url: u.to_string(),
                            dir,
                            file_name: job.file_name.clone(),
                        },
                        cfg,
                        sink,
                        cancel,
                    )
                    .await
                }
            };
            match result {
                Ok(o) => RunResult::Done { path: o.path },
                Err(Error::Cancelled) => RunResult::Cancelled,
                Err(e) => RunResult::Failed {
                    retryable: is_retryable(&e),
                    message: crate::urlsafe::redact_text(&e.to_string()),
                },
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_classification() {
        assert!(is_retryable(&Error::Network("x".into())));
        assert!(is_retryable(&Error::NoProgress(3)));
        assert!(is_retryable(&Error::Status {
            status: 503,
            url: "u".into()
        }));
        assert!(is_retryable(&Error::Status {
            status: 429,
            url: "u".into()
        }));
        assert!(!is_retryable(&Error::Status {
            status: 404,
            url: "u".into()
        }));
        assert!(!is_retryable(&Error::Status {
            status: 410,
            url: "u".into()
        }));
        assert!(!is_retryable(&Error::Forbidden(403)));
        assert!(!is_retryable(&Error::BadState("x".into())));
        assert!(!is_retryable(&Error::Checksum {
            expected: "a".into(),
            actual: "b".into()
        }));
    }

    #[test]
    fn job_dir_creation_and_refusals() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("dl");
        assert_eq!(ensure_job_dir(&base, None).unwrap(), base);
        let sub = ensure_job_dir(&base, Some("Videos")).unwrap();
        assert!(sub.is_dir() && sub.starts_with(&base));
        assert_eq!(
            ensure_job_dir(&base, Some("Videos")).unwrap(),
            sub,
            "idempotent"
        );
        for bad in ["..", "a/b", "CON", ""] {
            assert!(ensure_job_dir(&base, Some(bad)).is_err(), "{bad:?}");
        }
        // a plain file in the way
        std::fs::write(base.join("File"), b"x").unwrap();
        assert!(ensure_job_dir(&base, Some("File")).is_err());
        // a symlink in the way (AGENTS 6.1: fails loudly if links cannot be created)
        let outside = d.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        eid_platform::fs::symlink_file(&outside, &base.join("Link")).expect("symlinks must work");
        assert!(ensure_job_dir(&base, Some("Link")).is_err());
    }
}
