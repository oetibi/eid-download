//! Shared environment of the video CLI tests (G3, G4).
#![allow(dead_code)]
pub use eid_platform::process::command;
pub use eid_testserver::{Behavior, Content, TestServer};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const EID: &str = env!("CARGO_BIN_EXE_eid");

pub fn hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

pub fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../eid-fake-ytdlp/fixtures")
            .join(name),
    )
    .unwrap()
}

pub fn fake_ytdlp_bytes() -> Vec<u8> {
    let p = Path::new(EID).with_file_name(format!("fake-yt-dlp{}", std::env::consts::EXE_SUFFIX));
    assert!(
        p.is_file(),
        "fake-yt-dlp not built next to eid (cargo test --workspace)"
    );
    std::fs::read(p).unwrap()
}

pub fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (n, b) in entries {
        w.start_file(*n, o).unwrap();
        w.write_all(b).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// Percent-encode everything but unreserved characters (for URL query values).
pub fn pct(s: &str) -> String {
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

/// One isolated environment: test server with the tool assets, config/data folders, output
/// folder, a call log, and a test manifest pinning the fake tools.
pub struct Env {
    pub srv: TestServer,
    pub root: tempfile::TempDir,
    pub manifest: PathBuf,
}

impl Env {
    pub async fn new() -> Env {
        let srv = TestServer::start().await;
        let root = tempfile::tempdir().unwrap();
        let ytdlp = fake_ytdlp_bytes();
        let (ffmpeg, ffprobe, deno) = (vec![1u8; 1000], vec![2u8; 1000], vec![3u8; 1000]);
        let ffzip = zip_of(&[("b/ffmpeg.exe", &ffmpeg), ("b/ffprobe.exe", &ffprobe)]);
        let dzip = zip_of(&[("deno.exe", &deno)]);
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
        let file = |entry: Option<&str>, name: &str, b: &[u8]| serde_json::json!({"entry": entry, "name": name, "size": b.len(), "sha256": hex(b)});
        let m = serde_json::json!({
            "schema": 1, "id": "test-1",
            "allowed_hosts": ["github.com"],
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
        let manifest = root.path().join("manifest.json");
        std::fs::write(&manifest, m.to_string()).unwrap();
        for d in ["cfg", "data", "out"] {
            std::fs::create_dir(root.path().join(d)).unwrap();
        }
        Env {
            srv,
            root,
            manifest,
        }
    }
    pub fn out(&self) -> PathBuf {
        self.root.path().join("out")
    }
    pub fn log(&self) -> PathBuf {
        self.root.path().join("calls.jsonl")
    }
    pub fn tools_dir(&self) -> PathBuf {
        let d = self.root.path().join("data");
        if eid_platform::IS_WINDOWS {
            d.join("EidDownload").join("tools")
        } else {
            d.join("eid-download").join("tools")
        }
    }
    pub fn settings_file(&self) -> PathBuf {
        let d = self.root.path().join("cfg");
        if eid_platform::IS_WINDOWS {
            d.join("EidDownload").join("settings.json")
        } else {
            d.join("eid-download").join("settings.json")
        }
    }
    pub fn envs(&self) -> Vec<(String, String)> {
        let cfg = self.root.path().join("cfg").display().to_string();
        let data = self.root.path().join("data").display().to_string();
        vec![
            ("APPDATA".into(), cfg.clone()),
            ("LOCALAPPDATA".into(), data.clone()),
            ("XDG_CONFIG_HOME".into(), cfg),
            ("XDG_DATA_HOME".into(), data),
            (
                "EID_TEST_MANIFEST".into(),
                self.manifest.display().to_string(),
            ),
            ("EID_TEST_TOOLS_BASE".into(), self.srv.base_url()),
        ]
    }
    /// A fake page URL with the call log attached.
    pub fn url(&self, path: &str, extra: &str) -> String {
        let sep = if path.contains('?') { '&' } else { '?' };
        format!(
            "https://fake.test/{path}{sep}log={}{extra}",
            pct(&self.log().display().to_string())
        )
    }
    pub fn cmd(&self, args: &[&str]) -> std::process::Command {
        let mut c = command(EID, args);
        for (k, v) in self.envs() {
            c.env(k, v);
        }
        c
    }
    pub async fn run(&self, args: &[&str]) -> std::process::Output {
        let mut c = self.cmd(args);
        tokio::task::spawn_blocking(move || c.output().unwrap())
            .await
            .unwrap()
    }
    pub fn tool_requests(&self) -> usize {
        ["/yt-dlp.exe", "/ffmpeg.zip", "/deno.zip"]
            .iter()
            .map(|p| self.srv.stats(p).requests.len())
            .sum()
    }
    pub fn calls(&self) -> Vec<serde_json::Value> {
        match std::fs::read_to_string(self.log()) {
            Ok(s) => s
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect(),
            Err(_) => vec![],
        }
    }
}

pub fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

pub fn text(o: &std::process::Output) -> String {
    format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        o.status,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}
