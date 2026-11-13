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
    /// Compute and return SHA-256 of the final file.
    pub compute_sha256: bool,
    /// How many times a changed file restarts the download before failing.
    pub max_restarts: u32,
    pub max_redirects: usize,
    /// Extra trusted root certificates (PEM), e.g. a corporate CA. This only *adds* trust
    /// anchors; certificate verification is never disabled (guard test enforces that).
    pub extra_root_certs_pem: Vec<Vec<u8>>,
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
            extra_root_certs_pem: Vec::new(),
        }
    }
}

pub const MAX_CONNECTIONS: usize = 32;
/// Refuse absurd sizes from the network (1 PiB).
pub const MAX_TOTAL_BYTES: u64 = 1 << 50;
