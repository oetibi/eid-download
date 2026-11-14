//! The IPC server: authenticates each connection with the token, then serves bounded,
//! idempotent requests. It never trusts the peer: every frame is size-checked before it is read,
//! every message is validated, and any protocol violation closes that connection only.

use crate::frame::{read_frame, read_frame_max, write_frame, FrameError, MAX_FRAME};
use crate::proto::{self, Command, Request, Response};
use eid_platform::ipc::{token_eq, Listener, Stream};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Executes a validated command. Must be quick and must not block: it runs under the
/// idempotency lock. Errors must already be free of secrets.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, cmd: Command) -> Result<serde_json::Value, String>;
}

impl<F> Handler for F
where
    F: Fn(Command) -> Result<serde_json::Value, String> + Send + Sync + 'static,
{
    fn handle(&self, cmd: Command) -> Result<serde_json::Value, String> {
        self(cmd)
    }
}

#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Time a new connection has to send its hello.
    pub handshake_timeout: Duration,
    /// A connection idle for this long is closed.
    pub idle_timeout: Duration,
    /// Simultaneous connections; extra ones are dropped immediately.
    pub max_connections: usize,
    /// Remembered request ids (oldest forgotten first).
    pub dedup_capacity: usize,
}

impl Default for ServeOptions {
    fn default() -> Self {
        ServeOptions {
            handshake_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(120),
            max_connections: 64,
            dedup_capacity: 1024,
        }
    }
}

/// Remembers recent request ids so that a retried request returns the first answer instead of
/// running twice. The same id with a different command is an error.
struct Dedup {
    cap: usize,
    order: VecDeque<String>,
    map: HashMap<String, (String, Response)>,
}

impl Dedup {
    fn new(cap: usize) -> Dedup {
        Dedup {
            cap: cap.max(1),
            order: VecDeque::new(),
            map: HashMap::new(),
        }
    }
}

/// Run `handler` for `req` once per id (under the lock, so two connections racing with the same
/// id still run it once).
fn handle_once(dedup: &Mutex<Dedup>, handler: &dyn Handler, req: Request) -> Response {
    let fingerprint = serde_json::to_string(&req.command).unwrap_or_default();
    let mut d = dedup.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((fp, resp)) = d.map.get(&req.id) {
        return if *fp == fingerprint {
            resp.clone()
        } else {
            Response::err(
                &req.id,
                "request id was already used for a different command",
            )
        };
    }
    let resp = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handler.handle(req.command.clone())
    })) {
        Ok(Ok(v)) => Response::ok(&req.id, v),
        Ok(Err(e)) => Response::err(&req.id, e),
        Err(_) => Response::err(&req.id, "internal error"),
    };
    while d.order.len() >= d.cap {
        if let Some(old) = d.order.pop_front() {
            d.map.remove(&old);
        }
    }
    d.order.push_back(req.id.clone());
    d.map.insert(req.id.clone(), (fingerprint, resp.clone()));
    resp
}

async fn send(stream: &mut Stream, resp: &Response) -> Result<(), FrameError> {
    let mut bytes =
        serde_json::to_vec(resp).map_err(|e| FrameError::Io(std::io::Error::other(e)))?;
    if bytes.len() > MAX_FRAME {
        bytes = serde_json::to_vec(&Response::err(&resp.id, "response too large"))
            .map_err(|e| FrameError::Io(std::io::Error::other(e)))?;
    }
    write_frame(stream, &bytes).await
}

async fn serve_connection(
    mut s: Stream,
    token: Arc<String>,
    handler: Arc<dyn Handler>,
    dedup: Arc<Mutex<Dedup>>,
    opts: ServeOptions,
) {
    // --- handshake: one small frame, bounded in size and time
    let hello = tokio::time::timeout(opts.handshake_timeout, read_frame_max(&mut s, 4096)).await;
    let Ok(Ok(bytes)) = hello else {
        return;
    };
    match proto::parse_hello(&bytes) {
        Ok(h) if token_eq(&h.token, &token) => {
            if send(
                &mut s,
                &Response::ok("", serde_json::json!({"hello": true})),
            )
            .await
            .is_err()
            {
                return;
            }
        }
        Ok(_) => {
            let _ = send(&mut s, &Response::err("", "authentication failed")).await;
            let _ = s.shutdown().await;
            return;
        }
        Err(e) => {
            let _ = send(&mut s, &Response::err("", e)).await;
            let _ = s.shutdown().await;
            return;
        }
    }
    // --- requests
    loop {
        let frame = match tokio::time::timeout(opts.idle_timeout, read_frame(&mut s)).await {
            Ok(Ok(f)) => f,
            Ok(Err(FrameError::Closed)) | Err(_) => return,
            Ok(Err(e)) => {
                // oversized / truncated / empty / io: one error reply (best effort), then close
                let _ = send(&mut s, &Response::err("", e.to_string())).await;
                return;
            }
        };
        let resp = match proto::parse_request(&frame) {
            Ok(req) => handle_once(&dedup, handler.as_ref(), req),
            Err(e) => {
                let _ = send(&mut s, &Response::err("", e)).await;
                return; // a peer that sends garbage is disconnected
            }
        };
        if send(&mut s, &resp).await.is_err() {
            return;
        }
    }
}

/// Accept connections until `cancel` fires.
pub async fn serve(
    mut listener: Listener,
    token: String,
    handler: Arc<dyn Handler>,
    opts: ServeOptions,
    cancel: CancellationToken,
) {
    let token = Arc::new(token);
    let dedup = Arc::new(Mutex::new(Dedup::new(opts.dedup_capacity)));
    let slots = Arc::new(Semaphore::new(opts.max_connections.max(1)));
    let mut conns: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            r = listener.accept() => {
                let Ok(stream) = r else {
                    // transient accept failure: do not spin
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    drop(stream); // over the connection limit
                    continue;
                };
                let (t, h, d, o) = (token.clone(), handler.clone(), dedup.clone(), opts.clone());
                conns.spawn(async move {
                    serve_connection(stream, t, h, d, o).await;
                    drop(permit);
                });
            }
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
    conns.abort_all();
}
