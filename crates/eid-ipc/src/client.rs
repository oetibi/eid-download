//! Client side: connect, authenticate with the token file, send requests.

use crate::frame::{read_frame, write_frame, FrameError};
use crate::proto::{Command, Hello, Request, Response, VERSION};
use eid_platform::ipc::{self, Stream};
use std::path::PathBuf;
use std::time::Duration;

/// Where the running app listens and where it keeps the handshake token.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub name: String,
    pub token_path: PathBuf,
}

impl Endpoint {
    /// The per-user default for `name`: token at `<config dir>/ipc-<name>.token`.
    pub fn default_for(name: &str) -> Result<Endpoint, std::io::Error> {
        let dir = ipc::config_dir().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no per-user config directory")
        })?;
        Ok(Endpoint {
            name: name.to_owned(),
            token_path: dir.join(format!("ipc-{name}.token")),
        })
    }
}

#[derive(Debug)]
pub enum ClientError {
    /// Nobody is listening: the app is not running.
    NotRunning,
    /// The server refused the token / hello.
    Auth(String),
    /// Malformed or unexpected traffic.
    Protocol(String),
    /// The request was understood but failed.
    Server(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NotRunning => write!(f, "Eid Download is not running"),
            ClientError::Auth(e) => write!(f, "authentication failed: {e}"),
            ClientError::Protocol(e) => write!(f, "protocol error: {e}"),
            ClientError::Server(e) => write!(f, "{e}"),
            ClientError::Io(e) => write!(f, "i/o error: {e}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<FrameError> for ClientError {
    fn from(e: FrameError) -> Self {
        match e {
            FrameError::Io(e) => ClientError::Io(e),
            other => ClientError::Protocol(other.to_string()),
        }
    }
}

pub struct Client {
    stream: Stream,
}

/// Time allowed for one request/response round trip.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

impl Client {
    pub async fn connect(ep: &Endpoint) -> Result<Client, ClientError> {
        let mut stream = match ipc::connect(&ep.name).await {
            Ok(s) => s,
            Err(e) if ipc::is_absent(&e) => return Err(ClientError::NotRunning),
            Err(e) => return Err(ClientError::Io(e)),
        };
        let token = ipc::read_token_file(&ep.token_path).map_err(ClientError::Io)?;
        let hello = serde_json::to_vec(&Hello { v: VERSION, token })
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        write_frame(&mut stream, &hello).await?;
        let resp = tokio::time::timeout(CALL_TIMEOUT, read_frame(&mut stream))
            .await
            .map_err(|_| ClientError::Protocol("handshake timed out".into()))??;
        let r: Response = serde_json::from_slice(&resp)
            .map_err(|_| ClientError::Protocol("bad hello reply".into()))?;
        if !r.ok {
            return Err(ClientError::Auth(r.error.unwrap_or_default()));
        }
        Ok(Client { stream })
    }

    /// Send `command` under the idempotency key `id` and return the server's result.
    /// Retrying with the same `id` after a lost reply never runs the command twice.
    pub async fn call_with_id(
        &mut self,
        id: &str,
        command: Command,
    ) -> Result<serde_json::Value, ClientError> {
        let body = serde_json::to_vec(&Request {
            v: VERSION,
            id: id.to_owned(),
            command,
        })
        .map_err(|e| ClientError::Protocol(e.to_string()))?;
        write_frame(&mut self.stream, &body).await?;
        let bytes = tokio::time::timeout(CALL_TIMEOUT, read_frame(&mut self.stream))
            .await
            .map_err(|_| ClientError::Protocol("request timed out".into()))??;
        let r: Response = serde_json::from_slice(&bytes)
            .map_err(|_| ClientError::Protocol("bad reply".into()))?;
        if r.ok {
            Ok(r.result.unwrap_or(serde_json::Value::Null))
        } else {
            Err(ClientError::Server(
                r.error.unwrap_or_else(|| "failed".into()),
            ))
        }
    }

    /// Like [`call_with_id`](Self::call_with_id) with a fresh random id.
    pub async fn call(&mut self, command: Command) -> Result<serde_json::Value, ClientError> {
        let id = ipc::new_token().map_err(ClientError::Io)?[..32].to_owned();
        self.call_with_id(&id, command).await
    }
}
