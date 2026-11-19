//! v0.2 (real-window test on Windows, real-video.yml run 44631503045): "Install the tools"
//! closed the desktop app, because the window called `VideoSetup::install` from a plain Tauri
//! command (the window thread, outside the async runtime) and starting the worker panicked.
//! The command is async now; `install` itself also refuses, never panics, outside a runtime.
use eid_cli::media_jobs::{JobsConfig, VideoJobs};
use eid_cli::video_setup::VideoSetup;

#[test]
fn install_outside_the_runtime_is_an_error_not_a_crash() {
    let d = tempfile::tempdir().unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let jobs = rt.block_on(async {
        VideoJobs::start(JobsConfig {
            // never started: the call must fail before any process
            eid_exe: d.path().join("never-started.exe"),
            worker_args: vec![],
            state_file: d.path().join("video-jobs.json"),
            default_dir: d.path().to_path_buf(),
            max_running: 1,
            env: vec![],
        })
    });
    let setup = VideoSetup::new(jobs);
    // a plain thread: no runtime here, exactly like a non-async Tauri command
    let r = std::thread::spawn(move || setup.install(true)).join();
    let r = r.expect("install panicked outside the runtime (this closed the app)");
    let e = r.expect_err("install must refuse outside the runtime");
    assert!(e.contains("outside the app's runtime"), "{e}");
    rt.shutdown_timeout(std::time::Duration::from_secs(5));
}
