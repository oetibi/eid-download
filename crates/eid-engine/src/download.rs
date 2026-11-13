//! The download orchestrator: probe, claim files, run segmented workers, checkpoint, finalize.

use crate::config::{Config, MAX_CONNECTIONS, MAX_TOTAL_BYTES};
use crate::error::{Error, Result};
use crate::fault;
use crate::filename;
use crate::http::{self, parse_content_range, parse_retry_after, strong_etag};
use crate::limiter::Limiter;
use crate::sanitize::sanitize_file_name;
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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

pub const PART_SUFFIX: &str = ".eid-part";
pub const STATE_SUFFIX: &str = ".eid-state";

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
                let after = http::header_str(resp.headers(), "retry-after")
                    .and_then(|v| parse_retry_after(&v, self.cfg.retry_after_cap));
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
            fault::jitter();
            if n > 0 {
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
                    r = self.request(start, end - 1).send() => r,
                    _ = self.cancel.cancelled() => return Err(Fail::Cancelled),
                };
                let r = sent.map_err(|e| Fail::Retryable(net_err(e)))?;
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
                r = self.get(range).send() => r,
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
                            let ra = http::header_str(r.headers(), "retry-after")
                                .and_then(|v| parse_retry_after(&v, self.cfg.retry_after_cap));
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
                Err(e) => {
                    if e.is_builder() || e.is_redirect() {
                        return Err(Error::Network(net_err(e)));
                    }
                    http::backoff(self.cfg, attempt)
                }
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
        let lm = http::header_str(resp.headers(), "last-modified");
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

fn numbered(name: &str, n: u32) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    sanitize_file_name(&format!("{stem} ({n}){ext}"))
}

struct Files {
    name: String,
    part: PathBuf,
    state: PathBuf,
    file: File,
}

/// Atomically claim a unique `<name>.eid-part` (create_new) and make sure no final file or
/// stale state with that name exists.
fn claim_files(dir: &Path, base: &str) -> Result<Files> {
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
        match pfs::create_new_file(&part) {
            Ok(file) => {
                return Ok(Files {
                    name: cand,
                    part,
                    state,
                    file,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
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

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = pfs::open_regular_nofollow_read(path).map_err(Error::io("open for hashing"))?;
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

/// Durably record (name, digest) in the state BEFORE each rename attempt, so recovery after a
/// crash between rename and state removal verifies the exact destination by content.
fn finalize_name(
    dir: &Path,
    part: &Path,
    name: &str,
    state_path: &Path,
    sha256: &str,
) -> Result<PathBuf> {
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
        match pfs::rename_noreplace(part, &dest) {
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

async fn checkpoint(ctx: &Ctx, info: &JobInfo, state_path: &Path) -> Result<()> {
    // Snapshot FIRST, then make data durable, then persist: the snapshot can only claim bytes
    // that were already written before the sync.
    let snap = ctx.table.lock().unwrap().snapshot();
    let st = make_state(info, Some(ctx.total), true, snap);
    let file = ctx.file.clone();
    let path = state_path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        fault::hit("before_sync");
        file.sync_data().map_err(Error::io("sync data"))?;
        fault::hit("after_sync");
        state::save(&path, &st)
    })
    .await
    .map_err(|e| Error::Network(format!("checkpoint task failed: {e}")))??;
    ctx.emit(Event::Checkpoint {
        done: ctx.done_bytes.load(Ordering::Relaxed),
    });
    Ok(())
}

fn job_limiter(cfg: &Config) -> Option<Arc<Limiter>> {
    match (&cfg.global_limiter, cfg.job_limit_bps) {
        (Some(g), Some(r)) => Some(Limiter::child(g, r)),
        (Some(g), None) => Some(g.clone()),
        (None, Some(r)) => Some(Limiter::new(r)),
        (None, None) => None,
    }
}

async fn run_ranged(
    ctx: Arc<Ctx>,
    info: &JobInfo,
    state_path: &Path,
    first: Option<Response>,
) -> Result<()> {
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
    ckpt.reset();
    let mut fatal: Option<Error> = None;

    // the first worker starts immediately
    {
        let seg = ctx.table.lock().unwrap().take_pending();
        if let Some(seg) = seg {
            let f = first.take();
            spawn_worker(&mut set, &ctx, seg, f);
        }
    }
    loop {
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
            _ = ckpt.tick() => {
                if let Err(e) = checkpoint(&ctx, info, state_path).await {
                    fatal.get_or_insert(e);
                    ctx.cancel.cancel();
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
    // final durable checkpoint (also on cancel/error) so resume loses as little as possible
    let _ = checkpoint(&ctx, info, state_path).await;
    if let Some(e) = fatal {
        return Err(e);
    }
    if ctx.cancel.is_cancelled() && !ctx.table.lock().unwrap().is_complete() {
        return Err(Error::Cancelled);
    }
    Ok(())
}

/// Sequential single-connection download for servers without usable ranges.
async fn run_single(
    net: &Net<'_>,
    file: &Arc<File>,
    mut resp: Response,
    total: Option<u64>,
    limiter: Option<Arc<Limiter>>,
) -> Result<u64> {
    let cfg = net.cfg;
    let cancel_t = net.cancel;
    let sink_fn = net.sink;
    let mut attempt = 0u32;
    loop {
        let mut written: u64 = 0;
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
                    let data = chunk.to_vec();
                    let off = written;
                    tokio::task::spawn_blocking(move || pfs::write_all_at(&f, &data, off))
                        .await
                        .map_err(|e| Error::Network(e.to_string()))?
                        .map_err(Error::io("write data"))?;
                    written += chunk.len() as u64;
                    if written % (1 << 20) < chunk.len() as u64 {
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
                return Ok(written);
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
    let st = prepare(&spec)?;
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
            files = Some(claim_files(&st.dir, &name)?);
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
        file.set_len(0).map_err(Error::io("truncate"))?;
        if let Some(t) = probe.total {
            pfs::preallocate(&file, t).map_err(Error::io("preallocate"))?;
        }
        let peaks: (u64, usize);
        // Without a validator (ETag/Last-Modified) nothing proves that a later range request is
        // served from the SAME representation as the first one, so only the original response
        // is consumed, as ONE stream; a disconnect restarts the whole file (review B3).
        let can_resume = probe.etag.is_some() || probe.last_modified.is_some();
        let result: Result<u64> = if probe.ranged && can_resume {
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
            // initial state so that even a crash before the first checkpoint is recoverable
            let init = make_state(
                &info,
                Some(total),
                true,
                ctx.table.lock().unwrap().snapshot(),
            );
            let sp = state_path.clone();
            tokio::task::spawn_blocking(move || state::save(&sp, &init))
                .await
                .map_err(|e| Error::Network(e.to_string()))??;
            let r = run_ranged(ctx.clone(), &info, &state_path, Some(probe.resp)).await;
            let peak = (
                ctx.peak_buffered.load(Ordering::SeqCst),
                ctx.peak_workers.load(Ordering::SeqCst),
            );
            peaks = peak;
            r.map(|_| total)
        } else {
            let init = make_state(&info, probe.total, false, vec![]);
            let sp = f.state.clone();
            tokio::task::spawn_blocking(move || state::save(&sp, &init))
                .await
                .map_err(|e| Error::Network(e.to_string()))??;
            peaks = (0, 1);
            run_single(&net, &file, probe.resp, probe.total, job_limiter(&cfg)).await
        };
        match result {
            Ok(size) => {
                let done = Done {
                    size,
                    restarts,
                    peaks,
                };
                return finish(&cfg, &st.dir, files.take().unwrap(), file, done, &sink);
            }
            Err(Error::ServerChanged) if restarts < cfg.max_restarts => {
                restarts += 1;
                sink(Event::Restarted {
                    reason: "the file changed on the server or ranges stopped working".into(),
                });
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
}

fn finish(
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
    } = done;
    file.sync_all().map_err(Error::io("sync final"))?;
    let len = file.metadata().map_err(Error::io("stat final"))?.len();
    if len != size {
        return Err(Error::BadResponse(format!(
            "final file length {len} != expected {size}"
        )));
    }
    drop(file);
    let Files {
        part,
        state: state_path,
        file: f0,
        ..
    } = files;
    drop(f0);
    // The digest is always computed: the finalization record below needs it so that a crash
    // after the rename can be recognised by content, never by file size alone (review B2).
    let full_digest = sha256_file(&part)?;
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
    let dest = finalize_name(dir, &part, &dest_name, &state_path, &full_digest)?;
    fault::hit("part_renamed_final");
    state::remove_all(&state_path);
    let _ = pfs::sync_dir(dir);
    sink(Event::Finished { size });
    Ok(Outcome {
        path: dest,
        size,
        sha256: digest,
        restarts,
        peak_buffered,
        peak_workers,
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
    let st = state::load(state_path)?;
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
    if !part.exists() {
        if let (Some(t), Some(fz)) = (st.total, &st.finalize) {
            let fin = dir.join(&fz.name);
            if std::fs::symlink_metadata(&fin).is_ok_and(|m| m.is_file() && m.len() == t)
                && sha256_file(&fin)? == fz.sha256
            {
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
                sink(Event::Finished { size: t });
                return Ok(Outcome {
                    path: fin,
                    size: t,
                    sha256: digest,
                    restarts: st.restarts,
                    peak_buffered: 0,
                    peak_workers: 0,
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
        sink(Event::Restarted {
            reason: reason.to_string(),
        });
        let _ = std::fs::remove_file(&part);
        state::remove_all(state_path);
        return download(
            JobSpec {
                url: st.url.clone(),
                dir,
                file_name: Some(st.file_name.clone()),
            },
            cfg,
            sink,
            cancel,
        )
        .await;
    };

    let (url_clean, _) = urlsafe::parse_and_strip(&st.url).map_err(Error::BadState)?;
    let total = st
        .total
        .ok_or_else(|| Error::BadState("ranged state without total".into()))?;
    let client = http::build_client(&cfg)?;
    let file = Arc::new(part_file);
    pfs::preallocate(&file, total).map_err(Error::io("preallocate"))?;
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
        Ok(()) => {
            let part_name = part.clone();
            drop(ctx);
            let f0 = pfs::open_regular_nofollow(&part_name).map_err(Error::io("reopen part"))?;
            let files = Files {
                name: st.file_name.clone(),
                part: part_name,
                state: state_path.to_path_buf(),
                file: f0,
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
                },
                &sink,
            )
        }
        Err(Error::ServerChanged) => {
            sink(Event::Restarted {
                reason: "the file changed on the server".into(),
            });
            drop(ctx);
            drop(file);
            let _ = std::fs::remove_file(&part);
            state::remove_all(state_path);
            download(
                JobSpec {
                    url: st.url.clone(),
                    dir,
                    file_name: Some(st.file_name.clone()),
                },
                cfg,
                sink,
                cancel,
            )
            .await
        }
        Err(e) => Err(e),
    }
}
