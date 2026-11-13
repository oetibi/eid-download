mod common;
use common::*;
use eid_testserver::{Behavior, Content};

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

/// Real-time smoke test of the limiter (the precise one is the virtual-time test). The job is
/// timed in PHASES from engine events with their arrival `Instant`s: start, `Started`, first
/// `ResponseOpened`, last `ResponseClosed`, `Finished`, return. The 1.5-4 s smoke bound applies
/// to the TRANSFER phase (first `ResponseOpened` -> last `ResponseClosed`): total time and the
/// finalize overhead (hash, fsync, rename, checkpoints, antivirus on Windows) are REPORT-ONLY.
/// The lower bound is strict: it proves the limiter really limits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limiter_real_time_smoke_bounded() {
    // 2 MiB at 1 MiB/s ~ 2 s
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
    #[derive(Default)]
    struct Marks {
        started: Option<Instant>,
        first_open: Option<Instant>,
        last_close: Option<Instant>,
        finished: Option<Instant>,
    }
    let marks = std::sync::Arc::new(std::sync::Mutex::new(Marks::default()));
    let m2 = marks.clone();
    let sink: eid_engine::EventSink = std::sync::Arc::new(move |e| {
        let now = Instant::now();
        let mut m = m2.lock().unwrap();
        match e {
            Event::Started { .. } => m.started = Some(now),
            Event::ResponseOpened { .. } => {
                m.first_open.get_or_insert(now);
            }
            Event::ResponseClosed { .. } => m.last_close = Some(now),
            Event::Finished { .. } => m.finished = Some(now),
            _ => {}
        }
    });
    let t0 = Instant::now();
    let o = eid_engine::download(
        eid_engine::JobSpec {
            url,
            dir: dir.path().into(),
            file_name: None,
        },
        cfg,
        sink,
        eid_engine::CancellationToken::new(),
    )
    .await
    .unwrap();
    let returned = Instant::now();
    assert_eq!(o.size, len);
    let m = marks.lock().unwrap();
    let at = |t: Option<Instant>| t.map(|t| t.duration_since(t0).as_secs_f64());
    let (started, first_open, last_close, finished) = (
        at(m.started),
        at(m.first_open),
        at(m.last_close),
        at(m.finished),
    );
    let total = returned.duration_since(t0).as_secs_f64();
    let first_open = first_open.expect("no ResponseOpened event: the transfer phase is unknown");
    let last_close = last_close.expect("no ResponseClosed event: the transfer phase is unknown");
    let transfer = last_close - first_open;
    let fmt = |v: Option<f64>| v.map_or("n/a".to_string(), |v| format!("{v:.3}s"));
    eprintln!(
        "LIMITER PHASES start=0.000s started={} first_open={first_open:.3}s last_close={last_close:.3}s finished={} returned={total:.3}s | probe={} transfer={transfer:.3}s finalize={} total={total:.3}s (smoke bound applies to transfer only)",
        fmt(started),
        fmt(finished),
        fmt(started.map(|s| first_open - s)),
        fmt(finished.map(|f| f - last_close)),
    );
    // Lower bound strict: proves the limiter really limits. Upper bound loose (ADR-0011).
    // Rate accuracy is proven by `limiter_rate_within_5_percent_virtual_time`.
    assert!(
        transfer >= 1.5,
        "limiter did not slow the transfer: {transfer:.2}s"
    );
    assert!(
        transfer <= 4.0,
        "limiter stalled the transfer: {transfer:.2}s (total {total:.2}s)"
    );
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
        date: Some("Wed, 21 Oct 2026 07:29:00 GMT".into()), // strong: Date >= LM + 1 s
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

/// Review B3: a server that gives NO validator (no ETag, no Last-Modified) and silently swaps a
/// same-size file after the probe must never produce a splice of two representations. Without
/// a validator the engine must consume the original response as one stream and restart the
/// whole file after a disconnect, so the result is one coherent version.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_validator_same_size_change_never_splices_representations() {
    let a = Content::Generated {
        seed: 501,
        len: 6 << 20,
    };
    let b = Content::Generated {
        seed: 502,
        len: 6 << 20,
    };
    let beh = Behavior {
        etag: eid_testserver::EtagMode::None,
        last_modified: None,
        cut: Cut::AfterBytes {
            conn_index: 1,
            n: 1 << 20,
        },
        // the probe is request 1 (A); every later request is answered from B
        change_after: Some(ChangeAfter {
            requests: Some(1),
            bytes_sent: None,
            new_content: b.clone(),
        }),
        ..Behavior::default()
    };
    let (_srv, url) = serve("/nv.bin", a.clone(), beh).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(4);
    cfg.max_attempts = 20;
    let (o, log) = get(&url, dir.path(), cfg).await.unwrap();
    // the test only means something if the cut really forced a reconnect...
    assert!(
        log.count(|e| matches!(e, eid_engine::Event::Retry { .. })) >= 1,
        "the disconnect never happened: the scenario was not exercised"
    );
    // ...and the coherent answer after the whole-file restart is the NEW representation, whole
    assert_eq!(
        sha_of_file(&o.path),
        b.sha256_hex(),
        "result is not one coherent version (spliced, or stale A + B)"
    );
}

/// Review (RFC 9110 8.8.2.2): `Last-Modified` with no ETag is a strong validator only if the
/// server's `Date` is >= LM + 1 s. With `Date == LM` it is weak: the engine must treat the
/// download as having NO validator (one stream, no segments), even with 4 connections allowed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn last_modified_equal_to_date_is_a_weak_validator_so_the_download_is_one_stream() {
    let content = Content::Generated {
        seed: 801,
        len: 8 << 20,
    };
    let beh = Behavior {
        throttle_bps: Some(6_000_000),
        etag: eid_testserver::EtagMode::None,
        last_modified: Some("Wed, 21 Oct 2026 07:28:00 GMT".into()),
        date: Some("Wed, 21 Oct 2026 07:28:00 GMT".into()), // Date == LM: weak
        ..Behavior::default()
    };
    let (_srv, url) = serve("/lm-weak.bin", content.clone(), beh).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, log) = get(&url, dir.path(), fast_cfg(4)).await.unwrap();
    assert_eq!(sha_of_file(&o.path), content.sha256_hex());
    // the single-stream path emits no segment events (0); a segmented download shows 4
    assert!(
        log.peak_live_responses() <= 1,
        "a weak Last-Modified must not be used to split the download (peak live responses = {})",
        log.peak_live_responses()
    );
}

/// Counterpart: `Date >= LM + 1 s` makes the same `Last-Modified` strong, so the download is
/// segmented (several live responses) — proves the weak case above is the rule, not a harness
/// artefact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn last_modified_one_second_before_date_is_a_strong_validator_so_the_download_is_segmented() {
    let content = Content::Generated {
        seed: 802,
        len: 8 << 20,
    };
    let beh = Behavior {
        throttle_bps: Some(6_000_000),
        etag: eid_testserver::EtagMode::None,
        last_modified: Some("Wed, 21 Oct 2026 07:28:00 GMT".into()),
        date: Some("Wed, 21 Oct 2026 07:28:01 GMT".into()), // Date == LM + 1 s: strong
        ..Behavior::default()
    };
    let (_srv, url) = serve("/lm-strong.bin", content.clone(), beh).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, log) = get(&url, dir.path(), fast_cfg(4)).await.unwrap();
    assert_eq!(sha_of_file(&o.path), content.sha256_hex());
    assert!(
        log.peak_live_responses() > 1,
        "a strong Last-Modified must allow segmenting (peak live responses = {})",
        log.peak_live_responses()
    );
}
