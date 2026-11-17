//! `eid serve`: the headless app. It owns the engine, the queue scheduler and the per-user IPC
//! endpoint. A second `eid serve` does not start another instance: it forwards its `--add` URLs
//! to the running one and exits.

use crate::remote::{self, ConnArgs};
use eid_engine::config::Config;
use eid_engine::queue::{Queue, Status};
use eid_engine::queue_runner::EngineRunner;
use eid_engine::scheduler::{NoPower, QueueEvent, Scheduler, SchedulerConfig, SystemClock};
use eid_engine::urlsafe;
use eid_engine::CancellationToken;
use eid_ipc::{Command, ServeOptions, StartError};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub const HELP: &str = "\
  eid serve [-d DIR] [--queues FILE] [--queue NAME] [--parallel N] [--add URL]... [-c N] [--limit RATE] [-q]\n\
            [--endpoint NAME] [--token-file PATH]\n\
  eid add <url> [--subdir NAME] [--name FILE] | eid list | eid pause <id> | eid resume <id> | eid stop\n\
            [--endpoint NAME] [--token-file PATH]   (talk to the running `eid serve`)\n\n\
  serve runs the downloads of one queue and listens on a per-user local endpoint (no TCP port).\n\
  If one is already running, `serve --add URL` hands the URLs to it and exits.";

#[derive(Debug, Default, PartialEq)]
pub struct ServeArgs {
    pub conn: ConnArgs,
    pub dir: Option<PathBuf>,
    pub queues: Option<PathBuf>,
    pub queue: String,
    pub parallel: Option<u32>,
    pub adds: Vec<String>,
    pub connections: Option<usize>,
    pub limit_bps: Option<u64>,
    pub quiet: bool,
    pub ramp_ms: Option<u64>,
    pub checkpoint_ms: Option<u64>,
    pub min_split: Option<u64>,
    pub stall_ms: Option<u64>,
    pub attempts: Option<u32>,
}

pub fn parse(args: &[String]) -> Result<ServeArgs, String> {
    let mut a = ServeArgs {
        queue: "main".into(),
        ..Default::default()
    };
    let mut it = args.iter();
    let num = |it: &mut std::slice::Iter<String>, f: &str| -> Result<u64, String> {
        it.next()
            .ok_or(format!("{f} needs a value"))?
            .parse::<u64>()
            .map_err(|_| format!("{f}: not a number"))
    };
    while let Some(x) = it.next() {
        if remote::parse_conn_flag(&mut a.conn, x, &mut it)? {
            continue;
        }
        match x.as_str() {
            "-d" | "--dir" => a.dir = Some(PathBuf::from(it.next().ok_or("-d needs a value")?)),
            "--queues" => {
                a.queues = Some(PathBuf::from(it.next().ok_or("--queues needs a value")?))
            }
            "--queue" => a.queue = it.next().ok_or("--queue needs a value")?.clone(),
            "--parallel" => a.parallel = Some(num(&mut it, x)? as u32),
            "--add" => a.adds.push(it.next().ok_or("--add needs a value")?.clone()),
            "-c" | "--connections" => {
                let n = num(&mut it, x)?;
                if !(1..=32).contains(&n) {
                    return Err("-c must be between 1 and 32".into());
                }
                a.connections = Some(n as usize);
            }
            "--limit" => {
                a.limit_bps = Some(crate::args::parse_rate(
                    it.next().ok_or("--limit needs a value")?,
                )?)
            }
            "-q" | "--quiet" => a.quiet = true,
            "--ramp-ms" => a.ramp_ms = Some(num(&mut it, x)?),
            "--checkpoint-ms" => a.checkpoint_ms = Some(num(&mut it, x)?),
            "--min-split" => a.min_split = Some(num(&mut it, x)?),
            "--stall-ms" => a.stall_ms = Some(num(&mut it, x)?),
            "--attempts" => a.attempts = Some(num(&mut it, x)? as u32),
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option: {s}")),
            _ => return Err(format!("unexpected argument: {x}")),
        }
    }
    Ok(a)
}

pub fn config_for(a: &ServeArgs) -> Config {
    let mut cfg = Config::default();
    cfg.connections = a.connections.unwrap_or(cfg.connections);
    cfg.job_limit_bps = a.limit_bps;
    cfg.compute_sha256 = true;
    if let Some(v) = a.ramp_ms {
        cfg.ramp_interval = Duration::from_millis(v);
    }
    if let Some(v) = a.checkpoint_ms {
        cfg.checkpoint_interval = Duration::from_millis(v);
    }
    if let Some(v) = a.min_split {
        cfg.min_split = v;
    }
    if let Some(v) = a.stall_ms {
        cfg.stall_timeout = Duration::from_millis(v);
    }
    if let Some(v) = a.attempts {
        cfg.max_attempts = v;
    }
    cfg
}

/// What `ping` answers in its `app` field: the headless server or the desktop app.
pub const APP_SERVE: &str = "serve";
pub const APP_GUI: &str = "gui";

/// The per-user queue file shared by `eid serve` and the desktop app (one owner at a time).
pub fn default_queue_file() -> Result<PathBuf, String> {
    Ok(eid_platform::ipc::config_dir()
        .ok_or("no per-user config directory; pass --queues FILE")?
        .join("queues.json"))
}

/// Items returned by `list` (the response must stay below the 1 MiB frame limit).
const MAX_LIST: usize = 1500;

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Pending => "pending",
        Status::Running => "running",
        Status::Paused => "paused",
        Status::Done => "done",
        Status::Failed => "failed",
    }
}

/// The IPC command handler over the scheduler. It never lets the caller choose a directory and
/// never returns a URL with credentials or a query string.
pub fn handler(
    sched: Scheduler,
    queue: String,
    cancel: CancellationToken,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    app: &'static str,
) -> impl Fn(Command) -> Result<serde_json::Value, String> + Send + Sync + 'static {
    move |cmd| match cmd {
        Command::Ping {} => {
            Ok(json!({"pong": true, "version": env!("CARGO_PKG_VERSION"), "app": app}))
        }
        Command::Add {
            url,
            subdir,
            file_name,
        } => {
            let id = sched
                .add_item(&queue, &url, subdir.as_deref(), file_name.as_deref(), None)
                .map_err(|e| urlsafe::redact_text(&e.to_string()))?;
            Ok(json!({"id": id}))
        }
        Command::List {} => {
            let snap = sched.snapshot();
            let all: Vec<_> = snap.queues.iter().flat_map(|q| &q.items).collect();
            let items: Vec<_> = all
                .iter()
                .take(MAX_LIST)
                .map(|i| {
                    json!({
                        "id": i.id,
                        "status": status_name(i.status),
                        "url": urlsafe::redact_text(&i.url),
                        "file_name": i.file_name,
                        "attempts": i.attempts,
                        "error": i.last_error.as_deref().map(urlsafe::redact_text),
                    })
                })
                .collect();
            Ok(json!({"items": items, "total": all.len()}))
        }
        Command::Pause { item } => sched
            .pause_item(item)
            .map(|_| json!({}))
            .map_err(|e| e.to_string()),
        Command::Shutdown {} => {
            // answer first, then stop: the reply must not be cut off by the server shutting down
            stopped.store(true, std::sync::atomic::Ordering::SeqCst);
            let c = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                c.cancel();
            });
            Ok(json!({}))
        }
        Command::Resume { item } => sched
            .resume_item(item)
            .map(|_| json!({}))
            .map_err(|e| e.to_string()),
        // the desktop app answers this itself (eid-gui wraps this handler); the headless
        // server has no window to show
        Command::Show {} => Err("no window: this is the background server".into()),
    }
}

/// Hand the `--add` URLs (or a ping) to the instance that is already running.
async fn forward(a: &ServeArgs) -> Result<i32, String> {
    let ep = a.conn.endpoint().map_err(|e| e.to_string())?;
    if a.adds.is_empty() {
        remote::call_retrying(&ep, Command::Ping {}).await?;
        eprintln!("Eid Download is already running");
        return Ok(0);
    }
    for url in &a.adds {
        let r = remote::call_retrying(
            &ep,
            Command::Add {
                url: url.clone(),
                subdir: None,
                file_name: None,
            },
        )
        .await?;
        println!("added #{} (handed to the running instance)", r["id"]);
    }
    Ok(0)
}

/// Returns the exit code.
pub async fn run(a: ServeArgs, cancel: CancellationToken) -> Result<i32, String> {
    let ep = a.conn.endpoint().map_err(|e| e.to_string())?;
    // Is one already running? Then forward and leave: the single instance owns the queue file.
    if matches!(
        eid_ipc::Client::connect(&ep).await,
        Ok(_) | Err(eid_ipc::ClientError::Auth(_))
    ) {
        return forward(&a).await;
    }
    let dir = a
        .dir
        .clone()
        .or_else(eid_platform::dirs::default_download_dir)
        .ok_or("cannot find the Downloads folder; pass -d DIR")?;
    let dir = std::path::absolute(&dir).map_err(|e| e.to_string())?;
    let qfile = match &a.queues {
        Some(p) => p.clone(),
        None => default_queue_file()?,
    };
    if let Some(parent) = qfile.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let quiet = a.quiet;
    let sink: eid_engine::scheduler::QueueSink = Arc::new(move |e| {
        if quiet {
            return;
        }
        match e {
            QueueEvent::ItemStarted { queue, item } => eprintln!("[{queue}] #{item} started"),
            QueueEvent::ItemDone { queue, item } => eprintln!("[{queue}] #{item} done"),
            QueueEvent::ItemFailed {
                queue,
                item,
                message,
            } => eprintln!("[{queue}] #{item} FAILED: {message}"),
            QueueEvent::ItemStopped {
                queue,
                item,
                reason,
            } => {
                eprintln!("[{queue}] #{item} stopped ({reason:?})")
            }
            _ => {}
        }
    });
    let sched = Scheduler::open(
        SchedulerConfig::new(&qfile),
        Arc::new(EngineRunner::new(config_for(&a))),
        Arc::new(SystemClock),
        Arc::new(NoPower),
        sink,
    )
    .map_err(|e| e.to_string())?;
    // the default queue exists, with the download folder chosen here (never by an IPC caller)
    let (qname, parallel) = (a.queue.clone(), a.parallel);
    sched
        .edit(|qs| {
            if qs.queue_mut(&qname).is_none() {
                qs.queues.push(Queue::new(&qname, &dir.to_string_lossy()));
            }
            if let Some(p) = parallel {
                qs.queue_mut(&qname).expect("just ensured").max_parallel = p;
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;

    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let h = Arc::new(handler(
        sched.clone(),
        a.queue.clone(),
        cancel.clone(),
        stopped.clone(),
        APP_SERVE,
    ));
    let running = match eid_ipc::start(&ep, h, ServeOptions::default()) {
        Ok(r) => r,
        // lost a start-up race with another instance: behave as the second instance
        Err(StartError::AlreadyRunning) => return forward(&a).await,
        Err(e) => return Err(e.to_string()),
    };
    for url in &a.adds {
        let id = sched
            .add_item(&a.queue, url, None, None, None)
            .map_err(|e| urlsafe::redact_text(&e.to_string()))?;
        println!("added #{id}");
    }
    eprintln!("eid serve: ready (endpoint '{}')", ep.name);
    let r = sched.run(cancel.clone(), false).await;
    // The queue file is final before the endpoint goes away: the desktop app takes over the
    // queue as soon as it sees the endpoint vanish. The second flush keeps an `add` that arrived
    // while the first one was being written.
    let first = sched.flush().await;
    running.stop().await;
    first.map_err(|e| e.to_string())?;
    sched.flush().await.map_err(|e| e.to_string())?;
    r.map_err(|e| e.to_string())?;
    // Ctrl-C = 130; a requested `eid stop` is a normal exit
    Ok(
        if cancel.is_cancelled() && !stopped.load(std::sync::atomic::Ordering::SeqCst) {
            130
        } else {
            0
        },
    )
}
