//! `eid video info|get|consent|revoke` (P4 G3): video and audio through yt-dlp, standalone in
//! this process only (D7: not reachable through IPC or Native Messaging).
//!
//! Order of every run: consent (nothing is downloaded or started before it) → tools installed
//! (pinned manifest, verified) → tools locked and hashed through their handles (D11) → yt-dlp
//! runs with a fresh or resumed staging folder as its working directory → we publish the
//! finished file ourselves under a sanitized name → the staging folder is removed.

use eid_media::args::{Fallback, MediaUrl, Quality};
use eid_media::consent::{self, Lang};
use eid_media::fetch::Fetcher;
use eid_media::hints::Hint;
use eid_media::manifest::Manifest;
use eid_media::run::Runner;
use eid_media::settings::SettingsStore;
pub use eid_media::stage::{
    finished_file, playlist_folder, prepare_staging, publish_name, remove_empty_staging,
    staging_name, units, PublishError, TOOL_NAME_RESERVE,
};
#[cfg(test)]
use eid_media::stage::{publish_failure, retry_locked, STAGING_MAX_DEPTH};
use eid_media::tools::ToolStore;
use eid_media::MediaError;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

pub const HELP: &str = "eid video - video and audio through yt-dlp (after your consent)\n\n\
USAGE:\n  eid video info <page-url> [--yes-i-agree] [--lang ar|en]\n\
  eid video get <page-url> [-d DIR] [--quality 360p|480p|720p|1080p|1440p|2160p|best|audio] [--if-missing lower|higher|skip] [--limit RATE] [--yes-i-agree] [--lang ar|en] [-q]\n\
  eid video playlist <playlist-url> [-d DIR] [--quality Q] [--if-missing F] [--limit RATE] [--skip 2,5] [--retry-failed 3,7|all] [--list] [--yes-i-agree] [-q]
\n                                     every item, numbered 01 - ... in a folder named after the playlist;
\n                                     an item that failed 3 runs in a row, or that the site refused
\n                                     explicitly (private, removed, sign-in, country, DRM), is not
\n                                     tried again automatically: --retry-failed tries it again
\n  eid video probe <page-url>          print what the address is (video or playlist, title, items,\n\
                                     offered qualities) as one JSON object (needs consent + tools)\n\
  eid video setup [--yes-i-agree]    consent (if needed) and install the video tools, then stop\n\
  eid video consent [--lang ar|en]   show the consent text and whether it was given\n\
  eid video revoke                   withdraw consent and delete the video tools\n\n\
The first use asks for consent (or takes --yes-i-agree) and then downloads yt-dlp, FFmpeg and\n\
Deno from their official releases, checked against the SHA-256 pinned in this program.\n\
Without a terminal and without --yes-i-agree nothing is downloaded or started.\n\
Ctrl-C stops the tool and everything it started; running the same command again continues.\n\n\
For programs (the desktop app): --events prints one JSON object per line on stdout (plan,\n\
item, progress, end); --stop-on-stdin-eof stops like Ctrl-C when standard input is closed;\n\
--items 1,4 (playlist) downloads only those item numbers.";

/// Exit code when consent is missing.
pub const EXIT_NO_CONSENT: i32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sub {
    Info,
    Get,
    Consent,
    Revoke,
    Playlist,
    /// What an address is (video or playlist), as JSON (v0.1, for the desktop app).
    Probe,
    /// Consent + tool install only (v0.1, for the desktop app).
    Setup,
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
    /// Playlist: download only these item numbers (empty = every item not skipped).
    pub items: Vec<u32>,
    /// Machine-readable events (JSON lines) on stdout.
    pub events: bool,
    /// Stop (like Ctrl-C) when standard input reaches its end (the parent closed it).
    pub stdin_stop: bool,
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

/// The fixed v0.1 policy (`360p` ... `2160p`, `best` = best up to 4K, `audio`); `mp3` is kept
/// as an alias of `audio`. Anything taller than 2160 is refused (owner: no 8K).
fn parse_quality(s: &str) -> Result<Quality, String> {
    let s = if s == "mp3" { "audio" } else { s };
    Quality::from_policy(s).map_err(|_| {
        format!(
            "bad quality '{s}' (one of {}; nothing above 2160p)",
            eid_media::args::QUALITY_CHOICES.join(", ")
        )
    })
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
        Some("probe") => Sub::Probe,
        Some("setup") => Sub::Setup,
        other => {
            return Err(format!(
                "expected info, get, playlist, probe, setup, consent or revoke, got {other:?}"
            ))
        }
    };
    let mut fallback = Fallback::Lower;
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
        items: vec![],
        events: false,
        stdin_stop: false,
    };
    while let Some(x) = it.next() {
        match x.as_str() {
            "-d" => a.dir = Some(PathBuf::from(it.next().ok_or("-d needs a folder")?)),
            "--quality" => a.quality = parse_quality(it.next().ok_or("--quality needs a value")?)?,
            // v0.2: what to do when the chosen height is not available (default: lower)
            "--if-missing" => {
                fallback = Fallback::from_name(
                    it.next()
                        .ok_or("--if-missing needs lower, higher or skip")?,
                )
                .map_err(|e| e.to_string())?
            }
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
            "--events" => a.events = true,
            "--stop-on-stdin-eof" => a.stdin_stop = true,
            "--items" => a.items.extend(parse_numbers(
                it.next().ok_or("--items needs item numbers, e.g. 1,4")?,
            )?),
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
    a.quality = a.quality.with_fallback(fallback);
    let needs_url = matches!(a.sub, Sub::Info | Sub::Get | Sub::Playlist | Sub::Probe);
    if needs_url != a.url.is_some() {
        return Err(if needs_url {
            "a page address is required".into()
        } else {
            "this command takes no address".into()
        });
    }
    if (a.sub != Sub::Playlist)
        && (!a.skip.is_empty() || a.list_only || a.retry_failed.is_some() || !a.items.is_empty())
    {
        return Err(
            "--skip, --items, --list and --retry-failed only apply to `eid video playlist`".into(),
        );
    }
    if a.list_only && a.retry_failed.is_some() {
        return Err("--list only shows the plan; it cannot be combined with --retry-failed".into());
    }
    if !matches!(a.sub, Sub::Get | Sub::Playlist) && (a.dir.is_some() || a.limit_bps.is_some()) {
        return Err("-d and --limit only apply to `eid video get` and `eid video playlist`".into());
    }
    Ok(a)
}

// ------------------------------------------------------------------ test hooks

/// The pinned manifest. Builds with the `test-hooks` feature (tests only, never a release)
/// may replace it with `EID_TEST_MANIFEST` (a file path).
pub fn manifest() -> Result<Manifest, String> {
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
    /// `EID_TEST_PUBLISH_FAIL=<file name>`: publishing under exactly that name fails like an
    /// I/O error (review m3: one item's publish error must not end a playlist run);
    /// `EID_TEST_PUBLISH_FAIL=swap:<file name>`: it fails like a detected swap (review n1:
    /// that must stop the run).
    pub fn publish_fault(name: &str) -> Option<std::io::ErrorKind> {
        let v = std::env::var("EID_TEST_PUBLISH_FAIL").ok()?;
        match v.strip_prefix("swap:") {
            Some(n) if n == name => Some(std::io::ErrorKind::InvalidData),
            None if v == name => Some(std::io::ErrorKind::Other),
            _ => None,
        }
    }

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

// ------------------------------------------------------------------ events (--events)

/// `--events`: one JSON object per line on stdout, for the desktop app's job list. Every
/// string in an event comes from [`crate::remote::printable`]-safe sources or is serialized by
/// serde_json (so it cannot break the line framing).
pub mod events {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};

    static ON: AtomicBool = AtomicBool::new(false);

    pub fn enable() {
        ON.store(true, Ordering::Relaxed);
    }

    pub fn on() -> bool {
        ON.load(Ordering::Relaxed)
    }

    /// Print one event (no-op without `--events`). A broken stdout is ignored: the run goes on.
    pub fn emit(v: serde_json::Value) {
        if !on() {
            return;
        }
        let mut out = std::io::stdout().lock();
        let _ = serde_json::to_writer(&mut out, &v);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }
}

/// Forward tool progress as `progress` events for item `n` (0 = a single video), at most
/// every 250 ms (plus the last sample), so a fast download does not flood the reader.
fn progress_forwarder(
    n: u32,
    quiet: bool,
) -> (
    tokio::sync::mpsc::Sender<eid_media::progress::Progress>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<eid_media::progress::Progress>(64);
    let h = tokio::spawn(async move {
        let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let mut pending: Option<eid_media::progress::Progress> = None;
        let send = |p: &eid_media::progress::Progress| {
            events::emit(serde_json::json!({
                "ev": "progress", "n": n, "done": p.downloaded, "total": p.total,
                "speed": p.speed, "eta": p.eta_secs, "height": p.height,
            }))
        };
        while let Some(p) = rx.recv().await {
            if !quiet && !events::on() && n == 0 {
                match (p.downloaded, p.total) {
                    (Some(d), Some(t)) if t > 0 => {
                        eprintln!("  {d}/{t} ({}%)", d.saturating_mul(100) / t)
                    }
                    (Some(d), _) => eprintln!("  {d} bytes"),
                    _ => {}
                }
            }
            if last.elapsed() >= std::time::Duration::from_millis(250) {
                send(&p);
                last = std::time::Instant::now();
                pending = None;
            } else {
                pending = Some(p);
            }
        }
        if let Some(p) = pending {
            send(&p);
        }
    });
    (tx, h)
}

/// `--stop-on-stdin-eof`: the parent (the desktop app) pauses a job by closing our standard
/// input; that cancels exactly like Ctrl-C (the tool tree is stopped, partial data is kept).
fn cancel_on_stdin_eof(cancel: &eid_engine::CancellationToken) {
    let c = cancel.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 256];
        let mut stdin = std::io::stdin();
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        c.cancel();
    });
}

// ------------------------------------------------------------------ steps

pub fn stores() -> Result<(SettingsStore, ToolStore), String> {
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
    // With --stop-on-stdin-eof stdin belongs to the app (a pipe another thread is reading):
    // never prompt, and never query the handle either. On Windows a query of a synchronous
    // pipe handle waits behind that pending read, so `is_terminal()` would hang until the app
    // closes stdin (seen on the Windows CI runner).
    if a.stdin_stop || !std::io::stdin().is_terminal() {
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
        Hint::QualityMissing => "the chosen quality is not available for this media, and \"if missing\" is set to skip it; choose another quality or --if-missing lower|higher.",
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

/// [`eid_media::stage::publish_retrying`] with the `test-hooks` fault injector
/// (`EID_TEST_PUBLISH_FAIL`); release builds inject nothing.
async fn publish_retrying(
    staged: &Path,
    out_dir: &Path,
    name: &str,
    cancel: &eid_engine::CancellationToken,
) -> Result<PathBuf, PublishError> {
    #[cfg(feature = "test-hooks")]
    let fault = |n: &str| test_hooks::publish_fault(n);
    #[cfg(not(feature = "test-hooks"))]
    let fault = |_: &str| None;
    eid_media::stage::publish_retrying(staged, out_dir, name, cancel, &fault).await
}

/// The `probe` answer (the shape of the desktop app's `video_probe`, docs/V0.1-CONTRACT.md):
/// qualities offered for a video come from its formats ([`eid_media::args::offered_heights`],
/// never above 2160p) plus `best` and `audio`; a playlist offers every policy choice (its items
/// are not inspected one by one) and lists its items in order, numbered from 1.
pub fn probe_json(p: &eid_media::info::Probed) -> serde_json::Value {
    use eid_media::info::Probed;
    let pr = crate::remote::printable;
    match p {
        Probed::Video(i) => {
            let hs = i
                .formats
                .iter()
                .filter(|f| f.has_video)
                .filter_map(|f| f.height);
            let mut q: Vec<String> = eid_media::args::offered_heights(hs)
                .into_iter()
                .map(|h| format!("{h}p"))
                .collect();
            q.push("best".into());
            if i.formats.iter().any(|f| f.has_audio) || i.formats.is_empty() {
                q.push("audio".into());
            }
            serde_json::json!({
                "kind": "video", "id": pr(&i.id), "title": pr(&i.title),
                "uploader": i.uploader.as_deref().map(pr), "duration": i.duration_secs,
                "is_live": i.is_live, "qualities": q,
            })
        }
        Probed::Playlist(l) => {
            let items: Vec<serde_json::Value> = l
                .entries
                .iter()
                .enumerate()
                .map(|(k, e)| {
                    serde_json::json!({
                        "index": k + 1, "id": pr(&e.id), "title": pr(&e.title),
                        "duration": e.duration_secs,
                        "available": !eid_media::stage::listed_unavailable(e.availability.as_deref()),
                    })
                })
                .collect();
            serde_json::json!({
                "kind": "playlist", "id": pr(&l.id), "title": pr(&l.title),
                "uploader": l.uploader.as_deref().map(pr),
                "qualities": eid_media::args::QUALITY_CHOICES, "items": items,
            })
        }
    }
}

// ------------------------------------------------------------------ commands

/// Run a video command; returns the exit code. Messages go to stderr, the final path (get) or
/// the information (info) to stdout.
pub async fn run(a: VideoArgs, cancel: eid_engine::CancellationToken) -> i32 {
    #[cfg(feature = "test-hooks")]
    test_hooks::cancel_when_file_appears(&cancel);
    if a.events {
        events::enable();
    }
    if a.stdin_stop {
        cancel_on_stdin_eof(&cancel);
    }
    let r = run_inner(&a, cancel).await;
    let (code, msg) = match &r {
        Ok(()) => (0, None),
        Err(Fail::NoConsent(m)) => (EXIT_NO_CONSENT, Some(m.clone())),
        Err(Fail::Cancelled(m)) => (130, Some(m.clone())),
        Err(Fail::Media(e)) => (1, Some(describe(e))),
        Err(Fail::Other(m)) => (1, Some(crate::remote::printable(m))),
    };
    events::emit(serde_json::json!({"ev": "end", "code": code, "error": msg}));
    match r {
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
    if a.sub == Sub::Setup {
        ensure_consent(a, &settings, &m, &cancel).await?;
        let locked = ensure_tools(a, &tools, &m, &cancel).await?;
        drop(locked);
        events::emit(serde_json::json!({"ev": "ready"}));
        println!("video tools ready");
        return Ok(());
    }
    let url = MediaUrl::parse(a.url.as_deref().unwrap_or("")).map_err(Fail::media)?;
    // probe is for programs: it never asks and never installs (the app runs `setup` first)
    if a.sub == Sub::Probe {
        if !consent::is_granted(&settings, &m).map_err(Fail::media)? {
            return Err(Fail::NoConsent("TOOLS_MISSING: consent not given".into()));
        }
        if let Err(MediaError::NotInstalled) = tools.current(&m) {
            return Err(Fail::Other(
                "TOOLS_MISSING: video tools not installed".into(),
            ));
        }
    }
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
        Sub::Probe => {
            let work = tools.root().join(format!(".probe-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&work);
            std::fs::create_dir(&work).map_err(|e| Fail::Other(e.to_string()))?;
            let r = tokio::select! {
                r = runner.probe(&url, &work) => r,
                _ = cancel.cancelled() => {
                    let _ = std::fs::remove_dir_all(&work);
                    return Err(Fail::Cancelled("stopped".into()));
                }
            };
            let _ = std::fs::remove_dir_all(&work);
            drop(locked);
            let v = probe_json(&r.map_err(Fail::media)?);
            println!("{v}");
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
    events::emit(serde_json::json!({
        "ev": "plan", "kind": "video", "title": info.title, "folder": out_dir.display().to_string(),
        "items": [{"n": 0, "title": info.title, "state": "todo"}],
    }));
    events::emit(serde_json::json!({"ev": "item", "n": 0, "state": "running"}));
    let (tx, printer) = progress_forwarder(0, a.quiet);
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
    let dest = match publish_retrying(&staged, out_dir, &name, &cancel).await {
        Ok(d) => d,
        Err(PublishError::Cancelled) => {
            return Err(Fail::Cancelled(
                "stopped before publishing; the download is kept in the staging folder".into(),
            ))
        }
        Err(e) => return Err(Fail::Other(e.to_string())),
    };
    let _ = std::fs::remove_dir_all(&staging);
    events::emit(serde_json::json!({
        "ev": "item", "n": 0, "state": "done",
        "file": dest.file_name().map(|f| f.to_string_lossy().into_owned()),
    }));
    if !events::on() {
        println!("{}", dest.display());
    }
    Ok(())
}

/// A file name found on disk, shown in one line: control characters and the invisible
/// formatting characters (marks, bidi controls, zero-width, line/paragraph separators, BOM,
/// soft hyphen) become '?'.
fn neutral_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            let invisible = c.is_control()
                || matches!(
                    c,
                    '\u{00AD}'
                        | '\u{061C}'
                        | '\u{180E}'
                        | '\u{200B}'..='\u{200F}'
                        | '\u{2028}'..='\u{202E}'
                        | '\u{2060}'..='\u{206F}'
                        | '\u{FEFF}'
                        | '\u{FFF9}'..='\u{FFFB}'
                );
            if invisible {
                '?'
            } else {
                c
            }
        })
        .collect()
}

/// One line (control characters are replaced when printed): what was found, that nothing was
/// touched, and the two ways on.
fn lost_state_message(folder: &Path, left: &eid_media::playlist::Leftovers) -> String {
    use eid_media::playlist::{ARCHIVE_NAME, STATE_FILE};
    let mut found = vec![];
    if left.archive {
        found.push(format!(
            "a download archive ({ARCHIVE_NAME}) that lists items"
        ));
    }
    if left.numbered > 0 {
        let more = if left.numbered > left.examples.len() {
            ", ..."
        } else {
            ""
        };
        // names come from the disk, not from us: besides what `printable` replaces later, every
        // invisible format or separator character that could reorder or hide text becomes '?'
        let shown: Vec<String> = left.examples.iter().map(|n| neutral_name(n)).collect();
        found.push(format!(
            "{} numbered file(s) ({}{more})",
            left.numbered,
            shown.join(", ")
        ));
    }
    format!(
        "{STATE_FILE} is missing in {}, but the folder holds {} from earlier runs. Numbering \
         again could give a new item a number that is already used, so nothing was downloaded, \
         no new state was written and no existing file was renamed or changed. To continue: \
         copy {STATE_FILE} back into that folder from a backup made after the last run there \
         (an older backup does not know the newest numbers), or download into a new folder \
         with -d <another folder>",
        folder.display(),
        found.join(" and ")
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
    // Ctrl-C while listing is a cancel like in every other stage (exit 130, review m2); the
    // listing tool is killed when its future is dropped here
    let listed = tokio::select! {
        r = runner.playlist(url, &work) => Some(r),
        _ = cancel.cancelled() => None,
    };
    let _ = std::fs::remove_dir_all(&work);
    let Some(listed) = listed else {
        return Err(Fail::Cancelled(
            "stopped while listing the playlist; nothing was downloaded".into(),
        ));
    };
    let info = listed.map_err(Fail::media)?;
    let folder = playlist_folder(out_dir, &info.title).map_err(Fail::Other)?;
    let _lock = pl::lock_folder(&folder).map_err(Fail::media)?;
    let old = pl::load(&folder).map_err(Fail::media)?;
    let p = crate::remote::printable;
    // the state file is gone but earlier runs left files here: numbering again could reuse
    // their numbers (review M3, owner decision). Refuse before any download or new state;
    // `--list` only reads, so it shows the listing with a warning.
    if old.is_none() {
        let left = pl::leftovers(&folder).map_err(Fail::media)?;
        if left.any() {
            let m = lost_state_message(&folder, &left);
            if !a.list_only {
                return Err(Fail::Other(m));
            }
            eprintln!(
                "warning: {}. The numbers below are positions in the current listing, not the \
                 numbers of the files already there; nothing is written.",
                p(&m)
            );
        }
    }
    let (mut state, planned) = pl::plan(old, &info.id, &info.entries).map_err(Fail::media)?;
    // `--retry-failed`: only the named items, and only if they failed or are unavailable
    // a --skip number that no item of this listing has would otherwise be ignored silently
    // (review m9); the run is not stopped for it
    for n in a
        .skip
        .iter()
        .filter(|n| !planned.iter().any(|(k, _)| k == *n))
    {
        eprintln!("--skip {n}: no item of this playlist has that number; ignored");
    }
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
    for n in a
        .items
        .iter()
        .filter(|n| !planned.iter().any(|(k, _)| k == *n))
    {
        eprintln!("--items {n}: no item of this playlist has that number; ignored");
    }
    let deselected = |n: &u32| a.skip.contains(n) || (!a.items.is_empty() && !a.items.contains(n));
    if events::on() {
        let items: Vec<serde_json::Value> = planned
            .iter()
            .map(|(n, e)| {
                let st = match state.get(&e.id).map(|i| i.status) {
                    Some(Status::Done) => "done",
                    Some(Status::Unavailable) => "unavailable",
                    Some(Status::Failed) => "failed",
                    _ if deselected(n) => "skipped",
                    _ => "todo",
                };
                serde_json::json!({"n": n, "id": e.id, "title": e.title, "state": st,
                    "file": state.get(&e.id).and_then(|i| i.file.clone())})
            })
            .collect();
        events::emit(serde_json::json!({
            "ev": "plan", "kind": "playlist", "title": info.title,
            "folder": folder.display().to_string(), "items": items,
        }));
    }
    let item_ev = |n: u32, st: &str, err: Option<String>| {
        events::emit(serde_json::json!({"ev": "item", "n": n, "state": st, "error": err}))
    };
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
        if status == Some(Status::Unavailable)
            || eid_media::stage::listed_unavailable(e.availability.as_deref())
        {
            if let Some(i) = state.get_mut(&e.id) {
                i.status = Status::Unavailable;
                i.attempts = 0;
            }
            pl::save(&folder, &state).map_err(Fail::media)?;
            gone += 1;
            item_ev(*n, "unavailable", None);
            if !a.quiet {
                eprintln!(
                    "{n}: unavailable (private, removed, sign-in only, other country or DRM); \
                     skipped, number kept. If that changes: --retry-failed {n}"
                );
            }
            continue;
        }
        if deselected(n) {
            skipped += 1;
            continue;
        }
        let item_url = match e.url.as_deref().map(MediaUrl::parse) {
            Some(Ok(u)) => u,
            // a temporary failure like any other (owner decision M2): counted, and after
            // MAX_ATTEMPTS runs no longer asked automatically (review m4; it used to fail on
            // every run without ever being recorded)
            _ => {
                failed += 1;
                eprintln!("{n}: the playlist gave no usable address for this item");
                item_ev(*n, "failed", Some("no usable address".into()));
                temporary(&mut state, &e.id, *n);
                pl::save(&folder, &state).map_err(Fail::media)?;
                continue;
            }
        };
        if !a.quiet {
            eprintln!("{n}: {}", p(&e.title));
        }
        let staging =
            prepare_staging(&folder, &staging_name(&item_url, &a.quality)).map_err(Fail::Other)?;
        item_ev(*n, "running", None);
        let (tx, fwd) = if events::on() {
            let (tx, h) = progress_forwarder(*n, true);
            (Some(tx), Some(h))
        } else {
            (None, None)
        };
        let r = tokio::select! {
            r = runner.download(&item_url, &staging, a.quality.clone(), a.limit_bps, true, tx) => r,
            _ = cancel.cancelled() => {
                drop(locked);
                item_ev(*n, "paused", None);
                return Err(Fail::Cancelled(format!(
                    "stopped at item {n}; finished items are kept. Run the same command again \
                     to continue."
                )));
            }
        };
        if let Some(h) = fwd {
            let _ = h.await;
        }
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
                    match publish_retrying(&staged, &folder, &name, &cancel).await {
                        Ok(dest) => {
                            let _ = std::fs::remove_dir_all(&staging);
                            if let Some(i) = state.get_mut(&e.id) {
                                i.status = Status::Done;
                                i.attempts = 0;
                                i.file = dest.file_name().map(|f| f.to_string_lossy().into_owned());
                            }
                            got += 1;
                            item_ev(*n, "done", None);
                        }
                        // something planted in our staging folder: stop, fail closed
                        Err(PublishError::Refused(m)) => return Err(Fail::Other(m)),
                        // Ctrl-C while waiting for a locked file: a cancel like any other, not
                        // a failure of this item (review r3)
                        Err(PublishError::Cancelled) => {
                            drop(locked);
                            return Err(Fail::Cancelled(format!(
                                "stopped at item {n}; finished items are kept. Run the same \
                                 command again to continue."
                            )));
                        }
                        // this item only: counted, kept in staging, the run goes on (m3)
                        Err(PublishError::Item(m) | PublishError::Locked(m)) => {
                            failed += 1;
                            eprintln!("{n}: {}", p(&m));
                            item_ev(*n, "failed", Some(p(&m)));
                            temporary(&mut state, &e.id, *n);
                        }
                    }
                }
                // nothing new: yt-dlp skipped it because the archive already lists it
                Err(_) if empty(&staging) => {
                    let _ = std::fs::remove_dir(&staging);
                    if let Some(i) = state.get_mut(&e.id) {
                        i.status = Status::Done;
                        i.attempts = 0;
                    }
                    item_ev(*n, "done", None);
                    // the item was not done in our state, so the tool recorded it in the
                    // archive but we never published its file (for example the staging folder
                    // was emptied in between): say so, even with -q (owner decision m5(a))
                    eprintln!(
                        "{n}: warning: the download archive already lists this item, but no file \
                         was published for it. If {} is missing, remove the line ending in {} \
                         from {} and run again.",
                        p(&pl::item_name(state.width, *n, &e.title, ext)),
                        p(&e.id),
                        pl::ARCHIVE_NAME
                    );
                }
                Err(m) => {
                    failed += 1;
                    eprintln!("{n}: {m}");
                    item_ev(*n, "failed", Some(p(&m)));
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
                item_ev(
                    *n,
                    if explicit { "unavailable" } else { "failed" },
                    Some(describe(&err)),
                );
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
    if !events::on() {
        println!("{}", folder.display());
    }
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
        // v0.2 --if-missing: applies to a fixed height, in any option order
        let q = |args: &[&str]| v(args).unwrap().quality;
        assert_eq!(
            q(&["get", "u", "--if-missing", "higher", "--quality", "720p"]),
            Quality::AtLeast(720)
        );
        assert_eq!(
            q(&[
                "playlist",
                "u",
                "--quality",
                "1080p",
                "--if-missing",
                "skip"
            ]),
            Quality::Exactly(1080)
        );
        assert_eq!(
            q(&["get", "u", "--quality", "720p", "--if-missing", "lower"]),
            Quality::MaxHeight(720)
        );
        assert_eq!(
            q(&["get", "u", "--quality", "audio", "--if-missing", "skip"]),
            Quality::AudioMp3
        );
        for bad in [
            vec!["get", "u", "--if-missing"],
            vec!["get", "u", "--if-missing", "any"],
            vec!["get", "u", "--if-missing", "--quality"],
        ] {
            assert!(v(&bad).is_err(), "{bad:?}");
        }
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

    /// Review of m3: a swap detected while publishing (eid-platform reports it as
    /// `InvalidData`, pinned in its own tests) stops the run; other I/O errors fail one item.
    #[test]
    fn a_swap_while_publishing_stops_the_run_other_errors_fail_one_item() {
        use std::io::{Error, ErrorKind};
        let d = Path::new("out").join("01 - a.mp4");
        let swap = Error::new(ErrorKind::InvalidData, "source was swapped");
        assert!(matches!(
            publish_failure(&d, swap),
            PublishError::Refused(_)
        ));
        for k in [
            ErrorKind::PermissionDenied,
            ErrorKind::NotFound,
            ErrorKind::Other,
        ] {
            assert!(
                matches!(
                    publish_failure(&d, Error::new(k, "x")),
                    PublishError::Item(_)
                ),
                "{k:?}"
            );
        }
    }

    /// Review n4 (AGENTS §6.9): a locked staged file is retried with an async pause, then fails
    /// only this item; a refusal is never retried.
    #[test]
    fn a_locked_staged_file_is_retried_then_fails_only_the_item() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let z = std::time::Duration::ZERO;
        let locked = || PublishError::Locked("locked".into());
        let live = eid_engine::CancellationToken::new();
        // locked twice, then free: published at the third attempt
        let mut n = 0;
        let r = rt.block_on(retry_locked(5, z, &live, || {
            n += 1;
            if n < 3 {
                Err(locked())
            } else {
                Ok(n)
            }
        }));
        assert_eq!(r.unwrap(), 3);
        // always locked: exactly `tries` attempts, then an item failure
        let mut n = 0;
        let r = rt.block_on(retry_locked(4, z, &live, || -> Result<(), _> {
            n += 1;
            Err(locked())
        }));
        assert_eq!(n, 4);
        assert!(
            matches!(&r, Err(PublishError::Item(m)) if m.contains("still locked after 4 tries")),
            "{r:?}"
        );
        // a refusal (planted link, swap) stops at once
        let mut n = 0;
        let r = rt.block_on(retry_locked(4, z, &live, || -> Result<(), _> {
            n += 1;
            Err(PublishError::Refused("planted".into()))
        }));
        assert_eq!(n, 1);
        assert!(matches!(r, Err(PublishError::Refused(_))));
    }

    /// Review r3: Ctrl-C ends the wait for a locked file at once (a cancel, not a failed
    /// item), however long the pause.
    #[test]
    fn ctrl_c_ends_the_wait_for_a_locked_file() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let cancel = eid_engine::CancellationToken::new();
        let c2 = cancel.clone();
        let mut n = 0;
        let t = std::time::Instant::now();
        let r = rt.block_on(retry_locked(
            10,
            std::time::Duration::from_secs(3600),
            &cancel,
            || -> Result<(), _> {
                n += 1;
                c2.cancel(); // Ctrl-C arrives while the file is locked
                Err(PublishError::Locked("locked".into()))
            },
        ));
        assert!(matches!(r, Err(PublishError::Cancelled)), "{r:?}");
        assert_eq!(n, 1, "no attempt after the cancel");
        assert!(
            t.elapsed() < std::time::Duration::from_secs(60),
            "waited the pause"
        );
    }

    /// Review r2: sharing (32) and lock (33) violations are waited for on Windows; access
    /// denied (5: a folder or junction swapped in, a delete-pending file) is a refusal, and
    /// nothing is waited for on Unix, where an open takes no such lock.
    #[test]
    fn only_in_use_errors_count_as_a_locked_staged_file() {
        let in_use =
            |code| eid_platform::fs::is_file_in_use(&std::io::Error::from_raw_os_error(code));
        assert_eq!(
            (in_use(32), in_use(33), in_use(5)),
            (eid_platform::IS_WINDOWS, eid_platform::IS_WINDOWS, false)
        );
    }

    /// P4 review m9: the messages name `playlist` where it applies.
    #[test]
    fn video_arg_errors_name_the_playlist_command() {
        let e = v(&["nope"]).unwrap_err();
        assert!(e.contains("playlist"), "{e}");
        let e = v(&["info", "https://x.test/v", "-d", "out"]).unwrap_err();
        assert!(e.contains("`eid video playlist`"), "{e}");
        assert!(v(&["playlist", "https://x.test/p", "-d", "out", "--limit", "1M"]).is_ok());
    }

    /// P4 review m7: a reused staging folder is walked to a bounded depth; plain nested
    /// folders within the bound are accepted, deeper nesting is refused, nothing is changed.
    #[test]
    fn reused_staging_folder_is_walked_with_a_depth_bound() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("st");
        let mut deep = s.clone();
        for i in 0..STAGING_MAX_DEPTH - 1 {
            deep = deep.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("f.part"), b"x").unwrap();
        assert_eq!(prepare_staging(d.path(), "st").unwrap(), s);
        std::fs::create_dir(deep.join("one-more")).unwrap();
        let e = prepare_staging(d.path(), "st").unwrap_err();
        assert!(e.contains("nested more than"), "{e}");
        assert!(deep.join("f.part").is_file(), "nothing is removed");
    }
}
