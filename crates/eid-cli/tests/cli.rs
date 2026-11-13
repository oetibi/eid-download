//! End-to-end tests of the real `eid` binary (built with the `test-faults` feature).
use eid_engine::state;
use eid_platform::process::command;
use eid_testserver::{Behavior, Content, TestServer};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const EID: &str = env!("CARGO_BIN_EXE_eid");

fn sha(p: &Path) -> String {
    state::hex(&Sha256::digest(std::fs::read(p).unwrap()))
}
fn content(seed: u64) -> Content {
    Content::Generated {
        seed,
        len: 12 << 20,
    }
}
fn slow() -> Behavior {
    Behavior {
        throttle_bps: Some(2_500_000),
        ..Behavior::default()
    }
}
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}
fn state_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".eid-state"))
}
fn fast_args() -> Vec<&'static str> {
    vec![
        "--ramp-ms",
        "5",
        "--checkpoint-ms",
        "50",
        "--min-split",
        "65536",
        "-q",
    ]
}

async fn run(args: Vec<String>, envs: &[(&str, &str)]) -> std::process::Output {
    let envs: Vec<(String, String)> = envs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    tokio::task::spawn_blocking(move || {
        let mut c = command(EID, args);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    })
    .await
    .unwrap()
}

fn get_args(url: &str, dir: &Path, extra: &[&str]) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "get".into(),
        url.into(),
        "-d".into(),
        dir.to_string_lossy().into_owned(),
    ];
    a.extend(fast_args().into_iter().map(String::from));
    a.extend(extra.iter().map(|s| s.to_string()));
    a
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_get_downloads_and_verifies_sha256() {
    let srv = TestServer::start().await;
    srv.add("/a.bin", content(1), Behavior::default());
    let url = format!("{}/a.bin", srv.base_url());
    let dir = tempfile::tempdir().unwrap();
    let want = content(1).sha256_hex();
    let out = run(
        get_args(&url, dir.path(), &["-c", "8", "--sha256", &want]),
        &[],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8(out.stdout).unwrap();
    assert_eq!(printed.trim(), dir.path().join("a.bin").to_string_lossy());
    assert_eq!(sha(&dir.path().join("a.bin")), want);
    // wrong checksum => failure exit code and no final file
    let dir2 = tempfile::tempdir().unwrap();
    let out = run(
        get_args(&url, dir2.path(), &["--sha256", &"0".repeat(64)]),
        &[],
    )
    .await;
    assert_eq!(out.status.code(), Some(1));
    assert!(!dir2.path().join("a.bin").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_never_prints_credentials_or_query() {
    let srv = TestServer::start().await;
    srv.add("/s.bin", content(1), Behavior::default());
    let port = srv.base_url().rsplit(':').next().unwrap().to_string();
    let dir = tempfile::tempdir().unwrap();
    // failing download (404) with secrets in the URL: error output must not echo them
    let url = format!("http://zed:TOPSECRETPW@127.0.0.1:{port}/nope.bin?sig=TOPSECRETSIG");
    let mut args = get_args(&url, dir.path(), &["--attempts", "2"]);
    args.retain(|a| a != "-q");
    let out = run(args, &[]).await;
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
    assert!(
        !all.contains("TOPSECRETPW") && !all.contains("TOPSECRETSIG"),
        "{all}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_sigkill_then_resume_completes_from_frontier() {
    let srv = TestServer::start().await;
    srv.add("/k.bin", content(2), slow());
    let url = format!("{}/k.bin", srv.base_url());
    let dir = tempfile::tempdir().unwrap();
    let mut child = command(EID, get_args(&url, dir.path(), &["-c", "4"]))
        .spawn()
        .unwrap();
    // wait until a checkpoint with real progress exists, then kill -9
    let t = Instant::now();
    let st = loop {
        if let Some(sp) = state_file(dir.path()) {
            if let Ok(st) = state::load(&sp) {
                let done: u64 = st.segments.iter().map(|s| s.next - s.start).sum();
                if done > (2 << 20) {
                    break st;
                }
            }
        }
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "no checkpoint appeared"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success());
    // re-read after the kill: that is the state a restart will see
    let sp = state_file(dir.path()).unwrap();
    let st2 = state::load(&sp).unwrap_or(st);
    let done: u64 = st2.segments.iter().map(|s| s.next - s.start).sum();
    srv.reset_stats("/k.bin");
    srv.set_behavior("/k.bin", Behavior::default());
    let out = run(
        vec![
            "resume".into(),
            sp.to_string_lossy().into_owned(),
            "-q".into(),
            "-c".into(),
            "4".into(),
        ],
        &[],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(sha(&dir.path().join("k.bin")), content(2).sha256_hex());
    assert_eq!(names(dir.path()), vec!["k.bin".to_string()]);
    for r in srv.stats("/k.bin").requests {
        let start: u64 = r
            .range_header
            .unwrap()
            .trim_start_matches("bytes=")
            .split('-')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let seg = st2
            .segments
            .iter()
            .find(|s| s.start <= start && start < s.end)
            .unwrap();
        assert!(
            start >= seg.next,
            "re-requested checkpointed bytes: {start} < {}",
            seg.next
        );
    }
    assert!(srv.stats("/k.bin").bytes_sent_total <= (12 << 20) - done + (1 << 20));
}

/// Abort the process (like power loss) at each persistence point; a restart must always end
/// with the exact bytes, and never leave junk behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_at_every_persistence_fault_point_recovers() {
    let cases: &[(&str, &str)] = &[
        ("tmp_written", "2"),
        ("tmp_written", "4"),
        ("dest_moved_to_bak", "1"),
        ("dest_moved_to_bak", "3"),
        ("state_renamed", "1"),
        ("state_renamed", "3"),
        ("before_sync", "1"),
        ("before_sync", "3"),
        ("after_sync", "1"),
        ("after_sync", "3"),
        ("final_before_rename", "1"),
        ("part_renamed_final", "1"),
    ];
    for (point, after) in cases {
        let srv = TestServer::start().await;
        srv.add(
            "/f.bin",
            content(9),
            Behavior {
                throttle_bps: Some(6_000_000),
                ..Behavior::default()
            },
        );
        let url = format!("{}/f.bin", srv.base_url());
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            get_args(&url, dir.path(), &["-c", "4"]),
            &[("EID_FAULT", point), ("EID_FAULT_AFTER", after)],
        )
        .await;
        assert!(
            !out.status.success(),
            "{point}#{after}: fault point was never reached (process finished)"
        );
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(
                out.status.signal(),
                Some(6),
                "{point}#{after}: expected abort(), got {:?}",
                out.status
            );
        }
        let sp = state_file(dir.path()).or_else(|| {
            // primary may be mid-replace: only .bak exists
            let bak = dir.path().join("f.bin.eid-state.bak");
            bak.exists().then(|| dir.path().join("f.bin.eid-state"))
        });
        srv.reset_stats("/f.bin");
        srv.set_behavior("/f.bin", Behavior::default());
        let out = match sp {
            Some(sp) => {
                run(
                    vec![
                        "resume".into(),
                        sp.to_string_lossy().into_owned(),
                        "-q".into(),
                    ],
                    &[],
                )
                .await
            }
            None => panic!(
                "{point}#{after}: no state to resume from: {:?}",
                names(dir.path())
            ),
        };
        assert!(
            out.status.success(),
            "{point}#{after}: resume failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            sha(&dir.path().join("f.bin")),
            content(9).sha256_hex(),
            "{point}#{after}"
        );
        let left = names(dir.path());
        assert_eq!(
            left,
            vec!["f.bin".to_string()],
            "{point}#{after}: junk left behind"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_ctrl_c_prints_a_resume_command_and_it_works() {
    let srv = TestServer::start().await;
    srv.add("/c.bin", content(5), slow());
    let url = format!("{}/c.bin", srv.base_url());
    let dir = tempfile::tempdir().unwrap();
    let mut args = get_args(&url, dir.path(), &["-c", "2", "--limit", "2M"]);
    args.retain(|a| a != "-q");
    let child = command(EID, args)
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    // SIGINT == Ctrl-C (sent through the kill utility; unix only)
    #[cfg(unix)]
    {
        assert!(command("kill", ["-INT", &pid.to_string()])
            .status()
            .unwrap()
            .success());
        let out = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
            .await
            .unwrap();
        assert_eq!(out.status.code(), Some(130));
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            err.contains("To resume, run:") && err.contains("eid resume '"),
            "{err}"
        );
        let sp = state_file(dir.path()).unwrap();
        srv.set_behavior("/c.bin", Behavior::default());
        let out = run(
            vec![
                "resume".into(),
                sp.to_string_lossy().into_owned(),
                "-q".into(),
            ],
            &[],
        )
        .await;
        assert!(out.status.success());
        assert_eq!(sha(&dir.path().join("c.bin")), content(5).sha256_hex());
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, child);
    }
}

#[test]
fn cli_rejects_bad_arguments() {
    for bad in [
        vec!["get"],
        vec!["get", "u", "-c", "99"],
        vec!["nope"],
        vec!["get", "u", "--sha256", "zz"],
        vec!["get", "u", "--limit", "0"],
    ] {
        let out = command(EID, bad.iter().copied()).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{bad:?}");
    }
    let out = command(EID, ["--version"]).output().unwrap();
    assert!(out.status.success());
}
