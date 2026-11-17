//! `eid queue ...` and `eid grab`: standalone queue management on a queue file (the remote
//! variant against a running app arrives with IPC in P3). URLs are printed without query or
//! credentials; nothing here ever puts a secret on disk (queue items are credential-free).

use crate::args::parse_rate;
use eid_engine::batch;
use eid_engine::config::Config;
use eid_engine::grabber::{self, GrabOptions};
use eid_engine::porting::{self, Format, ListItem};
use eid_engine::queue::{Queue, Status};
use eid_engine::queue_runner::EngineRunner;
use eid_engine::scheduler::{
    load_queues, save_queues, NoPower, QueueEvent, Scheduler, SchedulerConfig, SystemClock,
};
use eid_engine::urlsafe;
use eid_engine::CancellationToken;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub const HELP: &str = "\
  eid queue add <queues.json> -d DIR [--queue NAME] [--parallel N] [--retries N] [--retry-delay SECS]\n\
                [--schedule-in SECS] [--subdir NAME] [--categorize] <url-or-pattern>...\n\
  eid queue list <queues.json>\n\
  eid queue run <queues.json> [-c N] [--limit RATE] [-q]\n\
  eid queue import <queues.json> <list.txt|json|csv> [--queue NAME] [-d DIR] [--categorize]\n\
  eid queue export <queues.json> <out.txt|json|csv> [--queue NAME]\n\
  eid grab <page-url> [--ext zip,pdf] [--contains TEXT] [--glob PATTERN] [--same-host]\n\
           [--add-to <queues.json> [--queue NAME] -d DIR] [--categorize]\n\n\
  --categorize: sort by the file extension in the URL into Compressed/Documents/Music/Video/...\n\n\
  Patterns: http://h/img(001-050).jpg  (a-e)  several groups, at most 10000 URLs.";

#[derive(Debug, PartialEq, Default)]
pub struct QueueArgs {
    pub sub: String,
    pub file: PathBuf,
    pub second: Option<String>,
    pub dir: Option<PathBuf>,
    pub queue: Option<String>,
    pub parallel: Option<u32>,
    pub retries: Option<u32>,
    pub retry_delay: Option<u64>,
    pub schedule_in: Option<u64>,
    pub subdir: Option<String>,
    pub categorize: bool,
    pub inputs: Vec<String>,
    pub connections: Option<usize>,
    pub limit_bps: Option<u64>,
    pub quiet: bool,
    pub ramp_ms: Option<u64>,
    pub checkpoint_ms: Option<u64>,
    pub min_split: Option<u64>,
    pub stall_ms: Option<u64>,
    pub attempts: Option<u32>,
}

#[derive(Debug, PartialEq, Default)]
pub struct GrabArgs {
    pub url: String,
    pub exts: Vec<String>,
    pub contains: Option<String>,
    pub glob: Option<String>,
    pub same_host: bool,
    pub categorize: bool,
    pub add_to: Option<PathBuf>,
    pub queue: Option<String>,
    pub dir: Option<PathBuf>,
}

pub fn parse_queue(args: &[String]) -> Result<QueueArgs, String> {
    let mut it = args.iter();
    let sub = it.next().ok_or("queue needs a subcommand")?.clone();
    if !matches!(sub.as_str(), "add" | "list" | "run" | "import" | "export") {
        return Err(format!("unknown queue subcommand: {sub}"));
    }
    let mut a = QueueArgs {
        sub: sub.clone(),
        ..Default::default()
    };
    let mut pos: Vec<String> = vec![];
    let num = |it: &mut std::slice::Iter<String>, f: &str| -> Result<u64, String> {
        it.next()
            .ok_or(format!("{f} needs a value"))?
            .parse::<u64>()
            .map_err(|_| format!("{f}: not a number"))
    };
    while let Some(x) = it.next() {
        match x.as_str() {
            "-d" | "--dir" => a.dir = Some(PathBuf::from(it.next().ok_or("-d needs a value")?)),
            "--queue" => a.queue = Some(it.next().ok_or("--queue needs a value")?.clone()),
            "--parallel" => a.parallel = Some(num(&mut it, x)? as u32),
            "--retries" => a.retries = Some(num(&mut it, x)? as u32),
            "--retry-delay" => a.retry_delay = Some(num(&mut it, x)?),
            "--schedule-in" => a.schedule_in = Some(num(&mut it, x)?),
            "--subdir" => a.subdir = Some(it.next().ok_or("--subdir needs a value")?.clone()),
            "--categorize" => a.categorize = true,
            "-c" | "--connections" => {
                let n = num(&mut it, x)?;
                if !(1..=32).contains(&n) {
                    return Err("-c must be between 1 and 32".into());
                }
                a.connections = Some(n as usize);
            }
            "--limit" => a.limit_bps = Some(parse_rate(it.next().ok_or("--limit needs a value")?)?),
            "-q" | "--quiet" => a.quiet = true,
            "--ramp-ms" => a.ramp_ms = Some(num(&mut it, x)?),
            "--checkpoint-ms" => a.checkpoint_ms = Some(num(&mut it, x)?),
            "--min-split" => a.min_split = Some(num(&mut it, x)?),
            "--stall-ms" => a.stall_ms = Some(num(&mut it, x)?),
            "--attempts" => a.attempts = Some(num(&mut it, x)? as u32),
            "--" => pos.extend(it.by_ref().cloned()),
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option: {s}")),
            _ => pos.push(x.clone()),
        }
    }
    let mut pos = pos.into_iter();
    a.file = PathBuf::from(pos.next().ok_or("queue commands need the queue file")?);
    match sub.as_str() {
        "add" => {
            a.inputs = pos.collect();
            if a.inputs.is_empty() {
                return Err("queue add needs at least one URL".into());
            }
        }
        "import" | "export" => {
            a.second = Some(pos.next().ok_or(format!("queue {sub} needs a list file"))?);
            if pos.next().is_some() {
                return Err("unexpected extra argument".into());
            }
        }
        _ => {
            if pos.next().is_some() {
                return Err("unexpected extra argument".into());
            }
        }
    }
    Ok(a)
}

pub fn parse_grab(args: &[String]) -> Result<GrabArgs, String> {
    let mut g = GrabArgs::default();
    let mut it = args.iter();
    let mut url = None;
    while let Some(x) = it.next() {
        match x.as_str() {
            "--ext" => {
                g.exts = it
                    .next()
                    .ok_or("--ext needs a value")?
                    .split(',')
                    .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                    .filter(|e| !e.is_empty())
                    .collect()
            }
            "--contains" => g.contains = Some(it.next().ok_or("--contains needs a value")?.clone()),
            "--glob" => g.glob = Some(it.next().ok_or("--glob needs a value")?.clone()),
            "--same-host" => g.same_host = true,
            "--categorize" => g.categorize = true,
            "--add-to" => {
                g.add_to = Some(PathBuf::from(it.next().ok_or("--add-to needs a value")?))
            }
            "--queue" => g.queue = Some(it.next().ok_or("--queue needs a value")?.clone()),
            "-d" | "--dir" => g.dir = Some(PathBuf::from(it.next().ok_or("-d needs a value")?)),
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option: {s}")),
            _ => {
                if url.is_some() {
                    return Err("unexpected extra argument".into());
                }
                url = Some(x.clone());
            }
        }
    }
    g.url = url.ok_or("grab needs a page URL")?;
    Ok(g)
}

fn load_or_new(file: &std::path::Path) -> Result<eid_engine::queue::QueueSet, String> {
    load_queues(file).map_err(|e| e.to_string())
}

fn ensure_queue<'a>(
    qs: &'a mut eid_engine::queue::QueueSet,
    name: &str,
    dir: Option<&PathBuf>,
    a: &QueueArgs,
) -> Result<&'a mut Queue, String> {
    if qs.queue_mut(name).is_none() {
        let dir = dir.ok_or("a new queue needs -d DIR")?;
        let dir = std::path::absolute(dir).map_err(|e| e.to_string())?;
        qs.queues.push(Queue::new(name, &dir.to_string_lossy()));
    }
    let q = qs.queue_mut(name).expect("just ensured");
    if let Some(p) = a.parallel {
        q.max_parallel = p;
    }
    if let Some(r) = a.retries {
        q.retries.max = r;
    }
    if let Some(d) = a.retry_delay {
        q.retries.delay_secs = d;
    }
    Ok(q)
}

/// Category folder for a URL, from the file name in its path (the final server-chosen name is not
/// known before the transfer starts, so Content-Disposition names are NOT considered here).
pub fn category_folder(url: &str) -> Option<String> {
    let (u, _) = urlsafe::parse_and_strip(url).ok()?;
    let name = eid_engine::filename::choose_name(None, &u);
    let cats = eid_engine::category::Categorizer::default();
    Some(cats.classify(&name, None).folder.clone())
}

fn now_unix() -> i64 {
    eid_platform::time::local_now().unix
}

pub fn run_add(a: &QueueArgs) -> Result<(), String> {
    let mut qs = load_or_new(&a.file)?;
    let name = a.queue.clone().unwrap_or_else(|| "main".into());
    ensure_queue(&mut qs, &name, a.dir.as_ref(), a)?;
    let not_before = a.schedule_in.map(|s| now_unix().saturating_add(s as i64));
    let mut added = 0usize;
    for input in &a.inputs {
        for url in batch::expand(input).map_err(|e| format!("{input}: {e}"))? {
            let sub = a
                .subdir
                .clone()
                .or_else(|| a.categorize.then(|| category_folder(&url)).flatten());
            qs.add_item(&name, &url, sub.as_deref(), None, not_before)
                .map_err(|e| format!("{}: {e}", display(&url)))?;
            added += 1;
        }
    }
    save_queues(&a.file, &qs).map_err(|e| e.to_string())?;
    println!("added {added} item(s) to queue '{name}'");
    Ok(())
}

fn display(url: &str) -> String {
    match urlsafe::parse_and_strip(url) {
        Ok((u, _)) => urlsafe::redact(&u),
        Err(_) => "<invalid url>".into(),
    }
}

pub fn run_list(a: &QueueArgs) -> Result<(), String> {
    let qs = load_or_new(&a.file)?;
    for q in &qs.queues {
        println!(
            "queue '{}' dir={} parallel={} retries={}x{}s{}",
            q.name,
            q.dir,
            q.max_parallel,
            q.retries.max,
            q.retries.delay_secs,
            if q.paused { " PAUSED" } else { "" }
        );
        for i in &q.items {
            println!(
                "  #{:<5} {:<8} {}{}",
                i.id,
                format!("{:?}", i.status).to_lowercase(),
                display(&i.url),
                i.last_error
                    .as_ref()
                    .map_or(String::new(), |e| format!("  ({e})"))
            );
        }
    }
    Ok(())
}

pub fn run_import(a: &QueueArgs) -> Result<(), String> {
    let list = a.second.as_ref().expect("parsed");
    let fmt = Format::from_extension(list).ok_or("list file must end in .txt, .json or .csv")?;
    let bytes = read_capped_file(list)?;
    let imp = porting::import(&bytes, fmt).map_err(|e| e.to_string())?;
    let mut qs = load_or_new(&a.file)?;
    let name = a.queue.clone().unwrap_or_else(|| "main".into());
    ensure_queue(&mut qs, &name, a.dir.as_ref(), a)?;
    for it in &imp.items {
        qs.add_item(
            &name,
            &it.url,
            it.subdir.as_deref(),
            it.name.as_deref(),
            None,
        )?;
    }
    save_queues(&a.file, &qs).map_err(|e| e.to_string())?;
    println!(
        "imported {} item(s); skipped {} invalid, {} duplicate; {} with credentials removed",
        imp.items.len(),
        imp.rejected_total,
        imp.duplicates,
        imp.credentials_dropped
    );
    for (line, why) in imp.rejected.iter().take(20) {
        eprintln!("  line {line}: {why}");
    }
    Ok(())
}

fn read_capped_file(path: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut v = Vec::new();
    f.take(porting::MAX_IMPORT_BYTES as u64 + 1)
        .read_to_end(&mut v)
        .map_err(|e| e.to_string())?;
    Ok(v)
}

pub fn run_export(a: &QueueArgs) -> Result<(), String> {
    let out = a.second.as_ref().expect("parsed");
    let fmt = Format::from_extension(out).ok_or("output file must end in .txt, .json or .csv")?;
    let mut qs = load_or_new(&a.file)?;
    let name = a.queue.clone().unwrap_or_else(|| "main".into());
    let q = qs
        .queue_mut(&name)
        .ok_or_else(|| format!("no queue named '{name}'"))?;
    let items: Vec<ListItem> = q
        .items
        .iter()
        .map(|i| ListItem {
            url: i.url.clone(),
            name: i.file_name.clone(),
            subdir: i.subdir.clone(),
        })
        .collect();
    let text = porting::export(&items, fmt);
    let mut f = eid_platform::fs::create_new_file(std::path::Path::new(out))
        .map_err(|e| format!("{out}: {e} (refusing to overwrite an existing file)"))?;
    std::io::Write::write_all(&mut f, text.as_bytes()).map_err(|e| e.to_string())?;
    println!("exported {} item(s) to {out}", items.len());
    Ok(())
}

fn config_for(a: &QueueArgs) -> Config {
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

/// Returns the process exit code: 0 all done, 1 something failed, 130 interrupted.
pub async fn run_queue(a: &QueueArgs, cancel: CancellationToken) -> Result<i32, String> {
    let quiet = a.quiet;
    let sink: eid_engine::scheduler::QueueSink = Arc::new(move |e| {
        if quiet {
            return;
        }
        match e {
            QueueEvent::ItemStarted { queue, item } => eprintln!("[{queue}] #{item} started"),
            QueueEvent::ItemDone { queue, item } => eprintln!("[{queue}] #{item} done"),
            QueueEvent::ItemRetry {
                queue,
                item,
                attempt,
                delay_secs,
            } => {
                eprintln!("[{queue}] #{item} failed, retry #{attempt} in {delay_secs}s")
            }
            QueueEvent::ItemFailed {
                queue,
                item,
                message,
            } => {
                eprintln!("[{queue}] #{item} FAILED: {message}")
            }
            QueueEvent::ItemStopped {
                queue,
                item,
                reason,
            } => {
                eprintln!("[{queue}] #{item} stopped ({reason:?}); run again to resume")
            }
            QueueEvent::QueueFinished { queue } => eprintln!("[{queue}] finished"),
            _ => {}
        }
    });
    let sched = Scheduler::open(
        SchedulerConfig::new(&a.file),
        Arc::new(EngineRunner::new(config_for(a))),
        Arc::new(SystemClock),
        Arc::new(NoPower),
        sink,
    )
    .map_err(|e| e.to_string())?;
    sched
        .run(cancel.clone(), true)
        .await
        .map_err(|e| e.to_string())?;
    let qs = load_queues(&a.file).map_err(|e| e.to_string())?;
    let failed = qs
        .queues
        .iter()
        .flat_map(|q| &q.items)
        .filter(|i| i.status == Status::Failed)
        .count();
    let open = qs
        .queues
        .iter()
        .flat_map(|q| &q.items)
        .any(|i| !i.status.is_terminal());
    Ok(if cancel.is_cancelled() && open {
        130
    } else if failed > 0 {
        1
    } else {
        0
    })
}

pub async fn run_grab(g: &GrabArgs, cancel: &CancellationToken) -> Result<(), String> {
    let opts = GrabOptions {
        exts: g.exts.clone(),
        contains: g.contains.clone(),
        glob: g.glob.clone(),
        same_host_only: g.same_host,
        ..GrabOptions::default()
    };
    let items = grabber::grab_url(&g.url, &Config::default(), &opts, cancel)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(file) = &g.add_to {
        let a = QueueArgs {
            queue: g.queue.clone(),
            dir: g.dir.clone(),
            ..Default::default()
        };
        let mut qs = load_or_new(file)?;
        let name = g.queue.clone().unwrap_or_else(|| "main".into());
        ensure_queue(&mut qs, &name, g.dir.as_ref(), &a)?;
        for it in &items {
            let sub = g.categorize.then(|| category_folder(&it.url)).flatten();
            qs.add_item(&name, &it.url, sub.as_deref(), None, None)?;
        }
        save_queues(file, &qs).map_err(|e| e.to_string())?;
        println!("added {} link(s) to queue '{name}'", items.len());
    } else {
        for it in &items {
            println!("{}", it.url);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn queue_args_parse_and_reject() {
        let a = parse_queue(&v(&[
            "add",
            "q.json",
            "-d",
            "out",
            "--parallel",
            "3",
            "http://h/a(1-3)",
        ]))
        .unwrap();
        assert_eq!((a.parallel, a.inputs.len()), (Some(3), 1));
        assert!(parse_queue(&v(&["add", "q.json"])).is_err());
        assert!(parse_queue(&v(&["nope", "q.json"])).is_err());
        assert!(parse_queue(&v(&["run", "q.json", "-c", "99"])).is_err());
        assert!(parse_queue(&v(&["import", "q.json"])).is_err());
        assert!(parse_queue(&v(&["list"])).is_err());
        assert!(parse_queue(&v(&["list", "q.json", "extra"])).is_err());
        assert!(parse_queue(&v(&["list", "q.json", "--weird"])).is_err());
        let g = parse_grab(&v(&["http://h/p", "--ext", "ZIP,.pdf", "--same-host"])).unwrap();
        assert_eq!(g.exts, ["zip", "pdf"]);
        assert!(g.same_host);
        assert!(parse_grab(&v(&[])).is_err());
        assert!(parse_grab(&v(&["a", "b"])).is_err());
        assert_eq!(
            category_folder("http://h/x/Setup.EXE?x=1").as_deref(),
            Some("Programs")
        );
        assert_eq!(
            category_folder("http://h/a.tar.gz").as_deref(),
            Some("Compressed")
        );
        assert_eq!(
            category_folder("http://h/download").as_deref(),
            Some("Other")
        );
        assert_eq!(category_folder("javascript:1"), None);
    }
}
