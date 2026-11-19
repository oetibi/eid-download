//! Prefix hashing during the transfer (branch dev/perf-playlists-ui, item 4/1 of the owner's
//! 2026-11-19 request): the final SHA-256 must stay exact in every path (single stream, ranged,
//! restart after a server-side change, cancel + resume), while finalization re-reads only the
//! tail that was not hashed yet (`Outcome::hashed_before_finish`).
mod common;
use common::*;
use eid_engine::{CancellationToken, Event, JobSpec};
use eid_testserver::{Behavior, ChangeAfter, Content, Cut};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_stream_is_hashed_entirely_during_the_transfer() {
    let content = Content::Generated {
        seed: 181,
        len: 3 << 20,
    };
    let (_s, url) = serve("/n.bin", content.clone(), Behavior::no_range()).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, _) = get(&url, dir.path(), fast_cfg(4)).await.unwrap();
    assert_eq!(o.sha256.as_deref(), Some(content.sha256_hex().as_str()));
    assert_eq!(sha_of_file(&o.path), content.sha256_hex());
    assert_eq!(
        o.hashed_before_finish,
        3 << 20,
        "nothing re-read at the end"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ranged_digest_is_exact_and_partly_computed_during_the_transfer() {
    let content = Content::Generated {
        seed: 182,
        len: 32 << 20,
    };
    let (_s, url) = serve("/r.bin", content.clone(), Behavior::slow(6_000_000)).await;
    let dir = tempfile::tempdir().unwrap();
    // two connections: segment 0 (the prefix) is about half the file and runs for seconds
    let mut cfg = fast_cfg(2);
    cfg.min_split = 1 << 20;
    let (o, _) = get(&url, dir.path(), cfg).await.unwrap();
    println!(
        "PREFIX ranged hashed_before_finish={}",
        o.hashed_before_finish
    );
    assert_eq!(o.sha256.as_deref(), Some(content.sha256_hex().as_str()));
    assert_eq!(sha_of_file(&o.path), content.sha256_hex());
    assert!(o.peak_workers > 1, "the case must really be segmented");
    // the task hashes in slices of >= 4 MiB, so any non-zero value is a real slice
    assert!(
        o.hashed_before_finish > 0 && o.hashed_before_finish <= 32 << 20,
        "the first segment is hashed while the others still download: {}",
        o.hashed_before_finish
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_after_a_server_change_never_reuses_the_old_prefix_digest() {
    let a = Content::Generated {
        seed: 183,
        len: 16 << 20,
    };
    let b = Content::Generated {
        seed: 184,
        len: 16 << 20,
    };
    let beh = Behavior {
        throttle_bps: Some(4_000_000),
        cut: Cut::EveryBytes(700_000),
        change_after: Some(ChangeAfter {
            requests: None,
            bytes_sent: Some(10 << 20),
            new_content: b.clone(),
        }),
        ..Behavior::default()
    };
    let (_s, url) = serve("/c.bin", a, beh).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(2);
    cfg.min_split = 1 << 20;
    cfg.max_attempts = 100;
    cfg.expected_sha256 = Some(b.sha256_hex());
    let (o, log) = get(&url, dir.path(), cfg).await.unwrap();
    assert!(o.restarts >= 1, "the change must be detected");
    // evidence that the OLD generation was far enough along for its first segment to be hashed
    let before_restart = {
        let ev = log.events.lock().unwrap();
        let cut = ev
            .iter()
            .position(|e| matches!(e, Event::Restarted { .. }))
            .expect("restarted");
        ev[..cut]
            .iter()
            .filter_map(|e| match e {
                Event::Progress { done, .. } => Some(*done),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    };
    println!("PREFIX restart old_generation_progress={before_restart}");
    assert!(before_restart >= 8 << 20, "{before_restart}");
    assert_eq!(o.sha256.as_deref(), Some(b.sha256_hex().as_str()));
    assert_eq!(sha_of_file(&o.path), b.sha256_hex());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_cancel_hashes_the_kept_prefix_and_the_digest_is_exact() {
    let content = Content::Generated {
        seed: 185,
        len: 20 << 20,
    };
    let (_s, url) = serve("/p.bin", content.clone(), Behavior::slow(4_000_000)).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(2);
    cfg.min_split = 1 << 20;
    // first run: cancel once a checkpoint has made at least 6 MiB durable
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let durable = Arc::new(Mutex::new(0u64));
    let d2 = durable.clone();
    let sink: eid_engine::EventSink = Arc::new(move |e| {
        if let Event::Checkpoint { done } = e {
            *d2.lock().unwrap() = done;
            if done >= 6 << 20 {
                c2.cancel();
            }
        }
    });
    let r = eid_engine::download(
        JobSpec {
            url: url.clone(),
            dir: dir.path().to_path_buf(),
            file_name: None,
        },
        cfg.clone(),
        sink,
        cancel,
    )
    .await;
    assert!(matches!(r, Err(eid_engine::Error::Cancelled)), "{r:?}");
    assert!(*durable.lock().unwrap() >= 6 << 20);
    let state = dir.path().join("p.bin.eid-state");
    assert!(state.exists());
    // second run: resume; the kept prefix is hashed from disk while the rest downloads
    let log = Log::default();
    let o = tokio::time::timeout(
        Duration::from_secs(60),
        eid_engine::resume(&state, cfg, log.sink(), CancellationToken::new()),
    )
    .await
    .expect("resume finished in time")
    .unwrap();
    println!(
        "PREFIX resume hashed_before_finish={}",
        o.hashed_before_finish
    );
    assert_eq!(o.sha256.as_deref(), Some(content.sha256_hex().as_str()));
    assert_eq!(sha_of_file(&o.path), content.sha256_hex());
    assert!(o.hashed_before_finish > 0);
}
