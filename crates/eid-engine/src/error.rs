use std::io;

pub type Result<T> = std::result::Result<T, Error>;

/// Errors never contain raw URLs: use [`crate::urlsafe::redact`] before formatting one.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("server error: HTTP {status} for {url}")]
    Status { status: u16, url: String },
    #[error("access denied by server (HTTP {0})")]
    Forbidden(u16),
    #[error("the file changed on the server while downloading")]
    ServerChanged,
    #[error("server does not honour byte ranges")]
    RangeIgnored,
    #[error("server sent an invalid response: {0}")]
    BadResponse(String),
    #[error("download made no progress after {0} attempts")]
    NoProgress(u32),
    #[error("transfer ended before the full length arrived (short body)")]
    ShortBody,
    #[error("I/O error ({context}): {source}")]
    Io {
        context: &'static str,
        source: io::Error,
    },
    #[error("bad or untrusted state file: {0}")]
    BadState(String),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    Checksum { expected: String, actual: String },
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    pub fn io(context: &'static str) -> impl FnOnce(io::Error) -> Error {
        move |source| Error::Io { context, source }
    }
}
