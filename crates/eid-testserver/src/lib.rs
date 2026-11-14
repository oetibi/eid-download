//! Deliberately faulty loopback HTTP/1.1 fixtures. No filesystem or external network access.
#![forbid(unsafe_code)]

mod content;
mod server;
pub use content::Content;
pub use server::TestServer;
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub enum EtagMode {
    None,
    Strong(String),
    Weak(String),
    #[default]
    Auto,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RangeMode {
    #[default]
    Honor,
    Ignore200,
    Reject416,
    WrongContentRange,
    IgnoreIfRange,
}

#[derive(Clone, Debug, Default)]
pub enum LengthLie {
    #[default]
    None,
    Header {
        delta: i64,
    },
    BodyShort {
        missing: u64,
    },
}

#[derive(Clone, Debug, Default)]
pub enum Cut {
    #[default]
    None,
    EveryBytes(u64),
    /// One-based server-wide accepted connection index.
    AfterBytes {
        conn_index: usize,
        n: u64,
    },
    Random {
        seed: u64,
        min: u64,
        max: u64,
    },
}

#[derive(Clone, Debug)]
pub struct Stall {
    pub after_bytes: u64,
    pub duration: Duration,
    /// Applies only to the first body served for this path since set_behavior.
    pub once: bool,
}

/// Delay (or trickle) the response HEADERS: the socket stays open and no complete header block
/// arrives until `delay` has passed. Applies to requests number `skip+1 ..= skip+count` of the
/// path (one-based, in arrival order).
#[derive(Clone, Debug)]
pub struct HeaderDelay {
    pub skip: u32,
    pub count: u32,
    pub delay: Duration,
    /// Send an incomplete status line first (`HTTP/1.1 200 O`) so the peer has seen bytes but
    /// never a complete header block (slow-loris on headers).
    pub partial: bool,
}

#[derive(Clone, Debug)]
pub enum LimitStatus {
    TooManyRequests,
    Forbidden,
}

#[derive(Clone, Debug)]
pub enum Redirect {
    To(String),
    Loop,
    /// An HTTP location on this server, suitable as the target of an HTTPS redirect fixture.
    ToHttpsDowngrade,
    ToOtherHost(String),
}

#[derive(Clone, Debug)]
pub struct ChangeAfter {
    /// Switch before the next request once this many requests have been received.
    pub requests: Option<u64>,
    /// Switch before the next request once this many body bytes have been written.
    pub bytes_sent: Option<u64>,
    pub new_content: Content,
}

#[derive(Clone, Debug)]
pub struct Behavior {
    pub etag: EtagMode,
    pub last_modified: Option<String>,
    /// Sent as the `Date` header when set (the server sends none otherwise).
    pub date: Option<String>,
    pub advertise_ranges: bool,
    pub range: RangeMode,
    pub length_lie: LengthLie,
    pub cut: Cut,
    pub throttle_bps: Option<u64>,
    /// The first K server-wide accepted connections are slow (one-based indices).
    pub slow_connections: Option<(usize, u64)>,
    pub stall: Option<Stall>,
    pub max_concurrent: Option<(usize, LimitStatus)>,
    pub retry_after_secs: Option<u64>,
    /// Raw `Retry-After` value (e.g. an HTTP-date); wins over `retry_after_secs`.
    pub retry_after_raw: Option<String>,
    pub redirect: Option<Redirect>,
    pub content_disposition: Option<String>,
    pub content_type: Option<String>,
    /// Labels the raw body as gzip; deliberately does not compress it.
    pub gzip_always: bool,
    pub single_use: bool,
    pub change_after: Option<ChangeAfter>,
    pub fail_first_requests: u32,
    pub head_lies: bool,
    pub disconnect_before_headers: u32,
    pub header_delay: Option<HeaderDelay>,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            etag: EtagMode::Auto,
            last_modified: None,
            date: None,
            advertise_ranges: true,
            range: RangeMode::Honor,
            length_lie: LengthLie::None,
            cut: Cut::None,
            throttle_bps: None,
            slow_connections: None,
            stall: None,
            max_concurrent: None,
            retry_after_secs: None,
            retry_after_raw: None,
            redirect: None,
            content_disposition: None,
            content_type: None,
            gzip_always: false,
            single_use: false,
            change_after: None,
            fail_first_requests: 0,
            head_lies: false,
            disconnect_before_headers: 0,
            header_delay: None,
        }
    }
}

impl Behavior {
    pub fn well_behaved() -> Self {
        Self::default()
    }
    pub fn no_range() -> Self {
        Self {
            advertise_ranges: false,
            range: RangeMode::Ignore200,
            ..Self::default()
        }
    }
    pub fn cuts_every(n: u64) -> Self {
        Self {
            cut: Cut::EveryBytes(n),
            ..Self::default()
        }
    }
    pub fn slow(bps: u64) -> Self {
        Self {
            throttle_bps: Some(bps),
            ..Self::default()
        }
    }
}

/// Wall-clock microseconds since the Unix epoch (comparable with client-side timestamps).
pub fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerEventKind {
    /// A request was accepted and its response started.
    RequestStart,
    /// The server finished (or gave up on) that response: it noticed the client was gone.
    RequestEnd,
}

#[derive(Clone, Debug)]
pub struct ServerEvent {
    pub t_us: u64,
    pub kind: ServerEventKind,
    pub conn_id: usize,
    pub request_index: usize,
    /// Number of in-flight responses on this path right after the event.
    pub active_after: usize,
}

#[derive(Default, Clone, Debug)]
pub struct Stats {
    /// Timestamped request start/end log (evidence for concurrency questions).
    pub events: Vec<ServerEvent>,
    pub requests: Vec<RequestLog>,
    pub connections_accepted: usize,
    pub peak_concurrent: usize,
    pub bytes_sent_total: u64,
}

#[derive(Clone, Debug)]
pub struct RequestLog {
    pub method: String,
    pub range_header: Option<String>,
    pub if_range: Option<String>,
    pub authorization: Option<String>,
    pub accept_encoding: Option<String>,
    pub conn_id: usize,
    /// Zero means the connection was closed before response headers.
    pub status: u16,
    pub bytes_sent: u64,
    pub started_ms: u64,
}

#[cfg(test)]
mod tests;
