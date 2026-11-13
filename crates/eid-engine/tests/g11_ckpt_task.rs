//! Coordinator checkpoint task (PR5): a parked checkpoint must not stall ramp/split,
//! and must be joined before any later state write (finalization, ServerChanged restart,
//! cancel). Own test binary: uses the process-global `fault::set_pause_point`. One test
//! function, sequential cases, no timing bounds on the properties (the 30 s waits are a safety
//! watchdog only).
mod common;
use common::*;
use eid_engine::{fault, Error, Event, JobSpec};
use eid_testserver::{Behavior, ChangeAfter, Content, Cut};
use futures_util::FutureExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const WATCHDOG: Duration = Duration::from_secs(30);

async fn recv_barrier(
    reached: std::sync::mpsc::Receiver<()>,
    what: &str,
) -> std::sync::mpsc::Receiver<()> {
    let (ok, reached) = tokio::task::spawn_blocking(move || {
        let ok = reached.recv_timeout(WATCHDOG).is_ok();
        (ok, reached)
    })
    .await
    .unwrap_or_else(|e| panic!("{what}: barrier task failed: {e}"));
    assert!(ok, "{what}: checkpoint barrier was never reached");
    reached
}

async fn wait_pred(what: &str, mut pred: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WATCHDOG;
    loop {
        if pred() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what} (safety watchdog, not a property bound)"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn peak_workers(log: &Log) -> usize {
    log.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            Event::Worker { active } => Some(*active),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn splits_after(log: &Log, before: usize) -> usize {
    log.count(|e| matches!(e, Event::Split { .. }))
        .saturating_sub(before)
}

fn state_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.eid-state"))
}

fn leftover_state(dir: &Path, name: &str) -> Vec<String> {
    let mut v = vec![];
    for suf in [".eid-state", ".eid-state.bak"] {
        let p = dir.join(format!("{name}{suf}"));
        if p.exists() {
            v.push(p.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    v
}

fn start_download(
    url: String,
    dir: PathBuf,
    cfg: eid_engine::Config,
    sink: eid_engine::EventSink,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<eid_engine::Result<eid_engine::Outcome>> {
    tokio::spawn(async move {
        eid_engine::download(
            JobSpec {
                url,
                dir,
                file_name: None,
            },
            cfg,
            sink,
            cancel,
        )
        .await
    })
}

/// (a) A parked checkpoint must not stall the coordinator: while it is parked we must
/// observe a Split and the ramp reaching the cap, and only then release it.
async fn parked_checkpoint_allows_split_and_ramp_to_cap() {
    let content = Content::Generated {
        seed: 1101,
        len: 8 << 20,
    };
    let (_srv, url) = serve(
        "/a.bin",
        content.clone(),
        Behavior {
            throttle_bps: Some(4_000_000),
            ..Behavior::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().expect("a: operation failed");
    let mut cfg = fast_cfg(8);
    cfg.checkpoint_interval = Duration::from_millis(5);
    cfg.ramp_interval = Duration::from_millis(5);
    let cap = cfg.connections;
    let (reached, release) = fault::set_pause_point("before_sync");
    let log = Log::default();
    let task = start_download(
        url,
        dir.path().to_path_buf(),
        cfg,
        log.sink(),
        CancellationToken::new(),
    );
    let _reached = recv_barrier(reached, "a").await;
    let splits_at_pause = log.count(|e| matches!(e, Event::Split { .. }));
    wait_pred(
        "a: Split and ramp to cap while the checkpoint is still parked",
        || splits_after(&log, splits_at_pause) >= 1 && peak_workers(&log) >= cap,
    )
    .await;
    release.send(()).expect("a: operation failed");
    let o = task
        .await
        .expect("a: operation failed")
        .expect("a: operation failed");
    assert_eq!(sha_of_file(&o.path), content.sha256_hex(), "a: sha");
}

/// (b) Park a checkpoint, let the download finish, then release: no state file may
/// remain, and the (final) state is only written after the release.
async fn parked_checkpoint_is_joined_before_remove_all() {
    let content = Content::Generated {
        seed: 1102,
        len: 4 << 20,
    };
    let (_srv, url) = serve("/b.bin", content.clone(), Behavior::default()).await;
    let dir = tempfile::tempdir().expect("b: operation failed");
    let mut cfg = fast_cfg(4);
    cfg.checkpoint_interval = Duration::from_millis(5);
    let (reached, release) = fault::set_pause_point("before_sync");
    let log = Log::default();
    let task = start_download(
        url,
        dir.path().to_path_buf(),
        cfg,
        log.sink(),
        CancellationToken::new(),
    );
    let _reached = recv_barrier(reached, "b").await;
    wait_pred(
        "b: transfer finished while the checkpoint is still parked",
        || log.peak_live_responses() > 0 && log.live_responses() == 0,
    )
    .await;
    // emit of Checkpoint happens after save, which is after the pause, so none yet
    assert_eq!(
        log.count(|e| matches!(e, Event::Checkpoint { .. })),
        0,
        "b: a checkpoint was persisted before release"
    );
    assert!(
        log.count(|e| matches!(e, Event::Finished { .. })) == 0,
        "b: Finished was emitted before the parked checkpoint was joined"
    );
    release.send(()).expect("b: operation failed");
    let o = task
        .await
        .expect("b: operation failed")
        .expect("b: operation failed");
    assert_eq!(sha_of_file(&o.path), content.sha256_hex(), "b: sha");
    assert!(
        log.count(|e| matches!(e, Event::Checkpoint { .. })) >= 1,
        "b: no checkpoint was written after release"
    );
    assert!(
        leftover_state(dir.path(), "b.bin").is_empty(),
        "b: state file remained after finish: {:?}",
        leftover_state(dir.path(), "b.bin")
    );
    let _ = o;
}

/// (c) B1 regression: a parked checkpoint of the old generation must be joined
/// before the restart writes the new-generation state, and must not overwrite it.
async fn parked_checkpoint_cannot_overwrite_new_generation_state() {
    let a = Content::Generated {
        seed: 1103,
        len: 6 << 20,
    };
    let b = Content::Generated {
        seed: 1104,
        len: 6 << 20,
    };
    let etag_a = "\"generated-1103-6291456\"".to_string();
    let etag_b = "\"generated-1104-6291456\"".to_string();
    let beh = Behavior {
        throttle_bps: Some(4_000_000),
        cut: Cut::EveryBytes(400_000),
        change_after: Some(ChangeAfter {
            requests: None,
            bytes_sent: Some(1 << 20),
            new_content: b.clone(),
        }),
        ..Behavior::default()
    };
    let (srv, url) = serve("/c.bin", a, beh).await;
    let dir = tempfile::tempdir().expect("c: operation failed");
    let mut cfg = fast_cfg(4);
    cfg.checkpoint_interval = Duration::from_millis(5);
    cfg.max_attempts = 80;
    let (reached, release) = fault::set_pause_point("before_sync");
    let log = Log::default();
    let task = start_download(
        url,
        dir.path().to_path_buf(),
        cfg,
        log.sink(),
        CancellationToken::new(),
    );
    let _reached = recv_barrier(reached, "c").await;
    wait_pred(
        "c: engine saw the new representation (If-Range -> 200)",
        || {
            srv.stats("/c.bin")
                .requests
                .iter()
                .any(|r| r.status == 200 && r.if_range.is_some())
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        log.count(|e| matches!(e, Event::Restarted { .. })),
        0,
        "c: Restarted before checkpoint join"
    );
    let sp = state_path(dir.path(), "c.bin");
    let st = eid_engine::state::load(&sp).expect("c: state should still be the old generation");
    assert_eq!(
        st.etag.as_deref(),
        Some(etag_a.as_str()),
        "c: new-generation state was written while an old checkpoint was still in flight (missing join)"
    );
    assert_eq!(
        st.restarts, 0,
        "c: restart began before the parked checkpoint was joined"
    );
    release.send(()).expect("c: operation failed");
    wait_pred("c: Restarted after the join", || {
        log.count(|e| matches!(e, Event::Restarted { .. })) >= 1
    })
    .await;
    let st = eid_engine::state::load(&sp).expect("c: state immediately after Restarted");
    assert_eq!(
        st.etag.as_deref(),
        Some(etag_b.as_str()),
        "c: Restarted preceded new state"
    );
    let job = task;
    let finished = loop {
        if job.is_finished() {
            break job.await.expect("c: operation failed");
        }
        if sp.exists() {
            if let Ok(st) = eid_engine::state::load(&sp) {
                assert_eq!(
                    st.etag.as_deref(),
                    Some(etag_b.as_str()),
                    "c: state after Restarted must remain B"
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let o = finished.expect("c: operation failed");
    assert_eq!(
        sha_of_file(&o.path),
        b.sha256_hex(),
        "c: sha of new content"
    );
    assert!(o.restarts >= 1, "c: expected a restart");
}

/// (d) Abort (cancel) while a checkpoint is parked, then resume: SHA is exact.
async fn cancel_while_checkpoint_parked_then_resume_is_exact() {
    let content = Content::Generated {
        seed: 1105,
        len: 6 << 20,
    };
    let (srv, url) = serve(
        "/d.bin",
        content.clone(),
        Behavior {
            throttle_bps: Some(3_000_000),
            ..Behavior::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().expect("d: operation failed");
    let mut cfg = fast_cfg(4);
    cfg.checkpoint_interval = Duration::from_millis(5);
    let (reached, release) = fault::set_pause_point("before_sync");
    let cancel = CancellationToken::new();
    let log = Log::default();
    let task = start_download(
        url,
        dir.path().to_path_buf(),
        cfg.clone(),
        log.sink(),
        cancel.clone(),
    );
    let _reached = recv_barrier(reached, "d").await;
    cancel.cancel();
    assert!(
        !task.is_finished(),
        "d: the job returned before the parked checkpoint was joined"
    );
    release.send(()).expect("d: operation failed");
    let r = task.await.expect("d: operation failed");
    assert!(
        matches!(r, Err(Error::Cancelled)),
        "d: expected Cancelled after join, got {:?}",
        r.map(|o| o.path)
    );
    srv.set_behavior("/d.bin", Behavior::default());
    let sp = state_path(dir.path(), "d.bin");
    assert!(sp.exists(), "d: final checkpoint after cancel is missing");
    let o = eid_engine::resume(&sp, cfg, Log::default().sink(), CancellationToken::new())
        .await
        .expect("d: operation failed");
    assert_eq!(
        sha_of_file(&o.path),
        content.sha256_hex(),
        "d: sha after resume"
    );
}

/// (e) Checkpoint failure must stop the job without spinning on the closed channel.
async fn checkpoint_error_has_bounded_drain() {
    let content = Content::Generated {
        seed: 1106,
        len: 6 << 20,
    };
    let (_srv, url) = serve(
        "/e.bin",
        content,
        Behavior {
            throttle_bps: Some(3_000_000),
            ..Behavior::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().expect("e: tempdir");
    let mut cfg = fast_cfg(4);
    cfg.checkpoint_interval = Duration::from_millis(5);
    let before = fault::drain_iterations();
    let (reached, release) = fault::set_pause_point("write_reserved");
    let task = start_download(
        url,
        dir.path().to_path_buf(),
        cfg,
        Log::default().sink(),
        CancellationToken::new(),
    );
    let _reached = recv_barrier(reached, "e").await;
    fault::set_fail_point("before_sync", 0, 1);
    wait_pred(
        "e: checkpoint error received while writer is parked",
        || fault::drain_iterations() > before,
    )
    .await;
    // Keep a worker in flight so a closed-channel spin cannot escape observation.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let held_iterations = fault::drain_iterations() - before;
    release.send(()).expect("e: release writer");
    let result = tokio::time::timeout(WATCHDOG, task)
        .await
        .expect("e: job timed out")
        .expect("e: job panicked");
    assert!(
        held_iterations < 100,
        "e: coordinator spun with writer parked: {held_iterations}"
    );
    let err = result.expect_err("e: checkpoint failure was ignored");
    assert!(
        err.to_string().contains("before_sync"),
        "e: wrong error: {err}"
    );
    let iterations = fault::drain_iterations() - before;
    assert!(
        iterations < 100,
        "e: coordinator spun during drain: {iterations}"
    );
}

/// (f) A failed new-generation state write must not announce Restarted.
async fn failed_restart_write_does_not_emit_restarted() {
    let content = Content::Generated {
        seed: 1107,
        len: 6 << 20,
    };
    let (_srv, url) = serve(
        "/f.bin",
        content,
        Behavior {
            cut: Cut::EveryBytes(400_000),
            change_after: Some(ChangeAfter {
                requests: None,
                bytes_sent: Some(1 << 20),
                new_content: Content::Generated {
                    seed: 1108,
                    len: 6 << 20,
                },
            }),
            ..Behavior::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().expect("f: tempdir");
    fault::set_fail_point("restart_state_write", 0, 1);
    let log = Log::default();
    let result = tokio::time::timeout(
        WATCHDOG,
        start_download(
            url,
            dir.path().to_path_buf(),
            fast_cfg(4),
            log.sink(),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("f: job timed out")
    .expect("f: job panicked");
    let err = result.expect_err("f: restart write failure was ignored");
    assert!(
        err.to_string().contains("restart_state_write"),
        "f: wrong error: {err}"
    );
    assert_eq!(
        log.count(|e| matches!(e, Event::Restarted { .. })),
        0,
        "f: Restarted emitted despite failed state write"
    );
}

async fn run_case(name: &str, case: impl std::future::Future<Output = ()>) {
    if let Err(payload) = std::panic::AssertUnwindSafe(case).catch_unwind().await {
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("non-string panic");
        panic!("{name}: {message}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checkpoint_task_does_not_block_ramp_and_is_joined_before_state_writes() {
    run_case(
        "a: parked_checkpoint_allows_split_and_ramp_to_cap",
        parked_checkpoint_allows_split_and_ramp_to_cap(),
    )
    .await;
    run_case(
        "b: parked_checkpoint_is_joined_before_remove_all",
        parked_checkpoint_is_joined_before_remove_all(),
    )
    .await;
    run_case(
        "c: parked_checkpoint_cannot_overwrite_new_generation_state",
        parked_checkpoint_cannot_overwrite_new_generation_state(),
    )
    .await;
    run_case(
        "d: cancel_while_checkpoint_parked_then_resume_is_exact",
        cancel_while_checkpoint_parked_then_resume_is_exact(),
    )
    .await;
    run_case(
        "e: checkpoint_error_has_bounded_drain",
        checkpoint_error_has_bounded_drain(),
    )
    .await;
    run_case(
        "f: failed_restart_write_does_not_emit_restarted",
        failed_restart_write_does_not_emit_restarted(),
    )
    .await;
}
