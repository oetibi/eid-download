//! `eid video info|get|consent|revoke` (P4 G3): video and audio through yt-dlp, standalone in
//! this process only (D7: not reachable through IPC or Native Messaging).
//!
//! Order of every run: consent (nothing is downloaded or started before it) → tools installed
//! (pinned manifest, verified) → tools locked and hashed through their handles (D11) → yt-dlp
//! runs with a fresh or resumed staging folder as its working directory → we publish the
//! finished file ourselves under a sanitized name → the staging folder is removed.

use eid_media::args::{MediaUrl, Quality};
use eid_media::consent::{self, Lang};
use eid_media::fetch::Fetcher;
use eid_media::hints::Hint;
use eid_media::manifest::Manifest;
use eid_media::run::Runner;
use eid_media::settings::SettingsStore;
use eid_media::tools::ToolStore;
use eid_media::MediaError;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

pub const HELP: &str = "eid video - video and audio through yt-dlp (after your consent)\n\n\
USAGE:\n  eid video info <page-url> [--yes-i-agree] [--lang ar|en]\n\
  eid video get <page-url> [-d DIR] [--quality best|audio|720p] [--limit RATE] [--yes-i-agree] [--lang ar|en] [-q]\n\
  eid video consent [--lang ar|en]   show the consent text and whether it was given\n\
  eid video revoke                   withdraw consent and delete the video tools\n\n\
The first use asks for consent (or takes --yes-i-agree) and then downloads yt-dlp, FFmpeg and\n\
Deno from their official releases, checked against the SHA-256 pinned in this program.\n\
Without a terminal and without --yes-i-agree nothing is downloaded or started.\n\
Ctrl-C stops the tool and everything it started; running the same command again continues.";

/// Exit code when consent is missing.
pub const EXIT_NO_CONSENT: i32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sub {
    Info,
    Get,
    Consent,
    Revoke,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoArgs {
    pub sub: Sub,
    pub url: Option<String>,
    pub dir: Option<PathBuf>,
    pub quality: Quality,
    pub limit_bps: Option<u64>,
    pub yes_i_agree: bool,
    pub lang: Lang,
    pub quiet: bool,
}

fn parse_quality(s: &str) -> Result<Quality, String> {
    match s {
        "best" => Ok(Quality::Best),
        "audio" | "mp3" => Ok(Quality::AudioMp3),
        _ => {
            let n = s.strip_suffix('p').unwrap_or(s);
            n.parse::<u32>()
                .ok()
                .filter(|h| (1..=eid_media::args::MAX_HEIGHT).contains(h))
                .map(Quality::MaxHeight)
                .ok_or_else(|| format!("bad quality '{s}' (best, audio, or a height like 720p)"))
        }
    }
}

/// `argv` without the leading `video`.
pub fn parse(argv: &[String]) -> Result<VideoArgs, String> {
    let mut it = argv.iter();
    let sub = match it.next().map(String::as_str) {
        Some("info") => Sub::Info,
        Some("get") => Sub::Get,
        Some("consent") => Sub::Consent,
        Some("revoke") => Sub::Revoke,
        other => {
            return Err(format!(
                "expected info, get, consent or revoke, got {other:?}"
            ))
        }
    };
    let mut a = VideoArgs {
        sub,
        url: None,
        dir: None,
        quality: Quality::Best,
        limit_bps: None,
        yes_i_agree: false,
        lang: Lang::En,
        quiet: false,
    };
    while let Some(x) = it.next() {
        match x.as_str() {
            "-d" => a.dir = Some(PathBuf::from(it.next().ok_or("-d needs a folder")?)),
            "--quality" => a.quality = parse_quality(it.next().ok_or("--quality needs a value")?)?,
            "--limit" => {
                a.limit_bps = Some(crate::args::parse_rate(
                    it.next().ok_or("--limit needs a value")?,
                )?)
            }
            "--yes-i-agree" => a.yes_i_agree = true,
            "--lang" => {
                a.lang = match it.next().map(String::as_str) {
                    Some("ar") => Lang::Ar,
                    Some("en") => Lang::En,
                    _ => return Err("--lang must be ar or en".into()),
                }
            }
            "-q" => a.quiet = true,
            // `--` ends the options: everything after it is the page URL
            "--" => {
                a.url = Some(it.next().ok_or("an address must follow --")?.clone());
                if it.next().is_some() {
                    return Err("only one address".into());
                }
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s => {
                if a.url.replace(s.to_string()).is_some() {
                    return Err("only one address".into());
                }
            }
        }
    }
    let needs_url = matches!(a.sub, Sub::Info | Sub::Get);
    if needs_url != a.url.is_some() {
        return Err(if needs_url {
            "a page address is required".into()
        } else {
            "this command takes no address".into()
        });
    }
    if a.sub != Sub::Get && (a.dir.is_some() || a.limit_bps.is_some()) {
        return Err("-d and --limit only apply to `eid video get`".into());
    }
    Ok(a)
}

// ------------------------------------------------------------------ test hooks

/// The pinned manifest. Builds with the `test-hooks` feature (tests only, never a release)
/// may replace it with `EID_TEST_MANIFEST` (a file path).
fn manifest() -> Result<Manifest, String> {
    #[cfg(feature = "test-hooks")]
    if let Some(p) = std::env::var_os("EID_TEST_MANIFEST") {
        let t = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
        return Manifest::parse(&t).map_err(|e| e.to_string());
    }
    Ok(Manifest::embedded())
}

/// The tool fetcher: the engine with the tool policy. With the `test-hooks` feature,
/// `EID_TEST_TOOLS_BASE=http://127.0.0.1:PORT` serves the assets (by name) from the local
/// test server instead, still through the engine with the same size and hash checks.
fn fetcher() -> Box<dyn Fetcher> {
    #[cfg(feature = "test-hooks")]
    if let Ok(base) = std::env::var("EID_TEST_TOOLS_BASE") {
        return Box::new(test_hooks::LocalFetcher { base });
    }
    Box::new(eid_media::fetch::EngineFetcher::default())
}

#[cfg(feature = "test-hooks")]
mod test_hooks {
    use eid_media::fetch::{engine_fetch, BoxFuture, FetchRequest, Fetcher};
    use eid_media::MediaError;
    use std::path::PathBuf;

    pub struct LocalFetcher {
        pub base: String,
    }

    impl Fetcher for LocalFetcher {
        fn fetch<'a>(
            &'a self,
            req: FetchRequest<'a>,
        ) -> BoxFuture<'a, Result<PathBuf, MediaError>> {
            Box::pin(async move {
                if !self.base.starts_with("http://127.0.0.1:") {
                    return Err(MediaError::Fetch(
                        "test base must be the local test server".into(),
                    ));
                }
                let asset = req.url.rsplit('/').next().unwrap_or("");
                let url = format!("{}/{asset}", self.base);
                let hosts = vec!["127.0.0.1".to_string()];
                let local = FetchRequest {
                    url: &url,
                    allowed_hosts: &hosts,
                    ..req
                };
                let cfg = eid_engine::Config {
                    connections: 1,
                    use_system_proxy: false,
                    ..eid_engine::Config::default()
                };
                engine_fetch(cfg, &local).await
            })
        }
    }
}

// ------------------------------------------------------------------ steps

fn stores() -> Result<(SettingsStore, ToolStore), String> {
    let s = SettingsStore::default_location()
        .ok_or("cannot find the configuration folder (APPDATA / XDG_CONFIG_HOME / HOME)")?;
    let t = ToolStore::default_location()
        .ok_or("cannot find the local data folder (LOCALAPPDATA / XDG_DATA_HOME / HOME)")?;
    Ok((s, t))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Consent, before anything is downloaded or started. Without a terminal and without
/// `--yes-i-agree` this refuses immediately: it never waits for input that cannot come.
async fn ensure_consent(a: &VideoArgs, settings: &SettingsStore, m: &Manifest) -> Result<(), Fail> {
    if consent::is_granted(settings, m).map_err(Fail::media)? {
        return Ok(());
    }
    let text = consent::consent_text(a.lang, m);
    if a.yes_i_agree {
        eprintln!("{}\n", text.text);
        consent::grant(settings, &text, now_secs()).map_err(Fail::media)?;
        eprintln!("consent recorded (--yes-i-agree)");
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(Fail::NoConsent(
            "video needs your consent first. Read it with `eid video consent`, then run \
             again in a terminal or add --yes-i-agree. Nothing was downloaded or started."
                .into(),
        ));
    }
    eprintln!("{}\n", text.text);
    eprint!("Type yes to agree: ");
    let answer = tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).map(|_| s)
    })
    .await
    .map_err(|e| Fail::Other(e.to_string()))?
    .map_err(|e| Fail::Other(e.to_string()))?;
    if answer.trim().eq_ignore_ascii_case("yes") {
        consent::grant(settings, &text, now_secs()).map_err(Fail::media)?;
        Ok(())
    } else {
        Err(Fail::NoConsent(
            "consent not given; nothing was downloaded or started".into(),
        ))
    }
}

async fn ensure_tools(
    a: &VideoArgs,
    tools: &ToolStore,
    m: &Manifest,
) -> Result<eid_media::tools::LockedTools, Fail> {
    match tools.current(m) {
        Ok(_) => {}
        Err(MediaError::NotInstalled) => {
            if !a.quiet {
                eprintln!("installing the video tools (pinned versions, checked by SHA-256)...");
            }
            tools
                .install(m, fetcher().as_ref())
                .await
                .map_err(Fail::media)?;
        }
        Err(e) => return Err(Fail::media(e)),
    }
    tools.lock_blocking(m).await.map_err(Fail::media)
}

/// How a run failed.
#[derive(Debug)]
pub enum Fail {
    NoConsent(String),
    Cancelled(String),
    Media(MediaError),
    Other(String),
}

impl Fail {
    fn media(e: MediaError) -> Fail {
        Fail::Media(e)
    }
}

/// What the user is told for a classified tool error (English, like every CLI message).
pub fn hint_text(h: Hint) -> &'static str {
    match h {
        Hint::Drm => "this media is DRM-protected: refused. Eid Download does not get around any protection.",
        Hint::BotCheck => "the site asks to prove you are not a bot. Signing in is not supported; try later or from another network.",
        Hint::SignInRequired => "this media needs signing in (age, membership or private access); that is not supported.",
        Hint::Unavailable => "the media is private, removed or otherwise unavailable.",
        Hint::RateLimited => "the site is limiting requests (HTTP 429); wait a while and try again.",
        Hint::Geo => "the media is not available in your country.",
        Hint::MissingJsRuntime => "yt-dlp could not use its JavaScript runtime (deno); reinstall the tools with `eid video revoke` and run again.",
        Hint::Unsupported => "yt-dlp does not support this site.",
    }
}

fn describe(e: &MediaError) -> String {
    match e {
        MediaError::ToolFailed { hint, message } => match hint {
            Some(h) => format!(
                "{}\n  tool said: {}",
                hint_text(*h),
                crate::remote::printable(message)
            ),
            None => format!("the tool failed: {}", crate::remote::printable(message)),
        },
        other => crate::remote::printable(&other.to_string()),
    }
}

// ------------------------------------------------------------------ staging and publishing

/// Name of the staging folder of one (address, quality) pair inside the output folder: the
/// same command finds and continues the same partial download.
pub fn staging_name(url: &MediaUrl, q: &Quality) -> String {
    use sha2::{Digest, Sha256};
    let key = format!("{}\n{q:?}", url.as_str());
    let d = Sha256::digest(key.as_bytes());
    let hex: String = d[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!(".eid-video-{hex}")
}

fn is_link_or_odd(md: &std::fs::Metadata) -> bool {
    md.file_type().is_symlink() || !(md.is_dir() || md.is_file())
}

/// Create the staging folder, or reuse an existing one when it is a real folder containing only
/// real files. A link or junction anywhere here is refused, never followed or "fixed".
pub fn prepare_staging(out_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let p = out_dir.join(name);
    match std::fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        }
        Err(e) => return Err(format!("cannot inspect {}: {e}", p.display())),
        Ok(md) if md.file_type().is_symlink() || !md.is_dir() => {
            return Err(format!(
                "refusing to use {}: it is a link, junction or file, not a folder we created",
                p.display()
            ))
        }
        Ok(_) => {}
    }
    // a reused folder (resume) must contain nothing but plain files and folders
    for e in std::fs::read_dir(&p).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let md = std::fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
        if is_link_or_odd(&md) {
            return Err(format!(
                "refusing to use {}: it contains a link ({})",
                p.display(),
                e.file_name().to_string_lossy()
            ));
        }
    }
    Ok(p)
}

/// The finished file in the staging folder: exactly one plain file with the expected extension.
fn finished_file(staging: &Path, ext: &str) -> Result<PathBuf, String> {
    let mut found = vec![];
    for e in std::fs::read_dir(staging).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let md = std::fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
        let n = e.file_name().to_string_lossy().into_owned();
        if md.is_file() && n.ends_with(&format!(".{ext}")) {
            found.push(e.path());
        }
    }
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("the tool finished but left no .{ext} file")),
        _ => Err(format!(
            "the tool left {} .{ext} files; refusing to guess",
            found.len()
        )),
    }
}

/// Longest staging path we allow for yt-dlp's own files (`<id>.f137.mp4.part` and friends).
const TOOL_NAME_RESERVE: usize = 96;

fn units(p: &Path) -> usize {
    p.as_os_str().to_string_lossy().encode_utf16().count()
}

/// The published name: the sanitized title with our extension, fitted to the path limit.
pub fn publish_name(title: &str, ext: &str, out_dir: &Path) -> Result<String, String> {
    let title = if title.trim().trim_matches('.').is_empty() {
        "video"
    } else {
        title
    };
    let base = eid_engine::sanitize::sanitize_file_name(&format!("{title}.{ext}"));
    match eid_platform::MAX_PATH_UNITS {
        Some(limit) => eid_engine::sanitize::fit_name_to_path_limit(&base, units(out_dir), limit),
        None => Ok(base),
    }
}

/// Publish `staged` (opened without following links and checked to be a plain file) into
/// `out_dir` under `name`, or `name (n)` if taken; never overwrites anything.
fn publish(staged: &Path, out_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let f = eid_platform::fs::open_regular_nofollow(staged)
        .map_err(|e| format!("refusing the staged file: {e}"))?;
    for n in 0..1000u32 {
        let cand = if n == 0 {
            name.to_string()
        } else {
            eid_engine::sanitize::numbered_name(name, n)
        };
        let dest = out_dir.join(&cand);
        match eid_platform::fs::publish_verified(&f, staged, &dest) {
            Ok(()) => return Ok(dest),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot publish {}: {e}", dest.display())),
        }
    }
    Err("no free file name (1000 taken)".into())
}

// ------------------------------------------------------------------ commands

/// Run a video command; returns the exit code. Messages go to stderr, the final path (get) or
/// the information (info) to stdout.
pub async fn run(a: VideoArgs, cancel: eid_engine::CancellationToken) -> i32 {
    match run_inner(&a, cancel).await {
        Ok(()) => 0,
        Err(Fail::NoConsent(m)) => {
            eprintln!("error: {m}");
            EXIT_NO_CONSENT
        }
        Err(Fail::Cancelled(m)) => {
            eprintln!("{m}");
            130
        }
        Err(Fail::Media(e)) => {
            eprintln!("error: {}", describe(&e));
            1
        }
        Err(Fail::Other(m)) => {
            eprintln!("error: {}", crate::remote::printable(&m));
            1
        }
    }
}

async fn run_inner(a: &VideoArgs, cancel: eid_engine::CancellationToken) -> Result<(), Fail> {
    let m = manifest().map_err(Fail::Other)?;
    let (settings, tools) = stores().map_err(Fail::Other)?;
    match a.sub {
        Sub::Consent => {
            let t = consent::consent_text(a.lang, &m);
            println!("{}", t.text);
            let given = consent::is_granted(&settings, &m).map_err(Fail::media)?;
            println!("\nconsent: {}", if given { "given" } else { "not given" });
            return Ok(());
        }
        Sub::Revoke => {
            consent::revoke(&settings, &tools).map_err(Fail::media)?;
            println!("consent withdrawn; video tools deleted");
            return Ok(());
        }
        _ => {}
    }
    let url = MediaUrl::parse(a.url.as_deref().unwrap_or("")).map_err(Fail::media)?;
    ensure_consent(a, &settings, &m).await?;
    let out_dir = match (&a.sub, &a.dir) {
        (Sub::Get, Some(d)) => d.clone(),
        (Sub::Get, None) => eid_platform::dirs::default_download_dir()
            .ok_or_else(|| Fail::Other("no download folder; pass -d DIR".into()))?,
        _ => PathBuf::new(),
    };
    if a.sub == Sub::Get {
        let ok = std::fs::metadata(&out_dir).is_ok_and(|m| m.is_dir());
        if !ok {
            return Err(Fail::Other(format!("not a folder: {}", out_dir.display())));
        }
    }
    let locked = ensure_tools(a, &tools, &m).await?;
    let runner = Runner::new(locked.paths.clone(), |k| std::env::var_os(k)).map_err(Fail::media)?;
    match a.sub {
        Sub::Info => {
            let work = tools.root().join(format!(".info-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&work);
            std::fs::create_dir(&work).map_err(|e| Fail::Other(e.to_string()))?;
            let r = tokio::select! {
                r = runner.info(&url, &work) => r,
                _ = cancel.cancelled() => {
                    let _ = std::fs::remove_dir_all(&work);
                    return Err(Fail::Cancelled("stopped".into()));
                }
            };
            let _ = std::fs::remove_dir_all(&work);
            let i = r.map_err(Fail::media)?;
            let p = crate::remote::printable;
            println!("id: {}", p(&i.id));
            println!("title: {}", p(&i.title));
            if let Some(d) = i.duration_secs {
                println!("duration: {d} s");
            }
            for f in &i.formats {
                println!(
                    "  {:<12} {:<5} {:>6} {}{}",
                    p(&f.format_id),
                    p(&f.ext),
                    f.height.map(|h| format!("{h}p")).unwrap_or_default(),
                    if f.has_video { "video " } else { "" },
                    if f.has_audio { "audio" } else { "" }
                );
            }
            drop(locked);
            Ok(())
        }
        Sub::Get => get(a, &url, &out_dir, &runner, locked, cancel).await,
        _ => unreachable!(),
    }
}

async fn get(
    a: &VideoArgs,
    url: &MediaUrl,
    out_dir: &Path,
    runner: &Runner,
    locked: eid_media::tools::LockedTools,
    cancel: eid_engine::CancellationToken,
) -> Result<(), Fail> {
    let staging_n = staging_name(url, &a.quality);
    if let Some(limit) = eid_platform::MAX_PATH_UNITS {
        let need = units(out_dir) + 1 + staging_n.len() + 1 + TOOL_NAME_RESERVE;
        if need > limit {
            return Err(Fail::Other(format!(
                "the download folder path is too long ({} characters) for video: Windows \
                 accepts at most {limit} characters per path; choose a shorter folder",
                units(out_dir)
            )));
        }
    }
    let staging = prepare_staging(out_dir, &staging_n).map_err(Fail::Other)?;
    // a staging folder that holds nothing (yet) is never left behind on failure
    let drop_if_empty = |s: &Path| {
        if std::fs::read_dir(s).is_ok_and(|mut d| d.next().is_none()) {
            let _ = std::fs::remove_dir(s);
        }
    };
    let info = match runner.info(url, &staging).await {
        Ok(i) => i,
        Err(e) => {
            drop_if_empty(&staging);
            return Err(Fail::media(e));
        }
    };
    let ext = if a.quality == Quality::AudioMp3 {
        "mp3"
    } else {
        "mp4"
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let quiet = a.quiet;
    let printer = tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let p: eid_media::progress::Progress = p;
            if quiet {
                continue;
            }
            match (p.downloaded, p.total) {
                (Some(d), Some(t)) if t > 0 => {
                    eprintln!("  {d}/{t} ({}%)", d.saturating_mul(100) / t)
                }
                (Some(d), _) => eprintln!("  {d} bytes"),
                _ => {}
            }
        }
    });
    let r = tokio::select! {
        r = runner.download(url, &staging, a.quality.clone(), a.limit_bps, false, Some(tx)) => r,
        // dropping the download future kills yt-dlp and everything it started (job / group)
        _ = cancel.cancelled() => {
            drop(locked);
            let _ = printer.await;
            return Err(Fail::Cancelled(format!(
                "stopped; the partial download is kept in {}. Run the same command again to \
                 continue.",
                staging.display()
            )));
        }
    };
    let _ = printer.await;
    drop(locked);
    if let Err(e) = r {
        // keep a staging folder that holds partial data (resume); drop an empty one
        drop_if_empty(&staging);
        return Err(Fail::media(e));
    }
    let staged = finished_file(&staging, ext).map_err(Fail::Other)?;
    let name = publish_name(&info.title, ext, out_dir).map_err(Fail::Other)?;
    let dest = publish(&staged, out_dir, &name).map_err(Fail::Other)?;
    let _ = std::fs::remove_dir_all(&staging);
    println!("{}", dest.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Result<VideoArgs, String> {
        parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn video_args_parse_and_reject() {
        let a = v(&[
            "get",
            "https://x.test/v",
            "-d",
            "out",
            "--quality",
            "720p",
            "--limit",
            "2M",
        ])
        .unwrap();
        assert_eq!(a.quality, Quality::MaxHeight(720));
        assert_eq!(a.limit_bps, Some(2 * 1024 * 1024));
        assert!(!a.yes_i_agree);
        assert_eq!(
            v(&["get", "--", "-https://x"]).unwrap().url.as_deref(),
            Some("-https://x")
        );
        assert_eq!(
            v(&["info", "u", "--quality", "audio"]).unwrap().quality,
            Quality::AudioMp3
        );
        assert!(v(&["get", "u", "--yes-i-agree"]).unwrap().yes_i_agree);
        for bad in [
            vec!["get"],
            vec!["info", "u", "-d", "x"],
            vec!["get", "a", "b"],
            vec!["get", "u", "--quality", "0p"],
            vec!["get", "u", "--quality", "$(x)"],
            vec!["get", "u", "--yes"],
            vec!["get", "u", "--exec", "calc"],
            vec!["consent", "u"],
            vec!["nope"],
        ] {
            assert!(v(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn hostile_titles_become_safe_names_inside_the_folder() {
        let d = std::env::temp_dir();
        for t in [
            "../../evil",
            "CON",
            "a\u{202E}4pm.exe",
            "x:ads",
            "/abs",
            "a\\b",
            "...",
            "",
        ] {
            let n = publish_name(t, "mp4", &d).unwrap();
            assert!(!n.contains(['/', '\\', ':']), "{t:?} -> {n:?}");
            assert!(!n.contains('\u{202E}'), "{t:?} -> {n:?}");
            assert!(n.ends_with(".mp4") || !n.is_empty(), "{t:?} -> {n:?}");
            assert_eq!(d.join(&n).parent().unwrap(), d, "{t:?} escapes: {n:?}");
        }
    }
}
