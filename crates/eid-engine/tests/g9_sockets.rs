//! Every test that uses the kernel socket table (`SocketWatch`) lives in THIS test binary, so the
//! sampler thread never competes with the other engine tests (a separate process: cargo runs test
//! binaries one after the other) and the tests of this file never compete with each other: each
//! takes `serial()`, i.e. they run one at a time whatever `--test-threads` says. CI additionally
//! runs this binary with `--test-threads=1` and prints `nproc` (docs/adr/0011).
mod common;
use common::*;
use eid_engine::Event;
use eid_testserver::{Behavior, Content};
use std::time::{Duration, Instant};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One socket-measuring test at a time (the sampler is a polling thread: concurrent heavy
/// tests starve it, which is what made the negative test flaky on 2-vCPU CI runners).
async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL.lock().await
}

fn big() -> Content {
    Content::Generated {
        seed: 7,
        len: 20 * 1024 * 1024,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sha256_matches_with_1_8_32_connections() {
    let _serial = serial().await;
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

/// Strict connection-cap test for the "peak 9 exceeds cap" CI failure (ADR-0011). STRICT: the
/// real client sockets (kernel socket table, owned by this process) must never exceed the cap.
/// Secondary: engine live responses (counted inside the sink at emit time; <= workers by
/// construction, diagnostic only) and the server's own in-flight counter (cap+1 tolerated, it
/// notices a close only when its task is scheduled). With EID_EVIDENCE_OUT set, 12 runs are
/// made and server-vs-client over-counts are written there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_cap_is_strict_by_real_sockets_engine_and_server() {
    let _serial = serial().await;
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

/// Negative test for the sampler itself (ADR-0011), EVENT-CONFIRMED part (asserted; no timing
/// bet): open cap+1 = 9 raw client sockets and keep them open UNTIL the sampler's timeline shows
/// a count above the cap (2 s deadline: missing it fails the test), then close them and wait
/// until the timeline is back at or below the cap (2 s deadline again). This proves the counting
/// itself (ownership by inode, parsing, a close is seen) which is what the strict cap test
/// depends on: its `<= cap` assertion would FAIL on such a peak. The hold is as long as the
/// sampler needs, so a starved sampler thread can delay the test but never make it miss.
/// What it cannot prove is how SHORT an overshoot the sampler can see on a given host: that is
/// the report-only test below plus `max_gap_us` in every SAMPLER line.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn socket_watch_sees_cap_plus_one_while_held_and_sees_it_closed() {
    let _serial = serial().await;
    let (srv, _url) = serve(
        "/f.bin",
        Content::Generated { seed: 1, len: 1 },
        Behavior::default(),
    )
    .await;
    if !socket_watch_supported() {
        eprintln!("NOT TESTED on this platform: kernel socket table is Linux only (STATUS.md)");
        return;
    }
    let addr = srv.base_url().trim_start_matches("http://").to_string();
    let cap = 8usize;
    let w = SocketWatch::start(&srv.base_url()).expect("supported platform");
    assert_eq!(w.current(), 0, "no client socket exists yet");
    let held: Vec<std::net::TcpStream> = (0..=cap)
        .map(|_| std::net::TcpStream::connect(&addr).unwrap())
        .collect();
    let deadline = Instant::now() + Duration::from_secs(2);
    while w.current() <= cap {
        assert!(
            Instant::now() < deadline,
            "the sampler never saw {} sockets while they were held open (saw {})",
            cap + 1,
            w.current()
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    eprintln!(
        "sampler saw {} sockets while {} were held",
        w.current(),
        cap + 1
    );
    drop(held);
    let deadline = Instant::now() + Duration::from_secs(2);
    while w.current() > cap {
        assert!(
            Instant::now() < deadline,
            "the sampler never saw the sockets close (still {})",
            w.current()
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let r = w.finish();
    eprintln!("{}", r.describe());
    assert_eq!(
        r.peak,
        cap + 1,
        "the peak must be exactly the {} sockets that were opened",
        cap + 1
    );
    assert!(
        closed_after_peak(&r),
        "the close was not recorded in the timeline: {:?}",
        r.timeline
    );
}

/// The timeline ended at or below the cap after having been above it.
fn closed_after_peak(r: &SocketReport) -> bool {
    let peak_at = r.timeline.iter().position(|x| x.1 == r.peak);
    peak_at.is_some_and(|i| r.timeline[i..].iter().any(|x| x.1 < r.peak))
}

/// REPORT-ONLY part of the sampler negative test (name kept: it is in `required_tests.txt`):
/// how often the sampler catches a cap+1 peak held for a FIXED 5, 2 and 1 ms, and the worst
/// `max_gap` between two samples during those tries. Printed on every platform and in CI (no
/// `if CI` branch), never asserted except that the watch itself must be valid (`finish` panics
/// on `samples == 0`, too many failed queries, ...). This is the measuring instrument's
/// resolution: an overshoot shorter than `max_gap` can be missed on a given host. Under 3x CPU
/// load the 5 ms catch rate was measured at only ~0.55 per try, which is why the asserted test
/// above is event-confirmed instead of a timing bet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn socket_watch_catches_a_brief_cap_plus_one_peak() {
    let _serial = serial().await;
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
    let base = srv.base_url();
    for hold_ms in [5u64, 2, 1] {
        let (mut seen, mut missed, mut not_measured) = (0u32, 0u32, 0u32);
        let mut worst_gap = Duration::ZERO;
        for _ in 0..30 {
            let w = SocketWatch::start(&base).expect("supported platform");
            let mut s: Vec<std::net::TcpStream> = (0..cap)
                .map(|_| std::net::TcpStream::connect(&addr).unwrap())
                .collect();
            s.push(std::net::TcpStream::connect(&addr).unwrap()); // socket #9
            tokio::time::sleep(Duration::from_millis(hold_ms)).await;
            drop(s);
            tokio::time::sleep(Duration::from_millis(3)).await;
            match w.finish_checked() {
                Ok(r) => {
                    worst_gap = worst_gap.max(r.max_gap);
                    if r.peak > cap {
                        seen += 1;
                    } else {
                        missed += 1;
                    }
                }
                Err(e) => {
                    not_measured += 1;
                    eprintln!("sampler detection (report only): not measured: {e}");
                }
            }
        }
        eprintln!(
            "sampler detection (report only): cap+1 held {hold_ms} ms caught {seen}/30 missed {missed} not_measured {not_measured} max_gap_us={}",
            worst_gap.as_micros()
        );
    }
}

/// Proves sockets close MID-download (pool_max_idle_per_host(0) + per-response close): with a
/// crawling first connection that gets split, fast segments finish and their sockets disappear
/// from the kernel count while the download is still running (count falls but stays > 0).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn socket_closes_mid_download_when_segments_finish_or_split() {
    let _serial = serial().await;
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
    let _serial = serial().await;
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
    let _serial = SERIAL.blocking_lock();
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
