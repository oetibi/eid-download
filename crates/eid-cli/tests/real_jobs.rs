//! v0.1 REAL test (owner: "mocks are not proof"): the desktop app's video job layer driving the
//! real `eid` with the real pinned yt-dlp / FFmpeg / Deno (Windows assets) against real YouTube
//! Creative Commons content. `#[ignore]`d: real internet, ~230 MB of tools, a few minutes. Run
//! on purpose only, on Windows (the workflow `real-video` does it on a GitHub Windows runner):
//!
//!   cargo test -p eid-cli --test real_jobs -- --ignored --nocapture --test-threads=1
//!
//! `EID_REAL_HOME` (absolute) keeps the per-user folders (and so the installed tools) between
//! runs. `EID_REAL_VIDEO` / `EID_REAL_PLAYLIST` / `EID_REAL_ITEMS` override the content.
#![cfg(windows)]
use eid_cli::media_jobs::{JobsConfig, Kind, VideoJobs};
use eid_cli::video_setup::VideoSetup;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// "Big Buck Bunny" (Blender Foundation, CC BY 3.0), Blender's own upload.
const CC_VIDEO: &str = "https://www.youtube.com/watch?v=aqz-KE-bpKQ";
/// "Blender Open Movies" (Blender Studio's official playlist; CC BY films).
const CC_PLAYLIST: &str =
    "https://www.youtube.com/playlist?list=PLav47HAVZMjnTFVZL-aImCQIC0uLZtNCz";

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "timed out after {secs}s waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("  {what}: {:.1}s", t0.elapsed().as_secs_f64());
}

fn job(v: &VideoJobs, id: u64) -> Value {
    v.list()
        .into_iter()
        .find(|j| j["job"] == id)
        .unwrap_or(Value::Null)
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            return Some(p);
        }
    }
    None
}

/// `(video height, has audio)` of a file by the pinned ffprobe.
fn probe_file(ffprobe: &Path, f: &Path) -> (Option<u64>, bool) {
    let o = std::process::Command::new(ffprobe)
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(f)
        .output()
        .unwrap();
    assert!(o.status.success(), "ffprobe failed on {}", f.display());
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let h = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .and_then(|s| s["height"].as_u64());
    let a = streams.iter().any(|s| s["codec_type"] == "audio");
    (h, a)
}

fn files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.retain(|p| p.extension().is_some_and(|e| e == ext));
    v.sort();
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real internet + real YouTube (owner-approved real test, Windows)"]
async fn real_youtube_video_audio_and_playlist_jobs() {
    let keep = tempfile::tempdir().unwrap();
    let home = std::env::var_os("EID_REAL_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| keep.path().to_path_buf());
    let (cfg_dir, data_dir, out) = (home.join("cfg"), home.join("data"), keep.path().join("out"));
    for d in [&cfg_dir, &data_dir, &out] {
        std::fs::create_dir_all(d).unwrap();
    }
    // the only test in this binary: the status functions read this process's environment
    std::env::set_var("APPDATA", &cfg_dir);
    std::env::set_var("LOCALAPPDATA", &data_dir);
    let env: Vec<(std::ffi::OsString, std::ffi::OsString)> = vec![
        ("APPDATA".into(), cfg_dir.clone().into()),
        ("LOCALAPPDATA".into(), data_dir.clone().into()),
    ];
    let v = VideoJobs::start(JobsConfig {
        eid_exe: env!("CARGO_BIN_EXE_eid").into(),
        state_file: cfg_dir.join("video-jobs.json"),
        default_dir: out.clone(),
        max_running: 1,
        env,
    });
    let setup = VideoSetup::new(v.clone());
    println!("== setup (agreement + pinned tools)");
    if setup.status()["installed"] != true {
        setup.install(true).unwrap();
        wait_for("tools installed", 1200, || {
            let s = setup.status();
            assert!(s["error"].is_null(), "setup failed: {s}");
            s["installed"] == true && s["installing"] == false
        })
        .await;
    }
    let ffprobe = find(&data_dir, "ffprobe.exe").expect("ffprobe.exe in the tools");

    println!("== probe video");
    let video = env_or("EID_REAL_VIDEO", CC_VIDEO);
    let t = Instant::now();
    let p = v.probe(&video).await.expect("probe video");
    println!("  {:.1}s {}", t.elapsed().as_secs_f64(), p);
    assert_eq!(p["kind"], "video");
    let q: Vec<String> = p["qualities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert!(
        q.contains(&"360p".to_string())
            && q.contains(&"best".to_string())
            && q.contains(&"audio".to_string()),
        "{q:?}"
    );
    assert!(
        !q.iter().any(|x| x == "4320p" || x == "2880p"),
        "no 8K/5K offered: {q:?}"
    );

    println!("== video 360p");
    let id = v.add(Kind::Video, &video, "360p", None, None).unwrap();
    wait_for("video 360p done", 900, || {
        let j = job(&v, id);
        assert_ne!(j["state"], "failed", "{j}");
        j["state"] == "done"
    })
    .await;
    let mp4 = files(&out, "mp4");
    assert_eq!(mp4.len(), 1, "{mp4:?}");
    let (h, a) = probe_file(&ffprobe, &mp4[0]);
    println!(
        "  {} height={h:?} audio={a} size={}",
        mp4[0].display(),
        std::fs::metadata(&mp4[0]).unwrap().len()
    );
    assert!(h.is_some_and(|h| h <= 360), "height {h:?}");
    assert!(a, "no audio stream");
    println!("  got_height reported: {}", job(&v, id)["got_height"]);

    println!("== audio only");
    let id = v.add(Kind::Video, &video, "audio", None, None).unwrap();
    wait_for("audio done", 900, || {
        let j = job(&v, id);
        assert_ne!(j["state"], "failed", "{j}");
        j["state"] == "done"
    })
    .await;
    let mp3 = files(&out, "mp3");
    assert_eq!(mp3.len(), 1, "{mp3:?}");
    let (h, a) = probe_file(&ffprobe, &mp3[0]);
    assert!(a && h.is_none(), "mp3: height {h:?} audio {a}");

    println!("== probe playlist");
    let pl = env_or("EID_REAL_PLAYLIST", CC_PLAYLIST);
    let t = Instant::now();
    let p = v.probe(&pl).await.expect("probe playlist");
    let n = p["items"].as_array().map_or(0, |a| a.len());
    println!(
        "  {:.1}s title={} items={n}",
        t.elapsed().as_secs_f64(),
        p["title"]
    );
    assert_eq!(p["kind"], "playlist");
    assert!(n >= 2);
    let items: Vec<u32> = env_or("EID_REAL_ITEMS", "1,4")
        .split(',')
        .map(|s| s.trim().parse().unwrap())
        .collect();

    println!("== playlist {items:?} at 360p, paused and resumed mid-way");
    let id = v
        .add(Kind::Playlist, &pl, "360p", Some(items.clone()), None)
        .unwrap();
    wait_for("first bytes", 600, || {
        let j = job(&v, id);
        assert_ne!(j["state"], "failed", "{j}");
        j["bytes"].as_u64().unwrap_or(0) > 0
    })
    .await;
    v.pause(id).unwrap();
    wait_for("paused (child ended)", 30, || {
        v.list()
            .iter()
            .all(|j| j["job"] != id || j["state"] == "paused")
    })
    .await;
    v.resume(id).unwrap();
    wait_for("playlist done", 1800, || {
        let j = job(&v, id);
        assert_ne!(j["state"], "failed", "{j}");
        j["state"] == "done"
    })
    .await;
    let j = job(&v, id);
    println!("  {j}");
    let folder = PathBuf::from(j["folder"].as_str().unwrap());
    let got = files(&folder, "mp4");
    assert_eq!(got.len(), items.len(), "{got:?}");
    for (f, n) in got.iter().zip(&items) {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(&format!("{n:02} - ")), "numbering: {name}");
        let (h, a) = probe_file(&ffprobe, f);
        println!(
            "  {name} height={h:?} audio={a} size={}",
            std::fs::metadata(f).unwrap().len()
        );
        assert!(h.is_some_and(|h| h <= 360) && a);
    }

    println!("== same playlist again: nothing downloaded again");
    let t = Instant::now();
    let id2 = v
        .add(Kind::Playlist, &pl, "360p", Some(items.clone()), None)
        .unwrap();
    wait_for("second run done", 300, || job(&v, id2)["state"] == "done").await;
    let j2 = job(&v, id2);
    println!("  {:.1}s {}", t.elapsed().as_secs_f64(), j2["items"]);
    assert_eq!(files(&folder, "mp4").len(), items.len());
    assert!(
        j2["bytes"].is_null(),
        "something was downloaded again: {j2}"
    );
    v.shutdown().await;
}
