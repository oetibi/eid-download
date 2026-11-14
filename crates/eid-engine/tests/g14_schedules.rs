//! P1 review M7: whole-engine randomized fault schedules (PLAN G3 `handoff_randomized_200_seeds`
//! at engine level). ONE test function because the fault hooks (jitter, fail points) are
//! process-global: no other test may share this binary.
//!
//! Every seed fixes a SCHEDULE (printed on any failure): content, connection count, split size,
//! write chunk, random mid-body cuts, an optional long stall, an optional injected disk error
//! during a write (after its reservation), and the jitter decision stream. The final file is
//! compared by SHA-256 AND length, nothing else may be left in the directory, and a seed that hit
//! the injected disk error must resume to the same bytes. Replay one seed:
//! `EID_SEED=<n> EID_REPEAT=<k> cargo test -p eid-engine --test g14_schedules -- --nocapture`.
//! The seed fixes the faults and the jitter decisions; the thread interleaving stays with the OS
//! scheduler (see fault::set_jitter), which is why a failing seed is repeated, not assumed stable.
mod common;
use common::*;
use eid_engine::{fault, Error};
use std::sync::atomic::{AtomicU32, Ordering};

/// How many schedules really went through the disk-error + resume path / saw cuts (evidence
/// that the injected faults fire, asserted at the end of the full run).
static RESUMED: AtomicU32 = AtomicU32::new(0);
static RETRIES: AtomicU32 = AtomicU32::new(0);
use eid_testserver::{Behavior, Content, Cut, Stall};
use std::time::Duration;

#[derive(Debug, Clone)]
struct Schedule {
    seed: u64,
    len: u64,
    connections: usize,
    min_split: u64,
    write_chunk: usize,
    cut: (u64, u64),
    stall: Option<(u64, u64)>,
    fail_write_after: Option<u32>,
}

fn schedule(seed: u64) -> Schedule {
    let mut r = fastrand::Rng::with_seed(seed);
    let len = (512 << 10) + r.u64(0..(3 << 20));
    let min = 2_000 + r.u64(0..20_000);
    Schedule {
        seed,
        len,
        connections: [2, 4, 8, 16, 32][r.usize(0..5)],
        min_split: 4096 + r.u64(0..(32 << 10)),
        write_chunk: 1024 << r.u32(0..4),
        cut: (min, min + 50_000 + r.u64(0..400_000)),
        stall: r.bool().then(|| (r.u64(0..len / 2), 20 + r.u64(0..60))),
        fail_write_after: (r.u8(0..4) == 0).then(|| r.u32(0..40)),
    }
}

async fn one(s: &Schedule) {
    let ctx = format!("schedule {s:?}");
    fault::set_jitter(s.seed);
    fault::set_fail_point(
        "write_data",
        s.fail_write_after.unwrap_or(0),
        u32::from(s.fail_write_after.is_some()),
    );
    let content = Content::Generated {
        seed: s.seed,
        len: s.len,
    };
    let (_srv, url) = serve(
        "/h.bin",
        content.clone(),
        Behavior {
            cut: Cut::Random {
                seed: s.seed,
                min: s.cut.0,
                max: s.cut.1,
            },
            stall: s.stall.map(|(after_bytes, ms)| Stall {
                after_bytes,
                duration: Duration::from_millis(ms),
                once: true,
            }),
            ..Behavior::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(s.connections);
    cfg.min_split = s.min_split;
    cfg.write_chunk = s.write_chunk;
    cfg.max_attempts = 500;
    let first = get(&url, dir.path(), cfg.clone()).await.map(|(o, log)| {
        let n = log.count(|e| matches!(e, eid_engine::Event::Retry { .. }));
        RETRIES.fetch_add(n as u32, Ordering::Relaxed);
        o
    });
    let outcome = match first {
        Ok(o) => o,
        Err(Error::Io {
            context: "write data",
            ..
        }) if s.fail_write_after.is_some() => {
            RESUMED.fetch_add(1, Ordering::Relaxed);
            // the disk error was fatal for this run; the job must be resumable and exact
            let state = dir.path().join("h.bin.eid-state");
            assert!(
                state.exists(),
                "no state to resume from after a disk error: {ctx}"
            );
            eid_engine::resume(
                &state,
                cfg,
                Log::default().sink(),
                eid_engine::CancellationToken::new(),
            )
            .await
            .unwrap_or_else(|e| panic!("resume after a disk error failed: {e}: {ctx}"))
        }
        Err(e) => panic!("download failed: {e}: {ctx}"),
    };
    assert_eq!(
        sha_of_file(&outcome.path),
        content.sha256_hex(),
        "SHA mismatch: {ctx}"
    );
    assert_eq!(
        std::fs::metadata(&outcome.path).unwrap().len(),
        s.len,
        "length: {ctx}"
    );
    assert_eq!(
        only_files(dir.path()),
        vec!["h.bin".to_string()],
        "leftovers: {ctx}"
    );
    fault::set_fail_point("write_data", 0, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handoff_engine_randomized_with_jitter() {
    let seeds: Vec<u64> = match std::env::var("EID_SEED")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        Some(one_seed) => {
            let n: u64 = std::env::var("EID_REPEAT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            (0..n).map(|_| one_seed).collect()
        }
        None => (1..=200).collect(),
    };
    let mut injected = 0;
    for seed in &seeds {
        let s = schedule(*seed);
        injected += u32::from(s.fail_write_after.is_some());
        one(&s).await;
    }
    fault::set_jitter(0);
    if seeds.len() == 200 {
        // the schedule space must actually include the dangerous cases
        let all: Vec<Schedule> = seeds.iter().map(|s| schedule(*s)).collect();
        assert!(
            injected >= 30,
            "only {injected} schedules inject a write error"
        );
        assert!(all.iter().any(|s| s.connections == 32));
        let (resumed, retries) = (
            RESUMED.load(Ordering::Relaxed),
            RETRIES.load(Ordering::Relaxed),
        );
        println!("SCHEDULES seeds=200 injected={injected} resumed_after_disk_error={resumed} retries={retries}");
        assert!(
            resumed >= 10,
            "the disk-error path ran only {resumed} times"
        );
        assert!(retries >= 200, "random cuts caused only {retries} retries");
        assert!(all.iter().filter(|s| s.stall.is_some()).count() >= 50);
    }
}
