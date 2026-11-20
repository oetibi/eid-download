//! v0.1 video and playlist jobs for the desktop app and the browser (docs/V0.1-CONTRACT.md).
//!
//! A job is one run of `eid video get|playlist ... --events --stop-on-stdin-eof` as a child
//! process (the tested CLI does the checks, tools, staging, numbering, the archive and publishing;
//! this layer only schedules, reads its JSON events and keeps a small persistent list). Pause
//! closes the child's standard input, which cancels it like Ctrl-C (partial data is kept; the
//! next run continues); a child that does not stop within [`STOP_GRACE`] is killed (its tool tree
//! goes with it: job object / process group). At most [`VideoJobs::max_running`] jobs run at
//! once; nothing polls: the scheduler sleeps until something changes.
//!
//! This module is reachable from IPC / Native Messaging (`eid serve`, the browser): it never
//! passes the agreement flag to the child and never writes the agreement record, so a child it
//! starts before the user agreed in the window or the CLI stops with exit code 3 and installs
//! or starts nothing (crates/eid-media/tests/g2_tools.rs scans this file for that).

use eid_platform::process::{AsyncChild, AsyncChildStdin, AsyncCommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::Notify;

/// How long a paused job gets to stop by itself before it is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(15);
/// How long a probe may take.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(90);
/// Jobs kept in the list (finished ones beyond this are dropped from the list, oldest first;
/// their files are never touched).
pub const MAX_JOBS: usize = 500;
/// Items reported per job (a playlist bigger than this still downloads; the view is bounded).
pub const MAX_ITEMS_VIEW: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Video,
    Playlist,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemView {
    pub index: u32,
    pub title: String,
    /// todo, running, done, failed, unavailable, skipped, paused
    pub state: String,
    pub progress: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub got_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub job: u64,
    pub kind: Kind,
    pub url: String,
    pub quality: String,
    /// v0.2: what to do when the chosen height is missing (`lower` | `higher` | `skip`).
    /// Jobs saved by v0.1 have none: `lower`, the v0.1 behaviour.
    #[serde(default = "default_fallback")]
    pub fallback: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items_sel: Option<Vec<u32>>,
    pub dir: PathBuf,
    pub title: String,
    pub state: JobState,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub items: Vec<ItemView>,
    #[serde(default)]
    pub error: Option<String>,
    /// Next run resets failed items (`--retry-failed all`).
    #[serde(default)]
    pub retry: bool,
    #[serde(skip)]
    pub bytes: Option<u64>,
    #[serde(skip)]
    pub total_bytes: Option<u64>,
    #[serde(skip)]
    pub speed: Option<u64>,
    #[serde(skip)]
    pub got_height: Option<u32>,
    #[serde(skip)]
    pub current: Option<u32>,
    /// Set while a child runs; dropping it closes the child's stdin (= pause).
    #[serde(skip)]
    ctl: Option<Ctl>,
}

#[derive(Debug)]
struct Ctl {
    stdin: Option<AsyncChildStdin>,
    /// Never sent: dropping it (the job was removed) makes the reader kill the child.
    _kill: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Clone for Ctl {
    fn clone(&self) -> Self {
        Ctl {
            stdin: None,
            _kill: None,
        }
    }
}

impl Job {
    /// The JSON the window and the browser see (`video_jobs`).
    pub fn view(&self) -> Value {
        let count = |s: &str| self.items.iter().filter(|i| i.state == s).count();
        let selected = self.items.iter().filter(|i| i.state != "skipped").count();
        json!({
            "job": self.job, "kind": self.kind, "title": self.title,
            "state": self.state, "quality": self.quality, "got_height": self.got_height,
            "done_items": count("done"), "total_items": selected,
            "failed_items": count("failed") + count("unavailable"),
            "bytes": self.bytes, "total_bytes": self.total_bytes, "speed": self.speed,
            "current": self.current, "folder": self.folder, "error": self.error,
            "items": self.items,
        })
    }
}

/// Where and how jobs run.
#[derive(Clone, Debug)]
pub struct JobsConfig {
    /// The program that runs `video ...` children: `eid` for `eid serve`, or (v0.2) the desktop
    /// app itself with [`Self::worker_args`] in front, so the window needs no second program.
    pub eid_exe: PathBuf,
    /// Arguments placed before `video ...` (v0.2: `--eid-video-worker` for the desktop app).
    pub worker_args: Vec<OsString>,
    /// The persistent job list (`video-jobs.json`).
    pub state_file: PathBuf,
    /// Downloads go here unless the window chose another folder.
    pub default_dir: PathBuf,
    pub max_running: usize,
    /// Extra environment for the child (tests only).
    pub env: Vec<(OsString, OsString)>,
}

struct Inner {
    jobs: Vec<Job>,
    next: u64,
}

/// The job list. Cheap to clone (shared).
#[derive(Clone)]
pub struct VideoJobs {
    cfg: Arc<JobsConfig>,
    inner: Arc<Mutex<Inner>>,
    wake: Arc<Notify>,
}

#[derive(Serialize, Deserialize)]
struct OnDisk {
    schema: u32,
    next: u64,
    jobs: Vec<Job>,
}

fn lock(m: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn default_fallback() -> String {
    "lower".into()
}

/// v0.2 "if the quality is missing" values: [`eid_ipc::VIDEO_FALLBACKS`] only.
pub fn check_fallback(f: &str) -> Result<(), String> {
    if eid_ipc::VIDEO_FALLBACKS.contains(&f) {
        Ok(())
    } else {
        Err(format!(
            "if_missing must be one of {}",
            eid_ipc::VIDEO_FALLBACKS.join(", ")
        ))
    }
}

/// Quality strings accepted everywhere (up to 8K): [`eid_ipc::VIDEO_QUALITIES`].
pub fn check_quality(q: &str) -> Result<(), String> {
    if eid_ipc::VIDEO_QUALITIES.contains(&q) {
        Ok(())
    } else {
        Err(format!(
            "quality must be one of {}",
            eid_ipc::VIDEO_QUALITIES.join(", ")
        ))
    }
}

/// A web page address (the child checks it fully again before running any tool).
fn check_url(url: &str) -> Result<(), String> {
    let web = url.starts_with("https://") || url.starts_with("http://");
    if !web || url.len() > eid_ipc::proto::MAX_URL_LEN || url.chars().any(char::is_control) {
        return Err("invalid page address".into());
    }
    Ok(())
}

impl VideoJobs {
    /// Load the list (jobs that were running become paused: nothing starts by itself after a
    /// restart) and start the scheduler task on the current tokio runtime.
    pub fn start(cfg: JobsConfig) -> VideoJobs {
        let (jobs, next) = load(&cfg.state_file);
        let v = VideoJobs {
            cfg: Arc::new(cfg),
            inner: Arc::new(Mutex::new(Inner { jobs, next })),
            wake: Arc::new(Notify::new()),
        };
        let me = v.clone();
        tokio::spawn(async move { me.scheduler().await });
        v
    }

    pub fn max_running(&self) -> usize {
        self.cfg.max_running.max(1)
    }

    fn persist(&self, g: &Inner) {
        let d = OnDisk {
            schema: 1,
            next: g.next,
            jobs: g.jobs.clone(),
        };
        if let Err(e) = save(&self.cfg.state_file, &d) {
            eprintln!(
                "video jobs: cannot save {}: {e}",
                self.cfg.state_file.display()
            );
        }
    }

    /// v0.2: a start failure says WHICH program could not be started and what to do (a bare
    /// "The system cannot find the file specified" told the owner nothing on a real PC).
    pub(crate) fn start_error(&self, e: &std::io::Error) -> String {
        let p = self.cfg.eid_exe.display();
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "cannot start the video worker {p}: the file is missing ({e}). \
                 If Eid Download was opened from inside the ZIP, extract the whole ZIP first; \
                 an antivirus may also have removed it."
            )
        } else {
            format!("cannot start the video worker {p}: {e}")
        }
    }

    pub(crate) fn command(&self, args: &[OsString]) -> AsyncCommand {
        let all: Vec<&OsString> = self.cfg.worker_args.iter().chain(args).collect();
        let mut c = eid_platform::process::async_command(&self.cfg.eid_exe, all);
        for (k, v) in &self.cfg.env {
            c.env(k, v);
        }
        c
    }

    // ------------------------------------------------------------ probe

    /// What `url` is (`eid video probe`): `{kind, title, qualities, items?}`. Errors starting
    /// with `TOOLS_MISSING:` mean the one-time setup in the window has not been done.
    pub async fn probe(&self, url: &str) -> Result<Value, String> {
        check_url(url)?;
        let args: Vec<OsString> = vec!["video".into(), "probe".into(), "--".into(), url.into()];
        let mut c = self.command(&args);
        c.stdin(std::process::Stdio::null());
        let child = c.spawn().map_err(|e| self.start_error(&e))?;
        let out = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| "the video information took too long; try again".to_string())?
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            return serde_json::from_slice(&out.stdout).map_err(|e| e.to_string());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err
            .lines()
            .rev()
            .find(|l| l.starts_with("error:"))
            .unwrap_or("")
            .trim_start_matches("error:")
            .trim();
        Err(if line.is_empty() {
            "the video information could not be read".into()
        } else {
            crate::remote::printable(line)
        })
    }

    // ------------------------------------------------------------ jobs

    /// Add a job (queued). `dir` must be an existing folder (the window's choice); `None` =
    /// the default folder. `items` (playlist): item numbers, `None` = all.
    pub fn add(
        &self,
        kind: Kind,
        url: &str,
        quality: &str,
        items: Option<Vec<u32>>,
        dir: Option<&Path>,
    ) -> Result<u64, String> {
        self.add_with(kind, url, quality, "lower", items, dir)
    }

    /// [`Self::add`] with the v0.2 "if the quality is missing" choice (`lower` | `higher` |
    /// `skip`, [`eid_ipc::VIDEO_FALLBACKS`]; the child `eid video` parses it again).
    pub fn add_with(
        &self,
        kind: Kind,
        url: &str,
        quality: &str,
        fallback: &str,
        items: Option<Vec<u32>>,
        dir: Option<&Path>,
    ) -> Result<u64, String> {
        check_url(url)?;
        check_quality(quality)?;
        check_fallback(fallback)?;
        if kind == Kind::Video && items.is_some() {
            return Err("items only apply to a playlist".into());
        }
        if let Some(v) = &items {
            if v.is_empty() || v.len() > MAX_ITEMS_VIEW || v.contains(&0) {
                return Err("bad item selection".into());
            }
        }
        let dir = match dir {
            Some(d) => d.to_path_buf(),
            None => self.cfg.default_dir.clone(),
        };
        if !dir.is_dir() {
            return Err(format!("not a folder: {}", dir.display()));
        }
        let mut g = lock(&self.inner);
        let id = g.next;
        g.next += 1;
        g.jobs.push(Job {
            job: id,
            kind,
            url: url.to_owned(),
            quality: quality.to_owned(),
            fallback: fallback.to_owned(),
            items_sel: items,
            dir,
            title: eid_engine::urlsafe::redact_text(url),
            state: JobState::Queued,
            folder: None,
            items: vec![],
            error: None,
            retry: false,
            bytes: None,
            total_bytes: None,
            speed: None,
            got_height: None,
            current: None,
            ctl: None,
        });
        // bounded list: drop the oldest finished jobs from the list (never their files)
        while g.jobs.len() > MAX_JOBS {
            match g.jobs.iter().position(|j| j.state == JobState::Done) {
                Some(i) => {
                    g.jobs.remove(i);
                }
                None => break,
            }
        }
        self.persist(&g);
        drop(g);
        self.wake.notify_one();
        Ok(id)
    }

    pub fn list(&self) -> Vec<Value> {
        lock(&self.inner).jobs.iter().map(Job::view).collect()
    }

    fn with_job<T>(
        &self,
        id: u64,
        f: impl FnOnce(&mut Job) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut g = lock(&self.inner);
        let j = g
            .jobs
            .iter_mut()
            .find(|j| j.job == id)
            .ok_or_else(|| format!("no video job {id}"))?;
        let r = f(j)?;
        self.persist(&g);
        drop(g);
        self.wake.notify_one();
        Ok(r)
    }

    /// Pause: a queued job just waits; a running one is asked to stop (stdin closed).
    pub fn pause(&self, id: u64) -> Result<(), String> {
        self.with_job(id, |j| {
            match j.state {
                JobState::Queued => j.state = JobState::Paused,
                JobState::Running => {
                    j.state = JobState::Paused;
                    if let Some(c) = j.ctl.as_mut() {
                        c.stdin.take();
                    }
                }
                _ => {}
            }
            Ok(())
        })
    }

    pub fn resume(&self, id: u64) -> Result<(), String> {
        self.with_job(id, |j| {
            // a job still stopping (paused a moment ago) is queued again; it restarts once
            // its child has ended
            if j.state == JobState::Paused || (j.state == JobState::Failed && j.ctl.is_none()) {
                j.state = JobState::Queued;
                j.error = None;
            }
            Ok(())
        })
    }

    /// Try the failed items again (all of them, from zero).
    pub fn retry(&self, id: u64) -> Result<(), String> {
        self.with_job(id, |j| {
            if j.ctl.is_some() {
                return Err("the job is running".into());
            }
            j.retry = true;
            j.state = JobState::Queued;
            j.error = None;
            Ok(())
        })
    }

    /// Remove from the list (stops it first if it runs). Downloaded files are kept.
    pub fn remove(&self, id: u64) -> Result<(), String> {
        let mut g = lock(&self.inner);
        let i = g
            .jobs
            .iter()
            .position(|j| j.job == id)
            .ok_or_else(|| format!("no video job {id}"))?;
        let j = g.jobs.remove(i);
        drop(j); // closes stdin and the kill channel: a running child stops
        self.persist(&g);
        Ok(())
    }

    /// Stop every running child (app exit). Waits up to `STOP_GRACE` for them to end.
    pub async fn shutdown(&self) {
        {
            let mut g = lock(&self.inner);
            for j in g.jobs.iter_mut() {
                if let Some(c) = j.ctl.as_mut() {
                    c.stdin.take();
                }
            }
        }
        let t0 = std::time::Instant::now();
        while t0.elapsed() < STOP_GRACE {
            let busy = {
                let g = lock(&self.inner);
                g.jobs.iter().any(|j| j.ctl.is_some())
            };
            if !busy {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // ------------------------------------------------------------ running

    async fn scheduler(self) {
        loop {
            self.start_ready();
            self.wake.notified().await;
        }
    }

    fn start_ready(&self) {
        let mut g = lock(&self.inner);
        let running = g.jobs.iter().filter(|j| j.ctl.is_some()).count();
        let free = self.max_running().saturating_sub(running);
        let ready: Vec<u64> = g
            .jobs
            .iter()
            .filter(|j| j.state == JobState::Queued && j.ctl.is_none())
            .map(|j| j.job)
            .take(free)
            .collect();
        for id in ready {
            let Some(j) = g.jobs.iter_mut().find(|j| j.job == id) else {
                continue;
            };
            let args = job_args(j);
            match self.command(&args).spawn() {
                Ok(mut child) => {
                    let (ktx, krx) = tokio::sync::oneshot::channel();
                    j.ctl = Some(Ctl {
                        stdin: child.stdin.take(),
                        _kill: Some(ktx),
                    });
                    j.state = JobState::Running;
                    j.retry = false;
                    j.error = None;
                    let me = self.clone();
                    tokio::spawn(async move {
                        let (code, err) = me.drive(Some(id), child, krx).await;
                        me.finished(id, code, err);
                    });
                }
                Err(e) => {
                    j.state = JobState::Failed;
                    j.error = Some(self.start_error(&e));
                }
            }
        }
        self.persist(&g);
    }

    /// Read the child's events until it exits; returns its exit code and last error.
    pub(crate) async fn drive(
        &self,
        id: Option<u64>,
        mut child: AsyncChild,
        kill: tokio::sync::oneshot::Receiver<()>,
    ) -> (Option<i32>, Option<String>) {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        // stderr is drained (a full pipe would block the child); its last error line is kept
        let errs = tokio::spawn(async move {
            let mut last: Option<String> = None;
            // v0.2: a child that dies without an "error: " line (a panic, a crash) still says
            // something: its last non-empty line is kept as a fallback
            let mut other: Option<String> = None;
            if let Some(e) = stderr {
                let mut r = BufReader::new(e).lines();
                while let Ok(Some(l)) = r.next_line().await {
                    if let Some(m) = l.strip_prefix("error: ") {
                        last = Some(crate::remote::printable(m));
                    } else if !l.trim().is_empty() {
                        other = Some(crate::remote::printable(l.trim()));
                    }
                }
            }
            last.or(other)
        });
        let mut end_err: Option<String> = None;
        if let Some(o) = stdout {
            let mut r = BufReader::new(o.take(1 << 30)).lines();
            let mut kill = kill;
            loop {
                tokio::select! {
                    l = r.next_line() => match l {
                        Ok(Some(l)) => {
                            if let Ok(v) = serde_json::from_str::<Value>(&l) {
                                if v["ev"] == "end" {
                                    end_err = v["error"].as_str().map(str::to_owned);
                                }
                                if let Some(id) = id {
                                    self.apply(id, &v);
                                }
                            }
                        }
                        _ => break,
                    },
                    _ = &mut kill => {
                        // the job was removed (sender dropped): stop the child now
                        let _ = child.start_kill();
                        break;
                    }
                }
            }
        }
        // stdin closed (pause) but the child does not end: kill it after the grace period
        let code = match tokio::time::timeout(STOP_GRACE, child.wait()).await {
            Ok(Ok(s)) => s.code(),
            _ => {
                let _ = child.start_kill();
                child.wait().await.ok().and_then(|s| s.code())
            }
        };
        // a tool the child started may still hold the stderr pipe open (Windows inherits handles):
        // never let that keep the job "running" after the child itself has exited
        let last = match tokio::time::timeout(Duration::from_secs(5), errs).await {
            Ok(r) => r.ok().flatten(),
            Err(_) => None,
        };
        (code, end_err.or(last))
    }

    /// One event of job `id`.
    fn apply(&self, id: u64, v: &Value) {
        let mut g = lock(&self.inner);
        let Some(j) = g.jobs.iter_mut().find(|j| j.job == id) else {
            return;
        };
        let s = |k: &str| v[k].as_str().map(str::to_owned);
        match v["ev"].as_str() {
            Some("plan") => {
                if let Some(t) = s("title") {
                    j.title = crate::remote::printable(&t);
                }
                j.folder = s("folder");
                let mut items = vec![];
                for (k, it) in v["items"].as_array().into_iter().flatten().enumerate() {
                    if k >= MAX_ITEMS_VIEW {
                        break;
                    }
                    let st = s_of(it, "state").unwrap_or_else(|| "todo".into());
                    items.push(ItemView {
                        index: it["n"].as_u64().unwrap_or(0) as u32,
                        title: crate::remote::printable(&s_of(it, "title").unwrap_or_default()),
                        progress: if st == "done" { 1.0 } else { 0.0 },
                        state: st,
                        got_height: None,
                        error: None,
                    });
                }
                j.items = items;
                self.persist(&g);
            }
            Some("item") => {
                let n = v["n"].as_u64().unwrap_or(0) as u32;
                let st = s("state").unwrap_or_default();
                if st == "running" {
                    j.current = Some(n);
                    j.bytes = None;
                    j.total_bytes = None;
                    j.speed = None;
                }
                if let Some(i) = j.items.iter_mut().find(|i| i.index == n) {
                    if st == "done" {
                        i.progress = 1.0;
                    }
                    i.state = st;
                    i.error = s("error").map(|e| crate::remote::printable(&e));
                }
                self.persist(&g);
            }
            Some("progress") => {
                let n = v["n"].as_u64().unwrap_or(0) as u32;
                j.bytes = v["done"].as_u64();
                j.total_bytes = v["total"].as_u64();
                j.speed = v["speed"].as_u64();
                let h = v["height"].as_u64().map(|h| h as u32);
                if h.is_some() {
                    j.got_height = h;
                }
                if let Some(i) = j.items.iter_mut().find(|i| i.index == n) {
                    if let (Some(d), Some(t)) = (j.bytes, j.total_bytes) {
                        if t > 0 {
                            i.progress = (d as f64 / t as f64).min(1.0);
                        }
                    }
                    if h.is_some() {
                        i.got_height = h;
                    }
                }
            }
            _ => {}
        }
    }

    fn finished(&self, id: u64, code: Option<i32>, err: Option<String>) {
        let mut g = lock(&self.inner);
        if let Some(j) = g.jobs.iter_mut().find(|j| j.job == id) {
            j.ctl = None;
            j.speed = None;
            j.current = None;
            let paused = j.state == JobState::Paused;
            let requeued = j.state == JobState::Queued;
            j.state = match code {
                Some(0) => JobState::Done,
                _ if requeued => JobState::Queued,
                _ if paused => JobState::Paused,
                Some(130) => JobState::Paused,
                _ => JobState::Failed,
            };
            for i in j.items.iter_mut().filter(|i| i.state == "running") {
                i.state = match j.state {
                    JobState::Done => "done",
                    JobState::Failed => "failed",
                    _ => "paused",
                }
                .into();
            }
            j.error = match j.state {
                JobState::Failed => {
                    let mut e = err.unwrap_or_else(|| "the download failed".into());
                    // the CLI's summary points at its own console ("see above"): name the items
                    let why: Vec<String> = j
                        .items
                        .iter()
                        .filter(|i| i.state == "failed")
                        .filter_map(|i| i.error.as_ref().map(|x| format!("{}: {x}", i.index)))
                        .take(3)
                        .collect();
                    if !why.is_empty() {
                        e = format!("{} ({})", e.trim_end_matches("; see above"), why.join("; "));
                    }
                    Some(e)
                }
                _ => None,
            };
        }
        self.persist(&g);
        drop(g);
        self.wake.notify_one();
    }
}

fn s_of(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().map(str::to_owned)
}

/// The `eid` arguments of one run of `j`.
pub fn job_args(j: &Job) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec!["video".into()];
    a.push(match j.kind {
        Kind::Video => "get".into(),
        Kind::Playlist => "playlist".into(),
    });
    a.push("-d".into());
    a.push(j.dir.clone().into_os_string());
    a.push("--quality".into());
    a.push(j.quality.clone().into());
    if j.fallback != "lower" {
        a.push("--if-missing".into());
        a.push(j.fallback.clone().into());
    }
    if let Some(items) = &j.items_sel {
        let s: Vec<String> = items.iter().map(u32::to_string).collect();
        a.push("--items".into());
        a.push(s.join(",").into());
    }
    if j.retry && j.kind == Kind::Playlist {
        a.push("--retry-failed".into());
        a.push("all".into());
    }
    for f in ["--events", "--stop-on-stdin-eof", "-q", "--"] {
        a.push(f.into());
    }
    a.push(j.url.clone().into());
    a
}

fn load(p: &Path) -> (Vec<Job>, u64) {
    let Ok(b) = std::fs::read(p) else {
        return (vec![], 1);
    };
    match serde_json::from_slice::<OnDisk>(&b) {
        Ok(d) if d.schema == 1 => {
            let mut jobs = d.jobs;
            for j in jobs.iter_mut() {
                if matches!(j.state, JobState::Running | JobState::Queued) {
                    j.state = JobState::Paused;
                }
                for i in j.items.iter_mut().filter(|i| i.state == "running") {
                    i.state = "paused".into();
                }
            }
            let next = d
                .next
                .max(jobs.iter().map(|j| j.job + 1).max().unwrap_or(1));
            (jobs, next)
        }
        _ => {
            // unreadable: keep it aside for the user, start with an empty list
            let mut bad = p.as_os_str().to_owned();
            bad.push(".unreadable");
            let _ = std::fs::rename(p, PathBuf::from(bad));
            (vec![], 1)
        }
    }
}

fn save(p: &Path, d: &OnDisk) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = p.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, serde_json::to_vec(d).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(kind: Kind) -> Job {
        Job {
            job: 1,
            kind,
            url: "https://www.youtube.com/playlist?list=PL1".into(),
            quality: "720p".into(),
            fallback: "lower".into(),
            items_sel: Some(vec![1, 3]),
            dir: PathBuf::from("/d"),
            title: String::new(),
            state: JobState::Queued,
            folder: None,
            items: vec![],
            error: None,
            retry: true,
            bytes: None,
            total_bytes: None,
            speed: None,
            got_height: None,
            current: None,
            ctl: None,
        }
    }

    #[test]
    fn job_args_put_the_url_after_double_dash_and_pass_the_selection() {
        let a: Vec<String> = job_args(&job(Kind::Playlist))
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        assert_eq!(&a[..2], ["video", "playlist"]);
        assert!(a.windows(2).any(|w| w == ["--items", "1,3"]));
        assert!(a.windows(2).any(|w| w == ["--retry-failed", "all"]));
        assert!(a.windows(2).any(|w| w == ["--quality", "720p"]));
        // "lower" is the default: not passed (v0.1 children understand the same arguments)
        assert!(!a.iter().any(|x| x == "--if-missing"));
        let mut j = job(Kind::Video);
        j.fallback = "skip".into();
        let a: Vec<String> = job_args(&j)
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        assert!(a.windows(2).any(|w| w == ["--if-missing", "skip"]));
        assert_eq!(a[a.len() - 2], "--");
        assert_eq!(
            a.last().unwrap(),
            "https://www.youtube.com/playlist?list=PL1"
        );
    }

    #[test]
    fn a_v01_saved_job_without_a_fallback_loads_as_lower_and_bad_fallbacks_are_refused() {
        let mut v = serde_json::to_value(job(Kind::Video)).unwrap();
        v.as_object_mut().unwrap().remove("fallback");
        let j: Job = serde_json::from_value(v).unwrap();
        assert_eq!(j.fallback, "lower");
        for ok in ["lower", "higher", "skip"] {
            assert!(check_fallback(ok).is_ok());
        }
        for bad in ["", "any", "--exec", "Lower"] {
            assert!(check_fallback(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn quality_is_only_the_fixed_list() {
        for q in [
            "144p", "240p", "360p", "480p", "720p", "1080p", "1440p", "2160p", "4320p", "best",
            "audio",
        ] {
            assert!(check_quality(q).is_ok(), "{q}");
        }
        for q in ["8640p", "4321p", "8k", "", "bv*", "720", "1080p60"] {
            assert!(check_quality(q).is_err(), "{q}");
        }
    }

    #[test]
    fn a_saved_running_job_loads_paused_and_a_bad_file_is_kept_aside() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("video-jobs.json");
        let mut j = job(Kind::Video);
        j.state = JobState::Running;
        j.job = 7;
        save(
            &p,
            &OnDisk {
                schema: 1,
                next: 3,
                jobs: vec![j],
            },
        )
        .unwrap();
        let (jobs, next) = load(&p);
        assert_eq!(jobs[0].state, JobState::Paused);
        assert_eq!(next, 8);
        std::fs::write(&p, b"{not json").unwrap();
        let (jobs, next) = load(&p);
        assert!(jobs.is_empty());
        assert_eq!(next, 1);
        assert!(d.path().join("video-jobs.json.unreadable").exists());
    }
}
