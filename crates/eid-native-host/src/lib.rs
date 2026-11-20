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

/// Starts Eid Download when the browser needs it and it is not running.
///
/// v0.2 (owner's request 2026-11-20: the button and the app work together, no duplicate windows
/// or processes): the DESKTOP APP next to this host (`EidDownload.exe --endpoint NAME`) is
/// started, so a click in the browser opens the window where the job shows (and "Open in Eid
/// Download" has a window to open). The app is single-instance on its endpoint. Without it next
/// to the host (a command-line-only setup), or with `EID_NATIVE_HOST_APP` / a token file set
/// (portable setups and tests: as trusted as the rest of the user's environment), it is
/// `<eid> serve --endpoint NAME --token-file PATH` as before.
pub struct AppLauncher {
    pub exe: Option<std::path::PathBuf>,
    pub endpoint: Endpoint,
    /// A token file other than the endpoint's default (`EID_IPC_TOKEN_FILE`): the desktop app
    /// has no option for it, so `eid serve` is used.
    pub custom_token: bool,
}

/// What [`AppLauncher`] starts: the program and its arguments. Pure (unit tested).
pub fn launch_plan(
    here: &std::path::Path,
    exe: Option<&std::path::Path>,
    endpoint: &Endpoint,
    custom_token: bool,
    is_file: &dyn Fn(&std::path::Path) -> bool,
) -> std::io::Result<(std::path::PathBuf, Vec<std::ffi::OsString>)> {
    let sfx = std::env::consts::EXE_SUFFIX;
    let serve = |p: std::path::PathBuf| {
        (
            p,
            vec![
                std::ffi::OsString::from("serve"),
                "--endpoint".into(),
                endpoint.name.clone().into(),
                "--token-file".into(),
                endpoint.token_path.clone().into(),
            ],
        )
    };
    if let Some(p) = exe {
        return if is_file(p) {
            Ok(serve(p.to_path_buf()))
        } else {
            Err(not_found())
        };
    }
    let gui = here.with_file_name(format!("EidDownload{sfx}"));
    if !custom_token && is_file(&gui) {
        return Ok((gui, vec!["--endpoint".into(), endpoint.name.clone().into()]));
    }
    let cli = here.with_file_name(format!("eid{sfx}"));
    if is_file(&cli) {
        Ok(serve(cli))
    } else {
        Err(not_found())
    }
}

fn not_found() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "the Eid Download program was not found",
    )
}

impl AppLauncher {
    pub fn resolve(&self) -> std::io::Result<(std::path::PathBuf, Vec<std::ffi::OsString>)> {
        launch_plan(
            &std::env::current_exe()?,
            self.exe.as_deref(),
            &self.endpoint,
            self.custom_token,
            &|p| p.is_file(),
        )
    }
}

impl Launcher for AppLauncher {
    fn launch(&self) -> std::io::Result<()> {
        let (exe, args) = self.resolve()?;
        eid_platform::process::spawn_detached(exe, args)
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
        let custom_token = token_file.is_some();
        if let Some(p) = token_file {
            endpoint.token_path = p.into();
        }
        Ok(HostConfig {
            launcher: Some(Box::new(AppLauncher {
                exe: std::env::var_os("EID_NATIVE_HOST_APP").map(Into::into),
                endpoint: endpoint.clone(),
                custom_token,
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

#[cfg(test)]
mod launch_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// v0.2: the browser starts the desktop app when it is next to the host (one window, no
    /// second process); `eid serve` only without it, or when the setup overrides the program or
    /// the token file; nothing is started when neither exists.
    #[test]
    fn the_desktop_app_next_to_the_host_is_started_first() {
        let sfx = std::env::consts::EXE_SUFFIX;
        let dir = PathBuf::from("inst");
        let here = dir.join(format!("eid-native-host{sfx}"));
        let gui = dir.join(format!("EidDownload{sfx}"));
        let cli = dir.join(format!("eid{sfx}"));
        let ep = Endpoint {
            name: "main".into(),
            token_path: dir.join("tok"),
        };
        let has = |l: Vec<PathBuf>| move |p: &Path| l.iter().any(|x| x == p);
        let (exe, args) = launch_plan(
            &here,
            None,
            &ep,
            false,
            &has(vec![gui.clone(), cli.clone()]),
        )
        .unwrap();
        assert_eq!(exe, gui);
        assert_eq!(args, ["--endpoint", "main"]);
        // no desktop app: eid serve, as before
        let (exe, args) = launch_plan(&here, None, &ep, false, &has(vec![cli.clone()])).unwrap();
        assert_eq!(exe, cli);
        assert_eq!(args[0], "serve");
        // a custom token file: the desktop app has no option for it, so eid serve
        let (exe, _) =
            launch_plan(&here, None, &ep, true, &has(vec![gui.clone(), cli.clone()])).unwrap();
        assert_eq!(exe, cli);
        // EID_NATIVE_HOST_APP: exactly that program, as `serve`
        let custom = PathBuf::from("elsewhere").join("eid");
        let (exe, args) = launch_plan(
            &here,
            Some(&custom),
            &ep,
            false,
            &has(vec![custom.clone(), gui]),
        )
        .unwrap();
        assert_eq!((exe, args[0].clone()), (custom, "serve".into()));
        assert!(launch_plan(&here, None, &ep, false, &has(vec![])).is_err());
    }
}
