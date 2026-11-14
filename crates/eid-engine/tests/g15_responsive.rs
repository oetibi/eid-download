//! P1 review m2: filesystem set-up work (state write + fsync, truncate, preallocation) runs on the
//! blocking pool, so a deliberately BLOCKED filesystem operation must not stall the async
//! executor. The runtime here has exactly ONE thread: if the engine did the work inline, the
//! ticker below could not run while the operation is parked. One test (the pause hooks are
//! process-global).
mod common;
use common::*;
use eid_engine::{fault, JobSpec};
use eid_testserver::{Behavior, Content};
use std::time::{Duration, Instant};

#[tokio::test(flavor = "current_thread")]
async fn blocking_filesystem_work_does_not_stall_the_executor() {
    for point in ["setup_blocking", "resume_preallocate"] {
        let content = Content::Generated {
            seed: 77,
            len: 3 << 20,
        };
        let (_srv, url) = serve("/r.bin", content.clone(), Behavior::slow(2_000_000)).await;
        let dir = tempfile::tempdir().unwrap();
        let cancel = eid_engine::CancellationToken::new();

        // For "resume_preallocate" first leave a resumable state behind (run, then cancel).
        let state = dir.path().join("r.bin.eid-state");
        if point == "resume_preallocate" {
            let c2 = cancel.clone();
            let h = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(400)).await;
                c2.cancel();
            });
            let r = eid_engine::download(
                JobSpec {
                    url: url.clone(),
                    dir: dir.path().into(),
                    file_name: None,
                },
                fast_cfg(2),
                Log::default().sink(),
                cancel.clone(),
            )
            .await;
            assert!(matches!(r, Err(eid_engine::Error::Cancelled)), "{r:?}");
            h.await.unwrap();
            assert!(state.exists());
        }

        let (reached, release) = fault::set_pause_point(point);
        let (u, d, st) = (url.clone(), dir.path().to_path_buf(), state.clone());
        let job = tokio::spawn(async move {
            if point == "setup_blocking" {
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
            } else {
                eid_engine::resume(
                    &st,
                    fast_cfg(2),
                    Log::default().sink(),
                    eid_engine::CancellationToken::new(),
                )
                .await
            }
        });
        // wait (without blocking the only executor thread) until the disk operation is parked
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f2 = flag.clone();
        std::thread::spawn(move || {
            if reached.recv_timeout(Duration::from_secs(20)).is_ok() {
                f2.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let t = Instant::now();
        while !flag.load(std::sync::atomic::Ordering::SeqCst) {
            assert!(
                t.elapsed() < Duration::from_secs(20),
                "{point}: never reached"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // the operation is parked on its blocking thread: the executor must keep ticking
        let mut ticks = 0;
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(400) {
            tokio::time::sleep(Duration::from_millis(10)).await;
            ticks += 1;
        }
        assert!(
            ticks >= 20,
            "{point}: the executor was stalled by blocking disk work ({ticks} ticks in 400 ms)"
        );
        release.send(()).unwrap();
        let o = job
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("{point}: {e}"));
        assert_eq!(sha_of_file(&o.path), content.sha256_hex(), "{point}");
    }
}
