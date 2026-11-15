//! P4 evidence type (ج): real yt-dlp / FFmpeg / Deno from their official releases and a real
//! Creative Commons video. Every test is `#[ignore]` (real internet, ~230 MB of tools on the
//! first run) and is run on purpose only:
//!
//!   cargo test -p eid-media --test g3_real -- --ignored --test-threads=1 --nocapture
//!
//! Results are recorded in docs/STATUS.md. `EID_REAL_TOOLS_DIR` (absolute) keeps the installed
//! tools between runs; otherwise a temporary folder is used.
use eid_media::args::{MediaUrl, Quality};
use eid_media::fetch::EngineFetcher;
use eid_media::manifest::Manifest;
use eid_media::run::Runner;
use eid_media::tools::{LockedTools, ToolStore};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// "Big Buck Bunny" (Blender Foundation, CC BY 3.0), uploaded by the Blender channel.
const CC_VIDEO: &str = "https://www.youtube.com/watch?v=aqz-KE-bpKQ";

async fn tools(keep: &tempfile::TempDir) -> (ToolStore, LockedTools) {
    let root = std::env::var_os("EID_REAL_TOOLS_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| keep.path().join("tools"));
    let m = Manifest::embedded();
    let store = ToolStore::new(root);
    if store.current(&m).is_err() {
        let t = Instant::now();
        store.install(&m, &EngineFetcher::default()).await.unwrap();
        println!("tools installed in {:?}", t.elapsed());
    }
    let t = Instant::now();
    let locked = store.lock_blocking(&m).await.unwrap();
    println!("tools locked and hashed in {:?}", t.elapsed());
    (store, locked)
}

fn runner(l: &LockedTools) -> Runner {
    Runner::new(l.paths.clone(), |k| std::env::var_os(k)).unwrap()
}

fn ffprobe_streams(l: &LockedTools, file: &Path) -> Vec<String> {
    let probe = l.paths.ffmpeg.with_file_name("ffprobe.exe");
    let o = std::process::Command::new(probe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn single_file(dir: &Path, ext: &str) -> PathBuf {
    let v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == ext))
        .collect();
    assert_eq!(v.len(), 1, "{v:?}");
    v[0].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real internet (ج)"]
async fn real_cc_video_has_video_and_audio_by_ffprobe() {
    let keep = tempfile::tempdir().unwrap();
    let (_s, l) = tools(&keep).await;
    let r = runner(&l);
    let staging = keep.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let u = MediaUrl::parse(CC_VIDEO).unwrap();
    let i = r.info(&u, &staging).await.unwrap();
    println!("title: {} ({} formats)", i.title, i.formats.len());
    let t = Instant::now();
    r.download(&u, &staging, Quality::MaxHeight(240), None, false, None)
        .await
        .unwrap();
    println!("downloaded in {:?}", t.elapsed());
    let f = single_file(&staging, "mp4");
    let s = ffprobe_streams(&l, &f);
    println!("ffprobe: {s:?}");
    assert!(
        s.iter().any(|x| x == "video") && s.iter().any(|x| x == "audio"),
        "{s:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real internet (ج)"]
async fn real_ytdlp_with_percent_and_dollar_in_paths() {
    // our tool paths may not contain $ or % (refused before any run); the working folder may:
    // the output template is relative, so yt-dlp's environment-variable expansion never sees it
    let keep = tempfile::tempdir().unwrap();
    let (_s, l) = tools(&keep).await;
    let r = runner(&l);
    let staging = keep.path().join("a%TEMP%b$HOME c${x}");
    std::fs::create_dir(&staging).unwrap();
    let u = MediaUrl::parse(CC_VIDEO).unwrap();
    r.download(&u, &staging, Quality::AudioMp3, None, false, None)
        .await
        .unwrap();
    let f = single_file(&staging, "mp3");
    assert_eq!(f.parent().unwrap(), staging, "{}", f.display());
    let mut others: Vec<String> = std::fs::read_dir(keep.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    others.sort();
    println!("folder after the run: {others:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real internet (ج)"]
async fn real_ytdlp_does_not_load_a_planted_plugin() {
    let keep = tempfile::tempdir().unwrap();
    let (_s, l) = tools(&keep).await;
    let r = runner(&l);
    let staging = keep.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let marker = keep.path().join("PLUGIN_RAN");
    let plugin = format!(
        "import pathlib\npathlib.Path(r'{}').write_text('ran')\n",
        marker.display()
    );
    // plugin locations yt-dlp documents: a `yt-dlp-plugins` folder next to the executable and
    // `yt_dlp_plugins` packages on the import path (the working folder)
    let beside = l.paths.ytdlp.parent().unwrap().join("yt-dlp-plugins");
    for base in [
        beside.join("eid").join("yt_dlp_plugins"),
        staging.join("yt_dlp_plugins"),
    ] {
        for kind in ["extractor", "postprocessor"] {
            let d = base.join(kind);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("eid_planted.py"), &plugin).unwrap();
        }
    }
    let u = MediaUrl::parse(CC_VIDEO).unwrap();
    let res = r.info(&u, &staging).await;
    let _ = std::fs::remove_dir_all(&beside);
    res.unwrap();
    assert!(!marker.exists(), "a planted plugin was imported");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real internet (ج)"]
async fn real_ytdlp_startup_time() {
    // for D9 (one yt-dlp.exe process per item): wall time of a run that fails at once
    // (unsupported address, no network needed) is dominated by the onefile start-up
    let keep = tempfile::tempdir().unwrap();
    let (_s, l) = tools(&keep).await;
    let r = runner(&l);
    let staging = keep.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let u = MediaUrl::parse("https://example.invalid/not-a-video").unwrap();
    let mut times = vec![];
    for _ in 0..5 {
        let t = Instant::now();
        let _ = r.info(&u, &staging).await;
        times.push(t.elapsed());
    }
    println!("yt-dlp start-up + immediate failure, 5 runs: {times:?}");
}
