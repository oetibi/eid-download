//! Review M4: abort the caller's task while a blocking disk operation is in flight (parked at a
//! fault barrier). The job must keep exclusive ownership until that operation drained, so an
//! IMMEDIATE resume is refused (`Busy`) instead of racing the old writer; after the drain a
//! resume yields the exact bytes. One test, three barriers (process-global fault hooks).
mod common;
use common::*;
use eid_engine::{fault, Error, JobSpec};
use eid_testserver::{Behavior, Content};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abort_at_first_save_write_and_checkpoint_barriers_then_immediate_resume() {
    for point in ["tmp_written", "write_reserved", "before_sync"] {
        let content = Content::Generated {
            seed: 701,
            len: 6 << 20,
        };
        let slow = Behavior {
            throttle_bps: Some(3_000_000),
            ..Behavior::default()
        };
        let (srv, url) = serve("/m4.bin", content.clone(), slow).await;
        let dir = tempfile::tempdir().unwrap();
        let (reached, release) = fault::set_pause_point(point);
        let (u, d) = (url.clone(), dir.path().to_path_buf());
        let task = tokio::spawn(async move {
            eid_engine::download(
                JobSpec {
                    url: u,
                    dir: d,
                    file_name: None,
                },
                fast_cfg(4),
                Log::default().sink(),
                eid_engine::CancellationToken::new(),
            )
            .await
        });
        let r = tokio::task::spawn_blocking(move || {
            let ok = reached.recv_timeout(Duration::from_secs(20)).is_ok();
            (ok, reached)
        })
        .await
        .unwrap();
        assert!(r.0, "{point}: barrier never reached");
        // the caller goes away while the blocking disk operation is parked
        task.abort();
        let _ = task.await;
        let sp = dir.path().join("m4.bin.eid-state");
        let imm = eid_engine::resume(
            &sp,
            fast_cfg(4),
            Log::default().sink(),
            eid_engine::CancellationToken::new(),
        )
        .await;
        assert!(
            matches!(imm, Err(Error::Busy)),
            "{point}: an immediate resume raced the old job: {:?}",
            imm.map(|o| o.path)
        );
        release.send(()).unwrap();
        srv.set_behavior("/m4.bin", Behavior::default());
        let mut tries = 0;
        let o = loop {
            match eid_engine::resume(
                &sp,
                fast_cfg(4),
                Log::default().sink(),
                eid_engine::CancellationToken::new(),
            )
            .await
            {
                Ok(o) => break o,
                Err(Error::Busy) if tries < 200 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => panic!("{point}: resume after the drain failed: {e}"),
            }
        };
        assert_eq!(sha_of_file(&o.path), content.sha256_hex(), "{point}");
        eprintln!("{point}: ok (busy retries after release: {tries})");
    }
}
