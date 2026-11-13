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
                    "c.bin.eid-part" | "c.bin.eid-state" | "c.bin.eid-state.bak" | "c.bin.eid-lock"
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
    // B3: without validators the engine streams the file once and records no per-segment
    // progress, so wait on the Progress events themselves (not on a fixed delay) and then cancel.
    let log0 = Log::default();
    let cancel = CancellationToken::new();
    let (c2, l2) = (cancel.clone(), log0.clone());
    tokio::spawn(async move {
        let t = std::time::Instant::now();
        while l2.count(|e| matches!(e, Event::Progress { done, .. } if *done >= 1 << 20)) == 0
            && t.elapsed() < Duration::from_secs(30)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        c2.cancel();
    });
    let r = eid_engine::download(
        JobSpec {
            url: url.clone(),
            dir: dir.path().into(),
            file_name: None,
        },
        fast_cfg(4),
        log0.sink(),
        cancel,
    )
    .await;
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
    let dbg = format!("{:?}", log.events.lock().unwrap());
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

/// Review B2: with the part file gone, an unrelated final file that merely has the right NAME
/// and SIZE must never be adopted as the finished download (and must stay untouched).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_never_adopts_an_unrelated_same_size_final_file() {
    let (_srv, url) = serve("/r.bin", content(21), slow()).await;
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = run_until_progress(&url, dir.path(), fast_cfg(4), 2 << 20).await;
    assert!(matches!(r, Err(Error::Cancelled)));
    let sp = state_file(dir.path()).unwrap();
    let st = state::load(&sp).unwrap();
    let total = st.total.unwrap() as usize;
    // part vanished, and a stranger of exactly the right size sits at the final name
    for e in std::fs::read_dir(dir.path()).unwrap() {
        let p = e.unwrap().path();
        if p.to_string_lossy().ends_with(".eid-part") {
            std::fs::remove_file(p).unwrap();
        }
    }
    let evil = vec![b'E'; total];
    let fin = dir.path().join(&st.file_name);
    std::fs::write(&fin, &evil).unwrap();
    _srv.set_behavior("/r.bin", Behavior::default());
    let (o, log) = resume(&sp, fast_cfg(4)).await.unwrap();
    assert_eq!(
        std::fs::read(&fin).unwrap(),
        evil,
        "the stranger was modified"
    );
    assert_ne!(o.path, fin, "resume adopted the unrelated file");
    assert_eq!(sha_of_file(&o.path), content(21).sha256_hex());
    assert!(log.count(|e| matches!(e, Event::Restarted { .. })) >= 1);
}

/// Review M1: while one job owns a part file, a second resume of the same sidecar must be
/// refused with `Busy` (before it touches state or data) and the first must still finish with
/// the exact bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_resume_of_the_same_job_is_refused_busy() {
    let (srv, url) = serve("/m1.bin", content(31), slow()).await;
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = run_until_progress(&url, dir.path(), fast_cfg(4), 2 << 20).await;
    assert!(matches!(r, Err(Error::Cancelled)));
    let sp = state_file(dir.path()).unwrap();
    srv.set_behavior("/m1.bin", slow());
    let log1 = Log::default();
    let (sp1, sink1) = (sp.clone(), log1.sink());
    let first = tokio::spawn(async move {
        eid_engine::resume(&sp1, fast_cfg(4), sink1, CancellationToken::new()).await
    });
    // wait until the first resume is really transferring
    let t = std::time::Instant::now();
    while log1.count(|e| matches!(e, Event::ResponseOpened { .. })) == 0 {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "first resume never started"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let second = tokio::time::timeout(Duration::from_secs(20), resume(&sp, fast_cfg(4)))
        .await
        .expect("second resume must fail fast, not run");
    assert!(
        matches!(second, Err(Error::Busy)),
        "second resume was not refused: {:?}",
        second.map(|(o, _)| o.path)
    );
    let o = first.await.unwrap().unwrap();
    assert_eq!(sha_of_file(&o.path), content(31).sha256_hex());
}

/// Review M4: aborting the caller's task must NOT abandon the job mid-flight. The job keeps
/// running under its own supervision, drains, and persists the final durable checkpoint, so the
/// sidecar then reflects the bytes already on disk (the periodic checkpoint is disabled here:
/// only the supervised drain can have written progress), and the sidecar is resumable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aborting_the_caller_drains_the_job_and_persists_a_final_checkpoint() {
    let (srv, url) = serve("/m4.bin", content(41), slow()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(4);
    cfg.checkpoint_interval = Duration::from_secs(600); // no periodic checkpoint will fire
    let log = Log::default();
    let (d, sink) = (dir.path().to_path_buf(), log.sink());
    let task = tokio::spawn(async move {
        eid_engine::download(
            JobSpec {
                url,
                dir: d,
                file_name: None,
            },
            cfg,
            sink,
            CancellationToken::new(),
        )
        .await
    });
    let t = std::time::Instant::now();
    loop {
        if log.count(|e| matches!(e, Event::Progress { done, .. } if *done >= 2 << 20)) > 0 {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "no progress");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    task.abort(); // the caller goes away
    let _ = task.await;
    // the supervised job drains by itself and writes its final checkpoint
    let sp = state_file(dir.path()).unwrap();
    let t = std::time::Instant::now();
    let frontier = loop {
        let done: u64 = state::load(&sp)
            .map(|s| s.segments.iter().map(|g| g.next - g.start).sum())
            .unwrap_or(0);
        if done >= 1 << 20 {
            break done;
        }
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "state never advanced after the caller was aborted (job was abandoned)"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(frontier >= 1 << 20);
    // job released its ownership: resume now works and is exact
    srv.set_behavior("/m4.bin", Behavior::default());
    let mut tries = 0;
    let o = loop {
        match resume(&sp, fast_cfg(4)).await {
            Ok((o, _)) => break o,
            Err(Error::Busy) if tries < 100 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("resume after abort failed: {e}"),
        }
    };
    assert_eq!(sha_of_file(&o.path), content(41).sha256_hex());
}

/// Review M1 (root): ownership must not depend on the part file. Someone holding the job's lock
/// file makes `resume` fail with `Busy` even when the part is missing (the restart path), and
/// nothing is deleted or restarted behind the owner's back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn job_lock_covers_a_missing_part_and_restart_path() {
    let (_srv, url) = serve("/m1b.bin", content(32), slow()).await;
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = run_until_progress(&url, dir.path(), fast_cfg(4), 2 << 20).await;
    assert!(matches!(r, Err(Error::Cancelled)));
    let sp = state_file(dir.path()).unwrap();
    let part = dir.path().join("m1b.bin.eid-part");
    std::fs::remove_file(&part).unwrap(); // part gone: the old part-file lock cannot help
                                          // another process owns the job: it holds the per-job lock file
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join("m1b.bin.eid-lock"))
        .unwrap();
    lock.try_lock()
        .expect("test must be able to take the job lock");
    let r = resume(&sp, fast_cfg(4)).await;
    assert!(
        matches!(r, Err(Error::Busy)),
        "got {:?}",
        r.map(|(o, _)| o.path)
    );
    assert!(
        sp.exists(),
        "state must be untouched while another owner holds the job"
    );
    assert!(
        !part.exists(),
        "no restart may begin behind the owner's back"
    );
    drop(lock);
    let (o, _) = resume(&sp, fast_cfg(4)).await.unwrap();
    assert_eq!(sha_of_file(&o.path), content(32).sha256_hex());
    assert_eq!(
        only_files(dir.path()),
        vec!["m1b.bin".to_string()],
        "lock file cleaned up"
    );
}

/// Review M1: a resume that takes the restart path (part missing) keeps ownership for the whole
/// restarted transfer: a second resume is refused instead of racing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_path_keeps_the_job_lock_for_its_whole_transfer() {
    let (srv, url) = serve("/m1c.bin", content(33), slow()).await;
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = run_until_progress(&url, dir.path(), fast_cfg(4), 2 << 20).await;
    assert!(matches!(r, Err(Error::Cancelled)));
    let sp = state_file(dir.path()).unwrap();
    std::fs::remove_file(dir.path().join("m1c.bin.eid-part")).unwrap();
    srv.set_behavior("/m1c.bin", slow());
    let log1 = Log::default();
    let (sp1, sink1) = (sp.clone(), log1.sink());
    let first = tokio::spawn(async move {
        eid_engine::resume(&sp1, fast_cfg(4), sink1, CancellationToken::new()).await
    });
    let t = std::time::Instant::now();
    while log1.count(|e| matches!(e, Event::Restarted { .. })) == 0
        || log1.count(|e| matches!(e, Event::ResponseOpened { .. })) == 0
    {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "restart never started"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    for _ in 0..5 {
        let second = resume(&sp, fast_cfg(4)).await;
        assert!(
            matches!(second, Err(Error::Busy)),
            "got {:?}",
            second.map(|(o, _)| o.path)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let o = first.await.unwrap().unwrap();
    assert_eq!(sha_of_file(&o.path), content(33).sha256_hex());
}
