//! The download orchestrator: probe, claim files, run segmented workers, checkpoint, finalize.

use crate::config::{Config, MAX_CONNECTIONS, MAX_TOTAL_BYTES};
use crate::error::{Error, Result};
use crate::fault;
use crate::filename;
use crate::http::{self, parse_content_range, strong_etag, strong_last_modified};
use crate::limiter::Limiter;
use crate::prefix_hash::{HashTask, PrefixHash};
use crate::sanitize::{numbered_name as numbered, sanitize_file_name};
use crate::state::{self, SegState, State, STATE_VERSION};
use crate::table::Table;
use crate::urlsafe;
use eid_platform::fs as pfs;
use futures_util::StreamExt;
use reqwest::header::{IF_RANGE, RANGE};
use reqwest::{Response, StatusCode};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use url::Url;

pub const PART_SUFFIX: &str = ".eid-part";
pub const STATE_SUFFIX: &str = ".eid-state";
pub const LOCK_SUFFIX: &str = ".eid-lock";

/// Exclusive ownership of one job (review M1): an advisory lock (`flock` / `LockFileEx`) on a
/// STABLE per-job file `<name>.eid-lock` that exists independently of the part file, so it
/// covers a missing part, restarts and finalization. Acquired BEFORE any state is read; held
/// until the job ends (including across restarts). The file is deleted only at the very end of
/// a successful finalization, while still locked.
#[derive(Debug)]
pub struct JobLock {
    file: File,
    path: PathBuf,
    name: String,
}

impl JobLock {
    /// Take the lock for job `name` in `dir`; `Err(Busy)` if another job/process holds it.
    pub fn acquire(dir: &Path, name: &str) -> Result<JobLock> {
        let path = dir.join(format!("{name}{LOCK_SUFFIX}"));
        // The lock file is deleted while locked (end of a finished job). A process that opened the
        // path BEFORE the deletion and locks AFTER the owner released would hold an ORPHAN inode
        // while a third process creates and locks a fresh file at the same path: two owners.
        // So after locking, the handle must still be the file the path names; else retry.
        for _ in 0..32 {
            let file = match pfs::open_or_create_lock_file(&path) {
                Ok(f) => f,
                Err(e) if pfs::is_lock_contention(&e) => return Err(Error::Busy),
                Err(e) => return Err(Error::io("open job lock")(e)),
            };
            fault::hit("lock_after_open");
            if !pfs::try_lock_exclusive(&file).map_err(Error::io("lock job"))? {
                return Err(Error::Busy);
            }
            match pfs::handle_is_path(&file, &path) {
                Ok(true) => {
                    return Ok(JobLock {
                        file,
                        path,
                        name: name.to_string(),
                    })
                }
                Ok(false) => continue, // orphan inode: reopen the path
                Err(e) if pfs::is_lock_contention(&e) => return Err(Error::Busy),
                Err(e) => return Err(Error::io("verify job lock")(e)),
            }
        }
        Err(Error::Busy)
    }

    /// Lock for the job whose sidecar is `state_path` (`<name>.eid-state`).
    pub fn for_state_file(state_path: &Path) -> Result<JobLock> {
        let fname = state_path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(STATE_SUFFIX))
            .filter(|n| !n.is_empty())
            .ok_or_else(|| Error::InvalidInput("not a .eid-state file".into()))?;
        let dir = match state_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        JobLock::acquire(dir, fname)
    }

    /// Delete the lock file while still holding the lock (end of a finished job).
    pub fn remove_file_keep_lock(&self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = &self.file;
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Started {
        name: String,
        total: Option<u64>,
        ranged: bool,
    },
    Worker {
        active: usize,
    },
    Split {
        seg: usize,
        new_start: u64,
        new_end: u64,
    },
    Progress {
        done: u64,
        total: Option<u64>,
    },
    Retry {
        attempt: u32,
        reason: String,
        delay_ms: u64,
    },
    Throttled {
        status: u16,
        retry_after_ms: u64,
    },
    ConnectionsLowered {
        to: usize,
    },
    Restarted {
        reason: String,
    },
    Checkpoint {
        done: u64,
    },
    Finished {
        size: u64,
    },
    /// A response (one TCP connection) became active for segment `seg`.
    ResponseOpened {
        seg: usize,
    },
    /// That response was dropped (emitted strictly after its body stream was dropped).
    ResponseClosed {
        seg: usize,
    },
    /// v0.2 (PLAN P1 open item (c)): the LAST checkpoint of a run (at completion, cancel or
    /// error) could not be saved. Nothing is lost or corrupted: the previous checkpoint stays,
    /// so a resume repeats at most the bytes since then. The run's own outcome is unchanged.
    CheckpointFailed {
        message: String,
    },
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct JobSpec {
    /// May contain `user:pass@`; used in memory for this run only, never persisted/printed.
    pub url: String,
    /// Existing directory the file is confined to.
    pub dir: PathBuf,
    /// Overrides the server-suggested name (still sanitized).
    pub file_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: Option<String>,
    pub restarts: u32,
    /// Highest number of bytes held in memory across all workers (bounded buffering evidence).
    pub peak_buffered: u64,
    /// Leading bytes whose SHA-256 was computed during the transfer, i.e. NOT re-read at
    /// finalization (evidence for the prefix hasher; 0 = the whole file was hashed at the end).
    pub hashed_before_finish: u64,
    pub peak_workers: usize,
}

// ---------------------------------------------------------------- failure classification

enum Fail {
    Retryable(String),
    Throttle {
        status: u16,
        after: Option<Duration>,
    },
    Forbidden(u16),
    Changed,
    Fatal(Error),
    Cancelled,
}

impl From<Error> for Fail {
    fn from(e: Error) -> Self {
        match e {
            Error::Cancelled => Fail::Cancelled,
            e => Fail::Fatal(e),
        }
    }
}

fn net_err(e: reqwest::Error) -> String {
    urlsafe::redact_text(&e.without_url().to_string())
}

// ---------------------------------------------------------------- shared job context

struct Ctx {
    cfg: Config,
    client: reqwest::Client,
    url: Url,
    auth: Option<(String, String)>,
    file: Arc<File>,
    table: Arc<Mutex<Table>>,
    total: u64,
    etag: Option<String>,
    last_modified: Option<String>,
    limiter: Option<Arc<Limiter>>,
    cancel: CancellationToken,
    sink: EventSink,
    active: AtomicUsize,
    peak_workers: AtomicUsize,
    cap: AtomicUsize,
    buffered: AtomicU64,
    peak_buffered: AtomicU64,
    done_bytes: AtomicU64,
    gate: Mutex<Instant>,
}

impl Ctx {
    fn emit(&self, e: Event) {
        (self.sink)(e)
    }

    fn if_range(&self) -> Option<String> {
        self.etag.clone().or_else(|| self.last_modified.clone())
    }

    fn request(&self, start: u64, end_incl: u64) -> reqwest::RequestBuilder {
        let mut r = self
            .client
            .get(self.url.clone())
            .header(RANGE, format!("bytes={start}-{end_incl}"));
        if let Some(v) = self.if_range() {
            r = r.header(IF_RANGE, v);
        }
        if let Some((u, p)) = &self.auth {
            r = r.basic_auth(u, Some(p));
        }
        r
    }

    async fn wait_gate(&self) {
        let until = *self.gate.lock().unwrap();
        if until > Instant::now() {
            tokio::select! {
                _ = tokio::time::sleep_until(until) => {}
                _ = self.cancel.cancelled() => {}
            }
        }
    }

    fn set_gate(&self, d: Duration) {
        let mut g = self.gate.lock().unwrap();
        let t = Instant::now() + d;
        if t > *g {
            *g = t;
        }
    }

    fn lower_cap(&self, to: usize) {
        let to = to.max(1);
        let prev = self.cap.fetch_min(to, Ordering::SeqCst);
        if to < prev {
            self.emit(Event::ConnectionsLowered { to });
        }
    }

    /// Validate a ranged response and classify non-success statuses.
    fn check_ranged(
        &self,
        resp: &Response,
        start: u64,
        end_incl: u64,
    ) -> std::result::Result<(), Fail> {
        let st = resp.status();
        match st {
            StatusCode::PARTIAL_CONTENT => {}
            StatusCode::OK => return Err(Fail::Changed), // validator mismatch or Range ignored
            StatusCode::RANGE_NOT_SATISFIABLE => return Err(Fail::Changed),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                let after = http::retry_after_of(resp.headers(), self.cfg.retry_after_cap);
                return Err(Fail::Throttle {
                    status: st.as_u16(),
                    after,
                });
            }
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
                return Err(Fail::Forbidden(st.as_u16()))
            }
            StatusCode::NOT_FOUND | StatusCode::GONE => {
                return Err(Fail::Fatal(Error::Status {
                    status: st.as_u16(),
                    url: urlsafe::redact(&self.url),
                }))
            }
            s if s.is_server_error() => {
                return Err(Fail::Retryable(format!("HTTP {}", s.as_u16())))
            }
            s => {
                return Err(Fail::Fatal(Error::Status {
                    status: s.as_u16(),
                    url: urlsafe::redact(&self.url),
                }))
            }
        }
        let cr = http::header_str(resp.headers(), "content-range")
            .and_then(|v| parse_content_range(&v))
            .ok_or_else(|| {
                Fail::Fatal(Error::BadResponse(
                    "missing or malformed Content-Range".into(),
                ))
            })?;
        if cr.first != start || cr.last > end_incl {
            return Err(Fail::Fatal(Error::BadResponse(
                "Content-Range does not match the request".into(),
            )));
        }
        match cr.total {
            Some(t) if t != self.total => return Err(Fail::Changed),
            _ => {}
        }
        if let (Some(want), Some(got)) = (&self.etag, http::header_str(resp.headers(), "etag")) {
            if strong_etag(&got).as_deref() != Some(want.as_str()) {
                return Err(Fail::Changed);
            }
        }
        if self.etag.is_none() {
            if let (Some(want), Some(got)) = (
                &self.last_modified,
                http::header_str(resp.headers(), "last-modified"),
            ) {
                if &got != want {
                    return Err(Fail::Changed);
                }
            }
        }
        if let Some(ce) = http::header_str(resp.headers(), "content-encoding") {
            if !ce.eq_ignore_ascii_case("identity") {
                return Err(Fail::Fatal(Error::BadResponse(
                    "unexpected Content-Encoding on a range".into(),
                )));
            }
        }
        if let Some(cl) = resp.content_length() {
            if cl != cr.last - cr.first + 1 {
                return Err(Fail::Retryable(
                    "Content-Length disagrees with Content-Range".into(),
                ));
            }
        }
        Ok(())
    }

    /// Write a buffer into segment `seg`. Reserve -> positional write -> complete, all inside one
    /// blocking closure (cannot be cancelled half-way). Returns (written, offered).
    async fn flush(&self, seg: usize, data: Vec<u8>) -> std::result::Result<(usize, usize), Fail> {
        let offered = data.len();
        let table = self.table.clone();
        let file = self.file.clone();
        let r = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
            let (off, n) = table.lock().unwrap().reserve(seg, data.len() as u64);
            fault::hit("write_reserved");
            fault::jitter();
            if n > 0 {
                // injectable disk error AFTER the reservation, BEFORE the bytes are written
                fault::fail("write_data")?;
                pfs::write_all_at(&file, &data[..n as usize], off)?;
            }
            fault::jitter();
            table.lock().unwrap().complete(seg, off, n);
            Ok(n)
        })
        .await
        .map_err(|e| Fail::Fatal(Error::Network(format!("writer task failed: {e}"))))?;
        let n = r.map_err(|e| {
            Fail::Fatal(Error::Io {
                context: "write data",
                source: e,
            })
        })?;
        self.done_bytes.fetch_add(n, Ordering::Relaxed);
        self.buffered.fetch_sub(offered as u64, Ordering::Relaxed);
        Ok((n as usize, offered))
    }

    /// Fetch/continue one segment from its done-frontier. Ok => segment finished or was cut short
    /// by a split (cursor says which).
    async fn fetch_segment(
        &self,
        seg: usize,
        first: Option<Response>,
    ) -> std::result::Result<(), Fail> {
        let (start, end) = self.table.lock().unwrap().cursor(seg);
        if start >= end {
            return Ok(());
        }
        let resp = match first {
            Some(r) => r,
            None => {
                self.wait_gate().await;
                if self.cancel.is_cancelled() {
                    return Err(Fail::Cancelled);
                }
                let sent = tokio::select! {
                    r = http::send_bounded(self.request(start, end - 1), self.cfg.header_timeout) => r,
                    _ = self.cancel.cancelled() => return Err(Fail::Cancelled),
                };
                let r = sent.map_err(|e| match e {
                    http::SendError::Net(e) => Fail::Retryable(net_err(e)),
                    http::SendError::HeaderTimeout => {
                        Fail::Retryable("timed out waiting for response headers".into())
                    }
                })?;
                self.check_ranged(&r, start, end - 1)?;
                r
            }
        };
        self.emit(Event::ResponseOpened { seg });
        let r = self.stream_segment(seg, resp).await;
        // hyper closes the socket asynchronously in its connection task after the body is
        // dropped; give it a moment so a replacement connection does not overlap the old
        // socket (ADR-0011: real-socket cap). Cost: 2 ms per response, i.e. per segment.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.emit(Event::ResponseClosed { seg });
        r
    }

    /// Consume one response body into segment `seg`. The body stream is dropped (connection
    /// closing) when this returns, before the caller reports the response closed.
    async fn stream_segment(&self, seg: usize, resp: Response) -> std::result::Result<(), Fail> {
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::with_capacity(self.cfg.write_chunk + 16 * 1024);
        loop {
            let next = tokio::select! {
                _ = self.cancel.cancelled() => { return Err(Fail::Cancelled); }
                r = tokio::time::timeout(self.cfg.stall_timeout, stream.next()) => r,
            };
            match next {
                Err(_) => return Err(Fail::Retryable("connection stalled".into())),
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    // keep what we already received: flush then report
                    if !buf.is_empty() {
                        self.flush_tracked(seg, std::mem::take(&mut buf)).await?;
                    }
                    return Err(Fail::Retryable(net_err(e)));
                }
                Ok(Some(Ok(chunk))) => {
                    if let Some(l) = &self.limiter {
                        tokio::select! {
                            _ = l.acquire(chunk.len()) => {}
                            _ = self.cancel.cancelled() => { return Err(Fail::Cancelled); }
                        }
                    }
                    let held = self
                        .buffered
                        .fetch_add(chunk.len() as u64, Ordering::Relaxed)
                        + chunk.len() as u64;
                    self.peak_buffered.fetch_max(held, Ordering::Relaxed);
                    buf.extend_from_slice(&chunk);
                    if buf.len() >= self.cfg.write_chunk {
                        let (w, o) = self.flush_tracked(seg, std::mem::take(&mut buf)).await?;
                        if w < o {
                            return Ok(()); // segment was shortened by a split: tail belongs to someone else
                        }
                    }
                }
            }
        }
        if !buf.is_empty() {
            self.flush_tracked(seg, std::mem::take(&mut buf)).await?;
        }
        let (done, end) = self.table.lock().unwrap().cursor(seg);
        if done < end {
            return Err(Fail::Retryable("short body: connection ended early".into()));
        }
        Ok(())
    }

    async fn flush_tracked(
        &self,
        seg: usize,
        data: Vec<u8>,
    ) -> std::result::Result<(usize, usize), Fail> {
        self.flush(seg, data).await
    }
}

struct ActiveGuard<'a>(&'a Ctx);
impl<'a> ActiveGuard<'a> {
    /// The count was already incremented by [`spawn_worker`] (at spawn time, so the ramp never
    /// over-spawns because a freshly spawned task has not been polled yet).
    fn new(c: &'a Ctx) -> Self {
        ActiveGuard(c)
    }
}

fn spawn_worker(
    set: &mut JoinSet<Result<()>>,
    ctx: &Arc<Ctx>,
    seg: usize,
    first: Option<Response>,
) {
    let n = ctx.active.fetch_add(1, Ordering::SeqCst) + 1;
    ctx.peak_workers.fetch_max(n, Ordering::SeqCst);
    ctx.emit(Event::Worker { active: n });
    set.spawn(worker(ctx.clone(), seg, first));
}
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn sleep_cancellable(ctx: &Ctx, d: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(d) => {}
        _ = ctx.cancel.cancelled() => {}
    }
}

/// One worker: owns a segment, reconnects with backoff, then steals more work.
async fn worker(ctx: Arc<Ctx>, mut seg: usize, mut first: Option<Response>) -> Result<()> {
    let _g = ActiveGuard::new(&ctx);
    let mut attempts = 0u32;
    loop {
        // ---- drive the current segment to completion
        loop {
            if ctx.cancel.is_cancelled() {
                ctx.table.lock().unwrap().release(seg);
                return Err(Error::Cancelled);
            }
            let (before, end) = ctx.table.lock().unwrap().cursor(seg);
            if before >= end {
                break;
            }
            let res = ctx.fetch_segment(seg, first.take()).await;
            let (after, _) = ctx.table.lock().unwrap().cursor(seg);
            match res {
                Ok(()) => {
                    attempts = 0;
                    continue;
                }
                Err(Fail::Cancelled) => {
                    ctx.table.lock().unwrap().release(seg);
                    return Err(Error::Cancelled);
                }
                Err(Fail::Changed) => {
                    ctx.table.lock().unwrap().release(seg);
                    return Err(Error::ServerChanged);
                }
                Err(Fail::Fatal(e)) => {
                    ctx.table.lock().unwrap().release(seg);
                    return Err(e);
                }
                Err(Fail::Forbidden(code)) => {
                    let active = ctx.active.load(Ordering::SeqCst);
                    if active > 1 {
                        // probably a per-client connection limit: shrink and hand the segment on
                        ctx.lower_cap(active - 1);
                        ctx.table.lock().unwrap().release(seg);
                        return Ok(());
                    }
                    attempts += 1;
                    if attempts >= 3 {
                        ctx.table.lock().unwrap().release(seg);
                        return Err(Error::Forbidden(code));
                    }
                    let d = http::backoff(&ctx.cfg, attempts);
                    ctx.emit(Event::Retry {
                        attempt: attempts,
                        reason: format!("HTTP {code}"),
                        delay_ms: d.as_millis() as u64,
                    });
                    sleep_cancellable(&ctx, d).await;
                }
                Err(Fail::Throttle { status, after: ra }) => {
                    let wait = ra.unwrap_or_else(|| http::backoff(&ctx.cfg, attempts.max(1)));
                    ctx.set_gate(wait);
                    ctx.emit(Event::Throttled {
                        status,
                        retry_after_ms: wait.as_millis() as u64,
                    });
                    let active = ctx.active.load(Ordering::SeqCst);
                    ctx.lower_cap((ctx.cap.load(Ordering::SeqCst) / 2).max(1));
                    if active > ctx.cap.load(Ordering::SeqCst) && active > 1 {
                        ctx.table.lock().unwrap().release(seg);
                        return Ok(());
                    }
                    attempts += 1;
                    if attempts >= ctx.cfg.max_attempts.saturating_mul(2) {
                        ctx.table.lock().unwrap().release(seg);
                        return Err(Error::NoProgress(attempts));
                    }
                    sleep_cancellable(&ctx, wait).await;
                }
                Err(Fail::Retryable(reason)) => {
                    if after > before {
                        attempts = 0;
                    }
                    attempts += 1;
                    if attempts > ctx.cfg.max_attempts {
                        ctx.table.lock().unwrap().release(seg);
                        return Err(if reason.starts_with("short body") {
                            Error::ShortBody
                        } else {
                            Error::NoProgress(attempts - 1)
                        });
                    }
                    let d = http::backoff(&ctx.cfg, attempts);
                    ctx.emit(Event::Retry {
                        attempt: attempts,
                        reason,
                        delay_ms: d.as_millis() as u64,
                    });
                    sleep_cancellable(&ctx, d).await;
                }
            }
        }
        // ---- segment finished: take more work (pending first, else steal half of the largest)
        let (next, split) = {
            let mut t = ctx.table.lock().unwrap();
            match t.take_pending() {
                Some(i) => (Some(i), None),
                None => match t.split_largest(ctx.cfg.min_split) {
                    Some(i) => {
                        let s = t.segs[i];
                        (Some(i), Some((i, s.start, s.end)))
                    }
                    None => (None, None),
                },
            }
        };
        if let Some((i, a, b)) = split {
            ctx.emit(Event::Split {
                seg: i,
                new_start: a,
                new_end: b,
            });
        }
        match next {
            Some(i) => seg = i,
            None => return Ok(()),
        }
    }
}

// ---------------------------------------------------------------- probe

struct Probe {
    resp: Response,
    ranged: bool,
    total: Option<u64>,
    etag: Option<String>,
    last_modified: Option<String>,
    final_url: Url,
    content_disposition: Option<String>,
}

struct Net<'a> {
    client: &'a reqwest::Client,
    url: &'a Url,
    auth: &'a Option<(String, String)>,
    cfg: &'a Config,
    cancel: &'a CancellationToken,
    sink: &'a EventSink,
}

impl Net<'_> {
    fn get(&self, range: bool) -> reqwest::RequestBuilder {
        let mut r = self.client.get(self.url.clone());
        if range {
            r = r.header(RANGE, "bytes=0-");
        }
        if let Some((u, p)) = self.auth {
            r = r.basic_auth(u, Some(p));
        }
        r
    }

    async fn send_retrying(&self, range: bool) -> Result<Response> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let sent = tokio::select! {
                r = http::send_bounded(self.get(range), self.cfg.header_timeout) => r,
                _ = self.cancel.cancelled() => return Err(Error::Cancelled),
            };
            let delay = match sent {
                Ok(r) => {
                    let st = r.status();
                    if st.is_success() || st == StatusCode::RANGE_NOT_SATISFIABLE {
                        return Ok(r);
                    }
                    match st {
                        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
                            return Err(Error::Forbidden(st.as_u16()))
                        }
                        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                            let ra = http::retry_after_of(r.headers(), self.cfg.retry_after_cap);
                            (self.sink)(Event::Throttled {
                                status: st.as_u16(),
                                retry_after_ms: ra.map_or(0, |d| d.as_millis() as u64),
                            });
                            ra.unwrap_or_else(|| http::backoff(self.cfg, attempt))
                        }
                        s if s.is_server_error() => http::backoff(self.cfg, attempt),
                        s => {
                            return Err(Error::Status {
                                status: s.as_u16(),
                                url: urlsafe::redact(self.url),
                            })
                        }
                    }
                }
                Err(http::SendError::Net(e)) => {
                    if e.is_builder() || e.is_redirect() {
                        return Err(Error::Network(net_err(e)));
                    }
                    http::backoff(self.cfg, attempt)
                }
                Err(http::SendError::HeaderTimeout) => http::backoff(self.cfg, attempt),
            };
            if attempt >= self.cfg.max_attempts {
                return Err(Error::NoProgress(attempt));
            }
            (self.sink)(Event::Retry {
                attempt,
                reason: "probe failed".into(),
                delay_ms: delay.as_millis() as u64,
            });
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = self.cancel.cancelled() => return Err(Error::Cancelled),
            }
        }
    }

    async fn probe(&self) -> Result<Probe> {
        let mut resp = self.send_retrying(true).await?;
        let mut used_range = true;
        let encoded = |r: &Response| {
            http::header_str(r.headers(), "content-encoding")
                .is_some_and(|v| !v.eq_ignore_ascii_case("identity"))
        };
        // Cases that cannot be a usable ranged response: refetch without Range.
        if resp.status() == StatusCode::RANGE_NOT_SATISFIABLE
            || (resp.status() == StatusCode::PARTIAL_CONTENT && encoded(&resp))
        {
            // the old response must be gone BEFORE the next request: it owns a socket, and with
            // connections = 1 holding it while connecting again exceeds the cap (and would
            // deadlock a permit-based cap)
            drop(resp);
            resp = self.send_retrying(false).await?;
            used_range = false;
        }
        let final_url = resp.url().clone();
        let cd = http::header_str(resp.headers(), "content-disposition");
        let etag = http::header_str(resp.headers(), "etag").and_then(|v| strong_etag(&v));
        // Last-Modified alone is weak unless the server's Date is >= LM + 1 s (RFC 9110 8.8.2.2);
        // a weak one is dropped here, so it is never stored, never sent in If-Range, and a
        // download with no other validator is one stream with whole-file restart (review B3).
        let lm = http::header_str(resp.headers(), "last-modified").and_then(|v| {
            strong_last_modified(&v, http::header_str(resp.headers(), "date").as_deref())
        });
        match resp.status() {
            StatusCode::PARTIAL_CONTENT if used_range => {
                let cr = http::header_str(resp.headers(), "content-range")
                    .and_then(|v| parse_content_range(&v))
                    .ok_or_else(|| {
                        Error::BadResponse("206 without a valid Content-Range".into())
                    })?;
                if cr.first != 0 {
                    return Err(Error::BadResponse("206 did not start at byte 0".into()));
                }
                let Some(total) = cr.total else {
                    return Err(Error::BadResponse("206 with unknown total length".into()));
                };
                if total == 0 || total > MAX_TOTAL_BYTES {
                    return Err(Error::BadResponse("implausible total length".into()));
                }
                http::check_size_cap(self.cfg.max_total_bytes, Some(total))?;
                Ok(Probe {
                    resp,
                    ranged: true,
                    total: Some(total),
                    etag,
                    last_modified: lm,
                    final_url,
                    content_disposition: cd,
                })
            }
            StatusCode::OK => {
                let total = resp.content_length().filter(|&n| n <= MAX_TOTAL_BYTES);
                http::check_size_cap(self.cfg.max_total_bytes, total)?;
                Ok(Probe {
                    resp,
                    ranged: false,
                    total,
                    etag,
                    last_modified: lm,
                    final_url,
                    content_disposition: cd,
                })
            }
            s => Err(Error::Status {
                status: s.as_u16(),
                url: urlsafe::redact(self.url),
            }),
        }
    }
}

// ---------------------------------------------------------------- files

struct Files {
    name: String,
    part: PathBuf,
    state: PathBuf,
    file: File,
    lock: JobLock,
}

/// Atomically claim a unique `<name>.eid-part` (create_new) and make sure no final file or
/// stale state with that name exists.
fn claim_files(dir: &Path, base: &str, held: &mut Option<JobLock>) -> Result<Files> {
    for n in 0..1000u32 {
        let cand = if n == 0 {
            base.to_string()
        } else {
            numbered(base, n)
        };
        if std::fs::symlink_metadata(dir.join(&cand)).is_ok() {
            continue;
        }
        let state = dir.join(format!("{cand}{STATE_SUFFIX}"));
        if std::fs::symlink_metadata(&state).is_ok() {
            continue;
        }
        let part = dir.join(format!("{cand}{PART_SUFFIX}"));
        // job ownership first (a lock already held by a restarting resume is reused as is)
        let (lock, was_held) = match held.take() {
            Some(l) if l.name == cand => (l, true),
            other => {
                *held = other;
                match JobLock::acquire(dir, &cand) {
                    Ok(l) => (l, false),
                    Err(Error::Busy) => continue, // someone else owns this name
                    Err(e) => return Err(e),
                }
            }
        };
        match pfs::create_new_private(&part) {
            Ok(file) => {
                return Ok(Files {
                    name: cand,
                    part,
                    state,
                    file,
                    lock,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if was_held {
                    *held = Some(lock);
                }
                continue;
            }
            Err(e) => {
                return Err(Error::Io {
                    context: "create part file",
                    source: e,
                })
            }
        }
    }
    Err(Error::InvalidInput(
        "could not find a free file name".into(),
    ))
}

/// Hash through an already-open handle (the one we verified and keep until finalization):
/// hashing the very handle we verified, not a re-opened path, closes a path swap. (The
/// advisory job lock lives on the separate `.eid-lock` file, not on this data file.)
/// Finish a digest whose first `prefix.len()` bytes were hashed during the transfer: only the
/// tail `[prefix.len(), size)` is read here (positionally, through the verified handle).
fn sha256_handle_from(f: &File, mut prefix: PrefixHash, size: u64) -> Result<String> {
    let mut buf = vec![0u8; 1 << 20];
    prefix
        .extend_from(f, size, &mut buf)
        .map_err(Error::io("read for hashing"))?;
    Ok(prefix.finish_hex())
}

fn sha256_handle(f: &File) -> Result<String> {
    use std::io::{Seek, SeekFrom};
    let mut f = f.try_clone().map_err(Error::io("clone for hashing"))?;
    f.seek(SeekFrom::Start(0))
        .map_err(Error::io("seek for hashing"))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(Error::io("read for hashing"))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(state::hex(&h.finalize()))
}

/// Durably record (name, digest) in the state BEFORE each publish attempt, so recovery after a
/// crash between publish and state removal verifies the exact destination by content. The
/// verified HANDLE `file` is what gets published (M2), not whatever the part name refers to.
fn finalize_name(
    file: &File,
    dir: &Path,
    part: &Path,
    name: &str,
    state_path: &Path,
    sha256: &str,
) -> Result<PathBuf> {
    // The part was owner-only while it could hold authenticated bytes; the published file is the
    // user's data and gets ordinary permissions (0666 & !umask), set on the verified handle.
    pfs::set_default_data_permissions(file).map_err(Error::io("set final file permissions"))?;
    let mut st = state::load(state_path)?;
    for n in 0..1000u32 {
        let cand = if n == 0 {
            name.to_string()
        } else {
            numbered(name, n)
        };
        let dest = dir.join(&cand);
        st.finalize = Some(state::Finalize {
            name: cand.clone(),
            sha256: sha256.to_string(),
        });
        state::save(state_path, &st)?;
        match pfs::publish_verified_hooked(file, part, &dest, &mut || fault::hit("final_linked")) {
            Ok(()) => return Ok(dest),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(Error::Io {
                    context: "rename to final name",
                    source: e,
                })
            }
        }
    }
    Err(Error::InvalidInput(
        "could not find a free final file name".into(),
    ))
}

// ---------------------------------------------------------------- running a job

#[derive(Clone)]
struct JobInfo {
    url_clean: Url,
    name: String,
    etag: Option<String>,
    last_modified: Option<String>,
    restarts: u32,
}

fn make_state(info: &JobInfo, total: Option<u64>, ranged: bool, segs: Vec<SegState>) -> State {
    let mut s = State {
        version: STATE_VERSION,
        url: info.url_clean.to_string(),
        file_name: info.name.clone(),
        total,
        etag: info.etag.clone(),
        last_modified: info.last_modified.clone(),
        ranged,
        segments: segs,
        restarts: info.restarts,
        finalize: None,
        checksum: String::new(),
    };
    s.seal();
    s
}

/// Snapshot now, then `sync_data`, then save. The snapshot is taken immediately before the
/// sync (inside the blocking task) so it can only claim bytes already written, and a coalesced
/// request always persists the freshest table, not a queued one. The `spawn_blocking` is
/// joined, never aborted.
async fn persist_checkpoint(ctx: &Ctx, info: &JobInfo, state_path: &Path) -> Result<()> {
    let file = ctx.file.clone();
    let path = state_path.to_path_buf();
    let info = info.clone();
    let total = ctx.total;
    let table = ctx.table.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<()> {
        let snap = table.lock().unwrap().snapshot();
        let st = make_state(&info, Some(total), true, snap);
        fault::hit("before_sync");
        fault::fail("before_sync").map_err(Error::io("sync data"))?;
        file.sync_data().map_err(Error::io("sync data"))?;
        fault::hit("after_sync");
        state::save(&path, &st)
    })
    .await;
    match result {
        Ok(r) => r?,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => {
            return Err(Error::Network(format!("checkpoint task failed: {e}")));
        }
    }
    ctx.emit(Event::Checkpoint {
        done: ctx.done_bytes.load(Ordering::Relaxed),
    });
    Ok(())
}

struct CkptCtl {
    notify: Arc<Notify>,
    pending: Arc<AtomicBool>,
    stop: CancellationToken,
    handle: tokio::task::JoinHandle<Result<()>>,
}

impl CkptCtl {
    /// Request a checkpoint without blocking. Concurrent requests coalesce: at most one
    /// checkpoint runs at a time, and the next run takes a fresh snapshot.
    fn request(&self) {
        self.pending.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    /// Stop accepting new work, join the in-flight `spawn_blocking` (never abort it), then
    /// return. Must be called before any later state write (final checkpoint, finalize /
    /// `remove_all`, ServerChanged restart, cancel).
    async fn join(mut self) -> Result<()> {
        self.stop.cancel();
        self.notify.notify_one();
        match (&mut self.handle).await {
            Ok(r) => r,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => Err(Error::Network(format!("checkpoint task failed: {e}"))),
        }
    }
}

impl Drop for CkptCtl {
    fn drop(&mut self) {
        self.stop.cancel();
        self.notify.notify_one();
    }
}

/// One background checkpoint task for a `run_ranged` invocation.
fn start_ckpt_task(
    ctx: Arc<Ctx>,
    info: JobInfo,
    state_path: &Path,
) -> (CkptCtl, mpsc::UnboundedReceiver<Error>) {
    let notify = Arc::new(Notify::new());
    let pending = Arc::new(AtomicBool::new(false));
    let stop = CancellationToken::new();
    let (err_tx, err_rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn({
        let notify = notify.clone();
        let pending = pending.clone();
        let stop = stop.clone();
        let path = state_path.to_path_buf();
        async move {
            loop {
                if stop.is_cancelled() {
                    break;
                }
                if pending.swap(false, Ordering::SeqCst) {
                    if let Err(e) = persist_checkpoint(&ctx, &info, &path).await {
                        return err_tx.send(e).map_err(|b| b.0);
                    }
                    continue;
                }
                tokio::select! {
                    _ = notify.notified() => {}
                    _ = stop.cancelled() => break,
                }
            }
            Ok(())
        }
    });
    (
        CkptCtl {
            notify,
            pending,
            stop,
            handle,
        },
        err_rx,
    )
}

fn job_limiter(cfg: &Config) -> Option<Arc<Limiter>> {
    match (&cfg.global_limiter, cfg.job_limit_bps) {
        (Some(g), Some(r)) => Some(Limiter::child(g, r)),
        (Some(g), None) => Some(g.clone()),
        (None, Some(r)) => Some(Limiter::new(r)),
        (None, None) => None,
    }
}

/// Adaptive ramp (v0.1): how many workers one ramp tick may add. Like TCP slow start: while
/// the measured throughput keeps growing (>= 10% per tick) after adding workers, the next tick
/// adds twice as many (1, 2, 4, 8); as soon as it stops growing (the link or the server is
/// saturated) it falls back to one per tick, the old gradual ramp. A per-connection-throttled
/// server so reaches its useful connection count in about a second instead of 0.3 s per
/// connection, while a saturated link never gets a burst of extra connections. The total is
/// still bounded by `connections` and by the server-limit cap (`target()`).
#[derive(Debug, Default)]
pub(crate) struct Ramp {
    last: Option<(u64, std::time::Instant)>,
    last_rate: f64,
    burst: usize,
}

impl Ramp {
    pub(crate) const MAX_BURST: usize = 8;

    pub(crate) fn burst(&mut self, done: u64, now: std::time::Instant) -> usize {
        let rate = match self.last {
            Some((d, t)) => {
                let dt = now.saturating_duration_since(t).as_secs_f64();
                if dt < 0.05 {
                    return self.burst.max(1);
                }
                done.saturating_sub(d) as f64 / dt
            }
            None => {
                self.last = Some((done, now));
                self.burst = 1;
                return 1;
            }
        };
        self.burst = if rate > 0.0 && rate >= self.last_rate * 1.10 {
            (self.burst * 2).clamp(1, Self::MAX_BURST)
        } else {
            1
        };
        self.last = Some((done, now));
        self.last_rate = rate;
        self.burst
    }
}

async fn run_ranged(
    ctx: Arc<Ctx>,
    info: &JobInfo,
    state_path: &Path,
    first: Option<Response>,
) -> Result<Option<PrefixHash>> {
    let target = || {
        ctx.cap
            .load(Ordering::SeqCst)
            .min(ctx.cfg.connections)
            .clamp(1, MAX_CONNECTIONS)
    };
    let mut set: JoinSet<Result<()>> = JoinSet::new();
    let mut first = first;
    let mut ramp = tokio::time::interval(ctx.cfg.ramp_interval.max(Duration::from_millis(1)));
    let mut ckpt = tokio::time::interval(ctx.cfg.checkpoint_interval.max(Duration::from_millis(5)));
    let mut prog = tokio::time::interval(Duration::from_millis(250));
    ramp.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ckpt.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ckpt.reset();
    let mut ramp_ctl = Ramp::default();
    let mut fatal: Option<Error> = None;
    let mut ckpt_closed = false;
    let (ckpt_ctl, mut ckpt_err) = start_ckpt_task(ctx.clone(), info.clone(), state_path);
    // Hashes the completed prefix while the transfer runs (finalization hashes only the rest).
    let hasher = HashTask::start(ctx.file.clone(), ctx.table.clone());

    // the first worker starts immediately
    {
        let seg = ctx.table.lock().unwrap().take_pending();
        if let Some(seg) = seg {
            let f = first.take();
            spawn_worker(&mut set, &ctx, seg, f);
        }
    }
    loop {
        fault::coordinator_iteration(fatal.is_some());
        let complete = ctx.table.lock().unwrap().is_complete();
        if complete && set.is_empty() {
            break;
        }
        tokio::select! {
            joined = set.join_next(), if !set.is_empty() => {
                match joined {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(e))) => {
                        if fatal.is_none() || matches!(fatal, Some(Error::Cancelled)) { fatal = Some(e); }
                        ctx.cancel.cancel(); // stop siblings
                    }
                    Some(Err(je)) => {
                        fatal.get_or_insert(Error::Network(format!("worker panicked: {je}")));
                        ctx.cancel.cancel();
                    }
                    None => {}
                }
                if set.is_empty() && fatal.is_none() {
                    let incomplete = !ctx.table.lock().unwrap().is_complete();
                    if incomplete && ctx.table.lock().unwrap().segs.iter().all(|s| s.owned && s.done < s.end) {
                        // everything is owned by exited workers: free it for the ramp to reassign
                        let mut t = ctx.table.lock().unwrap();
                        for i in 0..t.segs.len() { if t.segs[i].done < t.segs[i].end { t.release(i); } }
                    }
                }
            }
            _ = ramp.tick(), if fatal.is_none() && !ctx.cancel.is_cancelled() => {
                for _ in 0..ramp_ctl.burst(ctx.done_bytes.load(Ordering::Relaxed), std::time::Instant::now()) {
                let active = ctx.active.load(Ordering::SeqCst);
                if active < target() {
                    let (seg, split) = {
                        let mut t = ctx.table.lock().unwrap();
                        match t.take_pending() {
                            Some(i) => (Some(i), None),
                            None => match t.split_largest(ctx.cfg.min_split) {
                                Some(i) => { let s = t.segs[i]; (Some(i), Some((i, s.start, s.end))) }
                                None => (None, None),
                            },
                        }
                    };
                    if let Some((i, a, b)) = split { ctx.emit(Event::Split { seg: i, new_start: a, new_end: b }); }
                    if let Some(seg) = seg {
                        spawn_worker(&mut set, &ctx, seg, None);
                    }
                }
                }
            }
            _ = ckpt.tick() => {
                ckpt_ctl.request();
            }
            maybe_err = ckpt_err.recv(), if !ckpt_closed => {
                if let Some(e) = maybe_err {
                    fatal.get_or_insert(e);
                    ctx.cancel.cancel();
                } else {
                    ckpt_closed = true;
                }
            }
            _ = prog.tick() => {
                ctx.emit(Event::Progress { done: ctx.done_bytes.load(Ordering::Relaxed), total: Some(ctx.total) });
            }
        }
        if ctx.cancel.is_cancelled() && set.is_empty() {
            break;
        }
    }
    while set.join_next().await.is_some() {}
    // Joined before anything may truncate (restart) or finalize the file; read-only, bounded.
    let prefix = hasher.join().await;
    // Join the checkpoint task (and its spawn_blocking) BEFORE any further state write.
    // Skipping this would let an in-flight checkpoint overwrite a new-generation restart
    // state (B1) or recreate a state file after remove_all.
    if let Err(e) = ckpt_ctl.join().await {
        fatal.get_or_insert(e);
    }
    if let Ok(e) = ckpt_err.try_recv() {
        fatal.get_or_insert(e);
    }
    // final durable checkpoint (also on cancel/error) so resume loses as little as possible.
    // PLAN P1 open item (c): a failure here never replaces the run's own outcome (a complete,
    // verified download stays a success; a cancel or an earlier error stays exactly that), but it
    // is no longer silent: Event::CheckpointFailed carries the (redacted) reason.
    let last = match fault::fail("final_checkpoint") {
        Ok(()) => persist_checkpoint(&ctx, info, state_path).await,
        Err(e) => Err(Error::io("final checkpoint")(e)),
    };
    if let Err(e) = last {
        ctx.emit(Event::CheckpointFailed {
            message: crate::urlsafe::redact_text(&e.to_string()),
        });
    }
    if let Some(e) = fatal {
        return Err(e);
    }
    if ctx.cancel.is_cancelled() && !ctx.table.lock().unwrap().is_complete() {
        return Err(Error::Cancelled);
    }
    Ok(prefix)
}

/// Sequential single-connection download for servers without usable ranges.
async fn run_single(
    net: &Net<'_>,
    file: &Arc<File>,
    mut resp: Response,
    total: Option<u64>,
    limiter: Option<Arc<Limiter>>,
) -> Result<(u64, PrefixHash)> {
    let cfg = net.cfg;
    let cancel_t = net.cancel;
    let sink_fn = net.sink;
    let mut attempt = 0u32;
    loop {
        let mut written: u64 = 0;
        // sequential: the digest follows exactly the bytes written at [0, written)
        let mut ph = PrefixHash::new();
        let mut stream = resp.bytes_stream();
        let outcome: std::result::Result<(), String> = loop {
            let next = tokio::select! {
                _ = cancel_t.cancelled() => return Err(Error::Cancelled),
                r = tokio::time::timeout(cfg.stall_timeout, stream.next()) => r,
            };
            match next {
                Err(_) => break Err("connection stalled".into()),
                Ok(None) => break Ok(()),
                Ok(Some(Err(e))) => break Err(net_err(e)),
                Ok(Some(Ok(chunk))) => {
                    if let Some(l) = &limiter {
                        tokio::select! {
                            _ = l.acquire(chunk.len()) => {}
                            _ = cancel_t.cancelled() => return Err(Error::Cancelled),
                        }
                    }
                    if let Some(t) = total {
                        // never accept more than declared (a lying length must not grow the file)
                        if written.saturating_add(chunk.len() as u64) > t {
                            return Err(Error::BadResponse(
                                "server sent more bytes than Content-Length".into(),
                            ));
                        }
                    }
                    let f = file.clone();
                    let off = written;
                    let n = chunk.len() as u64;
                    let (back, r) = tokio::task::spawn_blocking(move || {
                        let r = pfs::write_all_at(&f, &chunk, off);
                        if r.is_ok() {
                            ph.update(&chunk);
                        }
                        (ph, r)
                    })
                    .await
                    .map_err(|e| Error::Network(e.to_string()))?;
                    ph = back;
                    r.map_err(Error::io("write data"))?;
                    written += n;
                    if written % (1 << 20) < n {
                        sink_fn(Event::Progress {
                            done: written,
                            total,
                        });
                    }
                }
            }
        };
        let short = matches!(total, Some(t) if written != t);
        match outcome {
            Ok(()) if !short => {
                if total.is_none() {
                    file.set_len(written).map_err(Error::io("truncate"))?;
                }
                return Ok((written, ph));
            }
            other => {
                // release the failed body (and its socket) before sleeping / reconnecting
                drop(stream);
                attempt += 1;
                let reason = match other {
                    Err(r) => r,
                    Ok(()) => "short body: connection ended early".to_string(),
                };
                if attempt > cfg.max_attempts {
                    return Err(if reason.starts_with("short body") {
                        Error::ShortBody
                    } else {
                        Error::NoProgress(attempt - 1)
                    });
                }
                let d = http::backoff(cfg, attempt);
                sink_fn(Event::Retry {
                    attempt,
                    reason,
                    delay_ms: d.as_millis() as u64,
                });
                tokio::select! {
                    _ = tokio::time::sleep(d) => {}
                    _ = cancel_t.cancelled() => return Err(Error::Cancelled),
                }
                // single-stream resume is impossible: start over
                file.set_len(0).map_err(Error::io("truncate"))?;
                if let Some(t) = total {
                    file.set_len(t).map_err(Error::io("preallocate"))?;
                }
                resp = net.send_retrying(false).await?;
                if resp.status() != StatusCode::OK {
                    return Err(Error::BadResponse(
                        "server changed behaviour during retry".into(),
                    ));
                }
            }
        }
    }
}

struct Start {
    url_clean: Url,
    auth: Option<(String, String)>,
    dir: PathBuf,
}

fn prepare(spec: &JobSpec) -> Result<Start> {
    let (url_clean, auth) = urlsafe::parse_and_strip(&spec.url).map_err(Error::InvalidInput)?;
    let md = std::fs::metadata(&spec.dir).map_err(Error::io("download directory"))?;
    if !md.is_dir() {
        return Err(Error::InvalidInput(
            "download target is not a directory".into(),
        ));
    }
    Ok(Start {
        url_clean,
        auth,
        dir: spec.dir.clone(),
    })
}

/// Download `spec` to completion (or until cancelled / failed; partial data stays resumable).
pub async fn download(
    spec: JobSpec,
    cfg: Config,
    sink: EventSink,
    cancel: CancellationToken,
) -> Result<Outcome> {
    supervised(cancel, move |c| {
        download_inner(spec, cfg, sink, c, None, None)
    })
    .await
}

/// Run the job on its OWN task so that dropping/aborting the caller's future cannot abandon
/// in-flight blocking disk work (review M4): the drop guard only REQUESTS cancellation; the job
/// keeps its exclusive ownership (file lock) until every write/checkpoint has drained and the
/// final checkpoint is durable.
async fn supervised<F, Fut>(cancel: CancellationToken, f: F) -> Result<Outcome>
where
    F: FnOnce(CancellationToken) -> Fut,
    Fut: std::future::Future<Output = Result<Outcome>> + Send + 'static,
{
    let token = cancel.child_token();
    let guard = token.clone().drop_guard();
    let handle = tokio::spawn(f(token));
    let r = handle.await;
    guard.disarm();
    match r {
        Ok(r) => r,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => Err(Error::Network(format!("job task failed: {e}"))),
    }
}

/// Run disk work on the blocking pool and flatten the join error (AGENTS 6.7: no blocking I/O on
/// the async executor). The closure cannot be cancelled once started; callers are supervised
/// jobs that own their files until it returns (M4).
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Network(format!("blocking task failed: {e}")))?
}

async fn download_inner(
    spec: JobSpec,
    cfg: Config,
    sink: EventSink,
    cancel: CancellationToken,
    mut held: Option<JobLock>,
    mut restart_reason: Option<String>,
) -> Result<Outcome> {
    let st = prepare(&spec)?;
    http::check_host(cfg.allowed_hosts.as_deref(), &st.url_clean)?;
    let client = http::build_client(&cfg)?;
    let mut restarts = 0u32;
    let mut files: Option<Files> = None;
    loop {
        let net = Net {
            client: &client,
            url: &st.url_clean,
            auth: &st.auth,
            cfg: &cfg,
            cancel: &cancel,
            sink: &sink,
        };
        let probe = net.probe().await?;
        let name = match (&files, &spec.file_name) {
            (Some(f), _) => f.name.clone(),
            (None, Some(n)) => sanitize_file_name(n),
            (None, None) => {
                filename::choose_name(probe.content_disposition.as_deref(), &probe.final_url)
            }
        };
        if files.is_none() {
            // paths the OS cannot open are refused/shortened HERE, not discovered later as an
            // obscure "path not found" from a rename (Windows MAX_PATH, CI run 43576537725)
            let name = match cfg.max_path_units {
                Some(limit) => {
                    let abs = std::path::absolute(&st.dir).unwrap_or_else(|_| st.dir.clone());
                    let dir_units = abs.to_string_lossy().encode_utf16().count();
                    crate::sanitize::fit_name_to_path_limit(&name, dir_units, limit)
                        .map_err(Error::InvalidInput)?
                }
                None => name,
            };
            files = Some(claim_files(&st.dir, &name, &mut held)?);
            if let Some(old) = held.take() {
                old.remove_file_keep_lock(); // handed-over lock for a name we did not end up using
            }
        }
        let f = files.as_ref().unwrap();
        sink(Event::Started {
            name: f.name.clone(),
            total: probe.total,
            ranged: probe.ranged,
        });
        let info = JobInfo {
            url_clean: st.url_clean.clone(),
            name: f.name.clone(),
            etag: probe.etag.clone(),
            last_modified: probe.last_modified.clone(),
            restarts,
        };
        let file = Arc::new(f.file.try_clone().map_err(Error::io("clone file handle"))?);
        // Without a validator (ETag/Last-Modified) nothing proves that a later range request is
        // served from the SAME representation as the first one, so only the original response
        // is consumed, as ONE stream; a disconnect restarts the whole file (review B3).
        let can_resume = probe.etag.is_some() || probe.last_modified.is_some();
        let ranged_mode = probe.ranged && can_resume;
        // Review B1: the NEW generation's state (zero frontier, new validators) replaces the old
        // one — and the old backup is deleted — BEFORE the data is truncated. A crash anywhere
        // after this point can no longer pair an old checkpoint's frontiers with new-generation
        // data; a crash before it leaves old state + old data, which are still consistent.
        let init = if ranged_mode {
            make_state(
                &info,
                probe.total,
                true,
                Table::new(probe.total.unwrap()).snapshot(),
            )
        } else {
            make_state(&info, probe.total, false, vec![])
        };
        let sp = f.state.clone();
        let restarting = restart_reason.is_some();
        // All of this is disk work (fsyncs, truncate, preallocation can take long on slow or
        // non-sparse filesystems): one blocking-pool task, never the async executor (m2).
        let (setup_file, total_for_prealloc) = (file.clone(), probe.total);
        tokio::task::spawn_blocking(move || -> Result<()> {
            fault::hit("setup_blocking");
            if restarting {
                fault::fail("restart_state_write").map_err(Error::io("restart state write"))?;
            }
            state::save(&sp, &init)?;
            state::remove_backup(&sp)?;
            setup_file.set_len(0).map_err(Error::io("truncate"))?;
            if let Some(t) = total_for_prealloc {
                pfs::preallocate(&setup_file, t).map_err(Error::io("preallocate"))?;
            }
            Ok(())
        })
        .await
        .map_err(|e| Error::Network(e.to_string()))??;
        if let Some(reason) = restart_reason.take() {
            sink(Event::Restarted { reason });
        }
        fault::hit("restart_truncated");
        let peaks: (u64, usize);
        let result: Result<(u64, Option<PrefixHash>)> = if ranged_mode {
            let total = probe.total.unwrap();
            let state_path = f.state.clone();
            let table = Arc::new(Mutex::new(Table::new(total)));
            let ctx = Arc::new(Ctx {
                cfg: cfg.clone(),
                client: client.clone(),
                url: st.url_clean.clone(),
                auth: st.auth.clone(),
                file: file.clone(),
                table,
                total,
                etag: probe.etag.clone(),
                last_modified: probe.last_modified.clone(),
                limiter: job_limiter(&cfg),
                cancel: cancel.child_token(),
                sink: sink.clone(),
                active: AtomicUsize::new(0),
                peak_workers: AtomicUsize::new(0),
                cap: AtomicUsize::new(cfg.connections.clamp(1, MAX_CONNECTIONS)),
                buffered: AtomicU64::new(0),
                peak_buffered: AtomicU64::new(0),
                done_bytes: AtomicU64::new(0),
                gate: Mutex::new(Instant::now()),
            });
            let r = run_ranged(ctx.clone(), &info, &state_path, Some(probe.resp)).await;
            let peak = (
                ctx.peak_buffered.load(Ordering::SeqCst),
                ctx.peak_workers.load(Ordering::SeqCst),
            );
            peaks = peak;
            r.map(|p| (total, p))
        } else {
            peaks = (0, 1);
            run_single(&net, &file, probe.resp, probe.total, job_limiter(&cfg))
                .await
                .map(|(n, p)| (n, Some(p)))
        };
        match result {
            Ok((size, prefix)) => {
                let done = Done {
                    size,
                    restarts,
                    peaks,
                    prefix,
                };
                return finish(&cfg, &st.dir, files.take().unwrap(), file, done, &sink).await;
            }
            Err(Error::ServerChanged) if restarts < cfg.max_restarts => {
                restarts += 1;
                restart_reason =
                    Some("the file changed on the server or ranges stopped working".into());
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

struct Done {
    size: u64,
    restarts: u32,
    /// (peak buffered bytes, peak workers)
    peaks: (u64, usize),
    /// Digest of the leading bytes, computed during the transfer (None = hash everything).
    prefix: Option<PrefixHash>,
}

/// Finalization does blocking disk work (fsync, full-file hash, rename, state removal): run it on
/// the blocking pool, never on an executor thread (AGENTS §6.7, review m2/B2).
async fn finish(
    cfg: &Config,
    dir: &Path,
    files: Files,
    file: Arc<File>,
    done: Done,
    sink: &EventSink,
) -> Result<Outcome> {
    let (cfg, dir, sink) = (cfg.clone(), dir.to_path_buf(), sink.clone());
    match tokio::task::spawn_blocking(move || finish_blocking(&cfg, &dir, files, file, done, &sink))
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => Err(Error::Network(format!("finalization task failed: {e}"))),
    }
}

fn finish_blocking(
    cfg: &Config,
    dir: &Path,
    files: Files,
    file: Arc<File>,
    done: Done,
    sink: &EventSink,
) -> Result<Outcome> {
    let Done {
        size,
        restarts,
        peaks: (peak_buffered, peak_workers),
        prefix,
    } = done;
    file.sync_all().map_err(Error::io("sync final"))?;
    let len = file.metadata().map_err(Error::io("stat final"))?.len();
    if len != size {
        return Err(Error::BadResponse(format!(
            "final file length {len} != expected {size}"
        )));
    }
    // Keep BOTH handles (and with them the exclusive job lock) until the final rename and the
    // state removal are done (review M1).
    let Files {
        part,
        state: state_path,
        file: f0,
        lock,
        ..
    } = files;
    // The digest is always computed: the finalization record below needs it so that a crash
    // after the rename can be recognised by content, never by file size alone (review B2).
    let prefix = prefix.filter(|p| p.len() <= size);
    let hashed_before_finish = prefix.as_ref().map_or(0, PrefixHash::len);
    let full_digest = sha256_handle_from(&file, prefix.unwrap_or_default(), size)?;
    if let Some(want) = &cfg.expected_sha256 {
        if !want.eq_ignore_ascii_case(&full_digest) {
            return Err(Error::Checksum {
                expected: want.clone(),
                actual: full_digest,
            });
        }
    }
    let digest = (cfg.compute_sha256 || cfg.expected_sha256.is_some()).then(|| full_digest.clone());
    fault::hit("final_before_rename");
    let dest_name = part
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(PART_SUFFIX))
        .ok_or_else(|| Error::InvalidInput("bad part name".into()))?
        .to_string();
    let dest = finalize_name(&file, dir, &part, &dest_name, &state_path, &full_digest)?;
    fault::hit("part_renamed_final");
    // M2: the destination must still be the verified file right before we report success;
    // otherwise keep the state (and whatever is on disk) and fail loudly.
    if !pfs::handle_is_path(&file, &dest).map_err(Error::io("verify final destination"))? {
        return Err(Error::Io {
            context: "verify final destination",
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "destination changed during finalization: {}",
                    dest.display()
                ),
            ),
        });
    }
    state::remove_all(&state_path);
    let _ = pfs::sync_dir(dir);
    drop(f0);
    drop(file);
    lock.remove_file_keep_lock();
    drop(lock);
    sink(Event::Finished { size });
    Ok(Outcome {
        path: dest,
        size,
        sha256: digest,
        restarts,
        peak_buffered,
        peak_workers,
        hashed_before_finish,
    })
}

/// Resume from a state file. Falls back to a clean restart when resuming would not be safe
/// (no validators, missing/short data file, non-ranged state).
pub async fn resume(
    state_path: &Path,
    cfg: Config,
    sink: EventSink,
    cancel: CancellationToken,
) -> Result<Outcome> {
    let state_path = state_path.to_path_buf();
    supervised(cancel, move |c| async move {
        resume_inner(&state_path, cfg, sink, c).await
    })
    .await
}

async fn resume_inner(
    state_path: &Path,
    cfg: Config,
    sink: EventSink,
    cancel: CancellationToken,
) -> Result<Outcome> {
    // Job ownership FIRST: before the state is read, and independent of the part file, so it
    // also covers a missing part, restarts and finalization (review M1).
    let lock = JobLock::for_state_file(state_path)?;
    let sp = state_path.to_path_buf();
    // The job lock is held and no checkpoint task exists yet: the one place where quarantining a
    // corrupt primary is safe. Every other reader uses the read-only `state::load`.
    let st = match blocking(move || state::load_repairing(&sp)).await {
        Ok(st) => st,
        Err(e) => {
            if !state_path.exists() {
                lock.remove_file_keep_lock(); // we created it just now; nothing to protect
            }
            return Err(e);
        }
    };
    if st.file_name != lock.name {
        return Err(Error::BadState(
            "file name in the state does not match the state file name".into(),
        ));
    }
    let dir = state_path
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                Path::new(".")
            } else {
                p
            }
        })
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let part = dir.join(format!("{}{}", st.file_name, PART_SUFFIX));
    // Crash after the final rename but before the state was removed: the job is already done,
    // but ONLY if the state carries the finalization record written before the rename and the
    // recorded destination still has exactly the recorded content. A same-size file that merely
    // shares the name proves nothing (review B2) and is never adopted.
    //
    // M2 addition: a crash between the link and the unlink of a promotion (Unix) leaves `part`
    // and the destination as two names of ONE inode; that exact pair (same inode, exactly two
    // links, size and SHA-256 equal to the record, hashed through the held handle) is finished
    // by dropping the leftover part name. Anything else keeps the strict refusal below.
    {
        if let (Some(t), Some(fz)) = (st.total, &st.finalize) {
            let fin = dir.join(&fz.name);
            let fin_ok = if !part.exists() {
                // open once (no symlink, single link), then check size and hash THROUGH that
                // handle and off the executor thread: no path re-resolution between check and use
                match pfs::open_regular_nofollow_read(&fin) {
                    Ok(h) if h.metadata().is_ok_and(|m| m.len() == t) => {
                        let want = fz.sha256.clone();
                        let fin2 = fin.clone();
                        tokio::task::spawn_blocking(move || -> Result<bool> {
                            let matches = sha256_handle(&h)? == want;
                            fault::hit("recovery_hashed");
                            // the name must STILL be the file we hashed before we may report
                            // success (a swap during hashing is refused, state kept)
                            if matches && !pfs::handle_is_path(&h, &fin2).unwrap_or(false) {
                                return Err(Error::BadState(
                                    "destination changed while it was being verified; state kept"
                                        .into(),
                                ));
                            }
                            Ok(matches)
                        })
                        .await
                        .map_err(|e| Error::Network(format!("hash task failed: {e}")))??
                    }
                    _ => false,
                }
            } else {
                // everything (open, hash, re-check, unlink) runs on one blocking thread
                let (want, part2, fin2) = (fz.sha256.clone(), part.clone(), fin.clone());
                let r = tokio::task::spawn_blocking(move || -> Result<Option<bool>> {
                    let Some(h) = pfs::open_linked_pair_for_recovery(&part2, &fin2)
                        .ok()
                        .flatten()
                    else {
                        return Ok(None); // not a link pair: the normal path decides
                    };
                    let ok = h.metadata().is_ok_and(|m| m.len() == t) && sha256_handle(&h)? == want;
                    fault::hit("recovery_hashed");
                    // the handle must STILL name both paths with exactly two links before we
                    // drop anything or report success (a swap/extra link during hashing refuses)
                    let exact = pfs::recovery_pair_is_exact(&h, &part2, &fin2).unwrap_or(false);
                    if !(ok && exact) {
                        return Ok(Some(false));
                    }
                    std::fs::remove_file(&part2).map_err(Error::io("drop leftover part"))?;
                    // the handle must still be the destination after the unlink (nothing was
                    // lost either way: the data lives under whatever name it still has)
                    if !pfs::handle_is_path(&h, &fin2).unwrap_or(false) {
                        return Err(Error::BadState(
                            "destination changed during recovery; state kept".into(),
                        ));
                    }
                    Ok(Some(true))
                })
                .await
                .map_err(|e| Error::Network(format!("recovery task failed: {e}")))??;
                match r {
                    Some(ok) => {
                        if !ok {
                            // a finalization candidate that does not verify: refuse and KEEP
                            // everything (never fall into the restart path that deletes the part)
                            return Err(Error::BadState(
                                "finalization candidate (part and destination are one file) does not match the recorded digest/size or changed during verification; nothing was deleted".into(),
                            ));
                        }
                        true
                    }
                    None => false,
                }
            };
            if fin_ok {
                let digest = if cfg.compute_sha256 || cfg.expected_sha256.is_some() {
                    Some(fz.sha256.clone())
                } else {
                    None
                };
                if let Some(want) = &cfg.expected_sha256 {
                    if !want.eq_ignore_ascii_case(&fz.sha256) {
                        return Err(Error::Checksum {
                            expected: want.clone(),
                            actual: fz.sha256.clone(),
                        });
                    }
                }
                state::remove_all(state_path);
                lock.remove_file_keep_lock();
                sink(Event::Finished { size: t });
                return Ok(Outcome {
                    path: fin,
                    size: t,
                    sha256: digest,
                    restarts: st.restarts,
                    peak_buffered: 0,
                    peak_workers: 0,
                    hashed_before_finish: 0,
                });
            }
        }
    }
    let have_validators = st.etag.is_some() || st.last_modified.is_some();
    let restart_reason = if !st.ranged {
        Some("previous download was not resumable")
    } else if !have_validators {
        Some("server provided no validators (ETag/Last-Modified): resuming would be unsafe")
    } else if !part.exists() {
        Some("data file missing")
    } else {
        None
    };
    let part_file = if restart_reason.is_none() {
        let f = pfs::open_regular_nofollow(&part).map_err(Error::io("open part file"))?;
        let len = f.metadata().map_err(Error::io("stat part"))?.len();
        let max_done = st.segments.iter().map(|s| s.next).max().unwrap_or(0);
        if len < max_done || st.total.is_some_and(|t| len > t) {
            None
        } else {
            Some(f)
        }
    } else {
        None
    };
    let Some(part_file) = part_file else {
        let reason = restart_reason.unwrap_or("data file does not match the state");

        let _ = std::fs::remove_file(&part);
        state::remove_all(state_path);
        // the job lock is handed over, never released in between (review M1)
        return download_inner(
            JobSpec {
                url: st.url.clone(),
                dir,
                file_name: Some(st.file_name.clone()),
            },
            cfg,
            sink,
            cancel,
            Some(lock),
            Some(reason.to_string()),
        )
        .await;
    };

    let (url_clean, _) = urlsafe::parse_and_strip(&st.url).map_err(Error::BadState)?;
    http::check_host(cfg.allowed_hosts.as_deref(), &url_clean)?;
    let total = st
        .total
        .ok_or_else(|| Error::BadState("ranged state without total".into()))?;
    let client = http::build_client(&cfg)?;
    let file = Arc::new(part_file);
    {
        let f = file.clone();
        blocking(move || {
            fault::hit("resume_preallocate");
            pfs::preallocate(&f, total).map_err(Error::io("preallocate"))
        })
        .await?;
    }
    let info = JobInfo {
        url_clean: url_clean.clone(),
        name: st.file_name.clone(),
        etag: st.etag.clone(),
        last_modified: st.last_modified.clone(),
        restarts: st.restarts,
    };
    sink(Event::Started {
        name: st.file_name.clone(),
        total: Some(total),
        ranged: true,
    });
    let table = Arc::new(Mutex::new(Table::from_state(total, &st.segments)));
    let already = table.lock().unwrap().done_bytes();
    let ctx = Arc::new(Ctx {
        cfg: cfg.clone(),
        client,
        url: url_clean,
        auth: None,
        file: file.clone(),
        table,
        total,
        etag: st.etag.clone(),
        last_modified: st.last_modified.clone(),
        limiter: job_limiter(&cfg),
        cancel: cancel.child_token(),
        sink: sink.clone(),
        active: AtomicUsize::new(0),
        peak_workers: AtomicUsize::new(0),
        cap: AtomicUsize::new(cfg.connections.clamp(1, MAX_CONNECTIONS)),
        buffered: AtomicU64::new(0),
        peak_buffered: AtomicU64::new(0),
        done_bytes: AtomicU64::new(already),
        gate: Mutex::new(Instant::now()),
    });
    let r = run_ranged(ctx.clone(), &info, state_path, None).await;
    let peaks = (
        ctx.peak_buffered.load(Ordering::SeqCst),
        ctx.peak_workers.load(Ordering::SeqCst),
    );
    match r {
        Ok(prefix) => {
            let part_name = part.clone();
            drop(ctx);
            let f0 = pfs::open_regular_nofollow(&part_name).map_err(Error::io("reopen part"))?;
            let files = Files {
                name: st.file_name.clone(),
                part: part_name,
                state: state_path.to_path_buf(),
                file: f0,
                lock,
            };
            finish(
                &cfg,
                &dir,
                files,
                file,
                Done {
                    size: total,
                    restarts: st.restarts,
                    peaks,
                    prefix,
                },
                &sink,
            )
            .await
        }
        Err(Error::ServerChanged) => {
            drop(ctx);
            drop(file);
            let _ = std::fs::remove_file(&part);
            state::remove_all(state_path);
            download_inner(
                JobSpec {
                    url: st.url.clone(),
                    dir,
                    file_name: Some(st.file_name.clone()),
                },
                cfg,
                sink,
                cancel,
                Some(lock),
                Some("the file changed on the server".into()),
            )
            .await
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod ramp_tests {
    use super::Ramp;
    use std::time::{Duration, Instant};

    #[test]
    fn doubles_while_throughput_grows_and_falls_back_to_one_when_it_does_not() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut r = Ramp::default();
        assert_eq!(r.burst(0, ms(0)), 1);
        // 4 MB/s, then 12, 28, 60, 124, 252: keeps growing -> 2, 4, 8, 8 (capped)
        assert_eq!(r.burst(1_200_000, ms(300)), 2);
        assert_eq!(r.burst(4_800_000, ms(600)), 4);
        assert_eq!(r.burst(13_200_000, ms(900)), 8);
        assert_eq!(r.burst(31_200_000, ms(1200)), 8);
        // saturated: same rate as before -> one per tick
        assert_eq!(r.burst(49_200_000, ms(1500)), 1);
        assert_eq!(r.burst(67_200_000, ms(1800)), 1);
        // nothing moving (stall, server not answering): never a burst
        assert_eq!(r.burst(67_200_000, ms(2100)), 1);
        assert_eq!(r.burst(67_200_000, ms(2400)), 1);
    }

    #[test]
    fn a_saturated_single_connection_never_gets_a_burst() {
        let t0 = Instant::now();
        let mut r = Ramp::default();
        r.burst(0, t0);
        let mut done = 0;
        for i in 1..20u64 {
            done += 30_000_000; // flat 100 MB/s from the start
            let b = r.burst(done, t0 + Duration::from_millis(300 * i));
            if i > 1 {
                assert_eq!(b, 1, "tick {i}");
            }
        }
    }
}
