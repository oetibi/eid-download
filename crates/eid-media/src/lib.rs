//! Video/audio through yt-dlp (ADR-0003, ADR-0007, P4).
//!
//! This crate decides **what** yt-dlp is asked to do; `eid_platform::process::spawn` decides
//! **how** it is started. Rules enforced here (each has a named test):
//!
//! * every flag we emit is in [`args::ALLOWED`]; the dangerous ones in [`args::FORBIDDEN`]
//!   can never be emitted; valued flags are always passed as one `--flag=value` argument;
//! * no user or network text ever reaches a template or path option: the output template and
//!   the archive name are constants relative to the working directory (our staging dir), the
//!   only paths are our own tool paths, which may not contain `$` or `%` (yt-dlp expands
//!   environment variables in path options);
//! * the URL is the last argument, after `--`, http(s) only, without credentials;
//! * the child environment is an allowlist ([`env::child_env`]);
//! * tool output is bounded and every text we show is redacted first ([`redact`]).

pub mod args;
pub mod consent;
pub mod env;
pub mod error;
pub mod fetch;
pub mod hints;
pub mod info;
pub mod manifest;
pub mod progress;
pub mod redact;
pub mod run;
pub mod settings;
pub mod tools;
pub mod zipx;

pub use error::MediaError;
