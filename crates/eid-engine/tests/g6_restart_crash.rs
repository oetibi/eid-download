//! Review B1: a restart after the server changed the file must never leave an OLD checkpoint
//! certifying bytes that the restart then destroyed. Own test binary: it uses the process-global
//! `fault::set_panic_point` (a simulated crash).
mod common;
use common::*;
use eid_engine::{fault, JobSpec};
use eid_testserver::{Behavior, ChangeAfter, Content, Cut};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_right_after_a_restart_truncation_never_corrupts_the_next_resume() {
    let a = Content::Generated {
        seed: 601,
        len: 8 << 20,
    };
    let b = Content::Generated {
        seed: 602,
        len: 8 << 20,
    };
    let beh = Behavior {
        throttle_bps: Some(3_000_000),
        // reconnects after the change are what makes the engine notice it (If-Range => 200)
        cut: Cut::EveryBytes(1 << 20),
        change_after: Some(ChangeAfter {
            requests: None,
            bytes_sent: Some(4 << 20),
            new_content: b.clone(),
        }),
        ..Behavior::default()
    };
    let (srv, url) = serve("/b1.bin", a.clone(), beh).await;
    let dir = tempfile::tempdir().unwrap();
    // 2nd hit: the first is the initial claim, the second is the restart after the change
    fault::set_panic_point("restart_truncated", 2);
    let (u, d) = (url.clone(), dir.path().to_path_buf());
    let crashed = tokio::spawn(async move {
        eid_engine::download(
            JobSpec {
                url: u,
                dir: d,
                file_name: None,
            },
            {
                let mut c = fast_cfg(4);
                c.max_attempts = 30;
                c
            },
            Log::default().sink(),
            eid_engine::CancellationToken::new(),
        )
        .await
    })
    .await;
    assert!(
        crashed.as_ref().is_err_and(|e| e.is_panic()),
        "the simulated crash at the restart point was never reached: {:?}",
        crashed.map(|r| r.map(|o| o.path))
    );
    // the origin serves the ORIGINAL representation again, with its original validator
    srv.add("/b1.bin", a.clone(), Behavior::default());
    let sp = dir.path().join("b1.bin.eid-state");
    assert!(sp.exists(), "no state left behind by the crashed job");
    let log = Log::default();
    let o = eid_engine::resume(
        &sp,
        fast_cfg(4),
        log.sink(),
        eid_engine::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        sha_of_file(&o.path),
        a.sha256_hex(),
        "resume after a crash during restart produced a corrupt file"
    );
}
