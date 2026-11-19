//! PLAN P1 open item (c), owner decision 2026-11-19: when the LAST checkpoint of a run cannot be
//! saved, the user is warned (`Event::CheckpointFailed`), a complete and verified download is NOT
//! failed for that alone, and a cancel or an earlier error keeps its own result. Real downloads
//! against the test server; the save failure is injected at the real save call
//! (`fault::fail("final_checkpoint")`, test builds only), the other failure at the real disk
//! write (`write_data`). Own test binary: fault points are process-global, so ONE test function
//! runs the cases in order.
mod common;
use common::*;
use eid_engine::{fault, state, CancellationToken, Error, Event, JobSpec};
use eid_testserver::{Behavior, Content};
use std::path::Path;
use std::time::Duration;

fn content(seed: u64) -> Content {
    Content::Generated { seed, len: 8 << 20 }
}

fn state_file(dir: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".eid-state"))
}

async fn run(
    url: &str,
    dir: &Path,
    cfg: eid_engine::Config,
    cancel: CancellationToken,
) -> (eid_engine::Result<eid_engine::Outcome>, Log) {
    let log = Log::default();
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

fn warnings(log: &Log) -> usize {
    log.count(|e| matches!(e, Event::CheckpointFailed { .. }))
}

async fn wait_checkpoint(log: &Log, what: &str) {
    let t = std::time::Instant::now();
    while log.count(|e| matches!(e, Event::Checkpoint { done } if *done > 0)) == 0 {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "{what}: no checkpoint"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Resume after the failed final save: the older checkpoint is enough, the file is exact.
async fn resume_is_exact(dir: &Path, want: &Content, what: &str) {
    let sp =
        state_file(dir).unwrap_or_else(|| panic!("{what}: the previous checkpoint must remain"));
    let log = Log::default();
    let o = eid_engine::resume(&sp, fast_cfg(4), log.sink(), CancellationToken::new())
        .await
        .unwrap_or_else(|e| panic!("{what}: resume failed: {e}"));
    assert_eq!(sha_of_file(&o.path), want.sha256_hex(), "{what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_final_checkpoint_warns_and_never_changes_the_outcome() {
    // (1) complete download: still a success, file exact, one warning
    let (_s1, url) = serve("/ok.bin", content(1), Behavior::well_behaved()).await;
    let d1 = tempfile::tempdir().unwrap();
    fault::set_fail_point("final_checkpoint", 0, 1);
    let (r, log) = run(&url, d1.path(), fast_cfg(4), CancellationToken::new()).await;
    let o = r.expect("(1) a complete download must not fail because its last checkpoint failed");
    assert_eq!(sha_of_file(&o.path), content(1).sha256_hex(), "(1)");
    assert_eq!(warnings(&log), 1, "(1) the failure must be reported");
    let msg = log
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            Event::CheckpointFailed { message } => Some(message.clone()),
            _ => None,
        })
        .unwrap();
    assert!(msg.contains("final checkpoint"), "(1) {msg}");
    assert_eq!(only_files(d1.path()).len(), 1, "(1) no leftovers");

    // (2) cancel: stays Cancelled, warns, the previous checkpoint resumes exactly
    let slow = Behavior {
        throttle_bps: Some(1_000_000),
        ..Behavior::default()
    };
    let (_s2, url) = serve("/c.bin", content(2), slow.clone()).await;
    let d2 = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(4);
    cfg.checkpoint_interval = Duration::from_millis(100);
    let cancel = CancellationToken::new();
    let log2 = Log::default();
    let (u2, p2, c2, s2) = (
        url.clone(),
        d2.path().to_path_buf(),
        cancel.clone(),
        log2.sink(),
    );
    let task = tokio::spawn(async move {
        eid_engine::download(
            JobSpec {
                url: u2,
                dir: p2,
                file_name: None,
            },
            cfg,
            s2,
            c2,
        )
        .await
    });
    wait_checkpoint(&log2, "(2)").await;
    fault::set_fail_point("final_checkpoint", 0, 1);
    cancel.cancel();
    let r = task.await.unwrap();
    assert!(
        matches!(r, Err(Error::Cancelled)),
        "(2) the cancel must stay a cancel: {:?}",
        r.map(|o| o.path)
    );
    assert_eq!(warnings(&log2), 1, "(2)");
    resume_is_exact(d2.path(), &content(2), "(2)").await;

    // (3) an earlier error (a failed disk write) stays THE error, the save failure only warns
    let (_s3, url) = serve("/e.bin", content(3), slow).await;
    let d3 = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(4);
    cfg.checkpoint_interval = Duration::from_millis(100);
    let log3 = Log::default();
    let (u3, p3, s3) = (url.clone(), d3.path().to_path_buf(), log3.sink());
    let task = tokio::spawn(async move {
        eid_engine::download(
            JobSpec {
                url: u3,
                dir: p3,
                file_name: None,
            },
            cfg,
            s3,
            CancellationToken::new(),
        )
        .await
    });
    wait_checkpoint(&log3, "(3)").await;
    fault::set_fail_point("write_data", 0, 1);
    fault::add_fail_point("final_checkpoint", 0, 1);
    let r = tokio::time::timeout(Duration::from_secs(60), task)
        .await
        .expect("(3) timed out")
        .unwrap();
    let err = r.expect_err("(3) the write failure must fail the run");
    let text = err.to_string();
    assert!(
        text.contains("write_data") && !text.contains("final checkpoint"),
        "(3) the original error must be kept: {text}"
    );
    assert_eq!(warnings(&log3), 1, "(3)");
    // the state file on disk is a valid, older checkpoint
    let sp = state_file(d3.path()).expect("(3) state");
    state::peek(&sp).expect("(3) the remaining checkpoint is valid");
    fault::set_fail_point("none", 0, 0);
    resume_is_exact(d3.path(), &content(3), "(3)").await;

    // (4) through the queue (the desktop app's path): the item finishes Done and the warning
    //     reaches the runner's warning sink with the item's id
    use eid_engine::queue::{Queue, QueueSet, Status};
    use eid_engine::queue_runner::EngineRunner;
    use eid_engine::scheduler::{
        load_queues, save_queues, NoPower, Scheduler, SchedulerConfig, SystemClock,
    };
    use std::sync::{Arc, Mutex};
    let (_s4, url) = serve("/q.bin", content(4), Behavior::well_behaved()).await;
    let d4 = tempfile::tempdir().unwrap();
    let qpath = d4.path().join("queues.json");
    let mut qs = QueueSet::default();
    qs.queues
        .push(Queue::new("main", d4.path().join("dl").to_str().unwrap()));
    let id = qs.add_item("main", &url, None, None, None).unwrap();
    save_queues(&qpath, &qs).unwrap();
    let got: Arc<Mutex<Vec<(u64, String)>>> = Arc::default();
    let g2 = got.clone();
    let mut runner = EngineRunner::new(fast_cfg(4));
    runner.warnings = Some(Arc::new(move |item, text| {
        g2.lock().unwrap().push((item, text))
    }));
    fault::set_fail_point("final_checkpoint", 0, 1);
    let s = Scheduler::open(
        SchedulerConfig::new(&qpath),
        Arc::new(runner),
        Arc::new(SystemClock),
        Arc::new(NoPower),
        Arc::new(|_| {}),
    )
    .unwrap();
    s.run(CancellationToken::new(), true).await.unwrap();
    let done = load_queues(&qpath).unwrap();
    assert_eq!(done.queues[0].items[0].status, Status::Done, "(4)");
    let got = got.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "(4) {got:?}");
    assert_eq!(got[0].0, id, "(4)");
    assert!(got[0].1.contains("final checkpoint"), "(4) {got:?}");
}
