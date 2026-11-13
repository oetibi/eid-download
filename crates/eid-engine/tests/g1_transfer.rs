mod common;
use common::*;
use eid_engine::Error;
use eid_testserver::{Behavior, Content, LengthLie, RangeMode, Redirect, Stall};
use std::time::{Duration, Instant};

fn gen(len: u64) -> Content {
    Content::Generated { seed: 11, len }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_206_enables_ranges_and_uses_open_response() {
    let (srv, url) = serve("/p.bin", gen(2 << 20), Behavior::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, log) = get(&url, dir.path(), fast_cfg(1)).await.unwrap();
    assert_eq!(o.sha256.unwrap(), gen(2 << 20).sha256_hex());
    let st = srv.stats("/p.bin");
    // single connection: the probe's own response is segment 0 => exactly one request
    assert_eq!(st.requests.len(), 1, "{:?}", st.requests);
    assert_eq!(st.requests[0].range_header.as_deref(), Some("bytes=0-"));
    assert_eq!(st.requests[0].accept_encoding.as_deref(), Some("identity"));
    assert!(log.count(|e| matches!(e, eid_engine::Event::Started { ranged: true, .. })) == 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_200_falls_back_to_single_stream() {
    let (srv, url) = serve("/n.bin", gen(3 << 20), Behavior::no_range()).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, log) = get(&url, dir.path(), fast_cfg(8)).await.unwrap();
    assert_eq!(o.sha256.unwrap(), gen(3 << 20).sha256_hex());
    assert_eq!(srv.stats("/n.bin").peak_concurrent, 1);
    assert_eq!(srv.stats("/n.bin").requests.len(), 1);
    assert!(log.count(|e| matches!(e, eid_engine::Event::Started { ranged: false, .. })) == 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn range_ignored_with_200_falls_back_to_single_stream() {
    // advertises Accept-Ranges but answers 200 to every Range request
    let b = Behavior {
        range: RangeMode::Ignore200,
        ..Behavior::default()
    };
    let (srv, url) = serve("/i.bin", gen(3 << 20), b).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, _) = get(&url, dir.path(), fast_cfg(8)).await.unwrap();
    assert_eq!(o.sha256.unwrap(), gen(3 << 20).sha256_hex());
    assert_eq!(srv.stats("/i.bin").peak_concurrent, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn range_416_falls_back() {
    let b = Behavior {
        range: RangeMode::Reject416,
        ..Behavior::default()
    };
    let (srv, url) = serve("/r.bin", gen(1 << 20), b).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, _) = get(&url, dir.path(), fast_cfg(4)).await.unwrap();
    assert_eq!(o.sha256.unwrap(), gen(1 << 20).sha256_hex());
    let reqs = srv.stats("/r.bin").requests;
    assert_eq!(reqs[0].status, 416);
    assert!(
        reqs[1].range_header.is_none(),
        "fallback must not send Range"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_content_range_rejected() {
    let b = Behavior {
        range: RangeMode::WrongContentRange,
        ..Behavior::default()
    };
    let (_srv, url) = serve("/w.bin", gen(1 << 20), b).await;
    let dir = tempfile::tempdir().unwrap();
    let r = get(&url, dir.path(), fast_cfg(4)).await;
    assert!(matches!(r, Err(Error::BadResponse(_))), "{:?}", r.err());
    assert!(
        only_files(dir.path()).is_empty(),
        "no files for a rejected probe"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forced_gzip_single_stream_raw_bytes() {
    let b = Behavior {
        gzip_always: true,
        ..Behavior::default()
    };
    let (srv, url) = serve("/z.bin", gen(1 << 20), b).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, _) = get(&url, dir.path(), fast_cfg(4)).await.unwrap();
    // raw bytes exactly as sent (no transparent decoding), single connection
    assert_eq!(sha_of_file(&o.path), gen(1 << 20).sha256_hex());
    let reqs = srv.stats("/z.bin").requests;
    assert!(reqs.iter().skip(1).all(|r| r.range_header.is_none()));
    assert_eq!(
        o.peak_workers, 1,
        "gzip-labelled content must be a single stream"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_use_url_not_burned_by_probe() {
    for b in [
        Behavior {
            single_use: true,
            ..Behavior::default()
        },
        Behavior {
            single_use: true,
            ..Behavior::no_range()
        },
    ] {
        let (srv, url) = serve("/s.bin", gen(1 << 20), b).await;
        let dir = tempfile::tempdir().unwrap();
        let (o, _) = get(&url, dir.path(), fast_cfg(1)).await.unwrap();
        assert_eq!(o.sha256.unwrap(), gen(1 << 20).sha256_hex());
        let ok = srv
            .stats("/s.bin")
            .requests
            .iter()
            .filter(|r| r.status == 200 || r.status == 206)
            .count();
        assert_eq!(ok, 1, "the probe must be the transfer itself");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redirect_loop_bounded() {
    let b = Behavior {
        redirect: Some(Redirect::Loop),
        ..Behavior::default()
    };
    let (srv, url) = serve("/l.bin", gen(1000), b).await;
    let dir = tempfile::tempdir().unwrap();
    let t = Instant::now();
    let r = get(&url, dir.path(), fast_cfg(4)).await;
    assert!(r.is_err());
    assert!(
        srv.stats("/l.bin").requests.len() <= 11 * 6,
        "bounded requests: {}",
        srv.stats("/l.bin").requests.len()
    );
    assert!(t.elapsed() < Duration::from_secs(20));
    assert!(only_files(dir.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_origin_redirect_drops_credentials() {
    let srv = eid_testserver::TestServer::start().await;
    let port = srv.base_url().rsplit(':').next().unwrap().to_string();
    // same server, different host name => different origin
    srv.add(
        "/a.bin",
        gen(1000),
        Behavior {
            redirect: Some(Redirect::To(format!("http://localhost:{port}/b.bin"))),
            ..Behavior::default()
        },
    );
    srv.add("/b.bin", gen(1000), Behavior::default());
    let url = format!("http://alice:s3cretpw@127.0.0.1:{port}/a.bin");
    let dir = tempfile::tempdir().unwrap();
    let (o, _) = get(&url, dir.path(), fast_cfg(1)).await.unwrap();
    assert_eq!(o.size, 1000);
    let a = srv.stats("/a.bin").requests;
    let b = srv.stats("/b.bin").requests;
    assert!(
        a.iter().all(|r| r.authorization.is_some()),
        "origin gets credentials"
    );
    assert!(
        !b.is_empty() && b.iter().all(|r| r.authorization.is_none()),
        "other origin must NOT: {b:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lying_content_length_short_body_never_succeeds() {
    let want = gen(2 << 20).sha256_hex();
    for (name, b) in [
        (
            "single/body-short",
            Behavior {
                length_lie: LengthLie::BodyShort { missing: 1000 },
                ..Behavior::no_range()
            },
        ),
        (
            "ranged/body-short",
            Behavior {
                length_lie: LengthLie::BodyShort { missing: 1000 },
                ..Behavior::default()
            },
        ),
        (
            "single/header+",
            Behavior {
                length_lie: LengthLie::Header { delta: 500 },
                ..Behavior::no_range()
            },
        ),
        (
            "ranged/header+",
            Behavior {
                length_lie: LengthLie::Header { delta: 500 },
                ..Behavior::default()
            },
        ),
        (
            "ranged/header-",
            Behavior {
                length_lie: LengthLie::Header { delta: -500 },
                ..Behavior::default()
            },
        ),
    ] {
        let (_srv, url) = serve("/lie.bin", gen(2 << 20), b).await;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fast_cfg(4);
        cfg.max_attempts = 3;
        let r = get(&url, dir.path(), cfg).await;
        match r {
            Ok((o, _)) => assert_eq!(
                sha_of_file(&o.path),
                want,
                "{name}: success must mean correct bytes"
            ),
            Err(_) => assert!(
                !dir.path().join("lie.bin").exists(),
                "{name}: no final file on failure"
            ),
        }
    }
}

/// Documented limitation: a *non-ranged* response whose Content-Length is smaller than the real
/// body is indistinguishable from a legitimately shorter file at the HTTP level; the engine
/// trusts the declared length (and never reads past it). It must not hang or corrupt memory.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn understated_content_length_on_plain_200_is_trusted_but_bounded() {
    let b = Behavior {
        length_lie: LengthLie::Header { delta: -500 },
        ..Behavior::no_range()
    };
    let (_srv, url) = serve("/under.bin", gen(1 << 20), b).await;
    let dir = tempfile::tempdir().unwrap();
    let r = get(&url, dir.path(), fast_cfg(1)).await;
    if let Ok((o, _)) = r {
        assert_eq!(o.size, (1 << 20) - 500);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lying_content_length_body_short_is_an_error_not_success() {
    // The lie that always truncates must fail (never a "successful" short file).
    let b = Behavior {
        length_lie: LengthLie::BodyShort { missing: 1000 },
        ..Behavior::no_range()
    };
    let (_srv, url) = serve("/lie2.bin", gen(1 << 20), b).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(1);
    cfg.max_attempts = 2;
    let r = get(&url, dir.path(), cfg).await;
    assert!(
        matches!(r, Err(Error::ShortBody) | Err(Error::NoProgress(_))),
        "{:?}",
        r.err()
    );
    assert!(!dir.path().join("lie2.bin").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_loris_and_stall_times_out() {
    // permanent stall with zero progress: must give up in bounded time instead of hanging
    let b = Behavior {
        stall: Some(Stall {
            after_bytes: 0,
            duration: Duration::from_secs(60),
            once: false,
        }),
        ..Behavior::default()
    };
    let (_srv, url) = serve("/st.bin", gen(1 << 20), b).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(2);
    cfg.stall_timeout = Duration::from_millis(300);
    cfg.max_attempts = 2;
    let t = Instant::now();
    let r = get(&url, dir.path(), cfg).await;
    assert!(r.is_err());
    assert!(
        t.elapsed() < Duration::from_secs(15),
        "took {:?}",
        t.elapsed()
    );
    // a one-off stall recovers by reconnecting
    let b = Behavior {
        stall: Some(Stall {
            after_bytes: 100_000,
            duration: Duration::from_secs(60),
            once: true,
        }),
        ..Behavior::default()
    };
    let (_srv, url) = serve("/st2.bin", gen(1 << 20), b).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(1);
    cfg.stall_timeout = Duration::from_millis(300);
    let (o, log) = get(&url, dir.path(), cfg).await.unwrap();
    assert_eq!(o.sha256.unwrap(), gen(1 << 20).sha256_hex());
    assert!(log.count(|e| matches!(e, eid_engine::Event::Retry { .. })) >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sha256_matches_single_connection() {
    let (_srv, url) = serve("/one.bin", gen(5 << 20), Behavior::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, _) = get(&url, dir.path(), fast_cfg(1)).await.unwrap();
    assert_eq!(sha_of_file(&o.path), gen(5 << 20).sha256_hex());
    assert_eq!(o.peak_workers, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expected_sha256_mismatch_is_an_error() {
    let (_srv, url) = serve("/c.bin", gen(100_000), Behavior::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(1);
    cfg.expected_sha256 = Some("0".repeat(64));
    let r = get(&url, dir.path(), cfg).await;
    assert!(matches!(r, Err(Error::Checksum { .. })));
    assert!(!dir.path().join("c.bin").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn existing_file_is_never_overwritten() {
    let (_srv, url) = serve("/dup.bin", gen(50_000), Behavior::default()).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dup.bin"), b"precious").unwrap();
    let (o, _) = get(&url, dir.path(), fast_cfg(1)).await.unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("dup.bin")).unwrap(),
        b"precious"
    );
    assert_eq!(o.path.file_name().unwrap().to_str().unwrap(), "dup (1).bin");
    let (o2, _) = get(&url, dir.path(), fast_cfg(1)).await.unwrap();
    assert_eq!(
        o2.path.file_name().unwrap().to_str().unwrap(),
        "dup (2).bin"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zero_length_and_tiny_files() {
    for len in [0u64, 1, 2, 4095] {
        let (_srv, url) = serve("/t.bin", gen(len), Behavior::default()).await;
        let dir = tempfile::tempdir().unwrap();
        let r = get(&url, dir.path(), fast_cfg(4)).await;
        match r {
            Ok((o, _)) => assert_eq!(sha_of_file(&o.path), gen(len).sha256_hex(), "len {len}"),
            Err(e) => panic!("len {len}: {e}"),
        }
    }
}
