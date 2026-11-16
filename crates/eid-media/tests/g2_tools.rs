//! P4 G2: consent, settings, tool manifest, tool installation through an injected Fetcher over
//! the local eid-testserver (test code only, D4), bounded zip extraction, locked hashing.
use eid_media::consent::{self, Lang};
use eid_media::fetch::{engine_fetch, BoxFuture, EngineFetcher, FetchRequest, Fetcher};
use eid_media::manifest::{Manifest, Tool, ToolFile};
use eid_media::settings::SettingsStore;
use eid_media::tools::ToolStore;
use eid_media::MediaError;
use eid_testserver::{Behavior, Content, TestServer};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn fake_ytdlp_bytes() -> Vec<u8> {
    let name = format!("fake-yt-dlp{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().unwrap();
    let p = exe
        .ancestors()
        .map(|a| a.join(&name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} not built (run cargo test --workspace)"));
    std::fs::read(p).unwrap()
}

fn junk(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

/// A zip with the given (name, bytes) entries, deflated.
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

fn tf(entry: Option<&str>, name: &str, b: &[u8]) -> ToolFile {
    ToolFile {
        entry: entry.map(str::to_string),
        name: name.into(),
        size: b.len() as u64,
        sha256: hex(b),
    }
}

/// The bytes of every file of a test tool set.
struct Set {
    ytdlp: Vec<u8>,
    ffmpeg: Vec<u8>,
    ffprobe: Vec<u8>,
    deno: Vec<u8>,
    ffmpeg_zip: Vec<u8>,
    deno_zip: Vec<u8>,
}

impl Set {
    fn new(seed: u8) -> Set {
        let ffmpeg = junk(seed, 300_000);
        let ffprobe = junk(seed.wrapping_add(1), 200_000);
        let deno = junk(seed.wrapping_add(2), 250_000);
        let ffmpeg_zip = zip_of(&[
            ("ffmpeg-x/README.txt", b"readme"),
            ("ffmpeg-x/bin/ffmpeg.exe", &ffmpeg),
            ("ffmpeg-x/bin/ffprobe.exe", &ffprobe),
            ("ffmpeg-x/bin/ffplay.exe", b"not wanted"),
        ]);
        let deno_zip = zip_of(&[("deno.exe", &deno)]);
        Set {
            ytdlp: fake_ytdlp_bytes(),
            ffmpeg,
            ffprobe,
            deno,
            ffmpeg_zip,
            deno_zip,
        }
    }

    fn manifest(&self, id: &str) -> Manifest {
        let base = "https://github.com/test/tools/releases/download/v1";
        Manifest {
            schema: 1,
            id: id.into(),
            allowed_hosts: vec![
                "github.com".into(),
                "release-assets.githubusercontent.com".into(),
            ],
            tools: vec![
                Tool {
                    name: "yt-dlp".into(),
                    version: "2026.10.26".into(),
                    license: "Unlicense".into(),
                    source: "https://github.com/test/tools/releases/tag/v1".into(),
                    url: format!("{base}/yt-dlp.exe"),
                    size: self.ytdlp.len() as u64,
                    sha256: hex(&self.ytdlp),
                    files: vec![tf(None, "yt-dlp.exe", &self.ytdlp)],
                },
                Tool {
                    name: "ffmpeg".into(),
                    version: "n8.1".into(),
                    license: "LGPL".into(),
                    source: "https://github.com/test/tools/releases/tag/v1".into(),
                    url: format!("{base}/ffmpeg.zip"),
                    size: self.ffmpeg_zip.len() as u64,
                    sha256: hex(&self.ffmpeg_zip),
                    files: vec![
                        tf(Some("ffmpeg-x/bin/ffmpeg.exe"), "ffmpeg.exe", &self.ffmpeg),
                        tf(
                            Some("ffmpeg-x/bin/ffprobe.exe"),
                            "ffprobe.exe",
                            &self.ffprobe,
                        ),
                    ],
                },
                Tool {
                    name: "deno".into(),
                    version: "v2.9.8".into(),
                    license: "MIT".into(),
                    source: "https://github.com/test/tools/releases/tag/v1".into(),
                    url: format!("{base}/deno.zip"),
                    size: self.deno_zip.len() as u64,
                    sha256: hex(&self.deno_zip),
                    files: vec![tf(Some("deno.exe"), "deno.exe", &self.deno)],
                },
            ],
        }
    }
}

/// Test-only fetcher: serves the manifest's assets from the local test server by asset name
/// (last URL segment), through the same engine download with the same size/hash policy.
struct LocalFetcher {
    base: String,
}

impl Fetcher for LocalFetcher {
    fn fetch<'a>(&'a self, req: FetchRequest<'a>) -> BoxFuture<'a, Result<PathBuf, MediaError>> {
        Box::pin(async move {
            let asset = req.url.rsplit('/').next().unwrap();
            let url = format!("{}/{asset}", self.base);
            let hosts = vec!["127.0.0.1".to_string()];
            let local = FetchRequest {
                url: &url,
                allowed_hosts: &hosts,
                ..req
            };
            let cfg = eid_engine::Config {
                connections: 1,
                stall_timeout: Duration::from_secs(10),
                use_system_proxy: false,
                ..eid_engine::Config::default()
            };
            engine_fetch(cfg, &local).await
        })
    }
}

struct Fx {
    srv: TestServer,
    dir: tempfile::TempDir,
}

impl Fx {
    async fn new() -> Fx {
        Fx {
            srv: TestServer::start().await,
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn serve(&self, s: &Set) {
        self.srv.add(
            "/yt-dlp.exe",
            Content::Bytes(s.ytdlp.clone()),
            Behavior::default(),
        );
        self.srv.add(
            "/ffmpeg.zip",
            Content::Bytes(s.ffmpeg_zip.clone()),
            Behavior::default(),
        );
        self.srv.add(
            "/deno.zip",
            Content::Bytes(s.deno_zip.clone()),
            Behavior::default(),
        );
    }
    fn fetcher(&self) -> LocalFetcher {
        LocalFetcher {
            base: self.srv.base_url(),
        }
    }
    fn store(&self) -> ToolStore {
        ToolStore::new(self.dir.path().join("tools"))
    }
    fn settings(&self) -> SettingsStore {
        SettingsStore::new(self.dir.path().join("config").join("settings.json"))
    }
    fn requests(&self) -> usize {
        ["/yt-dlp.exe", "/ffmpeg.zip", "/deno.zip"]
            .iter()
            .map(|p| self.srv.stats(p).requests.len())
            .sum()
    }
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = match std::fs::read_dir(dir) {
        Ok(r) => r
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => vec![],
    };
    v.sort();
    v
}

// ------------------------------------------------------------------ consent and settings

#[test]
fn consent_text_comes_from_locale_files_and_its_hash_is_stored() {
    let m = Manifest::embedded();
    for (lang, file) in [(Lang::Ar, "ar"), (Lang::En, "en")] {
        let on_disk = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("locales/{file}/consent.txt")),
        )
        .unwrap()
        .replace("\r\n", "\n");
        assert_eq!(consent::template(lang).replace("\r\n", "\n"), on_disk);
        let t = consent::consent_text(lang, &m);
        assert!(!t.text.contains("{tools}"));
        for tool in &m.tools {
            assert!(t.text.contains(&tool.name), "{lang:?} misses {}", tool.name);
            assert!(
                t.text.contains(&tool.version),
                "{lang:?} misses {}",
                tool.version
            );
            assert!(
                t.text.contains(&tool.source),
                "{lang:?} misses {}",
                tool.source
            );
        }
        assert!(
            t.text.contains("SHA-256") && t.text.contains("DRM"),
            "{lang:?}"
        );
        assert_eq!(t.sha256, hex(t.text.as_bytes()));
    }
    let d = tempfile::tempdir().unwrap();
    let store = SettingsStore::new(d.path().join("cfg").join("settings.json"));
    assert!(!consent::is_granted(&store, &m).unwrap());
    let t = consent::consent_text(Lang::Ar, &m);
    consent::grant(&store, &t, 1_763_190_000).unwrap();
    assert!(consent::is_granted(&store, &m).unwrap());
    let raw = std::fs::read_to_string(store.path()).unwrap();
    assert!(raw.contains(&t.sha256), "{raw}");
    assert!(raw.contains("1763190000"), "{raw}");
    // Unix: mode bits read back here. Windows: settings.json is written by the same
    // `write_private_atomic` as the IPC token, whose protected one-ACE DACL is read back by
    // eid-platform's `token_file_roundtrip_is_private_and_rejects_garbage` (P3).
    if let Some(private) = eid_platform::fs::owner_only(store.path()).unwrap() {
        assert!(private, "settings.json must be private");
    }
    // a different text (new tool versions) is not covered by the old consent
    let mut m2 = m.clone();
    m2.tools[0].version = "2099.01.01".into();
    assert!(!consent::is_granted(&store, &m2).unwrap());
    assert!(matches!(
        consent::require(&store, &m2),
        Err(MediaError::NoConsent)
    ));
}

#[test]
fn a_damaged_or_planted_settings_file_keeps_video_off() {
    let m = Manifest::embedded();
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    let store = SettingsStore::new(&p);
    let good = consent::consent_text(Lang::En, &m);
    for bad in [
        "not json".to_string(),
        format!(
            "{{\"media_consent\":{{\"version\":1,\"text_sha256\":\"{}\",\"ts\":1}},\"x\":1}}",
            good.sha256
        ),
        format!(
            "{{\"media_consent\":{{\"version\":2,\"text_sha256\":\"{}\",\"ts\":1}}}}",
            good.sha256
        ),
        "{\"media_consent\":{\"version\":1,\"text_sha256\":\"00\",\"ts\":1}}".to_string(),
        format!(
            "{{\"media_consent\":null,\"pad\":\"{}\"}}",
            "x".repeat(70 * 1024)
        ),
    ] {
        std::fs::write(&p, &bad).unwrap();
        assert!(
            !consent::is_granted(&store, &m).unwrap_or(false),
            "accepted: {}",
            &bad[..bad.len().min(80)]
        );
    }
}

#[test]
fn ipc_and_native_messaging_cannot_grant_consent() {
    // D7: video commands are not part of IPC / Native Messaging in P4, and nothing reachable
    // from them can write the consent record. Checked structurally: neither crate depends on
    // eid-media, and their sources never mention consent.
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for c in ["eid-ipc", "eid-native-host"] {
        let toml = std::fs::read_to_string(crates.join(c).join("Cargo.toml")).unwrap();
        assert!(!toml.contains("eid-media"), "{c} depends on eid-media");
        let mut stack = vec![crates.join(c).join("src")];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let s = std::fs::read_to_string(&p).unwrap().to_lowercase();
                    for w in ["consent", "eid_media", "yes-i-agree", "settings.json"] {
                        assert!(!s.contains(w), "{} mentions {w}", p.display());
                    }
                }
            }
        }
    }
    // The IPC server (`eid serve`) and the remote commands live in eid-cli, which does depend
    // on eid-media for `eid video` (review P4 M7). Every module they reach through `crate::`
    // must stay free of the video and consent code.
    let cli_src = crates.join("eid-cli").join("src");
    let reachable = cli_modules_reachable_from(&cli_src, &["serve", "remote"]);
    for m in ["serve", "remote", "args"] {
        assert!(
            reachable.iter().any(|r| r == m),
            "the scan did not reach {m}: {reachable:?}"
        );
    }
    assert!(
        !reachable.iter().any(|r| r == "video"),
        "`eid serve` or the remote commands reach the video module: {reachable:?}"
    );
    for m in &reachable {
        let hits =
            ipc_forbidden_words(&std::fs::read_to_string(cli_module_file(&cli_src, m)).unwrap());
        assert!(
            hits.is_empty(),
            "eid-cli/src/{m}.rs (reachable from IPC) mentions {hits:?}"
        );
    }
    // positive control: the same scan does see the consent code where it really is
    let video = std::fs::read_to_string(cli_module_file(&cli_src, "video")).unwrap();
    assert!(ipc_forbidden_words(&video).contains(&"consent"));
    assert!(ipc_forbidden_words("use crate::video::run;").contains(&"crate::video"));
    // and the only way to build a ConsentText is to render the real text
    let t = consent::consent_text(Lang::En, &Manifest::embedded());
    assert_eq!(t.sha256, hex(t.text.as_bytes()));
}

/// Words that must not appear in code reachable from IPC / Native Messaging (lowercased match).
fn ipc_forbidden_words(src: &str) -> Vec<&'static str> {
    let s = src.to_lowercase();
    [
        "consent",
        "eid_media",
        "yes-i-agree",
        "settings.json",
        "crate::video",
        "video::",
    ]
    .into_iter()
    .filter(|w| s.contains(w))
    .collect()
}

/// `src/<m>.rs` or `src/<m>/mod.rs` of eid-cli; panics if neither exists, so a module the
/// scan cannot find fails the test instead of being skipped.
fn cli_module_file(src: &Path, m: &str) -> PathBuf {
    let a = src.join(format!("{m}.rs"));
    let b = src.join(m).join("mod.rs");
    if a.is_file() {
        a
    } else if b.is_file() {
        b
    } else {
        panic!("eid-cli module {m} not found (src/{m}.rs or src/{m}/mod.rs)")
    }
}

/// Top-level modules named after `crate::` in a source text: `crate::a::x`, `crate::a`,
/// `use crate::{a, b::c}`.
fn crate_refs(src: &str) -> Vec<String> {
    let ident = |s: &str| -> String {
        s.chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect()
    };
    let mut out = vec![];
    for (i, _) in src.match_indices("crate::") {
        let rest = &src[i + "crate::".len()..];
        if let Some(group) = rest.strip_prefix('{') {
            let end = group.find('}').unwrap_or(group.len());
            for part in group[..end].split(',') {
                let n = ident(part.trim());
                if !n.is_empty() && n != "self" {
                    out.push(n);
                }
            }
        } else {
            let n = ident(rest);
            if !n.is_empty() && n != "self" {
                out.push(n);
            }
        }
    }
    out
}

/// Every eid-cli module reachable from `roots` through `crate::` references (transitive).
fn cli_modules_reachable_from(src: &Path, roots: &[&str]) -> Vec<String> {
    let mut seen: Vec<String> = vec![];
    let mut todo: Vec<String> = roots.iter().map(|r| r.to_string()).collect();
    while let Some(m) = todo.pop() {
        if seen.contains(&m) {
            continue;
        }
        let text = std::fs::read_to_string(cli_module_file(src, &m)).unwrap();
        seen.push(m);
        todo.extend(crate_refs(&text));
    }
    seen
}

#[test]
fn the_ipc_reachability_scan_follows_crate_paths() {
    assert_eq!(
        crate_refs("use crate::remote::{self, X};\nlet r = crate::args::parse_rate(s);"),
        ["remote", "args"]
    );
    assert_eq!(crate_refs("use crate::{a, b::c, self};"), ["a", "b"]);
    assert!(crate_refs("no paths here").is_empty());
}

// ------------------------------------------------------------------ manifest

fn real() -> Manifest {
    Manifest::embedded()
}

fn refused(m: &Manifest) -> String {
    match m.validate() {
        Err(MediaError::BadManifest(s)) => s,
        other => panic!("accepted: {other:?}"),
    }
}

#[test]
fn tool_manifest_rejects_non_https_and_unknown_hosts() {
    real().validate().unwrap();
    for url in [
        "http://github.com/yt-dlp/yt-dlp/releases/download/2026.10.26/yt-dlp.exe",
        "ftp://github.com/yt-dlp/yt-dlp/releases/download/2026.10.26/yt-dlp.exe",
        "https://evil.example/yt-dlp/yt-dlp/releases/download/2026.10.26/yt-dlp.exe",
        "https://github.com.evil.example/yt-dlp/yt-dlp/releases/download/2026.10.26/yt-dlp.exe",
        "https://user:pw@github.com/yt-dlp/yt-dlp/releases/download/2026.10.26/yt-dlp.exe",
        "https://github.com:8443/yt-dlp/yt-dlp/releases/download/2026.10.26/yt-dlp.exe",
        "https://127.0.0.1/yt-dlp/yt-dlp/releases/download/2026.10.26/yt-dlp.exe",
    ] {
        let mut m = real();
        m.tools[0].url = url.into();
        let why = refused(&m);
        assert!(!why.is_empty(), "{url}");
    }
    // a manifest cannot widen the program's own host list
    let mut m = real();
    m.allowed_hosts.push("evil.example".into());
    assert!(refused(&m).contains("evil.example"));
    // the production fetcher refuses a non-HTTPS URL before sending anything
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let srv = TestServer::start().await;
        srv.add("/x.exe", Content::Bytes(vec![1, 2, 3]), Behavior::default());
        let d = tempfile::tempdir().unwrap();
        let url = format!("{}/x.exe", srv.base_url());
        let hosts = vec!["127.0.0.1".to_string()];
        let r = EngineFetcher::default()
            .fetch(FetchRequest {
                url: &url,
                dir: d.path(),
                file_name: "x.exe",
                size: 3,
                sha256: &hex(&[1, 2, 3]),
                allowed_hosts: &hosts,
            })
            .await;
        assert!(matches!(r, Err(MediaError::BadManifest(_))), "{r:?}");
        assert!(
            srv.stats("/x.exe").requests.is_empty(),
            "a request was sent"
        );
    });
}

#[test]
fn tool_manifest_pins_immutable_assets() {
    let m = real();
    // the shipped manifest: every tool pinned by size + SHA-256, at a fixed release tag
    for t in &m.tools {
        assert!(!t.url.contains("/latest/"), "{}", t.url);
        assert_eq!(t.sha256.len(), 64);
        assert!(t.size > 0);
        for f in &t.files {
            assert_eq!(f.sha256.len(), 64);
        }
    }
    assert_eq!(
        m.file("yt-dlp.exe").unwrap().sha256,
        "f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d"
    );
    for (url, why) in [
        (
            "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe",
            "pinned",
        ),
        (
            "https://github.com/yt-dlp/yt-dlp/releases/download/latest/yt-dlp.exe",
            "immutable",
        ),
        (
            "https://github.com/yt-dlp/yt-dlp/releases/download/nightly/yt-dlp.exe",
            "immutable",
        ),
        (
            "https://github.com/yt-dlp/yt-dlp/raw/master/yt-dlp.exe",
            "pinned",
        ),
        (
            "https://github.com/yt-dlp/yt-dlp/releases/download/2026.10.26/yt-dlp.exe?x=1",
            "query",
        ),
        (
            "https://release-assets.githubusercontent.com/a/b/c/d/e/f",
            "pinned",
        ),
    ] {
        let mut m = real();
        m.tools[0].url = url.into();
        let got = refused(&m);
        assert!(got.contains(why), "{url}: {got}");
    }
    let mut m = real();
    m.tools[0].sha256 = "F5389D3698ECDE4EF06F4E6B1CE0294FE9AC1396228C8990B8D3416D6D177A7D".into();
    refused(&m);
    let mut m = real();
    m.tools[1].files[0].sha256 = "00".into();
    refused(&m);
    let mut m = real();
    m.tools[1].files[0].size = 0;
    refused(&m);
    let mut m = real();
    m.tools[2].files[0].entry = Some("../deno.exe".into());
    assert!(refused(&m).contains("unsafe archive entry"));
    // a direct file must be the asset itself
    let mut m = real();
    m.tools[0].files[0].sha256 = m.tools[1].sha256.clone();
    refused(&m);
    let mut m = real();
    m.tools.remove(2);
    assert!(refused(&m).contains("deno.exe is missing"));
}

#[test]
fn only_manifest_exe_tools_are_launched_never_bat_or_cmd() {
    for bad in [
        "yt-dlp.bat",
        "yt-dlp.cmd",
        "yt-dlp.ps1",
        "yt-dlp.com",
        "yt-dlp",
        "yt-dlp.exe.bat",
        "../yt-dlp.exe",
        "bin/yt-dlp.exe",
        "bin\\yt-dlp.exe",
        "C:yt-dlp.exe",
        ".exe",
        "-x.exe",
        "yt dlp.exe",
    ] {
        assert!(!eid_media::manifest::is_tool_file_name(bad), "{bad}");
        let mut m = real();
        m.tools[0].files[0].name = bad.into();
        refused(&m);
    }
    assert!(eid_media::manifest::is_tool_file_name("yt-dlp.exe"));
    // and the launcher refuses batch files whatever their origin (G1 platform rule)
    let d = tempfile::tempdir().unwrap();
    for b in ["x.bat", "x.CMD"] {
        assert!(eid_platform::process::validate_program(&d.path().join(b)).is_err());
    }
}

// ------------------------------------------------------------------ install

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_install_through_injected_fetcher_installs_and_verifies() {
    let fx = Fx::new().await;
    let s = Set::new(1);
    fx.serve(&s);
    let m = s.manifest("t1");
    let store = fx.store();
    assert!(matches!(store.current(&m), Err(MediaError::NotInstalled)));
    let paths = store.install(&m, &fx.fetcher()).await.unwrap();
    assert_eq!(fx.requests(), 3, "one request per asset");
    for (p, want) in [
        (&paths.ytdlp, &s.ytdlp),
        (&paths.ffmpeg, &s.ffmpeg),
        (&paths.deno, &s.deno),
        (&paths.ffmpeg.with_file_name("ffprobe.exe"), &s.ffprobe),
    ] {
        assert!(std::fs::read(p).unwrap() == **want, "{}", p.display());
    }
    // only the four named files in one set folder; nothing from the archives' other entries,
    // no staging left over
    let set = paths.ytdlp.parent().unwrap();
    assert_eq!(
        entries(set),
        ["deno.exe", "ffmpeg.exe", "ffprobe.exe", "yt-dlp.exe"]
    );
    let root = entries(store.root());
    assert_eq!(root.len(), 2, "{root:?}");
    assert!(root.contains(&"current.json".to_string()));
    assert!(root.iter().any(|e| e.starts_with("set-t1-")));
    let locked = store.lock(&m).unwrap();
    assert_eq!(locked.locked_files(), 4);
    assert_eq!(locked.paths, paths);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_install_through_injected_fetcher_keeps_the_previous_version_on_failure() {
    let fx = Fx::new().await;
    let s1 = Set::new(1);
    fx.serve(&s1);
    let m1 = s1.manifest("v1");
    let store = fx.store();
    let p1 = store.install(&m1, &fx.fetcher()).await.unwrap();
    // v2: the deno asset served does not match the manifest -> refused by the engine
    let s2 = Set::new(7);
    let mut m2 = s2.manifest("v2");
    fx.srv.add(
        "/ffmpeg.zip",
        Content::Bytes(s2.ffmpeg_zip.clone()),
        Behavior::default(),
    );
    fx.srv.add(
        "/deno.zip",
        Content::Bytes(s1.deno_zip.clone()),
        Behavior::default(),
    );
    let e = store.install(&m2, &fx.fetcher()).await.unwrap_err();
    assert!(matches!(e, MediaError::Fetch(_)), "{e:?}");
    // v2 again: assets match, but one extracted file does not match its pinned hash
    fx.srv.add(
        "/deno.zip",
        Content::Bytes(s2.deno_zip.clone()),
        Behavior::default(),
    );
    m2.tools[1].files[1].sha256 = hex(b"something else");
    let e = store.install(&m2, &fx.fetcher()).await.unwrap_err();
    assert!(matches!(e, MediaError::HashMismatch { .. }), "{e:?}");
    // the v1 set is still current, complete and verifiable; nothing of v2 is left
    assert_eq!(store.current(&m1).unwrap(), p1);
    store.lock(&m1).unwrap();
    let root = entries(store.root());
    assert_eq!(root.len(), 2, "leftovers: {root:?}");
    assert!(matches!(store.current(&m2), Err(MediaError::NotInstalled)));
    // a later successful install replaces v1 and removes it
    let mut m3 = s2.manifest("v3");
    m3.tools[0] = m1.tools[0].clone();
    fx.srv.add(
        "/yt-dlp.exe",
        Content::Bytes(s1.ytdlp.clone()),
        Behavior::default(),
    );
    let p3 = store.install(&m3, &fx.fetcher()).await.unwrap();
    assert!(!p1.ytdlp.exists(), "the previous set was not removed");
    assert_eq!(entries(store.root()).len(), 2);
    store.lock(&m3).unwrap();
    assert_ne!(p1, p3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_with_wrong_sha256_is_refused_before_run() {
    let fx = Fx::new().await;
    let s = Set::new(1);
    fx.serve(&s);
    let m = s.manifest("t1");
    let store = fx.store();
    let paths = store.install(&m, &fx.fetcher()).await.unwrap();
    store.lock(&m).unwrap();
    // one changed byte in an installed tool: refused at lock time, so nothing can be run
    let mut b = std::fs::read(&paths.deno).unwrap();
    b[1000] ^= 0xff;
    std::fs::write(&paths.deno, &b).unwrap();
    let e = store.lock(&m).unwrap_err();
    assert!(
        matches!(&e, MediaError::HashMismatch { file } if file == "deno.exe"),
        "{e:?}"
    );
    // a truncated one too
    std::fs::write(&paths.deno, &s.deno[..10]).unwrap();
    assert!(matches!(
        store.lock(&m),
        Err(MediaError::HashMismatch { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_with_right_sha256_runs() {
    let fx = Fx::new().await;
    let s = Set::new(1);
    fx.serve(&s);
    let m = s.manifest("t1");
    let store = fx.store();
    store.install(&m, &fx.fetcher()).await.unwrap();
    let locked = store.lock_blocking(&m).await.unwrap();
    let r = eid_media::run::Runner::new(locked.paths.clone(), |k| std::env::var_os(k)).unwrap();
    let staging = fx.dir.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    // the installed yt-dlp.exe (the fake) runs while it is locked
    let i = r
        .info(
            &eid_media::args::MediaUrl::parse("https://fake.test/video/abc").unwrap(),
            &staging,
        )
        .await
        .unwrap();
    assert_eq!(i.id, "abc");
    drop(locked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tools_are_never_taken_from_path() {
    // nothing installed: whatever yt-dlp the user has on PATH is never used
    let fx = Fx::new().await;
    let m = Set::new(1).manifest("t1");
    let store = fx.store();
    assert!(matches!(store.current(&m), Err(MediaError::NotInstalled)));
    assert!(matches!(store.lock(&m), Err(MediaError::NotInstalled)));
    // the launcher never searches PATH: a bare program name is refused
    let e = eid_media::run::Runner::new(
        eid_media::args::ToolPaths {
            ytdlp: "yt-dlp.exe".into(),
            ffmpeg: "ffmpeg.exe".into(),
            deno: "deno.exe".into(),
        },
        |_| None,
    );
    assert!(e.is_err());
    // and no code of eid-media reads PATH at all (the child's PATH is built, not inherited)
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for e in std::fs::read_dir(src).unwrap() {
        let p = e.unwrap().path();
        let s = std::fs::read_to_string(&p).unwrap();
        for needle in [
            "var_os(\"PATH\")",
            "var(\"PATH\")",
            "lookup(\"PATH\")",
            "which(",
        ] {
            assert!(!s.contains(needle), "{} uses {needle}", p.display());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_consent_deletes_the_tools() {
    let fx = Fx::new().await;
    let s = Set::new(1);
    fx.serve(&s);
    let m = s.manifest("t1");
    let (store, settings) = (fx.store(), fx.settings());
    store.install(&m, &fx.fetcher()).await.unwrap();
    consent::grant(&settings, &consent::consent_text(Lang::En, &m), 1).unwrap();
    assert!(consent::is_granted(&settings, &m).unwrap());
    consent::revoke(&settings, &store).unwrap();
    assert!(!consent::is_granted(&settings, &m).unwrap());
    assert!(!store.root().exists(), "the tools folder must be gone");
    assert!(matches!(store.current(&m), Err(MediaError::NotInstalled)));
    // revoking twice is fine
    consent::revoke(&settings, &store).unwrap();
}

// ------------------------------------------------------------------ zip

fn zfile(entry: &str, name: &str, b: &[u8]) -> ToolFile {
    tf(Some(entry), name, b)
}

fn write_zip(dir: &Path, bytes: &[u8]) -> PathBuf {
    let p = dir.join("a.zip");
    std::fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn zip_extracts_only_named_entries() {
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (junk(1, 5000), junk(2, 7000));
    let z = write_zip(
        d.path(),
        &zip_of(&[
            ("x/a.exe", &a),
            ("x/b.exe", &b),
            ("x/c.exe", b"c"),
            ("x/readme", b"r"),
        ]),
    );
    let out = d.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let want = zfile("x/b.exe", "b.exe", &b);
    let got = eid_media::zipx::extract_named(&z, &[&want], &out).unwrap();
    assert_eq!(got, [out.join("b.exe")]);
    assert_eq!(entries(&out), ["b.exe"]);
    assert!(std::fs::read(out.join("b.exe")).unwrap() == b);
    // a named entry that is missing is an error, not a silent skip
    let missing = zfile("x/nope.exe", "nope.exe", b"x");
    assert!(eid_media::zipx::extract_named(&z, &[&missing], &out).is_err());
}

#[test]
fn zip_slip_entry_is_ignored() {
    let d = tempfile::tempdir().unwrap();
    let good = junk(3, 4000);
    let z = write_zip(
        d.path(),
        &zip_of(&[
            ("../evil.exe", b"EVIL"),
            ("../../evil2.exe", b"EVIL"),
            ("/abs-evil.exe", b"EVIL"),
            ("C:/drive-evil.exe", b"EVIL"),
            ("..\\back-evil.exe", b"EVIL"),
            ("bin/ok.exe", &good),
        ]),
    );
    let out = d.path().join("deep").join("out");
    std::fs::create_dir_all(&out).unwrap();
    let want = zfile("bin/ok.exe", "ok.exe", &good);
    eid_media::zipx::extract_named(&z, &[&want], &out).unwrap();
    assert_eq!(entries(&out), ["ok.exe"]);
    assert_eq!(entries(&d.path().join("deep")), ["out"]);
    assert_eq!(
        entries(d.path()),
        ["a.zip", "deep"],
        "something escaped the folder"
    );
    // and a manifest can never name such an entry
    let mut m = real();
    for bad in [
        "../evil.exe",
        "/abs.exe",
        "C:/x.exe",
        "a\\b.exe",
        "a/../b.exe",
        "a//b.exe",
    ] {
        m.tools[2].files[0].entry = Some(bad.into());
        assert!(refused(&m).contains("unsafe archive entry"), "{bad}");
    }
}

#[test]
fn zip_bomb_is_cut_at_the_size_cap() {
    let d = tempfile::tempdir().unwrap();
    // 64 MiB of zeros deflate to ~64 KiB; the manifest pins the entry at 1 MiB
    let bomb = vec![0u8; 64 * 1024 * 1024];
    let z = write_zip(d.path(), &zip_of(&[("tool.exe", &bomb)]));
    assert!(std::fs::metadata(&z).unwrap().len() < 1024 * 1024);
    let out = d.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let pinned = ToolFile {
        entry: Some("tool.exe".into()),
        name: "tool.exe".into(),
        size: 1024 * 1024,
        sha256: hex(&bomb[..1024 * 1024]),
    };
    let e = eid_media::zipx::extract_named(&z, &[&pinned], &out).unwrap_err();
    assert!(e.to_string().contains("cut"), "{e}");
    assert!(entries(&out).is_empty(), "the cut file must be removed");
    // a shorter entry than pinned is refused too
    let short = ToolFile {
        size: 64 * 1024 * 1024 + 1,
        ..pinned
    };
    assert!(eid_media::zipx::extract_named(&z, &[&short], &out).is_err());
    assert!(entries(&out).is_empty());
}

#[test]
fn extracted_file_with_wrong_hash_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let a = junk(9, 10_000);
    let z = write_zip(d.path(), &zip_of(&[("a.exe", &a)]));
    let out = d.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let mut wrong = zfile("a.exe", "a.exe", &a);
    wrong.sha256 = hex(b"other");
    let e = eid_media::zipx::extract_named(&z, &[&wrong], &out).unwrap_err();
    assert!(
        matches!(&e, MediaError::HashMismatch { file } if file == "a.exe"),
        "{e:?}"
    );
    assert!(entries(&out).is_empty());
    let right = zfile("a.exe", "a.exe", &a);
    eid_media::zipx::extract_named(&z, &[&right], &out).unwrap();
    assert_eq!(entries(&out), ["a.exe"]);
}

// ------------------------------------------------------------------ locks

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_hash_is_read_through_the_locked_handle() {
    let fx = Fx::new().await;
    let s = Set::new(1);
    fx.serve(&s);
    let m = s.manifest("t1");
    let store = fx.store();
    let paths = store.install(&m, &fx.fetcher()).await.unwrap();
    // the file is opened without following a link, and the hash comes from that handle: a
    // correct copy elsewhere, linked in place of the tool, is refused
    let elsewhere = fx.dir.path().join("deno-copy.exe");
    std::fs::write(&elsewhere, &s.deno).unwrap();
    std::fs::remove_file(&paths.deno).unwrap();
    eid_platform::fs::symlink_file(&elsewhere, &paths.deno)
        .expect("cannot create a symlink here: this test must not pass without one");
    assert!(store.lock(&m).is_err(), "a linked tool was accepted");
    std::fs::remove_file(&paths.deno).unwrap();
    std::fs::write(&paths.deno, &s.deno).unwrap();
    let l = store.lock(&m).unwrap();
    assert_eq!(l.locked_files(), 4);
    // on Windows the lock holds: the file we hashed cannot change underneath us
    if eid_platform::fs::TOOL_LOCK_ENFORCED {
        assert!(std::fs::OpenOptions::new()
            .write(true)
            .open(&paths.deno)
            .is_err());
    }
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tools_are_locked_against_write_delete_rename_while_a_job_runs() {
    let fx = Fx::new().await;
    let s = Set::new(1);
    fx.serve(&s);
    let m = s.manifest("t1");
    let store = fx.store();
    store.install(&m, &fx.fetcher()).await.unwrap();
    let locked = store.lock_blocking(&m).await.unwrap();
    let r = eid_media::run::Runner::new(locked.paths.clone(), |k| std::env::var_os(k)).unwrap();
    let staging = fx.dir.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let u = eid_media::args::MediaUrl::parse("https://fake.test/sleep/60").unwrap();
    let a = eid_media::args::build(&r.tools, &u, &eid_media::args::Action::Info).unwrap();
    let job = eid_platform::process::spawn(r.spec(&staging, a, 4096)).unwrap();
    let set = locked.paths.ytdlp.parent().unwrap().to_path_buf();
    // deno.exe and ffprobe.exe are not running, so only OUR handle protects them
    for name in ["deno.exe", "ffprobe.exe", "yt-dlp.exe"] {
        let p = set.join(name);
        assert!(
            std::fs::OpenOptions::new().write(true).open(&p).is_err(),
            "{name}: write"
        );
        assert!(std::fs::remove_file(&p).is_err(), "{name}: delete");
        assert!(
            std::fs::rename(&p, set.join("moved.exe")).is_err(),
            "{name}: rename"
        );
    }
    assert!(
        std::fs::rename(&set, set.with_file_name("moved-set")).is_err(),
        "parent rename"
    );
    assert!(
        std::fs::rename(store.root(), store.root().with_file_name("moved-root")).is_err(),
        "grandparent rename"
    );
    // ...while the tool itself runs fine
    assert!(job
        .id()
        .is_some_and(eid_platform::process::process_is_alive));
    drop(job);
    drop(locked);
    // once the job and the lock are gone the files can be removed again (success counterpart)
    let p = set.join("deno.exe");
    let mut ok = false;
    for _ in 0..50 {
        if std::fs::remove_file(&p).is_ok() {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ok, "deno.exe still locked after the lock was dropped");
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tool_already_open_for_writing_is_refused() {
    let fx = Fx::new().await;
    let s = Set::new(1);
    fx.serve(&s);
    let m = s.manifest("t1");
    let store = fx.store();
    let paths = store.install(&m, &fx.fetcher()).await.unwrap();
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&paths.deno)
        .unwrap();
    let e = store.lock(&m).unwrap_err();
    assert!(
        matches!(&e, MediaError::ToolBusy { file } if file == "deno.exe"),
        "{e:?}"
    );
    drop(writer);
    store.lock(&m).unwrap();
}

// ------------------------------------------------------------------ (ج) real internet

/// Real download of the pinned tools from their official GitHub releases (~230 MB) with the
/// production fetcher, then lock + hash. Evidence type (ج): `#[ignore]`, run on purpose only
/// (`cargo test -p eid-media --test g2_tools -- --ignored real_tool_install`), result recorded
/// in docs/STATUS.md.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real internet (ج): downloads ~230 MB from github.com"]
async fn real_tool_install_from_official_release() {
    let d = tempfile::tempdir().unwrap();
    let m = Manifest::embedded();
    let store = ToolStore::new(d.path().join("tools"));
    let paths = store.install(&m, &EngineFetcher::default()).await.unwrap();
    let locked = store.lock_blocking(&m).await.unwrap();
    assert_eq!(locked.locked_files(), 4);
    assert_eq!(locked.paths, paths);
    println!(
        "installed and verified: {}",
        paths.ytdlp.parent().unwrap().display()
    );
}
