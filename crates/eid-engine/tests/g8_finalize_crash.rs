//! Review B2: a crash right after the final rename is recovered from the finalization record
//! (exact destination + SHA-256), including when the destination is a NUMBERED name because the
//! base name appeared during the transfer — and an unrelated same-size file at the base name is
//! never adopted. Own binary: process-global panic hook.
mod common;
use common::*;
use eid_engine::{fault, JobSpec};
use eid_testserver::{Behavior, Content};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_after_rename_to_a_numbered_name_is_recovered_by_content_without_the_network() {
    let content = Content::Generated {
        seed: 801,
        len: 2 << 20,
    };
    let slow = Behavior {
        throttle_bps: Some(3_000_000),
        ..Behavior::default()
    };
    let (srv, url) = serve("/f.bin", content.clone(), slow).await;
    let dir = tempfile::tempdir().unwrap();
    // a stranger of the SAME size appears at the base name while the transfer runs
    let stranger = vec![b'S'; 2 << 20];
    let sd = dir.path().to_path_buf();
    let st = stranger.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        std::fs::write(sd.join("f.bin"), &st).unwrap();
    });
    fault::set_panic_point("part_renamed_final", 1);
    let (u, d) = (url.clone(), dir.path().to_path_buf());
    let crashed = tokio::spawn(async move {
        eid_engine::download(
            JobSpec {
                url: u,
                dir: d,
                file_name: None,
            },
            fast_cfg(2),
            Log::default().sink(),
            eid_engine::CancellationToken::new(),
        )
        .await
    })
    .await;
    assert!(
        crashed.is_err_and(|e| e.is_panic()),
        "crash point not reached"
    );
    assert!(
        dir.path().join("f (1).bin").exists(),
        "rename to the numbered name did not happen"
    );
    assert_eq!(std::fs::read(dir.path().join("f.bin")).unwrap(), stranger);
    // the origin is gone: recovery must not need the network
    drop(srv);
    let sp = dir.path().join("f.bin.eid-state");
    let o = eid_engine::resume(
        &sp,
        fast_cfg(2),
        Log::default().sink(),
        eid_engine::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        o.path,
        dir.path().join("f (1).bin"),
        "recovered the wrong file"
    );
    assert_eq!(sha_of_file(&o.path), content.sha256_hex());
    assert_eq!(
        std::fs::read(dir.path().join("f.bin")).unwrap(),
        stranger,
        "stranger modified"
    );
    assert!(!sp.exists(), "state not cleaned up");
    assert!(
        !dir.path().join("f.bin.eid-lock").exists(),
        "lock file left behind"
    );
}
