//! `eid` command line: `eid get <url>` and `eid resume <state-file>`.
//! Progress goes to stderr; the final path goes to stdout. URLs are never printed with
//! credentials or query strings.

use eid_cli::args::{self, Cmd, Common};
use eid_cli::quote;
use eid_engine::{CancellationToken, Config, Event, EventSink, JobSpec};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HELP: &str = "eid - Eid Download command line\n\n\
USAGE:\n  eid get <url> [-d DIR] [-o NAME] [-c N] [--limit RATE] [--sha256 HEX] [-q]\n  eid resume <file.eid-state> [-c N] [--limit RATE] [--sha256 HEX] [-q]\n\n\
  eid queue ... / eid grab ...   download queues and the page link grabber (see `eid queue`)\n\n\
  -c N          connections (1..=32, default 8)\n  --limit RATE  bytes/s, e.g. 500K, 2M\n  -q            no progress output\n\n\
Ctrl-C stops safely and prints the command that resumes the download.";

fn apply(common: &Common, cfg: &mut Config) {
    if let Some(c) = common.connections {
        cfg.connections = c;
    }
    cfg.job_limit_bps = common.limit_bps;
    cfg.expected_sha256 = common.sha256.clone();
    cfg.compute_sha256 = true;
    if let Some(v) = common.ramp_ms {
        cfg.ramp_interval = Duration::from_millis(v);
    }
    if let Some(v) = common.checkpoint_ms {
        cfg.checkpoint_interval = Duration::from_millis(v);
    }
    if let Some(v) = common.min_split {
        cfg.min_split = v;
    }
    if let Some(v) = common.stall_ms {
        cfg.stall_timeout = Duration::from_millis(v);
    }
    if let Some(v) = common.attempts {
        cfg.max_attempts = v;
    }
}

fn print_resume_hint(state: &Path) {
    let s = state.to_string_lossy();
    eprintln!("To resume, run:");
    if eid_platform::IS_WINDOWS {
        eprintln!("  PowerShell: eid resume {}", quote::powershell(&s));
        match quote::cmd(&s) {
            Some(c) => eprintln!("  cmd:        eid resume {c}"),
            None => eprintln!(
                "  cmd:        (path contains characters cmd cannot quote safely; use PowerShell)"
            ),
        }
    } else {
        eprintln!("  eid resume {}", quote::posix(&s));
    }
}

fn sink(quiet: bool, name: Arc<Mutex<Option<String>>>) -> EventSink {
    Arc::new(move |e: Event| {
        if let Event::Started { name: n, .. } = &e {
            *name.lock().unwrap() = Some(n.clone());
        }
        if quiet {
            return;
        }
        match e {
            Event::Started {
                name,
                total,
                ranged,
            } => {
                eprintln!(
                    "{name}: {} bytes, {}",
                    total.map_or("unknown".into(), |t| t.to_string()),
                    if ranged { "segmented" } else { "single stream" }
                )
            }
            Event::Progress { done, total } => match total {
                Some(t) if t > 0 => eprintln!("  {done}/{t} ({}%)", done.saturating_mul(100) / t),
                _ => eprintln!("  {done} bytes"),
            },
            Event::Retry {
                attempt,
                reason,
                delay_ms,
            } => eprintln!("  retry #{attempt} in {delay_ms} ms: {reason}"),
            Event::Throttled {
                status,
                retry_after_ms,
            } => eprintln!("  server throttling (HTTP {status}), waiting {retry_after_ms} ms"),
            Event::ConnectionsLowered { to } => eprintln!("  connections lowered to {to}"),
            Event::Restarted { reason } => eprintln!("  restarting from scratch: {reason}"),
            Event::Finished { size } => eprintln!("done: {size} bytes"),
            _ => {}
        }
    })
}

async fn queue_main(argv: &[String]) -> i32 {
    use eid_cli::queue_cmd as q;
    let cancel = CancellationToken::new();
    {
        let c = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("stopping safely...");
                c.cancel();
            }
        });
    }
    let result: Result<i32, String> = match argv[0].as_str() {
        "grab" => match q::parse_grab(&argv[1..]) {
            Ok(g) => q::run_grab(&g, &cancel).await.map(|_| 0),
            Err(e) => Err(format!("usage: {e}")),
        },
        _ => match q::parse_queue(&argv[1..]) {
            Ok(a) => match a.sub.as_str() {
                "add" => q::run_add(&a).map(|_| 0),
                "list" => q::run_list(&a).map(|_| 0),
                "import" => q::run_import(&a).map(|_| 0),
                "export" => q::run_export(&a).map(|_| 0),
                _ => q::run_queue(&a, cancel).await,
            },
            Err(e) => Err(format!("usage: {e}")),
        },
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}\n\n{}", q::HELP);
            2
        }
    }
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if matches!(argv.first().map(String::as_str), Some("queue" | "grab")) {
        std::process::exit(queue_main(&argv).await);
    }
    let cmd = match args::parse(&argv) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}\n\n{HELP}");
            std::process::exit(2);
        }
    };
    let cancel = CancellationToken::new();
    {
        let c = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("stopping safely...");
                c.cancel();
            }
        });
    }
    let name = Arc::new(Mutex::new(None::<String>));
    let mut cfg = Config::default();
    let (result, dir, quiet, state_hint): (_, PathBuf, bool, Option<PathBuf>) = match cmd {
        Cmd::Help => {
            println!("{HELP}");
            return;
        }
        Cmd::Version => {
            println!("eid {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Cmd::Get(g) => {
            apply(&g.common, &mut cfg);
            let dir = g.dir.clone().unwrap_or_else(|| PathBuf::from("."));
            let spec = JobSpec {
                url: g.url,
                dir: dir.clone(),
                file_name: g.name,
            };
            let r =
                eid_engine::download(spec, cfg, sink(g.common.quiet, name.clone()), cancel).await;
            (r, dir, g.common.quiet, None)
        }
        Cmd::Resume(r) => {
            apply(&r.common, &mut cfg);
            let dir = r.state.parent().map(Path::to_path_buf).unwrap_or_default();
            let res =
                eid_engine::resume(&r.state, cfg, sink(r.common.quiet, name.clone()), cancel).await;
            (res, dir, r.common.quiet, Some(r.state))
        }
    };
    match result {
        Ok(o) => {
            println!("{}", o.path.display());
            if let Some(h) = o.sha256 {
                if !quiet {
                    eprintln!("sha256 {h}");
                }
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            let state = state_hint.or_else(|| {
                name.lock()
                    .unwrap()
                    .as_ref()
                    .map(|n| dir.join(format!("{n}{}", eid_engine::download::STATE_SUFFIX)))
            });
            if let Some(s) = state {
                if s.exists() {
                    print_resume_hint(&s);
                }
            }
            std::process::exit(if matches!(e, eid_engine::Error::Cancelled) {
                130
            } else {
                1
            });
        }
    }
}
