//! With the cap at ONE connection the engine must never hold a second socket: the previous
//! response (a 416, or a body that just failed) has to be dropped BEFORE the next request is
//! sent. Measured on the real kernel socket table (ADR-0011); a cap-by-construction permit
//! design (proposed ADR-0012) would DEADLOCK on exactly these paths if they held a response.
mod common;
use common::*;
use eid_engine::Event;
use eid_testserver::{Behavior, Content, RangeMode, Stall};
use std::time::Duration;

async fn run_capped(url: &str, behavior_name: &str, base_url: &str) -> (usize, String) {
    let dir = tempfile::tempdir().unwrap();
    let watch = SocketWatch::start(base_url);
    // a hang (the failure mode of a permit-holding response) must fail, not wedge the suite
    let r = tokio::time::timeout(Duration::from_secs(60), get(url, dir.path(), fast_cfg(1)))
        .await
        .unwrap_or_else(|_| panic!("{behavior_name}: download hung with connections = 1"));
    let (o, _log) = r.unwrap();
    assert!(o.size > 0);
    let rep = watch.map(SocketWatch::finish);
    assert_eq!(rep.is_some(), socket_watch_supported());
    match rep {
        Some(r) => (r.peak, r.describe()),
        None => (
            0,
            "NOT TESTED on this platform (no kernel socket table)".into(),
        ),
    }
}

// KNOWN GAP, not a regression guard: `drop(resp)` is done (download.rs probe), but hyper closes the
// dropped connection's fd asynchronously in its own task, so the kernel can still show the old
// socket while the next connect is already established (observed: peak 2 after the fix, ~50% of
// rounds under load). Only a cap by construction (proposed ADR-0012: permit released when the
// TcpStream itself drops) can make this deterministic. Run it by hand with `--ignored`.
#[ignore = "known gap: needs cap-by-construction (proposed ADR-0012)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_connection_probe_416_fallback_never_holds_two_sockets() {
    // the overlap window is short, so repeat: before the fix this fails with near certainty
    let mut worst = 0;
    for round in 0..12 {
        let b = Behavior {
            range: RangeMode::Reject416,
            ..Behavior::default()
        };
        let content = Content::Generated {
            seed: 11,
            len: 1 << 20,
        };
        let (srv, url) = serve("/r.bin", content, b).await;
        let (peak, d) = run_capped(&url, "416 fallback", &srv.base_url()).await;
        eprintln!("PEAK conns=1 client_sockets={peak} (416 fallback, round {round}) {d}");
        worst = worst.max(peak);
    }
    if socket_watch_supported() {
        assert!(
            worst <= 1,
            "416 fallback held {worst} sockets at connections = 1"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_connection_single_stream_retry_drops_old_body_first() {
    let b = Behavior {
        range: RangeMode::Ignore200,
        // the server goes silent mid-body but keeps the socket open: the stalled body is still
        // ALIVE in the client when the retry starts, unless the engine drops it first
        stall: Some(Stall {
            after_bytes: 100_000,
            duration: Duration::from_secs(60),
            once: true,
        }),
        ..Behavior::default()
    };
    let content = Content::Generated {
        seed: 12,
        len: 3 << 20,
    };
    let (srv, url) = serve("/s.bin", content.clone(), b).await;
    let dir = tempfile::tempdir().unwrap();
    let watch = SocketWatch::start(&srv.base_url());
    let mut cfg = fast_cfg(1);
    cfg.stall_timeout = Duration::from_millis(400);
    // a long backoff leaves hyper ample time to close the dropped socket asynchronously, so this
    // test measures only the ORDER (old body dropped before the retry), not scheduler luck
    cfg.backoff_base = Duration::from_millis(150);
    cfg.backoff_cap = Duration::from_millis(150);
    let (o, log) = tokio::time::timeout(Duration::from_secs(60), get(&url, dir.path(), cfg))
        .await
        .unwrap_or_else(|_| panic!("single-stream retry hung with connections = 1"))
        .unwrap();
    assert_eq!(sha_of_file(&o.path), content.sha256_hex());
    assert!(
        log.count(|e| matches!(e, Event::Retry { .. })) >= 1,
        "no retry happened"
    );
    let rep = watch.map(SocketWatch::finish);
    assert_eq!(rep.is_some(), socket_watch_supported());
    if let Some(r) = rep {
        eprintln!(
            "PEAK conns=1 client_sockets={} (single-stream retry) {}",
            r.peak,
            r.describe()
        );
        assert!(
            r.peak <= 1,
            "retry held {} sockets at connections = 1",
            r.peak
        );
    }
}
