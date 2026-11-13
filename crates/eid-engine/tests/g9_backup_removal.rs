//! Review B1 (critical): a restart replaces the state and deletes the old `.bak` BEFORE it
//! truncates the data. If the `.bak` cannot be deleted (antivirus holds it, Windows) the restart
//! must retry, and if it still fails it must STOP before truncating: a surviving `.bak` could
//! later be loaded as a valid older generation next to new-generation data. Own test binary:
//! uses the process-global `fault::set_fail_point` / `set_panic_point`, so ONE test function.
mod common;
use common::*;
use eid_engine::{fault, JobSpec};
use eid_testserver::{Behavior, ChangeAfter, Content, Cut};

async fn run(
    seed: u64,
    skip: u32,
    fail_times: u32,
) -> (
    Result<eid_engine::Outcome, eid_engine::Error>,
    tempfile::TempDir,
    Content,
    Content,
) {
    let a = Content::Generated { seed, len: 8 << 20 };
    let b = Content::Generated {
        seed: seed + 1,
        len: 8 << 20,
    };
    let beh = Behavior {
        throttle_bps: Some(3_000_000),
        cut: Cut::EveryBytes(1 << 20),
        change_after: Some(ChangeAfter {
            requests: None,
            bytes_sent: Some(4 << 20),
            new_content: b.clone(),
        }),
        ..Behavior::default()
    };
    let (_srv, url) = serve("/bk.bin", a.clone(), beh).await;
    let dir = tempfile::tempdir().unwrap();
    // `skip` = the initial claim's removal (and any earlier ones) succeed; the RESTART's fail
    fault::set_fail_point("remove_backup", skip, fail_times);
    let mut cfg = fast_cfg(4);
    cfg.max_attempts = 30;
    let r = eid_engine::download(
        JobSpec {
            url,
            dir: dir.path().to_path_buf(),
            file_name: None,
        },
        cfg,
        Log::default().sink(),
        eid_engine::CancellationToken::new(),
    )
    .await;
    (r, dir, a, b)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_retries_a_locked_bak_and_never_truncates_when_it_cannot_delete_it() {
    // (1) transient lock: 3 failures on the restart's removal are retried; the restart goes on
    // and the final file is the NEW representation, whole.
    let (r, _d, _a, b) = run(701, 1, 3).await;
    let o = r.expect("a transient .bak lock must be retried, not fail the job");
    assert_eq!(sha_of_file(&o.path), b.sha256_hex());

    // (2) permanent lock: the restart must fail BEFORE the truncation (the panic point would
    // fire at truncation and show up as a panic, not as an Err).
    fault::set_panic_point("restart_truncated", 2);
    let (r, d, _a, _b) = run(711, 1, 10_000).await;
    let err = r.expect_err("an undeletable .bak must stop the restart");
    let msg = err.to_string();
    assert!(msg.contains(".bak"), "unexpected error: {msg}");
    // the old data is intact (not truncated): the part file still holds the original bytes
    let part = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".eid-part"))
        .expect("the part file must still exist after the refused restart");
    assert!(
        std::fs::metadata(&part).unwrap().len() > 0,
        "the part file was truncated although the old .bak could not be deleted"
    );
}
