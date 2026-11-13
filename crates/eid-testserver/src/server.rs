use crate::*;
use std::{
    collections::{HashMap, HashSet},
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::{JoinHandle, JoinSet},
    time::Instant,
};

const MAX_HEADERS: usize = 16 * 1024;
const CHUNK: usize = 16 * 1024;

struct Entry {
    content: Arc<Content>,
    behavior: Behavior,
    stats: Stats,
    connections: HashSet<usize>,
    active: usize,
    requests: u64,
    bytes: u64,
    stalled: bool,
    generation: u64,
}

struct Shared {
    entries: Mutex<HashMap<String, Entry>>,
    changed: Notify,
    start: Instant,
    address: SocketAddr,
}

/// Owns the listener and all client tasks; dropping it cancels even stalled clients.
pub struct TestServer {
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl TestServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback test listener");
        let shared = Arc::new(Shared {
            entries: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            start: Instant::now(),
            address: listener.local_addr().expect("listener address"),
        });
        let state = shared.clone();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            let mut next_id = 0usize;
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        next_id = next_id.checked_add(1).expect("connection index exhausted");
                        let state = state.clone();
                        let id = next_id;
                        clients.spawn(async move { let _ = connection(socket, state, id).await; });
                    }
                    _ = clients.join_next(), if !clients.is_empty() => {}
                }
            }
        });
        Self { shared, task }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.shared.address)
    }

    pub fn add(&self, path: &str, content: Content, b: Behavior) {
        let mut entries = self.shared.entries.lock().expect("fixture state");
        // Preserve active request accounting if a fixture is replaced live.
        if let Some(entry) = entries.get_mut(path) {
            entry.content = Arc::new(content);
            entry.behavior = b;
            entry.requests = 0;
            entry.bytes = 0;
            entry.stalled = false;
        } else {
            entries.insert(
                path.to_owned(),
                Entry {
                    content: Arc::new(content),
                    behavior: b,
                    stats: Stats::default(),
                    connections: HashSet::new(),
                    active: 0,
                    requests: 0,
                    bytes: 0,
                    stalled: false,
                    generation: 0,
                },
            );
        }
        self.shared.changed.notify_waiters();
    }

    pub fn set_behavior(&self, path: &str, b: Behavior) {
        if let Some(entry) = self
            .shared
            .entries
            .lock()
            .expect("fixture state")
            .get_mut(path)
        {
            entry.behavior = b;
            entry.stalled = false;
        }
        self.shared.changed.notify_waiters();
    }

    pub fn set_content(&self, path: &str, c: Content) {
        if let Some(entry) = self
            .shared
            .entries
            .lock()
            .expect("fixture state")
            .get_mut(path)
        {
            entry.content = Arc::new(c);
        }
        self.shared.changed.notify_waiters();
    }

    /// Connections are counted once per path when their first request is parsed.
    /// Concurrency counts in-flight responses, excluding idle keep-alive sockets.
    pub fn stats(&self, path: &str) -> Stats {
        self.shared
            .entries
            .lock()
            .expect("fixture state")
            .get(path)
            .map(|e| e.stats.clone())
            .unwrap_or_default()
    }

    /// Clears observation counters only; fault thresholds and active responses persist.
    pub fn reset_stats(&self, path: &str) {
        if let Some(entry) = self
            .shared
            .entries
            .lock()
            .expect("fixture state")
            .get_mut(path)
        {
            entry.stats = Stats {
                peak_concurrent: entry.active,
                ..Stats::default()
            };
            entry.connections.clear();
            entry.generation = entry.generation.wrapping_add(1);
        }
        self.shared.changed.notify_waiters();
    }

    /// Event-driven observation helper. Callers should impose a test deadline.
    pub async fn wait_for_stats(&self, path: &str, predicate: impl Fn(&Stats) -> bool) -> Stats {
        loop {
            let changed = self.shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let stats = self.stats(path);
            if predicate(&stats) {
                return stats;
            }
            changed.await;
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    close: bool,
}

async fn read_request(socket: &mut TcpStream, buffer: &mut Vec<u8>) -> io::Result<Option<Request>> {
    loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            if end + 4 > MAX_HEADERS {
                return Err(io::Error::other("headers too large"));
            }
            let raw =
                String::from_utf8(buffer.drain(..end + 4).collect()).map_err(io::Error::other)?;
            let mut lines = raw.split("\r\n");
            let mut first = lines.next().unwrap_or_default().split_whitespace();
            let method = first.next().unwrap_or_default().to_owned();
            let path = first
                .next()
                .unwrap_or_default()
                .split('?')
                .next()
                .unwrap_or_default()
                .to_owned();
            if first.next() != Some("HTTP/1.1") || first.next().is_some() || !path.starts_with('/')
            {
                return Err(io::Error::other("invalid request line"));
            }
            let mut headers = HashMap::new();
            for line in lines.filter(|l| !l.is_empty()) {
                let Some((key, value)) = line.split_once(':') else {
                    return Err(io::Error::other("invalid header"));
                };
                if headers
                    .insert(key.trim().to_ascii_lowercase(), value.trim().to_owned())
                    .is_some()
                {
                    return Err(io::Error::other("duplicate header"));
                }
            }
            // Request bodies and transfer encodings are outside this fixture protocol.
            if headers.contains_key("transfer-encoding")
                || headers.get("content-length").is_some_and(|v| v != "0")
            {
                return Err(io::Error::other("request body unsupported"));
            }
            let close = headers
                .get("connection")
                .is_some_and(|v| v.split(',').any(|s| s.trim().eq_ignore_ascii_case("close")));
            return Ok(Some(Request {
                method,
                path,
                headers,
                close,
            }));
        }
        if buffer.len() >= MAX_HEADERS {
            return Err(io::Error::other("headers too large"));
        }
        let mut chunk = [0; 2048];
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
}

struct Active {
    shared: Arc<Shared>,
    path: String,
    index: usize,
    conn_id: usize,
    generation: u64,
}
impl Active {
    fn bytes(&self, n: u64) {
        let mut entries = self.shared.entries.lock().expect("fixture state");
        if let Some(entry) = entries.get_mut(&self.path) {
            entry.bytes = entry.bytes.saturating_add(n);
            if entry.generation == self.generation {
                entry.stats.bytes_sent_total = entry.stats.bytes_sent_total.saturating_add(n);
                entry.stats.requests[self.index].bytes_sent = entry.stats.requests[self.index]
                    .bytes_sent
                    .saturating_add(n);
            }
        }
        self.shared.changed.notify_waiters();
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        if let Some(entry) = self
            .shared
            .entries
            .lock()
            .expect("fixture state")
            .get_mut(&self.path)
        {
            entry.active = entry.active.saturating_sub(1);
            if entry.generation == self.generation {
                let after = entry.active;
                entry.stats.events.push(crate::ServerEvent {
                    t_us: crate::now_us(),
                    kind: crate::ServerEventKind::RequestEnd,
                    conn_id: self.conn_id,
                    request_index: self.index,
                    active_after: after,
                });
            }
        }
        self.shared.changed.notify_waiters();
    }
}

struct Response {
    content: Arc<Content>,
    behavior: Behavior,
    status: u16,
    start: u64,
    len: u64,
    total: u64,
    etag: Option<String>,
    stall: Option<Stall>,
    active: Active,
}

fn prepare(shared: &Arc<Shared>, req: &Request, conn_id: usize) -> Option<Response> {
    let mut entries = shared.entries.lock().expect("fixture state");
    let entry = entries.get_mut(&req.path)?;
    if let Some(change) = &entry.behavior.change_after {
        if change.requests.is_some_and(|n| entry.requests >= n)
            || change.bytes_sent.is_some_and(|n| entry.bytes >= n)
        {
            entry.content = Arc::new(change.new_content.clone());
            entry.behavior.change_after = None;
        }
    }
    entry.requests = entry.requests.saturating_add(1);
    entry.active = entry.active.saturating_add(1);
    if entry.connections.insert(conn_id) {
        entry.stats.connections_accepted = entry.stats.connections_accepted.saturating_add(1);
    }
    entry.stats.peak_concurrent = entry.stats.peak_concurrent.max(entry.active);
    let b = entry.behavior.clone();
    let etag = match &b.etag {
        EtagMode::None => None,
        EtagMode::Auto => Some(entry.content.auto_etag()),
        EtagMode::Strong(s) => Some(format!("\"{}\"", s.trim_matches('"'))),
        EtagMode::Weak(s) => Some(format!("W/\"{}\"", s.trim_matches('"'))),
    };
    let total = entry.content.len();
    let mut status = 200;
    let mut start = 0;
    let mut len = total;
    if entry.requests <= u64::from(b.disconnect_before_headers) {
        status = 0;
    } else if req.method != "GET" && req.method != "HEAD" {
        status = 405;
    } else if entry.requests <= u64::from(b.fail_first_requests) {
        status = 503;
    } else if b.single_use && entry.requests > 1 {
        status = 410;
    } else if let Some((max, limit)) = &b.max_concurrent {
        if entry.active > *max {
            status = match limit {
                LimitStatus::TooManyRequests => 429,
                LimitStatus::Forbidden => 403,
            };
        }
    }
    if status == 200 && b.redirect.is_some() {
        status = 302;
    }
    if status == 200 && req.method == "GET" {
        if let Some(range) = req.headers.get("range") {
            let matches = req.headers.get("if-range").is_none_or(|v| {
                (!v.starts_with("W/")
                    && etag
                        .as_ref()
                        .is_some_and(|e| !e.starts_with("W/") && e == v))
                    || (v.contains(',') && b.last_modified.as_ref() == Some(v))
            });
            if b.range != RangeMode::Ignore200 && (matches || b.range == RangeMode::IgnoreIfRange) {
                match parse_range(range, total) {
                    Some((s, n)) if b.range != RangeMode::Reject416 => {
                        status = 206;
                        start = s;
                        len = n;
                    }
                    _ => status = 416,
                }
            }
        }
    }
    let stall = if b.stall.as_ref().is_some_and(|s| !s.once || !entry.stalled)
        && matches!(status, 200 | 206)
        && req.method == "GET"
    {
        entry.stalled = true;
        b.stall.clone()
    } else {
        None
    };
    let index = entry.stats.requests.len();
    let active_now = entry.active;
    entry.stats.events.push(crate::ServerEvent {
        t_us: crate::now_us(),
        kind: crate::ServerEventKind::RequestStart,
        conn_id,
        request_index: index,
        active_after: active_now,
    });
    entry.stats.requests.push(RequestLog {
        method: req.method.clone(),
        range_header: req.headers.get("range").cloned(),
        if_range: req.headers.get("if-range").cloned(),
        authorization: req.headers.get("authorization").cloned(),
        accept_encoding: req.headers.get("accept-encoding").cloned(),
        conn_id,
        status,
        bytes_sent: 0,
        started_ms: shared.start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    });
    shared.changed.notify_waiters();
    Some(Response {
        content: entry.content.clone(),
        behavior: b,
        status,
        start,
        len,
        total,
        etag,
        stall,
        active: Active {
            shared: shared.clone(),
            path: req.path.clone(),
            index,
            conn_id,
            generation: entry.generation,
        },
    })
}

fn parse_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let (a, b) = value.strip_prefix("bytes=")?.split_once('-')?;
    if total == 0
        || (!a.is_empty() && !a.bytes().all(|c| c.is_ascii_digit()))
        || (!b.is_empty() && !b.bytes().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    if a.is_empty() {
        let n = b.parse::<u64>().ok()?.min(total);
        return (n > 0).then_some((total - n, n));
    }
    let start = a.parse::<u64>().ok()?;
    if start >= total {
        return None;
    }
    let end = if b.is_empty() {
        total - 1
    } else {
        b.parse::<u64>().ok()?.min(total - 1)
    };
    Some((start, end.checked_sub(start)?.checked_add(1)?))
}

async fn connection(mut socket: TcpStream, shared: Arc<Shared>, id: usize) -> io::Result<()> {
    let mut buffer = Vec::new();
    while let Some(req) = read_request(&mut socket, &mut buffer).await? {
        let Some(response) = prepare(&shared, &req, id) else {
            socket
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await?;
            if req.close {
                break;
            }
            continue;
        };
        if !send_response(&mut socket, &req, &response, id).await? {
            break;
        }
    }
    Ok(())
}

fn cut_limit(cut: &Cut, id: usize) -> Option<u64> {
    match *cut {
        Cut::None => None,
        Cut::EveryBytes(n) => Some(n),
        Cut::AfterBytes { conn_index, n } => (id == conn_index).then_some(n),
        Cut::Random { seed, min, max } => {
            let high = max.max(min);
            let mut x = seed.wrapping_add(id as u64).max(1);
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let width = high - min;
            Some(if width == u64::MAX {
                x
            } else {
                min + x % (width + 1)
            })
        }
    }
}

/// Fails promptly when the peer has closed (EOF/RST), so the in-flight counter is released at
/// the moment of close instead of at the next failed write (ADR-0011). Uses a non-blocking
/// PEEK, so no byte of a pipelined/next request is ever consumed.
fn check_open(socket: &TcpStream) -> io::Result<()> {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let mut b = [0u8; 1];
    let mut fut = std::pin::pin!(socket.peek(&mut b));
    match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(Ok(0)) => Err(io::ErrorKind::ConnectionAborted.into()),
        Poll::Ready(Ok(_)) | Poll::Pending => Ok(()),
        Poll::Ready(Err(e)) => Err(e),
    }
}

async fn sleep_unless_closed(socket: &TcpStream, deadline: Instant) -> io::Result<()> {
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return Ok(()),
            r = socket.readable() => {
                r?;
                check_open(socket)?;
                // readable because of pipelined data (peek leaves it): keep waiting without spinning
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    }
}

async fn send_response(
    socket: &mut TcpStream,
    req: &Request,
    r: &Response,
    id: usize,
) -> io::Result<bool> {
    if r.status == 0 {
        return Ok(false);
    }
    let b = &r.behavior;
    let success = matches!(r.status, 200 | 206);
    let real = if success { r.len } else { 0 };
    let mut advertised = match b.length_lie {
        LengthLie::Header { delta } if success => real.saturating_add_signed(delta),
        _ => real,
    };
    if b.head_lies && req.method == "HEAD" && success {
        advertised = advertised.saturating_add(1);
    }
    let body_len = match b.length_lie {
        LengthLie::BodyShort { missing } if success => real.saturating_sub(missing),
        _ => real,
    };
    let cut = cut_limit(&b.cut, id);
    let send_len = cut.map_or(body_len, |n| body_len.min(n));
    let faulty_close = body_len != real || advertised != real || cut.is_some_and(|n| n <= body_len);
    let close = req.close || faulty_close;
    let reason = match r.status {
        200 => "OK",
        206 => "Partial Content",
        302 => "Found",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        410 => "Gone",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let mut headers = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Length: {advertised}\r\n",
        r.status
    );
    if close {
        headers.push_str("Connection: close\r\n");
    }
    if success {
        if let Some(etag) = &r.etag {
            headers.push_str(&format!("ETag: {etag}\r\n"));
        }
        if let Some(date) = &b.last_modified {
            headers.push_str(&format!("Last-Modified: {date}\r\n"));
        }
        if b.advertise_ranges && !(b.head_lies && req.method == "HEAD") {
            headers.push_str("Accept-Ranges: bytes\r\n");
        }
        if r.status == 206 {
            let start = r
                .start
                .saturating_add(u64::from(b.range == RangeMode::WrongContentRange));
            headers.push_str(&format!(
                "Content-Range: bytes {start}-{}/{}\r\n",
                start.saturating_add(r.len.saturating_sub(1)),
                r.total
            ));
        }
        if let Some(value) = &b.content_disposition {
            headers.push_str(&format!("Content-Disposition: {value}\r\n"));
        }
        if let Some(value) = &b.content_type {
            headers.push_str(&format!("Content-Type: {value}\r\n"));
        }
        if b.gzip_always {
            headers.push_str("Content-Encoding: gzip\r\n");
        }
    }
    if r.status == 416 {
        headers.push_str(&format!("Content-Range: bytes */{}\r\n", r.total));
    }
    if matches!(r.status, 429 | 503) {
        if let Some(seconds) = b.retry_after_secs {
            headers.push_str(&format!("Retry-After: {seconds}\r\n"));
        }
    }
    if r.status == 302 {
        let location = match b.redirect.as_ref().expect("redirect status") {
            Redirect::To(s) | Redirect::ToOtherHost(s) => s.clone(),
            Redirect::Loop => req.path.clone(),
            Redirect::ToHttpsDowngrade => format!("http://{}{}", r.active.shared.address, req.path),
        };
        headers.push_str(&format!("Location: {location}\r\n"));
    }
    headers.push_str("\r\n");
    socket.write_all(headers.as_bytes()).await?;
    if req.method == "HEAD" || !success {
        return Ok(!close);
    }
    let bps = b
        .slow_connections
        .filter(|(k, _)| id <= *k)
        .map(|(_, speed)| speed)
        .or(b.throttle_bps);
    let mut deadline = Instant::now();
    let mut sent = 0;
    let mut stalled = false;
    while sent < send_len {
        if let Some(stall) = &r.stall {
            if !stalled && sent >= stall.after_bytes {
                sleep_unless_closed(socket, Instant::now() + stall.duration).await?;
                stalled = true;
            }
        }
        if bps == Some(0) {
            sleep_unless_closed(socket, Instant::now() + Duration::from_secs(86_400)).await?;
        }
        check_open(socket)?;
        let mut n = (send_len - sent).min(CHUNK as u64);
        if let Some(stall) = &r.stall {
            if !stalled && sent < stall.after_bytes {
                n = n.min(stall.after_bytes - sent);
            }
        }
        if let Some(speed) = bps {
            // A maximum of one second of data per write, with no initial burst.
            n = n.min(speed.max(1));
            // Each increment is at most one second, even for u64::MAX fixtures.
            // Keep a cumulative deadline so write overhead cannot increase the cap.
            deadline += Duration::from_secs_f64(n as f64 / speed.max(1) as f64);
            sleep_unless_closed(socket, deadline).await?;
        }
        let bytes = r
            .content
            .read_range(r.start.saturating_add(sent), n as usize);
        // Count successful writes individually even when the peer disconnects mid-chunk.
        let mut offset = 0;
        while offset < bytes.len() {
            let wrote = socket.write(&bytes[offset..]).await?;
            if wrote == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            offset += wrote;
            sent += wrote as u64;
            r.active.bytes(wrote as u64);
        }
    }
    Ok(!close)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `check_open` must never consume bytes (keep-alive / pipelined requests) and must notice
    /// a close at once.
    #[tokio::test]
    async fn check_open_peeks_without_consuming_and_detects_close() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let mut peer = TcpStream::connect(addr).await.unwrap();
        let (mut srv, _) = l.accept().await.unwrap();
        assert!(check_open(&srv).is_ok(), "idle open socket");
        peer.write_all(b"x").await.unwrap();
        srv.readable().await.unwrap();
        assert!(check_open(&srv).is_ok(), "pending byte is not an error");
        assert!(check_open(&srv).is_ok(), "and peeking twice still works");
        let mut b = [0u8; 1];
        srv.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"x", "the byte was not consumed by check_open");
        drop(peer);
        let t = std::time::Instant::now();
        while check_open(&srv).is_ok() {
            assert!(t.elapsed() < Duration::from_secs(2), "close never detected");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[test]
    fn range_parser_checks_overflow_and_strict_decimal_syntax() {
        for value in [
            "bytes=+1-2",
            "bytes=1-+2",
            "bytes=1-2-3",
            "bytes=--1",
            "bytes=18446744073709551616-",
            "bytes=0-18446744073709551616",
            "bytes=0-1,3-4",
            "bytes=1-0",
            "bytes=-0",
            "bytes=-",
            "bytes= 1-2",
        ] {
            assert_eq!(parse_range(value, u64::MAX), None, "{value}");
        }
        assert_eq!(
            parse_range("bytes=0-18446744073709551615", u64::MAX),
            Some((0, u64::MAX))
        );
        assert_eq!(
            parse_range("bytes=-18446744073709551615", u64::MAX),
            Some((0, u64::MAX))
        );
        assert_eq!(
            parse_range("bytes=18446744073709551614-", u64::MAX),
            Some((u64::MAX - 1, 1))
        );
        assert_eq!(parse_range("bytes=0-0", 0), None);
    }

    #[test]
    fn random_cut_extremes_never_overflow() {
        for seed in [0, 1, u64::MAX] {
            for (min, max) in [(0, u64::MAX), (u64::MAX, u64::MAX), (9, 3)] {
                let cut = Cut::Random { seed, min, max };
                let n = cut_limit(&cut, usize::MAX).unwrap();
                assert!((min..=max.max(min)).contains(&n), "seed={seed}");
                assert_eq!(Some(n), cut_limit(&cut, usize::MAX), "seed={seed}");
            }
        }
    }
}
