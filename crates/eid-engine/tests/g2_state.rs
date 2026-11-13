mod common;
use common::*;
use eid_engine::state::{self, State};
use eid_engine::{CancellationToken, Config, Error, Event, JobSpec};
use eid_testserver::{Behavior, Content};
use std::path::Path;
use std::time::Duration;

fn content(seed: u64) -> Content {
    Content::Generated {
        seed,
        len: 16 << 20,
    }
}
fn slow() -> Behavior {
    Behavior {
        throttle_bps: Some(2_000_000),
        ..Behavior::default()
    }
}

async fn run_then_cancel(
    url: &str,
    dir: &Path,
    cfg: Config,
    after: Duration,
) -> (eid_engine::Result<eid_engine::Outcome>, Log) {
    let log = Log::default();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        c2.cancel();
    });
    let r = eid_engine::download(
        JobSpec {
            url: url.into(),
            dir: dir.into(),
            file_name: None,
        },
        cfg,
        log.sink(),
        cancel,
    )
    .await;
    (r, log)
}

/// Cancel once a checkpoint with at least `min_done` bytes exists (progress-based, not
/// timing-based, so slow CI machines cannot make the test vacuous).
async fn run_until_progress(
    url: &str,
    dir: &Path,
    cfg: Config,
    min_done: u64,
) -> (eid_engine::Result<eid_engine::Outcome>, Log) {
    let log = Log::default();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let d = dir.to_path_buf();
    tokio::spawn(async move {
        let t = std::time::Instant::now();
        loop {
            if let Some(sp) = state_file(&d) {
                if let Ok(st) = state::load(&sp) {
                    let done: u64 = st.segments.iter().map(|s| s.next - s.start).sum();
                    if done >= min_done {
                        break;
                    }
                }
            }
            if t.elapsed() > Duration::from_secs(30) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        c2.cancel();
    });
    let r = eid_engine::download(
        JobSpec {
            url: url.into(),
            dir: dir.into(),
            file_name: None,
        },
        cfg,
        log.sink(),
        cancel,
    )
    .await;
    (r, log)
}

fn state_file(dir: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".eid-state"))
}

async fn resume(path: &Path, cfg: Config) -> eid_engine::Result<(eid_engine::Outcome, Log)> {
    let log = Log::default();
    let o = eid_engine::resume(path, cfg, log.sink(), CancellationToken::new()).await?;
    Ok((o, log))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_at_many_points_leaves_no_orphans() {
    for delay_ms in [0u64, 2, 8, 20, 50, 120, 300, 700] {
        let (_srv, url) = serve("/c.bin", content(1), slow()).await;
        let dir = tempfile::tempdir().unwrap();
        let (r, _) = run_then_cancel(
            &url,
            dir.path(),
            fast_cfg(4),
            Duration::from_millis(delay_ms),
        )
        .await;
        match r {
            Err(Error::Cancelled) => {}
            Ok(_) => continue, // finished before the cancel: fine
            Err(e) => panic!("delay {delay_ms}: unexpected {e}"),
        }
        let names = only_files(dir.path());
        for n in &names {
            assert!(
                matches!(
                    n.as_str(),
                    "c.bin.eid-part" | "c.bin.eid-state" | "c.bin.eid-state.bak"
                ),
                "delay {delay_ms}: orphan/unexpected file {n:?} in {names:?}"
            );
        }
        assert!(
            !names.contains(&"c.bin".to_string()),
            "no final file after a cancel"
        );
        let Some(sp) = state_file(dir.path()) else {
            assert!(
                names.is_empty(),
                "cancelled before claim => nothing on disk, got {names:?}"
            );
            continue;
        };
        let (o, _) = resume(&sp, fast_cfg(4))
            .await
            .unwrap_or_else(|e| panic!("delay {delay_ms}: resume {e}"));
        assert_eq!(
            sha_of_file(&o.path),
            content(1).sha256_hex(),
            "delay {delay_ms}"
        );
        assert_eq!(
            only_files(dir.path()),
            vec!["c.bin".to_string()],
            "cleanup after resume"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_then_resume_requests_only_from_checkpointed_frontier() {
    let (srv, url) = serve("/k.bin", content(2), slow()).await;
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = run_until_progress(&url, dir.path(), fast_cfg(4), 5 << 20).await;
    assert!(matches!(r, Err(Error::Cancelled)));
    let sp = state_file(dir.path()).unwrap();
    let st: State = state::load(&sp).unwrap();
    let done: u64 = st.segments.iter().map(|s| s.next - s.start).sum();
    assert!(
        done > 0 && done < 16 << 20,
        "partial progress expected, got {done}"
    );
    srv.reset_stats("/k.bin");
    srv.set_behavior("/k.bin", Behavior::default());
    let (o, _) = resume(&sp, fast_cfg(4)).await.unwrap();
    assert_eq!(sha_of_file(&o.path), content(2).sha256_hex());
    let reqs = srv.stats("/k.bin").requests;
    assert!(!reqs.is_empty());
    let starts: Vec<u64> = reqs
        .iter()
        .map(|r| {
            assert!(r.if_range.is_some(), "resume must send If-Range");
            r.range_header
                .as_ref()
                .unwrap()
                .trim_start_matches("bytes=")
                .split('-')
                .next()
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect();
    // (1) nothing below any segment's checkpointed frontier is ever re-requested
    for &start in &starts {
        let seg = st
            .segments
            .iter()
            .find(|s| s.start <= start && start < s.end)
            .expect("start inside a known segment");
        assert!(
            start >= seg.next,
            "re-requested checkpointed bytes: start {start} < frontier {} of {seg:?}",
            seg.next
        );
    }
    // (2) the first incomplete segment is resumed EXACTLY at its persisted frontier (the first
    //     worker always takes it; later splits only start above it)
    let first = st
        .segments
        .iter()
        .find(|s| s.next < s.end)
        .expect("an incomplete segment");
    let min_in_first = starts
        .iter()
        .copied()
        .filter(|&x| first.start <= x && x < first.end)
        .min()
        .expect("a request into the first incomplete segment");
    assert_eq!(
        min_in_first, first.next,
        "first resumed request must start at the persisted frontier"
    );
    // (3) every incomplete segment's frontier is the start of some request or lies inside a split
    //     sub-range that begins at/after it (never below)
    assert!(
        done >= 5 << 20,
        "cancel must happen after >=30% durable progress, done {done}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn etag_change_between_attempts_restarts_with_new_content() {
    let (srv, url) = serve("/e.bin", content(3), slow()).await;
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = run_until_progress(&url, dir.path(), fast_cfg(4), 2 << 20).await;
    assert!(matches!(r, Err(Error::Cancelled)));
    let sp = state_file(dir.path()).unwrap();
    srv.set_content("/e.bin", content(4)); // new bytes => new ETag
    srv.set_behavior("/e.bin", Behavior::default());
    let (o, log) = resume(&sp, fast_cfg(4)).await.unwrap();
    assert!(log.count(|e| matches!(e, Event::Restarted { .. })) >= 1);
    assert_eq!(
        sha_of_file(&o.path),
        content(4).sha256_hex(),
        "must be the new version, not a splice"
    );
    assert_eq!(only_files(dir.path()), vec!["e.bin".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_without_validators_restarts_instead_of_trusting() {
    let b = Behavior {
        etag: eid_testserver::EtagMode::None,
        throttle_bps: Some(2_000_000),
        ..Behavior::default()
    };
    let (srv, url) = serve("/nv.bin", content(5), b).await;
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = run_until_progress(&url, dir.path(), fast_cfg(4), 2 << 20).await;
    assert!(matches!(r, Err(Error::Cancelled)));
    let sp = state_file(dir.path()).unwrap();
    srv.set_behavior(
        "/nv.bin",
        Behavior {
            etag: eid_testserver::EtagMode::None,
            ..Behavior::default()
        },
    );
    let (o, log) = resume(&sp, fast_cfg(4)).await.unwrap();
    assert!(log.count(|e| matches!(e, Event::Restarted { .. })) == 1);
    assert_eq!(sha_of_file(&o.path), content(5).sha256_hex());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn state_roundtrip_through_a_real_cancelled_download() {
    let (_srv, url) = serve("/r.bin", content(6), slow()).await;
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = run_until_progress(&url, dir.path(), fast_cfg(4), 2 << 20).await;
    assert!(matches!(r, Err(Error::Cancelled)));
    let sp = state_file(dir.path()).unwrap();
    let st = state::load(&sp).unwrap();
    assert_eq!(st.file_name, "r.bin");
    assert_eq!(st.total, Some(16 << 20));
    assert!(st.ranged && st.etag.is_some());
    // exact coverage and frontier inside the segment
    let mut cursor = 0;
    for s in &st.segments {
        assert_eq!(s.start, cursor);
        assert!(s.start <= s.next && s.next <= s.end);
        cursor = s.end;
    }
    assert_eq!(cursor, 16 << 20);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn state_has_no_secrets_and_is_private() {
    let srv = eid_testserver::TestServer::start().await;
    srv.add("/sec.bin", content(7), slow());
    let port = srv.base_url().rsplit(':').next().unwrap().to_string();
    let url = format!("http://bob:HUNTER2pw@127.0.0.1:{port}/sec.bin");
    let dir = tempfile::tempdir().unwrap();
    let (r, log) = run_then_cancel(&url, dir.path(), fast_cfg(4), Duration::from_millis(500)).await;
    assert!(matches!(r, Err(Error::Cancelled)));
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let p = entry.unwrap().path();
        if p.to_string_lossy().ends_with(".eid-part") {
            continue; // payload bytes
        }
        let text = std::fs::read(&p).unwrap();
        let text = String::from_utf8_lossy(&text);
        assert!(
            !text.contains("HUNTER2pw") && !text.contains("bob"),
            "{p:?} leaks credentials"
        );
        assert_ne!(
            eid_platform::fs::owner_only(&p).unwrap(),
            Some(false),
            "{p:?} must be owner-only"
        );
    }
    let dbg = format!("{:?}", log.0.lock().unwrap());
    assert!(
        !dbg.contains("HUNTER2pw"),
        "events must not carry the password"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn errors_never_contain_query_or_credentials() {
    let srv = eid_testserver::TestServer::start().await;
    let port = srv.base_url().rsplit(':').next().unwrap().to_string();
    let dir = tempfile::tempdir().unwrap();
    // 404 on a URL with credentials and a secret token in the query
    let url = format!("http://u:PASSW0RD@127.0.0.1:{port}/missing.bin?token=SECRETTOKEN");
    let e = get(&url, dir.path(), fast_cfg(2))
        .await
        .err()
        .expect("must fail");
    let msg = format!("{e} / {e:?}");
    assert!(
        !msg.contains("SECRETTOKEN") && !msg.contains("PASSW0RD"),
        "{msg}"
    );
    // connection refused (nothing listening): reqwest embeds the URL in its error text
    let url = "http://u:PASSW0RD@127.0.0.1:1/x.bin?token=SECRETTOKEN";
    let mut cfg = fast_cfg(2);
    cfg.max_attempts = 2;
    let e = get(url, dir.path(), cfg).await.err().expect("must fail");
    let msg = format!("{e} / {e:?}");
    assert!(
        !msg.contains("SECRETTOKEN") && !msg.contains("PASSW0RD"),
        "{msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_state_files_are_rejected_before_any_io() {
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, b"keep me").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let base = |name: &str, url: &str| {
        let mut s = State {
            version: 1,
            url: url.into(),
            file_name: name.into(),
            total: Some(100),
            etag: Some("\"e\"".into()),
            last_modified: None,
            ranged: true,
            segments: vec![eid_engine::state::SegState {
                start: 0,
                next: 0,
                end: 100,
            }],
            restarts: 0,
            finalize: None,
            checksum: String::new(),
        };
        s.seal();
        s
    };
    let evil_names = [
        "../victim.txt".to_string(),
        format!("{}/victim.txt", outside.path().display()),
        "..\\..\\x".to_string(),
        "CON".to_string(),
        "a:stream".to_string(),
        "trailing.".to_string(),
        "".to_string(),
    ];
    for (i, n) in evil_names.iter().enumerate() {
        let sp = dir.path().join(format!("job{i}.eid-state"));
        let st = base(n, "http://127.0.0.1:1/x");
        // write it raw (bypassing save()'s own validation)
        std::fs::write(&sp, serde_json::to_vec(&st).unwrap()).unwrap();
        let r = eid_engine::resume(
            &sp,
            fast_cfg(1),
            Log::default().sink(),
            CancellationToken::new(),
        )
        .await;
        assert!(
            matches!(r, Err(Error::BadState(_))),
            "name {n:?}: {:?}",
            r.err()
        );
    }
    // credentials inside the stored URL are also refused
    let sp = dir.path().join("cred.eid-state");
    std::fs::write(
        &sp,
        serde_json::to_vec(&base("ok.bin", "http://u:p@127.0.0.1:1/x")).unwrap(),
    )
    .unwrap();
    let r = eid_engine::resume(
        &sp,
        fast_cfg(1),
        Log::default().sink(),
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(r, Err(Error::BadState(_))));
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
}
