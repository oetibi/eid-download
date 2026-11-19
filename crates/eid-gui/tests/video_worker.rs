//! v0.2, owner's Windows test (2026-11-19): the window opened from inside the ZIP had no
//! `eid.exe` next to it, so "download the tools" and the video probe failed with a bare
//! "The system cannot find the file specified. (os error 2)". The app is now its own video
//! worker (`EidDownload --eid-video-worker video ...`). Here the REAL app binary is copied
//! ALONE into an empty folder (that situation) and the window's own video layer
//! (`VideoSetup` + `VideoJobs`, as `app.rs` builds them) agrees, installs the tools from the
//! local test server (SHA-256 checked), probes and downloads a video with the fake yt-dlp.
//! Plumbing only: not proof that real YouTube works (that is `real-video.yml`).
use eid_cli::media_jobs::{JobsConfig, Kind, VideoJobs};
use eid_cli::video_setup::VideoSetup;
use eid_testserver::{Behavior, Content, TestServer};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const APP: &str = env!("CARGO_BIN_EXE_EidDownload");

fn hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (n, b) in entries {
        w.start_file(*n, o).unwrap();
        w.write_all(b).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The app binary alone in a fresh folder: no eid.exe, no eid-native-host.exe.
fn lone_copy(dir: &Path) -> PathBuf {
    let p = dir.join(Path::new(APP).file_name().unwrap());
    std::fs::copy(APP, &p).unwrap();
    p
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "timed out: {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn job(v: &VideoJobs, id: u64) -> Value {
    v.list()
        .into_iter()
        .find(|j| j["job"] == id)
        .unwrap_or(Value::Null)
}

// one test: the setup status reads this process's environment (the per-user folders)
#[test]
fn the_app_alone_installs_the_tools_and_downloads_a_video_as_its_own_worker() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(300), body())
            .await
            .expect("hung for 300 s")
    });
    rt.shutdown_timeout(Duration::from_secs(20));
}

async fn body() {
    // ---- the test server with the tools, and a manifest pinning them
    let fake =
        Path::new(APP).with_file_name(format!("fake-yt-dlp{}", std::env::consts::EXE_SUFFIX));
    assert!(
        fake.is_file(),
        "fake-yt-dlp is not built (cargo test --workspace)"
    );
    let ytdlp = std::fs::read(&fake).unwrap();
    let (ffmpeg, ffprobe, deno) = (vec![1u8; 1000], vec![2u8; 1000], vec![3u8; 1000]);
    let ffzip = zip_of(&[("b/ffmpeg.exe", &ffmpeg), ("b/ffprobe.exe", &ffprobe)]);
    let dzip = zip_of(&[("deno.exe", &deno)]);
    let srv = TestServer::start().await;
    srv.add(
        "/yt-dlp.exe",
        Content::Bytes(ytdlp.clone()),
        Behavior::default(),
    );
    srv.add(
        "/ffmpeg.zip",
        Content::Bytes(ffzip.clone()),
        Behavior::default(),
    );
    srv.add(
        "/deno.zip",
        Content::Bytes(dzip.clone()),
        Behavior::default(),
    );
    let base = "https://github.com/test/tools/releases/download/v1";
    let file = |entry: Option<&str>, name: &str, b: &[u8]| json!({"entry": entry, "name": name, "size": b.len(), "sha256": hex(b)});
    let m = json!({
        "schema": 1, "id": "test-1", "allowed_hosts": ["github.com"],
        "tools": [
            {"name": "yt-dlp", "version": "fake", "license": "test", "source": "local",
             "url": format!("{base}/yt-dlp.exe"), "size": ytdlp.len(), "sha256": hex(&ytdlp),
             "files": [file(None, "yt-dlp.exe", &ytdlp)]},
            {"name": "ffmpeg", "version": "fake", "license": "test", "source": "local",
             "url": format!("{base}/ffmpeg.zip"), "size": ffzip.len(), "sha256": hex(&ffzip),
             "files": [file(Some("b/ffmpeg.exe"), "ffmpeg.exe", &ffmpeg),
                       file(Some("b/ffprobe.exe"), "ffprobe.exe", &ffprobe)]},
            {"name": "deno", "version": "fake", "license": "test", "source": "local",
             "url": format!("{base}/deno.zip"), "size": dzip.len(), "sha256": hex(&dzip),
             "files": [file(Some("deno.exe"), "deno.exe", &deno)]}
        ]
    });
    let root = tempfile::tempdir().unwrap();
    let manifest = root.path().join("manifest.json");
    std::fs::write(&manifest, m.to_string()).unwrap();
    for d in ["cfg", "data", "out"] {
        std::fs::create_dir(root.path().join(d)).unwrap();
    }
    let (cfg, data) = (root.path().join("cfg"), root.path().join("data"));
    let envs: Vec<(String, String)> = vec![
        ("APPDATA".into(), cfg.display().to_string()),
        ("LOCALAPPDATA".into(), data.display().to_string()),
        ("XDG_CONFIG_HOME".into(), cfg.display().to_string()),
        ("XDG_DATA_HOME".into(), data.display().to_string()),
        ("EID_TEST_MANIFEST".into(), manifest.display().to_string()),
        ("EID_TEST_TOOLS_BASE".into(), srv.base_url()),
    ];
    for (k, v) in &envs {
        // the only test in this binary; set before anything reads the environment
        std::env::set_var(k, v);
    }
    let tool_requests = || {
        ["/yt-dlp.exe", "/ffmpeg.zip", "/deno.zip"]
            .iter()
            .map(|p| srv.stats(p).requests.len())
            .sum::<usize>()
    };
    let log = root.path().join("calls.jsonl");
    let url = |path: &str, extra: &str| {
        format!(
            "https://fake.test/{path}?log={}{extra}",
            pct(&log.display().to_string())
        )
    };

    // ---- the app ALONE in a folder, as when Explorer runs it from inside the ZIP
    let alone = tempfile::tempdir().unwrap();
    let exe = lone_copy(alone.path());
    assert_eq!(std::fs::read_dir(alone.path()).unwrap().count(), 1);
    let config = |exe: PathBuf| JobsConfig {
        eid_exe: exe,
        worker_args: vec![eid_gui::worker::VIDEO_WORKER_FLAG.into()],
        state_file: cfg.join("video-jobs.json"),
        default_dir: root.path().join("out"),
        max_running: 1,
        env: envs.iter().map(|(k, v)| (k.into(), v.into())).collect(),
    };
    let v = VideoJobs::start(config(exe));
    let setup = VideoSetup::new(v.clone());

    // before the agreement: the worker starts and answers TOOLS_MISSING (was: os error 2)
    let err = v.probe(&url("video/abc", "")).await.unwrap_err();
    assert!(err.contains("TOOLS_MISSING"), "{err}");
    assert!(!err.contains("os error"), "{err}");
    assert_eq!(tool_requests(), 0);

    // "download the tools" from the window: through the app's own worker, hashes checked
    setup.install(true).unwrap();
    wait_for("tools installed", 120, || {
        let s = setup.status();
        s["installed"] == true && s["installing"] == false
    })
    .await;
    let s = setup.status();
    assert!(s["error"].is_null() || s["error"] == "", "{s}");
    assert_eq!(tool_requests(), 3, "each tool fetched once");

    // probe + a real video job, the file published in the chosen folder
    let p = v.probe(&url("video/abc", "&title=Clip")).await.unwrap();
    assert_eq!(p["kind"], "video", "{p}");
    let id = v
        .add(
            Kind::Video,
            &url("video/abc", "&title=Clip"),
            "best",
            None,
            None,
        )
        .unwrap();
    wait_for("video job", 120, || {
        let s = job(&v, id)["state"].clone();
        s == "done" || s == "failed"
    })
    .await;
    let j = job(&v, id);
    assert_eq!(j["state"], "done", "{j}");
    let files: Vec<String> = std::fs::read_dir(root.path().join("out"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        files
            .iter()
            .any(|f| f.ends_with(".mp4") && f.contains("Clip")),
        "{files:?}"
    );
    // still nothing but the app in its folder: no eid.exe was needed or created
    assert_eq!(std::fs::read_dir(alone.path()).unwrap().count(), 1);
    v.shutdown().await;

    // a worker that is really missing: the message names the file and says what to do
    let gone = VideoJobs::start(config(alone.path().join("missing-app.exe")));
    let err = gone.probe(&url("video/abc", "")).await.unwrap_err();
    assert!(
        err.contains("missing-app.exe") && err.contains("extract the whole ZIP"),
        "{err}"
    );
    gone.shutdown().await;
}

#[test]
fn the_worker_flag_only_runs_video_commands() {
    for args in [vec!["get", "http://h/x"], vec!["serve"], vec![]] {
        let all: Vec<&str> = std::iter::once(eid_gui::worker::VIDEO_WORKER_FLAG)
            .chain(args.iter().copied())
            .collect();
        let o = eid_platform::process::command(APP, &all).output().unwrap();
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("only runs `video"),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}
