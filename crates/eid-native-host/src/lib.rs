//! Browser Native Messaging host: relays the extension's messages to the running app over the
//! per-user IPC endpoint (ADR-0002). The browser is the only party that can start it, and the
//! browser itself checks the extension id (`allowed_origins` in the host manifest).
//!
//! Rules enforced here, each with a test:
//! * stdout carries protocol frames only (nothing in this crate prints; logs go to stderr);
//! * frames are bounded (1 MiB) and checked before being read; a bad frame ends the session
//!   because the stream can no longer be trusted to be in sync;
//! * messages must be exactly a `Request` (unknown fields refused) and only `ping`/`add` are
//!   allowed from a browser;
//! * a retried request keeps its id, so the app never runs it twice;
//! * if the app is not running it is started once and awaited with a deadline.

use eid_ipc::frame::{read_frame, write_frame, FrameError};
use eid_ipc::{proto, Client, ClientError, Endpoint, Response};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};

/// Starts the app when nobody is listening.
pub trait Launcher: Send + Sync {
    fn launch(&self) -> std::io::Result<()>;
}

/// Launches the Eid Download program as `<exe> serve --endpoint NAME --token-file PATH`.
/// `exe` is the `eid` executable next to this host, or `EID_NATIVE_HOST_APP` when set (portable
/// installs and tests; the variable is as trusted as the rest of the user's own environment).
pub struct AppLauncher {
    pub exe: Option<std::path::PathBuf>,
    pub endpoint: Endpoint,
}

impl AppLauncher {
    pub fn resolve(&self) -> std::io::Result<std::path::PathBuf> {
        let exe = match &self.exe {
            Some(p) => p.clone(),
            None => std::env::current_exe()?
                .with_file_name(format!("eid{}", std::env::consts::EXE_SUFFIX)),
        };
        if exe.is_file() {
            Ok(exe)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the Eid Download program was not found",
            ))
        }
    }
}

impl Launcher for AppLauncher {
    fn launch(&self) -> std::io::Result<()> {
        eid_platform::process::spawn_detached(
            self.resolve()?,
            [
                std::ffi::OsString::from("serve"),
                "--endpoint".into(),
                self.endpoint.name.clone().into(),
                "--token-file".into(),
                self.endpoint.token_path.clone().into(),
            ],
        )
    }
}

pub struct HostConfig {
    pub endpoint: Endpoint,
    pub launcher: Option<Box<dyn Launcher>>,
    /// How long to wait for a freshly launched app to start listening.
    pub ready_deadline: Duration,
    pub poll: Duration,
}

impl HostConfig {
    /// Production configuration: the per-user default endpoint, overridable (for portable
    /// installs and tests) with `EID_IPC_ENDPOINT` / `EID_IPC_TOKEN_FILE`.
    pub fn from_env() -> std::io::Result<HostConfig> {
        let name = std::env::var("EID_IPC_ENDPOINT").unwrap_or_else(|_| "main".into());
        let token_file = std::env::var_os("EID_IPC_TOKEN_FILE");
        let mut endpoint = match Endpoint::default_for(&name) {
            Ok(e) => e,
            Err(e) => match &token_file {
                Some(p) => Endpoint {
                    name: name.clone(),
                    token_path: p.into(),
                },
                None => return Err(e),
            },
        };
        if let Some(p) = token_file {
            endpoint.token_path = p.into();
        }
        Ok(HostConfig {
            launcher: Some(Box::new(AppLauncher {
                exe: std::env::var_os("EID_NATIVE_HOST_APP").map(Into::into),
                endpoint: endpoint.clone(),
            })),
            endpoint,
            // `EID_NATIVE_HOST_READY_MS` (1..=60000) shortens/lengthens the wait for a launched app
            ready_deadline: std::env::var("EID_NATIVE_HOST_READY_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| (1..=60_000).contains(v))
                .map_or(Duration::from_secs(15), Duration::from_millis),
            poll: Duration::from_millis(100),
        })
    }
}

/// How the session ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// The browser closed our stdin.
    Eof,
    /// A frame violated the protocol; the stream was abandoned.
    ProtocolError,
}

/// Connect, starting the app once if nobody is listening, until the deadline.
async fn connect(cfg: &HostConfig) -> Result<Client, String> {
    let deadline = Instant::now() + cfg.ready_deadline;
    let mut launched = false;
    loop {
        match Client::connect(&cfg.endpoint).await {
            Ok(c) => return Ok(c),
            Err(e) => {
                if matches!(e, ClientError::NotRunning) && !launched {
                    launched = true;
                    if let Some(l) = &cfg.launcher {
                        if let Err(e) = l.launch() {
                            eprintln!("eid-native-host: cannot start the app: {e}");
                        }
                    }
                }
                if Instant::now() >= deadline {
                    return Err(match e {
                        ClientError::NotRunning => "Eid Download is not running".to_string(),
                        _ => "cannot reach Eid Download".to_string(),
                    });
                }
                tokio::time::sleep(cfg.poll).await;
            }
        }
    }
}

/// Relay one validated request. A transport failure is retried once on a fresh connection
/// **with the same id** (the app answers duplicates from its cache).
async fn relay(cfg: &HostConfig, client: &mut Option<Client>, req: &proto::Request) -> Response {
    let mut last = "cannot reach Eid Download".to_string();
    for _ in 0..2 {
        if client.is_none() {
            match connect(cfg).await {
                Ok(c) => *client = Some(c),
                Err(e) => return Response::err(&req.id, e),
            }
        }
        let c = client.as_mut().expect("connected above");
        match c.call_with_id(&req.id, req.command.clone()).await {
            Ok(v) => return Response::ok(&req.id, v),
            Err(ClientError::Server(m)) => return Response::err(&req.id, m),
            Err(e) => {
                eprintln!("eid-native-host: relay failed: {e}");
                last = "lost the connection to Eid Download".to_string();
                *client = None;
            }
        }
    }
    Response::err(&req.id, last)
}

async fn reply<W: AsyncWrite + Unpin>(out: &mut W, r: &Response) -> bool {
    match serde_json::to_vec(r) {
        Ok(b) => write_frame(out, &b).await.is_ok(),
        Err(_) => false,
    }
}

/// Serve one browser session until it closes stdin or breaks the protocol.
pub async fn run<R, W>(input: &mut R, output: &mut W, cfg: &HostConfig) -> Exit
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut client: Option<Client> = None;
    loop {
        let frame = match read_frame(input).await {
            Ok(f) => f,
            Err(FrameError::Closed) => return Exit::Eof,
            Err(e) => {
                // oversized / truncated / empty / i/o: tell the browser (best effort) and stop
                eprintln!("eid-native-host: bad frame: {e}");
                let _ = reply(output, &Response::err("", e.to_string())).await;
                return Exit::ProtocolError;
            }
        };
        let req = match proto::parse_request(&frame) {
            Ok(r) => r,
            Err(e) => {
                // the framing is intact, so the session can continue
                if !reply(output, &Response::err("", e)).await {
                    return Exit::ProtocolError;
                }
                continue;
            }
        };
        let resp = if req.command.allowed_from_browser() {
            relay(cfg, &mut client, &req).await
        } else {
            Response::err(&req.id, "command not allowed from the browser")
        };
        if !reply(output, &resp).await {
            return Exit::ProtocolError;
        }
    }
}
