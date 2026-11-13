# eid-testserver — spec (P1)

Purpose: a *deliberately bad* local HTTP/1.1 server (library + small bin) used by all engine tests. Hand-rolled on `tokio::net::TcpListener` (no hyper) so it can lie, cut, stall, and send malformed things on purpose. Binds `127.0.0.1:0` (and optionally `[::1]`). Persistent connections (keep-alive) supported; HEAD and GET only; Range `bytes=a-b`, `a-`, `-n`; `If-Range` (ETag or HTTP-date).

## API (Rust, crate `eid-testserver`, deps: tokio, sha2, parking_lot or std Mutex)
```rust
pub struct TestServer { /* spawn on start, aborts on drop */ }
impl TestServer {
    pub async fn start() -> TestServer;
    pub fn base_url(&self) -> String;                       // "http://127.0.0.1:PORT"
    pub fn add(&self, path: &str, content: Content, b: Behavior);   // path like "/f.bin"
    pub fn set_behavior(&self, path: &str, b: Behavior);    // live change, affects next request
    pub fn set_content(&self, path: &str, c: Content);      // live change (new bytes, new etag if Behavior.etag is Auto)
    pub fn stats(&self, path: &str) -> Stats;               // snapshot
    pub fn reset_stats(&self, path: &str);
}
pub enum Content { Generated{seed:u64,len:u64}, Bytes(Vec<u8>) }  // Generated streams a deterministic xorshift64* stream; never materialised
impl Content { pub fn len(&self)->u64; pub fn sha256_hex(&self)->String; /* streaming */ pub fn read_range(&self, start:u64, len:usize)->Vec<u8>; }
#[derive(Clone)] pub struct Behavior {
    pub etag: EtagMode,                // None | Strong(String) | Weak(String) | Auto (strong, derived from content hash/seed, changes with set_content)
    pub last_modified: Option<String>, // raw HTTP-date
    pub advertise_ranges: bool,        // Accept-Ranges: bytes
    pub range: RangeMode,              // Honor | Ignore200 (always full 200) | Reject416 | WrongContentRange (206 but Content-Range start shifted) | IgnoreIfRange (serve 206 even if If-Range mismatched)
    pub length_lie: LengthLie,         // None | Header{delta:i64} (Content-Length header = real+delta, body real) | BodyShort{missing:u64} (header real, body truncated then close)
    pub cut: Cut,                      // None | EveryBytes(n) | AfterBytes{conn_index:usize,n:u64} (cut Nth connection) | Random{seed:u64,min:u64,max:u64}; cutting = abrupt TCP close mid-body
    pub throttle_bps: Option<u64>,     // per-connection cap
    pub slow_connections: Option<(usize /*first K new connections*/, u64 /*bps*/)>,
    pub stall: Option<Stall>,          // {after_bytes:u64, duration:Duration, once:bool} stop sending but keep socket open
    pub max_concurrent: Option<(usize, LimitStatus)>, // when > N concurrent connections serving this path: respond 429 (with Retry-After secs) or 403
    pub retry_after_secs: Option<u64>,
    pub redirect: Option<Redirect>,    // To(String) 302 | Loop | ToHttpsDowngrade | ToOtherHost(String)
    pub content_disposition: Option<String>, // raw header value
    pub content_type: Option<String>,
    pub gzip_always: bool,             // Content-Encoding: gzip regardless of Accept-Encoding
    pub single_use: bool,              // second request answered 410
    pub change_after: Option<ChangeAfter>, // {requests:u64 or bytes_sent:u64, new_content:Content}: after threshold serve new content + new ETag (mid-download change)
    pub fail_first_requests: u32,      // first N requests answered 503
    pub head_lies: bool,               // HEAD returns different length / no ranges than GET
    pub disconnect_before_headers: u32,// first N requests: close socket without response
}
#[derive(Default,Clone,Debug)] pub struct Stats {
    pub requests: Vec<RequestLog>,     // {method, range_header, if_range, conn_id, status, bytes_sent, started_ms}
    pub connections_accepted: usize, pub peak_concurrent: usize, pub bytes_sent_total: u64,
}
```
Defaults: `Behavior::default()` = well-behaved server (Auto strong ETag, ranges honoured, no faults).
Helper constructors: `Behavior::well_behaved()`, `no_range()`, `cuts_every(n)`, `slow(bps)`...

## Required unit tests in the crate (client = raw tokio TcpStream, not the engine)
Each behaviour has at least one test proving the server really does it (e.g. cut closes after exactly n body bytes; BodyShort sends fewer bytes than header; Ignore200 returns 200 with full body for a Range request; WrongContentRange; If-Range mismatch gives 200; keep-alive reuses a connection for 2 requests; peak_concurrent counts; max_concurrent 429 carries Retry-After; stall pauses; single_use gives 410; change_after switches content and ETag; Content-Disposition raw; redirect loop; Generated content determinism + `read_range` equals streamed bytes; `sha256_hex` equals sha256 of streamed bytes for 3 MB).
## Non-goals
TLS (later), HTTP/2, chunked requests. Chunked *responses* optional (`Behavior.chunked: bool`, no Content-Length).
