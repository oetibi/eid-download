//! Phase breakdown of one download (benchmark tool, not a test gate; `scripts/bench/`).
//!
//! `cargo run --release -p eid-engine --example bench_phases -- URL DIR CONNS [RAMP_MS]`
//! Prints one `PHASES` line: time to first response, to the end of the last response body
//! (transfer), to the returned outcome (finalize = hash + fsync + rename + last checkpoint), the
//! process CPU time at those points (Linux `/proc/self/stat`; `-1` elsewhere), the number of
//! responses (connections, since the engine never reuses one), splits, retries and progress
//! events. Run it against a separate `eid-benchserver` process so server CPU is not counted.
use eid_engine::{download, CancellationToken, Config, Event, JobSpec};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Default)]
struct Marks {
    first_open: Option<f64>,
    last_close: Option<f64>,
    cpu_at_last_close: f64,
    responses: u64,
    splits: u64,
    retries: u64,
    progress: u64,
    checkpoints: u64,
    peak_workers: usize,
}

/// utime+stime of this process in seconds, from /proc/self/stat (Linux only).
fn cpu_s() -> f64 {
    let Ok(s) = std::fs::read_to_string("/proc/self/stat") else {
        return -1.0;
    };
    // fields after the ")" that closes comm: state is field 3; utime=14, stime=15 (1-based)
    let Some(rest) = s.rsplit_once(')').map(|(_, r)| r) else {
        return -1.0;
    };
    let f: Vec<&str> = rest.split_whitespace().collect();
    let tick = 100.0; // USER_HZ on Linux
    let ut: f64 = f.get(11).and_then(|v| v.parse().ok()).unwrap_or(-1.0);
    let st: f64 = f.get(12).and_then(|v| v.parse().ok()).unwrap_or(-1.0);
    (ut + st) / tick
}

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 3 {
        eprintln!("usage: bench_phases URL DIR CONNS [RAMP_MS]");
        std::process::exit(2);
    }
    let mut cfg = Config {
        connections: a[2].parse().expect("CONNS"),
        ..Config::default()
    };
    if let Some(r) = a.get(3) {
        cfg.ramp_interval = std::time::Duration::from_millis(r.parse().expect("RAMP_MS"));
    }
    let t0 = Instant::now();
    let marks = Arc::new(Mutex::new(Marks::default()));
    let m2 = marks.clone();
    let sink = Arc::new(move |e: Event| {
        let now = t0.elapsed().as_secs_f64();
        let mut m = m2.lock().unwrap();
        match e {
            Event::ResponseOpened { .. } => {
                m.responses += 1;
                m.first_open.get_or_insert(now);
            }
            Event::ResponseClosed { .. } => {
                m.last_close = Some(now);
                m.cpu_at_last_close = cpu_s();
            }
            Event::Split { .. } => m.splits += 1,
            Event::Retry { .. } => m.retries += 1,
            Event::Progress { .. } => m.progress += 1,
            Event::Checkpoint { .. } => m.checkpoints += 1,
            Event::Worker { active } => m.peak_workers = m.peak_workers.max(active),
            _ => {}
        }
    });
    let spec = JobSpec {
        url: a[0].clone(),
        dir: a[1].clone().into(),
        file_name: None,
    };
    let out = download(spec, cfg, sink, CancellationToken::new())
        .await
        .expect("download failed");
    let total = t0.elapsed().as_secs_f64();
    let cpu_total = cpu_s();
    let m = marks.lock().unwrap();
    let transfer = m.last_close.unwrap_or(total);
    println!(
        "PHASES size={} conns={} first_response_s={:.3} transfer_end_s={:.3} total_s={:.3} \
         finalize_s={:.3} cpu_transfer_s={:.2} cpu_total_s={:.2} responses={} splits={} \
         retries={} progress_events={} checkpoints={} peak_workers={} peak_buffered={} hashed_before_finish={}",
        out.size,
        a[2],
        m.first_open.unwrap_or(-1.0),
        transfer,
        total,
        total - transfer,
        m.cpu_at_last_close,
        cpu_total,
        m.responses,
        m.splits,
        m.retries,
        m.progress,
        m.checkpoints,
        m.peak_workers,
        out.peak_buffered,
        out.hashed_before_finish,
    );
    let _ = std::fs::remove_file(&out.path);
}
