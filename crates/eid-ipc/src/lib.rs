//! IPC between the app, the CLI and the browser's native-messaging host (ADR-0002).
//!
//! * [`frame`]: length-prefixed framing (≤ 1 MiB), shared with Native Messaging;
//! * [`proto`]: versioned, bounded, `deny_unknown_fields` messages;
//! * [`server`]/[`client`]: token handshake over the per-user endpoint of `eid-platform`, with
//!   idempotent request ids. No TCP port exists anywhere.
//!
//! The token is defence in depth; it does not protect against other code running as the same
//! user (documented limitation).

pub mod client;
pub mod frame;
pub mod proto;
pub mod server;

pub use client::{Client, ClientError, Endpoint};
pub use proto::{Command, Request, Response, VERSION};
pub use server::{serve, Handler, ServeOptions};

use eid_platform::ipc as plat;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// A running server: dropping it does not stop it; call [`Running::stop`].
pub struct Running {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    /// Stop accepting and drop live connections; releases the endpoint.
    pub async fn stop(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }
}

/// Why [`start`] failed.
#[derive(Debug)]
pub enum StartError {
    /// Another instance already owns the endpoint (single-instance).
    AlreadyRunning,
    Io(std::io::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::AlreadyRunning => write!(f, "another instance is already running"),
            StartError::Io(e) => write!(f, "cannot start the IPC server: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

/// Claim the endpoint, publish a fresh token, and serve in the background.
/// The endpoint is claimed **before** the token file is replaced, so a second instance never
/// invalidates the token of the running one.
pub fn start(
    ep: &Endpoint,
    handler: Arc<dyn Handler>,
    opts: ServeOptions,
) -> Result<Running, StartError> {
    let listener = plat::Listener::bind(&ep.name).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            StartError::AlreadyRunning
        } else {
            StartError::Io(e)
        }
    })?;
    let token = plat::new_token().map_err(StartError::Io)?;
    plat::write_token_file(&ep.token_path, &token).map_err(StartError::Io)?;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(serve(listener, token, handler, opts, cancel.clone()));
    Ok(Running { cancel, task })
}
