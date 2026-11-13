mod common;
use common::*;
use eid_testserver::{Behavior, Content};

fn big() -> Content {
    Content::Generated {
        seed: 7,
        len: 20 * 1024 * 1024,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sha256_matches_with_1_8_32_connections() {
    let want = big().sha256_hex();
    for conns in [1usize, 8, 32] {
        let dir = tempfile::tempdir().unwrap();
        let (srv, url) = serve("/f.bin", big(), Behavior::default()).await;
        let (o, log) = get(&url, dir.path(), fast_cfg(conns)).await.unwrap();
        assert_eq!(
            o.sha256.as_deref(),
            Some(want.as_str()),
            "{conns} connections"
        );
        assert_eq!(sha_of_file(&o.path), want);
        assert_eq!(o.size, 20 * 1024 * 1024);
        let st = srv.stats("/f.bin");
        if conns > 1 {
            assert!(
                st.peak_concurrent > 1,
                "{conns}: expected parallelism, peak {}",
                st.peak_concurrent
            );
        }
        // The server's in-flight counter is NOT a valid cap measurement (it learns of a close
        // late; CI run 43361242168 saw 9 while the client never had more than 8). The cap is
        // asserted from the ENGINE's own events (ADR-0011); the server counter only has to
        // prove real parallelism (> 1, above).
        eprintln!(
            "PEAK conns={conns} client_responses={} server={} workers={}",
            log.peak_live_responses(),
            st.peak_concurrent,
            o.peak_workers
        );
        assert!(
            log.peak_live_responses() <= conns,
            "{conns}: engine had {} live responses",
            log.peak_live_responses()
        );
        assert!(
            o.peak_workers <= conns,
            "{conns}: {} workers",
            o.peak_workers
        );
        assert_eq!(
            only_files(dir.path()),
            vec!["f.bin".to_string()],
            "no leftovers"
        );
    }
}

use eid_engine::Event;
use eid_testserver::{ChangeAfter, Cut, LimitStatus};
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn midstream_disconnects_still_complete() {
    let want = Content::Generated {
        seed: 3,
        len: 12 * 1024 * 1024,
    }
    .sha256_hex();
    let cuts = [
        Cut::EveryBytes(700_000),
        Cut::EveryBytes(65_537),
        Cut::Random {
            seed: 1,
            min: 10_000,
            max: 900_000,
        },
        Cut::Random {
            seed: 99,
            min: 1,
            max: 50_000,
        },
        Cut::AfterBytes {
            conn_index: 1,
            n: 1_000_000,
        },
    ];
    for (i, cut) in cuts.into_iter().enumerate() {
        let (srv, url) = serve(
            "/d.bin",
            Content::Generated {
                seed: 3,
                len: 12 * 1024 * 1024,
            },
            Behavior {
                cut,
                ..Behavior::default()
            },
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fast_cfg(8);
        cfg.max_attempts = 200; // progress resets the counter; this only bounds pathological loops
        let (o, log) = get(&url, dir.path(), cfg)
            .await
            .unwrap_or_else(|e| panic!("cut #{i}: {e}"));
        assert_eq!(sha_of_file(&o.path), want, "cut #{i}");
        assert!(
            log.count(|e| matches!(e, Event::Retry { .. })) >= 1,
            "cut #{i}: disconnects must have been exercised"
        );
        assert!(srv.stats("/d.bin").requests.len() > 2);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_segment_is_split() {
    // connection #1 (the probe's, owning segment 0) crawls at 150 KB/s; the file is 6 MB, so
    // finishing on that connection alone would need ~40 s. Work stealing must beat that by far.
    let b = Behavior {
        slow_connections: Some((1, 150_000)),
        ..Behavior::default()
    };
    let (srv, url) = serve(
        "/slow.bin",
        Content::Generated {
            seed: 5,
            len: 6 << 20,
        },
        b,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let t = Instant::now();
    let (o, log) = get(&url, dir.path(), fast_cfg(8)).await.unwrap();
    let took = t.elapsed();
    assert_eq!(
        o.sha256.unwrap(),
        Content::Generated {
            seed: 5,
            len: 6 << 20
        }
        .sha256_hex()
    );
    assert!(log.count(|e| matches!(e, Event::Split { .. })) >= 1);
    assert!(
        took < Duration::from_secs(15),
        "took {took:?}; peak {}",
        srv.stats("/slow.bin").peak_concurrent
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_after_429_respected_and_403_lowers_connections() {
    // 429 with Retry-After: 1 when more than 2 connections are open
    let b = Behavior {
        max_concurrent: Some((2, LimitStatus::TooManyRequests)),
        retry_after_secs: Some(1),
        throttle_bps: Some(3_000_000),
        ..Behavior::default()
    };
    let (srv, url) = serve(
        "/t.bin",
        Content::Generated {
            seed: 9,
            len: 8 << 20,
        },
        b,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let (o, log) = get(&url, dir.path(), fast_cfg(8)).await.unwrap();
    assert_eq!(
        o.sha256.unwrap(),
        Content::Generated {
            seed: 9,
            len: 8 << 20
        }
        .sha256_hex()
    );
    let reqs = srv.stats("/t.bin").requests;
    let first429 = reqs
        .iter()
        .find(|r| r.status == 429)
        .expect("server must have throttled")
        .started_ms;
    assert!(log.count(|e| matches!(e, Event::Throttled { .. })) >= 1);
    assert!(log.count(|e| matches!(e, Event::ConnectionsLowered { .. })) >= 1);
    // honoured: nobody sends a new request inside the Retry-After window (small race tolerance)
    let violators: Vec<_> = reqs
        .iter()
        .filter(|r| r.started_ms > first429 + 150 && r.started_ms < first429 + 900)
        .collect();
    assert!(
        violators.is_empty(),
        "requests inside Retry-After window: {violators:?}"
    );
    // 403 above 3 connections
    let b = Behavior {
        max_concurrent: Some((3, LimitStatus::Forbidden)),
        throttle_bps: Some(3_000_000),
        ..Behavior::default()
    };
    let (srv, url) = serve(
        "/f.bin",
        Content::Generated {
            seed: 9,
            len: 8 << 20,
        },
        b,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let (o, log) = get(&url, dir.path(), fast_cfg(8)).await.unwrap();
    assert_eq!(
        o.sha256.unwrap(),
        Content::Generated {
            seed: 9,
            len: 8 << 20
        }
        .sha256_hex()
    );
    assert!(
        log.count(|e| matches!(e, Event::ConnectionsLowered { .. })) >= 1,
        "403 with several workers must lower the cap"
    );
    assert!(srv.stats("/f.bin").peak_concurrent <= 3 + 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limiter_real_time_smoke_bounded() {
    // 2 MiB at 1 MiB/s ~ 2 s (smoke only; the precise test is the virtual-time one)
    let len = 2u64 << 20;
    let (_srv, url) = serve(
        "/lim.bin",
        Content::Generated { seed: 2, len },
        Behavior::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(4);
    cfg.job_limit_bps = Some(1 << 20);
    let t = Instant::now();
    let (o, _) = get(&url, dir.path(), cfg).await.unwrap();
    let secs = t.elapsed().as_secs_f64();
    assert_eq!(o.size, len);
    // Lower bound strict: proves the limiter really limits. Upper bound loose (ADR-0011): slow
    // 2-core CI runners add hashing/fsync/scheduling time. Rate accuracy is proven by the 5%
    // virtual-time test `limiter_rate_within_5_percent_virtual_time`.
    assert!(secs >= 1.5, "limiter did not slow the transfer: {secs:.2}s");
    assert!(secs <= 4.0, "limiter stalled the transfer: {secs:.2}s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn max_in_flight_bytes_bounded() {
    let (_srv, url) = serve(
        "/m.bin",
        Content::Generated {
            seed: 4,
            len: 64 << 20,
        },
        Behavior::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = fast_cfg(8);
    let chunk = cfg.write_chunk as u64;
    let (o, _) = get(&url, dir.path(), cfg).await.unwrap();
    // each worker holds at most one write_chunk plus one network read (hyper reads <= ~400 KiB);
    // the point is that the bound does not depend on the 64 MiB file size.
    let bound = 8 * (chunk + 512 * 1024);
    assert!(bound < 64 << 20 >> 3);
    assert!(
        o.peak_buffered <= bound,
        "peak buffered {} > bound {bound}",
        o.peak_buffered
    );
    assert!(o.peak_buffered > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handoff_engine_randomized_with_jitter() {
    // Whole-engine version of the table-level arbiter: random cuts + jitter at the
    // reserve/write/complete points, many connections, tiny min_split => constant splitting.
    let len = 3u64 << 20;
    for seed in 1..=30u64 {
        eid_engine::fault::set_jitter(seed);
        let content = Content::Generated { seed, len };
        let (_srv, url) = serve(
            "/h.bin",
            content.clone(),
            Behavior {
                cut: Cut::Random {
                    seed,
                    min: 5_000,
                    max: 400_000,
                },
                ..Behavior::default()
            },
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fast_cfg(16);
        cfg.min_split = 8 * 1024;
        cfg.write_chunk = 4096;
        cfg.max_attempts = 500;
        let (o, _) = get(&url, dir.path(), cfg)
            .await
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(sha_of_file(&o.path), content.sha256_hex(), "seed {seed}");
    }
    eid_engine::fault::set_jitter(0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_changes_during_download_is_detected() {
    // After ~2 MB were sent, the server switches to different content (new ETag, same length).
    let a = Content::Generated {
        seed: 100,
        len: 8 << 20,
    };
    let b = Content::Generated {
        seed: 200,
        len: 8 << 20,
    };
    let beh = Behavior {
        throttle_bps: Some(4_000_000),
        // frequent disconnects force new requests after the switch (existing connections keep
        // streaming their old snapshot, which is coherent by itself)
        cut: Cut::EveryBytes(600_000),
        change_after: Some(ChangeAfter {
            requests: None,
            bytes_sent: Some(3 << 19),
            new_content: b.clone(),
        }),
        ..Behavior::default()
    };
    let (_srv, url) = serve("/c.bin", a, beh).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(4);
    cfg.max_attempts = 100;
    let (o, log) = get(&url, dir.path(), cfg).await.unwrap();
    assert!(o.restarts >= 1, "change must be detected");
    assert!(log.count(|e| matches!(e, Event::Restarted { .. })) >= 1);
    // the result must be one coherent version, never a splice of both
    assert_eq!(sha_of_file(&o.path), b.sha256_hex());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn length_change_during_download_is_detected() {
    let a = Content::Generated {
        seed: 100,
        len: 8 << 20,
    };
    let b = Content::Generated {
        seed: 300,
        len: 6 << 20,
    };
    let _ = &a;
    let beh = Behavior {
        throttle_bps: Some(4_000_000),
        etag: eid_testserver::EtagMode::None,
        last_modified: Some("Wed, 21 Oct 2026 07:28:00 GMT".into()),
        cut: Cut::EveryBytes(600_000),
        change_after: Some(ChangeAfter {
            requests: None,
            bytes_sent: Some(3 << 19),
            new_content: b.clone(),
        }),
        ..Behavior::default()
    };
    let (_srv, url) = serve("/c2.bin", a, beh).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(4);
    cfg.max_attempts = 100;
    let (o, _) = get(&url, dir.path(), cfg).await.unwrap();
    assert_eq!(sha_of_file(&o.path), b.sha256_hex());
    assert!(o.restarts >= 1);
}

/// Every `ResponseOpened` has a matching `ResponseClosed` (emitted after the body stream is
/// dropped) and the live count never exceeds the cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn response_events_are_balanced_and_bounded_by_the_cap() {
    let (_srv, url) = serve("/f.bin", big(), Behavior::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, log) = get(&url, dir.path(), fast_cfg(8)).await.unwrap();
    let opened = log.count(|e| matches!(e, Event::ResponseOpened { .. }));
    let closed = log.count(|e| matches!(e, Event::ResponseClosed { .. }));
    assert!(opened >= 1, "no response events at all");
    assert_eq!(opened, closed, "unbalanced response events");
    assert!(
        log.peak_live_responses() <= 8,
        "live peak {}",
        log.peak_live_responses()
    );
    assert!(o.peak_workers <= 8);
}
