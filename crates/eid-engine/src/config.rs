use crate::limiter::Limiter;
use std::sync::Arc;
use std::time::Duration;

/// Tunables. Defaults are conservative production values; tests shrink the timings.
#[derive(Clone)]
pub struct Config {
    /// Target number of connections for one download (1..=32).
    pub connections: usize,
    /// A segment is only split when each half stays >= this many bytes.
    pub min_split: u64,
    /// Add one worker per tick until `connections` is reached (IDM-style gradual ramp).
    pub ramp_interval: Duration,
    /// How often progress is made durable (sync_data + atomic state write).
    pub checkpoint_interval: Duration,
    /// No bytes for this long on a connection => treat as failed and retry.
    pub stall_timeout: Duration,
    pub connect_timeout: Duration,
    /// Consecutive attempts without progress before giving up.
    pub max_attempts: u32,
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
    /// Upper bound for honouring `Retry-After`.
    pub retry_after_cap: Duration,
    /// Buffer flushed to disk when it reaches this size.
    pub write_chunk: usize,
    pub user_agent: String,
    /// Honour HTTP(S)_PROXY from the environment.
    pub use_system_proxy: bool,
    /// Process-wide limiter shared by all jobs (`None` = unlimited).
    pub global_limiter: Option<Arc<Limiter>>,
    /// Per-job limit in bytes/second.
    pub job_limit_bps: Option<u64>,
    /// Verify the final file against this SHA-256 (hex).
    pub expected_sha256: Option<String>,
    /// Controls ONLY whether the SHA-256 is returned/displayed in the outcome. The digest is
    /// ALWAYS computed at finish (on the blocking pool): the finalization record needs the
    /// content identity to recognise a finished file after a crash (docs/DESIGN_NOTES.md).
    pub compute_sha256: bool,
    /// How many times a changed file restarts the download before failing.
    pub max_restarts: u32,
    pub max_redirects: usize,
    /// Response headers (including redirects) must arrive within this time.
    pub header_timeout: Duration,
    /// Always hash the finished file and record it before the final rename (enables safe
    /// crash recovery after the rename). Turning it off makes such a crash restart the download.
    pub hash_on_finish: bool,
    /// Use several connections even when the server gave no ETag/Last-Modified. Off by default:
    /// without a validator a file that changes mid-transfer could be silently spliced.
    pub allow_unvalidated_segments: bool,
    /// Extra trusted root certificates (PEM), e.g. a corporate CA. This only *adds* trust
    /// anchors; certificate verification is never disabled (guard test enforces that).
    pub extra_root_certs_pem: Vec<Vec<u8>>,
    /// Longest full path the engine may create (UTF-16 units). Defaults to the platform limit
    /// (`eid_platform::MAX_PATH_UNITS`: 259 on Windows, none elsewhere); tests set it on Linux to
    /// exercise the Windows behaviour.
    pub max_path_units: Option<usize>,
    /// When set, only these hosts (exact match, ASCII case-insensitive) may be contacted: the
    /// initial URL and every redirect hop (P4 D14, tool downloads). `None` = any host.
    pub allowed_hosts: Option<Vec<String>>,
    /// When set, a response whose total length is unknown or larger than this is refused
    /// before any byte is written (P4 tool downloads: the manifest knows the size).
    pub max_total_bytes: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            connections: 8,
            min_split: 4 * 1024 * 1024,
            ramp_interval: Duration::from_millis(300),
            checkpoint_interval: Duration::from_secs(2),
            stall_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(15),
            max_attempts: 8,
            backoff_base: Duration::from_millis(250),
            backoff_cap: Duration::from_secs(8),
            retry_after_cap: Duration::from_secs(120),
            write_chunk: 128 * 1024,
            user_agent: format!("EidDownload/{}", env!("CARGO_PKG_VERSION")),
            use_system_proxy: true,
            global_limiter: None,
            job_limit_bps: None,
            expected_sha256: None,
            compute_sha256: false,
            max_restarts: 3,
            max_redirects: 10,
            header_timeout: Duration::from_secs(30),
            hash_on_finish: true,
            allow_unvalidated_segments: false,
            extra_root_certs_pem: Vec::new(),
            max_path_units: eid_platform::MAX_PATH_UNITS,
            allowed_hosts: None,
            max_total_bytes: None,
        }
    }
}

pub const MAX_CONNECTIONS: usize = 32;
/// Refuse absurd sizes from the network (1 PiB).
pub const MAX_TOTAL_BYTES: u64 = 1 << 50;
