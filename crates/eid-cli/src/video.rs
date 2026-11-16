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
  eid video playlist <playlist-url> [-d DIR] [--quality Q] [--limit RATE] [--skip 2,5] [--retry-failed 3,7|all] [--list] [--yes-i-agree] [-q]
\n                                     every item, numbered 01 - ... in a folder named after the playlist;
\n                                     an item that failed 3 runs in a row, or that the site refused
\n                                     explicitly (private, removed, sign-in, country, DRM), is not
\n                                     tried again automatically: --retry-failed tries it again
\n  eid video consent [--lang ar|en]   show the consent text and whether it was given\n\
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
    Playlist,
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
    /// Playlist item numbers NOT to download (F11a(1): everything else is selected).
    pub skip: Vec<u32>,
    /// Playlist: print the numbered plan and stop.
    pub list_only: bool,
    /// Playlist: try these failed or unavailable items again from zero (owner decision M2).
    pub retry_failed: Option<RetrySel>,
}

/// Which items `--retry-failed` resets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetrySel {
    All,
    Numbers(Vec<u32>),
}

impl RetrySel {
    fn covers(&self, n: u32) -> bool {
        match self {
            RetrySel::All => true,
            RetrySel::Numbers(v) => v.contains(&n),
        }
    }
}

/// `2,5` → item numbers (each > 0).
fn parse_numbers(s: &str) -> Result<Vec<u32>, String> {
    s.split(',')
        .map(|n| {
            n.trim()
                .parse()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("bad item number {n:?}"))
        })
        .collect()
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
        Some("playlist") => Sub::Playlist,
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
        skip: vec![],
        list_only: false,
        retry_failed: None,
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
            "--list" => a.list_only = true,
            "--skip" => a.skip.extend(parse_numbers(
                it.next().ok_or("--skip needs item numbers, e.g. 2,5")?,
            )?),
            "--retry-failed" => {
                let v = it
                    .next()
                    .ok_or("--retry-failed needs item numbers (e.g. 3,7) or all")?;
                a.retry_failed = Some(if v == "all" {
                    RetrySel::All
                } else {
                    RetrySel::Numbers(parse_numbers(v)?)
                });
            }
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
    let needs_url = matches!(a.sub, Sub::Info | Sub::Get | Sub::Playlist);
    if needs_url != a.url.is_some() {
        return Err(if needs_url {
            "a page address is required".into()
        } else {
            "this command takes no address".into()
        });
    }
    if (a.sub != Sub::Playlist) && (!a.skip.is_empty() || a.list_only || a.retry_failed.is_some()) {
        return Err("--skip, --list and --retry-failed only apply to `eid video playlist`".into());
    }
    if a.list_only && a.retry_failed.is_some() {
        return Err("--list only shows the plan; it cannot be combined with --retry-failed".into());
    }
    if !matches!(a.sub, Sub::Get | Sub::Playlist) && (a.dir.is_some() || a.limit_bps.is_some()) {
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

    /// `EID_TEST_CANCEL_FILE=<path>`: cancel the run (exactly what Ctrl-C does) once that file
    /// exists. A real console Ctrl-C cannot be sent to a child on Windows CI (MANUAL-TESTS #14),
    /// so the tests use this on Windows; on Unix they send a real SIGINT.
    pub fn cancel_when_file_appears(cancel: &eid_engine::CancellationToken) {
        let Some(p) = std::env::var_os("EID_TEST_CANCEL_FILE") else {
            return;
        };
        let c = cancel.clone();
        tokio::spawn(async move {
            loop {
                if std::path::Path::new(&p).exists() {
                    eprintln!("stopping the tool and everything it started...");
                    c.cancel();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        });
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
/// `--yes-i-agree` this refuses immediately: it never waits for input that cannot come. Ctrl-C
/// while the question waits stops at once (the blocked read is left behind; the process exits).
async fn ensure_consent(
    a: &VideoArgs,
    settings: &SettingsStore,
    m: &Manifest,
    cancel: &eid_engine::CancellationToken,
) -> Result<(), Fail> {
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
    let read = tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).map(|_| s)
    });
    let answer = tokio::select! {
        r = read => r
            .map_err(|e| Fail::Other(e.to_string()))?
            .map_err(|e| Fail::Other(e.to_string()))?,
        _ = cancel.cancelled() => {
            return Err(Fail::Cancelled(
                "stopped; consent was not given and nothing was downloaded or started".into(),
            ))
        }
    };
    if answer.trim().eq_ignore_ascii_case("yes") {
        consent::grant(settings, &text, now_secs()).map_err(Fail::media)?;
        Ok(())
    } else {
        Err(Fail::NoConsent(
            "consent not given; nothing was downloaded or started".into(),
        ))
    }
}

/// Install (if needed) and lock the tools. Ctrl-C during the install cancels the download and
/// waits until its staging folder is removed; during the hash check it stops at once (the
/// blocking hash ends on its own and its handles close when the process exits).
async fn ensure_tools(
    a: &VideoArgs,
    tools: &ToolStore,
    m: &Manifest,
    cancel: &eid_engine::CancellationToken,
) -> Result<eid_media::tools::LockedTools, Fail> {
    let stopped = || {
        Fail::Cancelled(
            "stopped; the video tools were not installed or changed. Run the same command \
             again to start over."
                .into(),
        )
    };
    match tools.current(m) {
        Ok(_) => {}
        Err(MediaError::NotInstalled) => {
            if !a.quiet {
                eprintln!("installing the video tools (pinned versions, checked by SHA-256)...");
            }
            match tools
                .install_cancellable(m, fetcher().as_ref(), cancel)
                .await
            {
                Ok(_) => {}
                Err(MediaError::Cancelled) => return Err(stopped()),
                // a failure caused by the cancel itself is reported as a stop
                Err(_) if cancel.is_cancelled() => return Err(stopped()),
                Err(e) => return Err(Fail::media(e)),
            }
        }
        Err(e) => return Err(Fail::media(e)),
    }
    tokio::select! {
        r = tools.lock_blocking(m) => r.map_err(Fail::media),
        _ = cancel.cancelled() => Err(stopped()),
    }
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
        Hint::Private => "the media is private; signing in is not supported.",
        Hint::Removed => "the media was removed (by the uploader, or its account was closed).",
        Hint::Unavailable => "the site says the media is unavailable without giving a reason; this can be temporary (for example while the site limits requests).",
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

/// Remove a staging folder that is still empty after a cancel. The killed tree dies
/// asynchronously (Windows `TerminateJobObject`), and until it has, the tool's working
/// directory cannot be removed, so the removal is retried for up to 2 s (AGENTS 6.9). A folder
/// that is not empty (partial data, for resume) is never touched.
async fn remove_empty_staging(s: &Path) {
    for _ in 0..100 {
        if !std::fs::read_dir(s).is_ok_and(|mut d| d.next().is_none()) {
            return;
        }
        if std::fs::remove_dir(s).is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
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
    #[cfg(feature = "test-hooks")]
    test_hooks::cancel_when_file_appears(&cancel);
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
    ensure_consent(a, &settings, &m, &cancel).await?;
    let out_dir = match (&a.sub, &a.dir) {
        (Sub::Get | Sub::Playlist, Some(d)) => d.clone(),
        (Sub::Get | Sub::Playlist, None) => eid_platform::dirs::default_download_dir()
            .ok_or_else(|| Fail::Other("no download folder; pass -d DIR".into()))?,
        _ => PathBuf::new(),
    };
    if matches!(a.sub, Sub::Get | Sub::Playlist) {
        let ok = std::fs::metadata(&out_dir).is_ok_and(|m| m.is_dir());
        if !ok {
            return Err(Fail::Other(format!("not a folder: {}", out_dir.display())));
        }
    }
    let locked = ensure_tools(a, &tools, &m, &cancel).await?;
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
        Sub::Playlist => playlist(a, &url, &out_dir, &tools, &runner, locked, cancel).await,
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
    // dropping the `-J` future on Ctrl-C kills yt-dlp's whole tree (job / process group)
    let r = tokio::select! {
        r = runner.info(url, &staging) => r,
        _ = cancel.cancelled() => {
            remove_empty_staging(&staging).await;
            return Err(Fail::Cancelled(
                "stopped while reading the media information; nothing was downloaded".into(),
            ));
        }
    };
    let info = match r {
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

/// Playlist folder inside `out_dir`, named after the sanitized playlist title (F11a(2)).
pub fn playlist_folder(out_dir: &Path, title: &str) -> Result<PathBuf, String> {
    let t = if title.trim().trim_matches('.').is_empty() {
        "playlist"
    } else {
        title
    };
    let mut name = eid_engine::sanitize::sanitize_file_name(t);
    if name.starts_with('.') {
        name = format!("_{name}");
    }
    if let Some(limit) = eid_platform::MAX_PATH_UNITS {
        // room for the folder, a staging folder, and yt-dlp's own file names inside it
        let reserve = 1 + 27 + 1 + TOOL_NAME_RESERVE;
        name = eid_engine::sanitize::fit_name_to_path_limit(
            &name,
            units(out_dir),
            limit.saturating_sub(reserve),
        )?;
    }
    let p = out_dir.join(&name);
    match std::fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&p).map_err(|e| format!("cannot create {}: {e}", p.display()))?
        }
        Err(e) => return Err(format!("cannot inspect {}: {e}", p.display())),
        Ok(md) if md.file_type().is_symlink() || !md.is_dir() => {
            return Err(format!(
                "refusing to use {}: it is a link, junction or file, not a folder",
                p.display()
            ))
        }
        Ok(_) => {}
    }
    Ok(p)
}

/// Items yt-dlp reports as not downloadable without signing in (skipped, number kept).
fn unavailable(availability: Option<&str>) -> bool {
    matches!(
        availability,
        Some("private" | "needs_auth" | "subscriber_only" | "premium_only")
    )
}

async fn playlist(
    a: &VideoArgs,
    url: &MediaUrl,
    out_dir: &Path,
    tools: &ToolStore,
    runner: &Runner,
    locked: eid_media::tools::LockedTools,
    cancel: eid_engine::CancellationToken,
) -> Result<(), Fail> {
    use eid_media::playlist::{self as pl, Status};
    let work = tools.root().join(format!(".list-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir(&work).map_err(|e| Fail::Other(e.to_string()))?;
    let listed = tokio::select! {
        r = runner.playlist(url, &work) => r,
        _ = cancel.cancelled() => Err(MediaError::Playlist("stopped".into())),
    };
    let _ = std::fs::remove_dir_all(&work);
    let info = listed.map_err(Fail::media)?;
    let folder = playlist_folder(out_dir, &info.title).map_err(Fail::Other)?;
    let _lock = pl::lock_folder(&folder).map_err(Fail::media)?;
    let old = pl::load(&folder).map_err(Fail::media)?;
    let (mut state, planned) = pl::plan(old, &info.id, &info.entries).map_err(Fail::media)?;
    let p = crate::remote::printable;
    // `--retry-failed`: only the named items, and only if they failed or are unavailable
    if let Some(sel) = &a.retry_failed {
        let mut reset = vec![];
        for (n, e) in &planned {
            if sel.covers(*n) {
                if let Some(i) = state.get_mut(&e.id) {
                    if i.reset_for_retry() {
                        reset.push(*n);
                    }
                }
            }
        }
        if let RetrySel::Numbers(v) = sel {
            for n in v.iter().filter(|n| !reset.contains(n)) {
                eprintln!(
                    "{n}: not a failed or unavailable item of this playlist; nothing to retry"
                );
            }
        }
        if !a.quiet && !reset.is_empty() {
            eprintln!("trying again: {reset:?}");
        }
    }
    if a.list_only {
        for (n, e) in &planned {
            let item = state.get(&e.id);
            let tag = match item.map(|i| (i.status, i.attempts)) {
                Some((Status::Done, _)) => "done".to_string(),
                Some((Status::Unavailable, _)) => "unavailable".to_string(),
                Some((Status::Failed, _)) => "failed".to_string(),
                _ if a.skip.contains(n) => "deselected".to_string(),
                Some((_, k)) if k > 0 => format!("retry {}/{}", k + 1, pl::MAX_ATTEMPTS),
                _ => "to download".to_string(),
            };
            println!(
                "{n:0w$}  {tag:<12} {}",
                p(&e.title),
                w = state.width as usize
            );
        }
        return Ok(());
    }
    // numbers are fixed from here on (D8)
    pl::save(&folder, &state).map_err(Fail::media)?;
    let ext = if a.quality == Quality::AudioMp3 {
        "mp3"
    } else {
        "mp4"
    };
    let (mut got, mut skipped, mut gone, mut failed) = (0u32, 0u32, 0u32, 0u32);
    let mut gave_up: Vec<u32> = vec![];
    let empty = |s: &Path| std::fs::read_dir(s).is_ok_and(|mut d| d.next().is_none());
    // a temporary failure of item `id`: counted, and after MAX_ATTEMPTS no automatic retry
    let temporary = |state: &mut pl::State, id: &str, n: u32| {
        if let Some(i) = state.get_mut(id) {
            if i.record_failure() {
                eprintln!(
                    "{n}: failed {} runs in a row; not retried automatically any more. Try \
                     again with --retry-failed {n}",
                    pl::MAX_ATTEMPTS
                );
            } else {
                eprintln!(
                    "{n}: attempt {} of {} failed; it is tried again at the next run",
                    i.attempts,
                    pl::MAX_ATTEMPTS
                );
            }
        }
    };
    for (n, e) in &planned {
        let status = state.get(&e.id).map(|i| i.status);
        if status == Some(Status::Done) {
            continue;
        }
        if status == Some(Status::Failed) {
            gave_up.push(*n);
            continue;
        }
        // an explicit restriction, in the listing or recorded earlier: never asked again
        // automatically, never worked around
        if status == Some(Status::Unavailable) || unavailable(e.availability.as_deref()) {
            if let Some(i) = state.get_mut(&e.id) {
                i.status = Status::Unavailable;
                i.attempts = 0;
            }
            pl::save(&folder, &state).map_err(Fail::media)?;
            gone += 1;
            if !a.quiet {
                eprintln!(
                    "{n}: unavailable (private, removed, sign-in only, other country or DRM); \
                     skipped, number kept. If that changes: --retry-failed {n}"
                );
            }
            continue;
        }
        if a.skip.contains(n) {
            skipped += 1;
            continue;
        }
        let item_url = match e.url.as_deref().map(MediaUrl::parse) {
            Some(Ok(u)) => u,
            _ => {
                failed += 1;
                eprintln!("{n}: the playlist gave no usable address for this item");
                continue;
            }
        };
        if !a.quiet {
            eprintln!("{n}: {}", p(&e.title));
        }
        let staging =
            prepare_staging(&folder, &staging_name(&item_url, &a.quality)).map_err(Fail::Other)?;
        let r = tokio::select! {
            r = runner.download(&item_url, &staging, a.quality.clone(), a.limit_bps, true, None) => r,
            _ = cancel.cancelled() => {
                drop(locked);
                return Err(Fail::Cancelled(format!(
                    "stopped at item {n}; finished items are kept. Run the same command again \
                     to continue."
                )));
            }
        };
        match r {
            Ok(_) => match finished_file(&staging, ext) {
                Ok(staged) => {
                    let name = pl::item_name(state.width, *n, &e.title, ext);
                    let name = match eid_platform::MAX_PATH_UNITS {
                        Some(l) => {
                            eid_engine::sanitize::fit_name_to_path_limit(&name, units(&folder), l)
                                .map_err(Fail::Other)?
                        }
                        None => name,
                    };
                    let dest = publish(&staged, &folder, &name).map_err(Fail::Other)?;
                    let _ = std::fs::remove_dir_all(&staging);
                    if let Some(i) = state.get_mut(&e.id) {
                        i.status = Status::Done;
                        i.attempts = 0;
                        i.file = dest.file_name().map(|f| f.to_string_lossy().into_owned());
                    }
                    got += 1;
                }
                // nothing new: yt-dlp skipped it because the archive already lists it
                Err(_) if empty(&staging) => {
                    let _ = std::fs::remove_dir(&staging);
                    if let Some(i) = state.get_mut(&e.id) {
                        i.status = Status::Done;
                        i.attempts = 0;
                    }
                }
                Err(m) => {
                    failed += 1;
                    eprintln!("{n}: {m}");
                    temporary(&mut state, &e.id, *n);
                }
            },
            Err(err) => {
                if empty(&staging) {
                    let _ = std::fs::remove_dir(&staging);
                }
                // only an explicit restriction from an ERROR line stops automatic retries
                // (hints::classify; owner decision M2)
                let explicit = matches!(
                    &err,
                    MediaError::ToolFailed { hint: Some(h), .. } if h.blocks_auto_retry()
                );
                eprintln!("{n}: {}", describe(&err));
                if explicit {
                    if let Some(i) = state.get_mut(&e.id) {
                        i.status = Status::Unavailable;
                        i.attempts = 0;
                    }
                    gone += 1;
                } else {
                    failed += 1;
                    temporary(&mut state, &e.id, *n);
                }
            }
        }
        pl::save(&folder, &state).map_err(Fail::media)?;
    }
    drop(locked);
    let now_given_up: Vec<u32> = planned
        .iter()
        .filter(|(_, e)| state.get(&e.id).map(|i| i.status) == Some(Status::Failed))
        .map(|(n, _)| *n)
        .filter(|n| !gave_up.contains(n))
        .collect();
    eprintln!(
        "{got} downloaded, {skipped} deselected, {gone} unavailable, {failed} failed, {} not \
         retried automatically (failed {} runs in a row)",
        gave_up.len() + now_given_up.len(),
        pl::MAX_ATTEMPTS
    );
    println!("{}", folder.display());
    if !gave_up.is_empty() {
        let list: Vec<String> = gave_up.iter().map(|n| n.to_string()).collect();
        eprintln!(
            "not retried automatically any more: {}. Try them again with --retry-failed {}",
            list.join(", "),
            list.join(",")
        );
    }
    if failed > 0 || !gave_up.is_empty() {
        return Err(Fail::Other(format!(
            "{} item(s) failed; see above",
            failed as usize + gave_up.len()
        )));
    }
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
