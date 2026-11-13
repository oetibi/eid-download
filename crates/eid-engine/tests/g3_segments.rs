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
        let watch = SocketWatch::start(&srv.base_url());
        let (o, log) = get(&url, dir.path(), fast_cfg(conns)).await.unwrap();
        let sock = watch.map(SocketWatch::finish);
        // No silent skip: a watch exists exactly where the platform supports it (Linux).
        assert_eq!(
            sock.is_some(),
            socket_watch_supported(),
            "socket watch missing on a supported platform"
        );
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
        // Strict cap, real-socket kernel count is the strict one; engine/server counters are secondary (ADR-0011).
        let server_peak = st.peak_concurrent;
        eprintln!(
            "PEAK conns={conns} client_sockets={} client_responses={} server={server_peak} workers={}",
            sock.as_ref().map_or("n/a".to_string(), |r| r.peak.to_string()),
            log.peak_live_responses(),
            o.peak_workers
        );
        if let Some(r) = &sock {
            eprintln!("{}", r.describe());
            assert!(
                r.peak <= conns,
                "{conns}: {} real client sockets (active+idle)",
                r.peak
            );
        }
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
        // Secondary check only (ADR-0011): cap+1 tolerated for the server's own counter.
        if server_peak > conns {
            eprintln!("NOTE server in-flight peak {server_peak} > cap {conns} (server-side lag)");
        }
        assert!(
            server_peak <= conns + 1,
            "{conns}: server saw {server_peak} in flight"
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

/// Strict connection-cap test for the "peak 9 exceeds cap" CI failure (ADR-0011). STRICT: the
/// real client sockets (kernel socket table, owned by this process) must never exceed the cap.
/// Secondary: engine live responses (counted inside the sink at emit time; <= workers by
/// construction, diagnostic only) and the server's own in-flight counter (cap+1 tolerated, it
/// notices a close only when its task is scheduled). With EID_EVIDENCE_OUT set, 12 runs are
/// made and server-vs-client over-counts are written there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_cap_is_strict_by_real_sockets_engine_and_server() {
    use eid_engine::Event;
    use eid_testserver::{now_us, ServerEventKind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    let conns = 8usize;
    let mut report = String::new();
    let mut saw_server_overcount = false;
    let runs = if std::env::var_os("EID_EVIDENCE_OUT").is_some() {
        12
    } else {
        3
    };
    for run in 0..runs {
        let content = Content::Generated {
            seed: 7,
            len: 20 * 1024 * 1024,
        };
        let (srv, url) = serve("/f.bin", content.clone(), Behavior::default()).await;
        let dir = tempfile::tempdir().unwrap();
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let client_log: Arc<Mutex<Vec<(u64, bool, usize)>>> = Arc::default(); // (t_us, opened?, seg)
        let (l2, p2, c2) = (live.clone(), peak.clone(), client_log.clone());
        let sink: eid_engine::EventSink = Arc::new(move |e| match e {
            Event::ResponseOpened { seg } => {
                let n = l2.fetch_add(1, Ordering::SeqCst) + 1;
                p2.fetch_max(n, Ordering::SeqCst);
                c2.lock().unwrap().push((now_us(), true, seg));
            }
            Event::ResponseClosed { seg } => {
                l2.fetch_sub(1, Ordering::SeqCst);
                c2.lock().unwrap().push((now_us(), false, seg));
            }
            _ => {}
        });
        let watch = SocketWatch::start(&srv.base_url());
        let o = eid_engine::download(
            eid_engine::JobSpec {
                url: url.clone(),
                dir: dir.path().into(),
                file_name: None,
            },
            fast_cfg(conns),
            sink,
            eid_engine::CancellationToken::new(),
        )
        .await
        .unwrap();
        let sock_report = watch.map(SocketWatch::finish);
        assert_eq!(
            sock_report.is_some(),
            socket_watch_supported(),
            "socket watch missing on a supported platform"
        );
        assert_eq!(sha_of_file(&o.path), content.sha256_hex());
        eprintln!(
            "PEAK conns={conns} client_sockets={} client_responses={} server={} workers={} (run {run})",
            sock_report.as_ref().map_or("n/a".to_string(), |r| r.peak.to_string()),
            peak.load(Ordering::SeqCst),
            srv.stats("/f.bin").peak_concurrent,
            o.peak_workers
        );
        if let Some(r) = &sock_report {
            eprintln!("{}", r.describe());
            if r.peak > conns {
                // evidence for a cap violation: engine events around the first over-cap sample
                let t0 = r.timeline.iter().find(|x| x.1 > conns).unwrap().0;
                let cl = client_log.lock().unwrap();
                let mut ctx = String::new();
                for (t, op, seg) in cl.iter().filter(|(t, _, _)| t.abs_diff(t0) < 20_000) {
                    ctx.push_str(&format!(
                        "  {:+}us engine {} seg {seg}\n",
                        *t as i64 - t0 as i64,
                        if *op {
                            "ResponseOpened"
                        } else {
                            "ResponseClosed"
                        }
                    ));
                }
                for (t, n) in r.timeline.iter().filter(|(t, _)| t.abs_diff(t0) < 20_000) {
                    ctx.push_str(&format!(
                        "  {:+}us kernel sockets = {n}\n",
                        *t as i64 - t0 as i64
                    ));
                }
                eprintln!("OVER-CAP CONTEXT (t0 = first sample > cap):\n{ctx}");
            }
            assert!(
                r.peak <= conns,
                "run {run}: {} real client sockets (cap {conns})",
                r.peak
            );
        }
        assert!(
            srv.stats("/f.bin").peak_concurrent <= conns + 1,
            "run {run}: server saw more than {} in flight",
            conns + 1
        );
        assert!(
            peak.load(Ordering::SeqCst) <= conns,
            "run {run}: client had {} live responses (cap {conns})",
            peak.load(Ordering::SeqCst)
        );
        assert!(o.peak_workers <= conns);
        let st = srv.stats("/f.bin");
        let server_peak = st.events.iter().map(|e| e.active_after).max().unwrap_or(0);
        if server_peak > conns {
            saw_server_overcount = true;
            // first server instant above the cap, and what the client had live right then
            let at = st.events.iter().find(|e| e.active_after > conns).unwrap();
            let cl = client_log.lock().unwrap();
            let live_at = cl
                .iter()
                .filter(|(t, _, _)| *t <= at.t_us)
                .fold(0i64, |a, (_, op, _)| a + if *op { 1 } else { -1 });
            // lag between each client close and the server's next RequestEnd after it
            let ends: Vec<u64> = st
                .events
                .iter()
                .filter(|e| e.kind == ServerEventKind::RequestEnd)
                .map(|e| e.t_us)
                .collect();
            let mut lags: Vec<u64> = cl
                .iter()
                .filter(|(_, op, _)| !*op)
                .filter_map(|(t, _, _)| ends.iter().copied().find(|e| e >= t).map(|e| e - t))
                .collect();
            lags.sort_unstable();
            let med = lags.get(lags.len() / 2).copied().unwrap_or(0);
            let max = lags.last().copied().unwrap_or(0);
            report.push_str(&format!(
                "run {run}: server peak in-flight {server_peak} > cap {conns} at t={}us (request #{}, conn {}); client live responses at that instant = {live_at} (client peak over run = {}); client-close -> server-RequestEnd lag: median {med}us, max {max}us over {} closes\n",
                at.t_us, at.request_index, at.conn_id, peak.load(Ordering::SeqCst), lags.len()
            ));
        }
    }
    if let Ok(path) = std::env::var("EID_EVIDENCE_OUT") {
        std::fs::write(path, &report).unwrap();
    }
    eprintln!("server over-count observed: {saw_server_overcount}\n{report}");
}

/// Negative test for the sampler itself (ADR-0011): deliberately open cap+1 = 9 raw sockets for
/// a FIXED 5 ms and require the watcher to see the over-cap count, so the `<= cap` assertions
/// would FAIL on such a peak. Sampler: netlink SOCK_DIAG query in a loop with a 50 us sleep
/// (query itself tens of us), a sampling period of roughly 100 us.
///
/// Pass criterion (no new flake source): 3 independent rounds, each of which must catch the
/// peak at least once within K = 16 tries (the 5 ms hold and the 3 rounds are unchanged).
/// Measured per-try detection of a 5 ms peak: 100/100 idle; 89..99/100 under 8 busy loops on
/// 8 cores; but only ~0.55 per try under 3x oversubscription (24 busy loops on 8 cores: 1 round
/// of 600 missed all 8 tries, 0.45^8 ~ 1/600). K was raised from 8 to 16 on THAT measurement
/// (it is a statistical parameter, not a relaxation of what is asserted): a round fails only if
/// all 16 tries miss, 0.45^16 ~ 2.8e-6 per round, so the whole test false-fails with probability
/// ~3 * 0.45^16 ~ 1e-5 at the measured 3x-load rate. Starvation of the sampler thread can only
/// cause a MISS, never a false alarm, and the extra tries cost milliseconds. Shorter holds
/// (2 ms, 1 ms) are REPORT-ONLY (measured 52/100 for 2 ms under load): never asserted. A longer
/// hold would weaken the test (a real engine overshoot lasts about 2-7 ms).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn socket_watch_catches_a_brief_cap_plus_one_peak() {
    let (srv, _url) = serve(
        "/f.bin",
        Content::Generated { seed: 1, len: 1 },
        Behavior::default(),
    )
    .await;
    let addr = srv.base_url().trim_start_matches("http://").to_string();
    let cap = 8usize;
    if !socket_watch_supported() {
        eprintln!("NOT TESTED on this platform: kernel socket table is Linux only (STATUS.md)");
        return;
    }
    let last_sampler = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let base = srv.base_url();
    let one_try = |hold_ms: u64| {
        let (base, addr, last_sampler) = (base.clone(), addr.clone(), last_sampler.clone());
        async move {
            let w = SocketWatch::start(&base).expect("supported platform");
            // `start` returns only after the sampler's first sample: the peak is a real change
            let mut s: Vec<std::net::TcpStream> = (0..cap)
                .map(|_| std::net::TcpStream::connect(&addr).unwrap())
                .collect();
            s.push(std::net::TcpStream::connect(&addr).unwrap()); // socket #9 = the violation
            tokio::time::sleep(Duration::from_millis(hold_ms)).await;
            drop(s);
            tokio::time::sleep(Duration::from_millis(3)).await;
            // `finish` (panic on an invalid watch) is for the ASSERTED 5 ms rounds only; the
            // report-only loops use `finish_checked` and count "not measured".
            let r = if hold_ms == 5 {
                w.finish()
            } else {
                match w.finish_checked() {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("sampler detection (report only): not measured: {e}");
                        return None;
                    }
                }
            };
            *last_sampler.lock().unwrap() = r.describe();
            Some(r.peak > cap)
        }
    };
    const K: usize = 16;
    for round in 0..3 {
        let mut caught_after = None;
        for t in 1..=K {
            if one_try(5).await == Some(true) {
                caught_after = Some(t);
                break;
            }
        }
        eprintln!("sampler negative test round {round}: 5 ms cap+1 peak caught after {caught_after:?} tries (K={K})");
        assert!(
            caught_after.is_some(),
            "round {round}: sampler missed a 5 ms cap+1 peak in all {K} tries"
        );
    }
    eprintln!("(last try) {}", last_sampler.lock().unwrap());
    // report-only detection rates for shorter holds (never asserted)
    for hold_ms in [2u64, 1] {
        let (mut seen, mut not_measured) = (0, 0);
        for _ in 0..30 {
            match one_try(hold_ms).await {
                Some(true) => seen += 1,
                Some(false) => {}
                None => not_measured += 1,
            }
        }
        eprintln!("sampler detection (report only): cap+1 held {hold_ms} ms caught {seen}/30 (not measured: {not_measured})");
    }
}

/// Proves sockets close MID-download (pool_max_idle_per_host(0) + per-response close): with a
/// crawling first connection that gets split, fast segments finish and their sockets disappear
/// from the kernel count while the download is still running (count falls but stays > 0).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn socket_closes_mid_download_when_segments_finish_or_split() {
    let b = Behavior {
        slow_connections: Some((1, 400_000)),
        ..Behavior::default()
    };
    let content = Content::Generated {
        seed: 5,
        len: 8 << 20,
    };
    if !socket_watch_supported() {
        eprintln!("NOT TESTED on this platform: kernel socket table is Linux only (STATUS.md)");
        return;
    }
    let (srv, url) = serve("/m.bin", content.clone(), b).await;
    let watch = SocketWatch::start(&srv.base_url()).expect("supported platform");
    let dir = tempfile::tempdir().unwrap();
    let (o, log) = get(&url, dir.path(), fast_cfg(8)).await.unwrap();
    let r = watch.finish();
    eprintln!("{}", r.describe());
    assert_eq!(sha_of_file(&o.path), content.sha256_hex());
    assert!(
        log.count(|e| matches!(e, Event::Split { .. })) >= 1,
        "no split happened"
    );
    let counts: Vec<usize> = r.timeline.iter().map(|x| x.1).collect();
    eprintln!("socket timeline (count at each change): {counts:?}");
    assert!(
        r.decreased_mid_download(),
        "sockets never decreased while the download was running: {counts:?}"
    );
    assert!(r.peak <= 8);
    assert_eq!(
        *counts.last().unwrap(),
        0,
        "sockets left open after the download: {counts:?}"
    );
}

/// Failure paths of the watch itself must FAIL, never skip (ADR-0011, AGENTS §5). Each case is
/// injected: an unavailable table, every query failing, and the pure validation rules.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socket_watch_failure_paths_fail_instead_of_skipping() {
    if !socket_watch_supported() {
        eprintln!("NOT TESTED on this platform: kernel socket table is Linux only (STATUS.md)");
        return;
    }
    let url = "http://127.0.0.1:9";
    // (1) the kernel socket table cannot be opened -> start() panics on Linux
    let r = std::panic::catch_unwind(|| SocketWatch::start_with_open(url, || None).is_some());
    assert!(
        r.is_err(),
        "start must panic when the socket table is unavailable"
    );
    // (2) every kernel query fails -> finish() panics (and finish_checked returns Err)
    let w = SocketWatch::start_with(url, Box::new(|_| None));
    tokio::time::sleep(Duration::from_millis(60)).await;
    let err = w
        .finish_checked()
        .err()
        .expect("all-failing queries must not validate");
    eprintln!("injected failure reported as: {err}");
    let w = SocketWatch::start_with(url, Box::new(|_| None));
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| w.finish())).is_err(),
        "finish must panic on an invalid watch"
    );
    // (3) zero samples can never validate; a healthy watch does; 10% failures is the limit
    assert!(validate_watch(0, 0, false).is_err(), "zero samples");
    assert!(validate_watch(100, 0, true).is_err(), "gave up");
    assert!(
        validate_watch(80, 20, false).is_err(),
        "too many failed queries"
    );
    assert!(validate_watch(1000, 5, false).is_ok(), "healthy watch");
    // (4) a mostly working query with sporadic failures is still accepted and counted
    let mut n = 0u32;
    let w = SocketWatch::start_with(
        url,
        Box::new(move |_| {
            n += 1;
            (!n.is_multiple_of(50)).then_some(0)
        }),
    );
    tokio::time::sleep(Duration::from_millis(120)).await;
    let r = w.finish();
    assert!(r.samples > 0 && r.failed > 0, "{}", r.describe());
}

/// CI failure (run 43387076289) = `samples == 0`: `start` returned before the sampler thread
/// took a single sample. `start` must now be a readiness barrier: even when the first kernel
/// query is slow, a `finish_checked` right after `start` has at least one sample.
#[test]
fn socket_watch_start_returns_only_after_the_first_sample() {
    let mut first = true;
    let w = SocketWatch::start_with(
        "http://127.0.0.1:9",
        Box::new(move |_| {
            if std::mem::take(&mut first) {
                std::thread::sleep(Duration::from_millis(300)); // a starved/slow sampler
            }
            Some(0)
        }),
    );
    let r = w
        .finish_checked()
        .expect("start must not return before the first sample");
    assert!(r.samples >= 1, "{}", r.describe());
}
