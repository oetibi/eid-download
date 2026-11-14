//! P1 review follow-ups: M3 (response-header deadline), m3 (Retry-After HTTP-date), m1 (numbering).
mod common;
use common::*;
use eid_engine::{Error, Event};
use eid_testserver::{Behavior, Content, HeaderDelay, LimitStatus};
use std::time::{Duration, Instant};

fn gen(len: u64) -> Content {
    Content::Generated { seed: 21, len }
}

fn delayed(skip: u32, count: u32, delay_ms: u64, partial: bool) -> Behavior {
    Behavior {
        header_delay: Some(HeaderDelay {
            skip,
            count,
            delay: Duration::from_millis(delay_ms),
            partial,
        }),
        ..Behavior::default()
    }
}

/// A server that accepts the connection and never completes its headers (nothing at all, or a
/// trickled partial status line) must not hang the job: probe attempts hit the header deadline
/// and the job fails in bounded time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headers_that_never_arrive_time_out_in_the_probe() {
    for partial in [false, true] {
        let (srv, url) = serve("/h.bin", gen(1 << 20), delayed(0, 1000, 60_000, partial)).await;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fast_cfg(2);
        cfg.header_timeout = Duration::from_millis(200);
        cfg.max_attempts = 3;
        let t = Instant::now();
        let r = get(&url, dir.path(), cfg).await;
        assert!(
            matches!(r, Err(Error::NoProgress(3))),
            "partial={partial}: {:?}",
            r.as_ref().map(|_| ())
        );
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
        assert_eq!(
            srv.stats("/h.bin").requests.len(),
            3,
            "every attempt reached the server"
        );
        assert_eq!(only_files(dir.path()), Vec::<String>::new(), "no leftovers");
    }
}

/// Paired success: headers that are merely slow (shorter than the deadline) are accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_but_in_time_headers_are_accepted() {
    for partial in [false, true] {
        let (_srv, url) = serve("/s.bin", gen(1 << 20), delayed(0, 1000, 150, partial)).await;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fast_cfg(1);
        cfg.header_timeout = Duration::from_secs(5);
        let (o, _) = get(&url, dir.path(), cfg).await.unwrap();
        assert_eq!(
            o.sha256.unwrap(),
            gen(1 << 20).sha256_hex(),
            "partial={partial}"
        );
    }
}

/// The ranged workers have the same deadline: the probe is fine, later segment requests hang on
/// their headers a few times, and the download still completes with the right bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ranged_requests_with_stuck_headers_are_retried() {
    let (srv, url) = serve("/r.bin", gen(4 << 20), delayed(1, 3, 60_000, false)).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(4);
    cfg.header_timeout = Duration::from_millis(300);
    let t = Instant::now();
    let (o, log) = get(&url, dir.path(), cfg).await.unwrap();
    assert_eq!(o.sha256.unwrap(), gen(4 << 20).sha256_hex());
    assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
    assert!(
        log.count(|e| matches!(e, Event::Retry { .. })) >= 1,
        "a stuck ranged request must be retried"
    );
    assert!(srv.stats("/r.bin").requests.len() >= 4);
}

/// m3 live: a 429 with an HTTP-date `Retry-After` relative to the server's own `Date`
/// (so the local clock is irrelevant) is honoured as that many seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_date_retry_after_is_honoured_relative_to_the_server_date() {
    let b = Behavior {
        max_concurrent: Some((1, LimitStatus::TooManyRequests)),
        date: Some("Sun, 06 Nov 1994 08:49:37 GMT".into()),
        retry_after_raw: Some("Sun, 06 Nov 1994 08:49:38 GMT".into()),
        throttle_bps: Some(2_000_000),
        ..Behavior::default()
    };
    let (_srv, url) = serve("/d.bin", gen(6 << 20), b).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, log) = get(&url, dir.path(), fast_cfg(4)).await.unwrap();
    assert_eq!(o.sha256.unwrap(), gen(6 << 20).sha256_hex());
    let waits: Vec<u64> = log
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            Event::Throttled { retry_after_ms, .. } => Some(*retry_after_ms),
            _ => None,
        })
        .collect();
    assert!(!waits.is_empty(), "the server must have throttled");
    assert!(
        waits.iter().all(|&w| w == 1000),
        "date form must be read as 1 s, got {waits:?}"
    );
}

/// m1 live: downloading the same URL twice when the name is already maximal (200 bytes, with and
/// without an extension, ASCII and Arabic) must pick a numbered free name, never fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_download_of_a_maximal_name_gets_a_numbered_free_name() {
    let names = [
        "a".repeat(200),
        format!("{}.zip", "b".repeat(196)),
        "ع".repeat(100),
        format!("{}.pdf", "ع".repeat(98)),
    ];
    for (i, name) in names.iter().enumerate() {
        // the name travels in Content-Disposition (RFC 6266 filename*), so any script works
        let enc = percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
        let b = Behavior {
            content_disposition: Some(format!("attachment; filename*=UTF-8''{enc}")),
            ..Behavior::default()
        };
        let (_srv, url) = serve("/m.bin", gen(300_000), b).await;
        let dir = tempfile::tempdir().unwrap();
        let (o1, _) = get(&url, dir.path(), fast_cfg(1)).await.unwrap();
        let (o2, _) = get(&url, dir.path(), fast_cfg(1)).await.unwrap();
        let (o3, _) = get(&url, dir.path(), fast_cfg(1)).await.unwrap();
        let paths = [o1.path, o2.path, o3.path];
        let uniq: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(uniq.len(), 3, "case {i}: {paths:?}");
        for p in &paths {
            assert!(p.exists());
            let n = p.file_name().unwrap().to_string_lossy().len();
            assert!(n <= 200, "case {i}: name is {n} bytes");
        }
        assert!(paths[1].to_string_lossy().contains(" (1)"), "case {i}");
        assert!(paths[2].to_string_lossy().contains(" (2)"), "case {i}");
        for p in &paths {
            assert_eq!(sha_of_file(p), gen(300_000).sha256_hex());
        }
    }
}

/// Windows MAX_PATH (CI run 43576537725: `replace state: path not found` for a 200-byte name in a
/// deep temp dir). The engine must keep EVERY path it creates (final file, `.eid-part`,
/// `.eid-state`, `.eid-lock`, `.broken`, numbered copies) within `max_path_units`. Windows sets it
/// to 259; this test sets the same limit on Linux so the behaviour is exercised on every platform.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_created_path_stays_within_the_path_limit_and_an_impossible_folder_is_refused() {
    let units = |p: &std::path::Path| p.to_string_lossy().encode_utf16().count();
    let limit = 259usize;
    for (i, name) in [
        "a".repeat(200),
        format!("{}.zip", "b".repeat(196)),
        "ع".repeat(100),
        format!("{}.pdf", "ع".repeat(98)),
    ]
    .iter()
    .enumerate()
    {
        let enc = percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
        let b = Behavior {
            content_disposition: Some(format!("attachment; filename*=UTF-8''{enc}")),
            throttle_bps: Some(400_000),
            ..Behavior::default()
        };
        let (_srv, url) = serve("/m.bin", gen(1 << 20), b).await;
        // a folder about as deep as a CI temp dir
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("x".repeat(40));
        std::fs::create_dir(&dir).unwrap();
        let mut cfg = fast_cfg(1);
        cfg.max_path_units = Some(limit);
        // 1) interrupted job: all its sidecars exist at once
        let cancel = eid_engine::CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            c2.cancel();
        });
        let r = eid_engine::download(
            eid_engine::JobSpec {
                url: url.clone(),
                dir: dir.clone(),
                file_name: None,
            },
            cfg.clone(),
            Log::default().sink(),
            cancel,
        )
        .await;
        assert!(matches!(r, Err(Error::Cancelled)), "case {i}: {r:?}");
        for f in std::fs::read_dir(&dir).unwrap() {
            let p = f.unwrap().path();
            assert!(units(&p) <= limit, "case {i}: {} units: {p:?}", units(&p));
        }
        // 2) finish it, then two more copies: numbered names fit too
        let (_srv2, url2) = serve(
            "/m.bin",
            gen(300_000),
            Behavior {
                content_disposition: Some(format!("attachment; filename*=UTF-8''{enc}")),
                ..Behavior::default()
            },
        )
        .await;
        for k in 0..3 {
            let (o, _) = get(&url2, &dir, cfg.clone())
                .await
                .unwrap_or_else(|e| panic!("case {i} copy {k}: {e}"));
            assert!(units(&o.path) <= limit, "case {i} copy {k}: {:?}", o.path);
            assert_eq!(sha_of_file(&o.path), gen(300_000).sha256_hex());
        }
    }
    // a folder that leaves no room: a clear, early refusal (nothing created)
    let (_srv, url) = serve("/f.bin", gen(1000), Behavior::default()).await;
    let root = tempfile::tempdir().unwrap();
    let deep = root.path().join("d".repeat(230));
    std::fs::create_dir(&deep).unwrap();
    let mut cfg = fast_cfg(1);
    cfg.max_path_units = Some(limit);
    let r = get(&url, &deep, cfg).await;
    assert!(
        matches!(&r, Err(Error::InvalidInput(m)) if m.contains("too long")),
        "{:?}",
        r.as_ref().map(|_| ())
    );
    assert_eq!(only_files(&deep), Vec::<String>::new());
}
