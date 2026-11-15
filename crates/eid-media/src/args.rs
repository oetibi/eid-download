//! yt-dlp command lines: an allowlist, never a denylist (P4 plan section 3, ADR-0003).

use crate::MediaError;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The only output template we ever pass: constant, relative to the working directory, which
/// is a fresh staging directory we created. The final, human-readable name is given by us when
/// we publish the file, never by yt-dlp.
pub const OUTPUT_TEMPLATE: &str = "%(id)s.%(ext)s";
/// Download-archive file name, relative to the working directory (G4: inside the playlist
/// folder).
pub const ARCHIVE_FILE: &str = ".eid-archive.txt";
/// Prefix of our progress lines on stdout.
pub const PROGRESS_PREFIX: &str = "[eid-progress]";
/// Progress template: constant, only numeric fields.
pub const PROGRESS_TEMPLATE: &str = "download:[eid-progress] %(progress.downloaded_bytes)s \
     %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s \
     %(progress.eta)s";
/// JS-runtime option for yt-dlp's YouTube EJS solver: deno without JIT (D15).
pub const EJS_JITLESS: &str = "youtube-ejs:jitless=true";
/// Longest URL we accept.
pub const MAX_URL: usize = 8192;
/// Largest `--max-height` style quality we accept (8K is 4320; leave room).
pub const MAX_HEIGHT: u32 = 8640;

/// Every flag we may emit, and whether it takes a value. A valued flag is always emitted as a
/// single `--flag=value` argument, so a value can never be mistaken for a flag.
pub const ALLOWED: &[(&str, bool)] = &[
    ("--ignore-config", false),
    ("--no-cache-dir", false),
    ("--no-plugin-dirs", false),
    ("--no-remote-components", false),
    ("--no-js-runtimes", false),
    ("--js-runtimes", true),
    ("--ffmpeg-location", true),
    ("--no-update", false),
    ("--extractor-args", true),
    ("--encoding", true),
    ("--dump-single-json", false),
    ("--flat-playlist", false),
    ("--no-playlist", false),
    ("--yes-playlist", false),
    ("--format", true),
    ("--merge-output-format", true),
    ("--output", true),
    ("--newline", false),
    ("--progress-template", true),
    ("--extract-audio", false),
    ("--audio-format", true),
    ("--audio-quality", true),
    ("--limit-rate", true),
    ("--download-archive", true),
    ("--no-mtime", false),
];

/// Flags (or flag prefixes) that must never be emitted: code execution, credentials, TLS
/// downgrades, local files, config/plugin loading, self-update, arbitrary paths.
pub const FORBIDDEN: &[&str] = &[
    "--exec",
    "--netrc",
    "--no-check-certificate",
    "--legacy-server-connect",
    "--enable-file-urls",
    "--cookies",
    "-U",
    "--update",
    "--remote-components",
    "--plugin-dirs",
    "--load-info-json",
    "-a",
    "--batch-file",
    "--config-location",
    "--downloader",
    "--external-downloader",
    "--postprocessor-args",
    "--ppa",
    "--use-postprocessor",
    "--allow-unplayable-formats",
    "--username",
    "--password",
    "-u",
    "-p",
    "--video-password",
    "--client-certificate",
    "-P",
    "--paths",
    "--print-to-file",
    "--alias",
];

/// Whether `name` (a flag without its `=value`) is forbidden.
pub fn is_forbidden(name: &str) -> bool {
    // long flags are matched as prefixes (`--cookies` also covers `--cookies-from-browser`),
    // short ones exactly
    FORBIDDEN.iter().any(|f| {
        if f.starts_with("--") {
            name.starts_with(f)
        } else {
            name == *f
        }
    })
}

/// Absolute paths of the three tools, as installed by the tool manager (G2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolPaths {
    pub ytdlp: PathBuf,
    pub ffmpeg: PathBuf,
    pub deno: PathBuf,
}

/// A path that may be handed to yt-dlp: absolute, valid Unicode, no control or bidi
/// characters, and none of the characters yt-dlp's environment-variable expansion reacts to.
pub fn check_tool_path(what: &str, p: &Path) -> Result<String, MediaError> {
    let s = p
        .to_str()
        .ok_or_else(|| MediaError::BadToolPath(format!("{what}: not valid Unicode")))?;
    if !p.is_absolute() {
        return Err(MediaError::BadToolPath(format!(
            "{what}: not an absolute path"
        )));
    }
    if s.contains('$') || s.contains('%') {
        return Err(MediaError::BadToolPath(format!(
            "{what}: the folder contains '$' or '%', which yt-dlp would expand as an \
             environment variable; install the tools in a folder without these characters"
        )));
    }
    if s.chars().any(|c| c.is_control() || is_bidi_control(c)) {
        return Err(MediaError::BadToolPath(format!(
            "{what}: contains control characters"
        )));
    }
    Ok(s.to_string())
}

impl ToolPaths {
    pub fn validate(&self) -> Result<(), MediaError> {
        check_tool_path("yt-dlp", &self.ytdlp)?;
        check_tool_path("ffmpeg", &self.ffmpeg)?;
        check_tool_path("deno", &self.deno)?;
        Ok(())
    }
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A page address the user gave us, validated for yt-dlp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaUrl {
    raw: String,
    parsed: url::Url,
}

impl MediaUrl {
    /// http(s) only, with a host, at most [`MAX_URL`] bytes, no whitespace, control or bidi
    /// characters, no `user:pass@` (sign-in is not supported in v1), and it cannot look like a
    /// flag. The string reaches yt-dlp exactly as given.
    pub fn parse(raw: &str) -> Result<MediaUrl, MediaError> {
        let bad = |m: &str| Err(MediaError::BadUrl(m.to_string()));
        if raw.is_empty() || raw.len() > MAX_URL {
            return bad("empty or longer than 8192 bytes");
        }
        if raw.starts_with('-') {
            return bad("an address cannot start with '-'");
        }
        if raw
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || is_bidi_control(c))
        {
            return bad("the address contains spaces, control or direction characters");
        }
        let lower = raw.get(..8).unwrap_or(raw).to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return bad("only http:// and https:// addresses are supported");
        }
        let parsed =
            url::Url::parse(raw).map_err(|_| MediaError::BadUrl("not a valid address".into()))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return bad("only http:// and https:// addresses with a host are supported");
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return bad("addresses with a user name or password are not supported for video");
        }
        Ok(MediaUrl {
            raw: raw.to_string(),
            parsed,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Safe display form: scheme://host[:port]/path, no query or fragment.
    pub fn redacted(&self) -> String {
        let mut u = self.parsed.clone();
        u.set_query(None);
        u.set_fragment(None);
        u.to_string()
    }
}

/// Quality policy, chosen by the user (never a raw yt-dlp format string from the user).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Quality {
    /// Best video + best audio, merged to mp4.
    Best,
    /// Best video not taller than this, + best audio, merged to mp4.
    MaxHeight(u32),
    /// Audio only, converted to mp3.
    AudioMp3,
    /// A `format_id` taken from the tool's own `-J` output.
    Format(String),
}

/// What yt-dlp is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// `-J` of a single video.
    Info,
    /// `--flat-playlist -J` of a playlist.
    PlaylistInfo,
    Download {
        quality: Quality,
        /// Bytes per second (from `--limit`).
        limit_rate: Option<u64>,
        /// Record finished items in [`ARCHIVE_FILE`] and skip those already in it.
        archive: bool,
    },
}

/// A `format_id` from the network is accepted only if it is short and made of characters that
/// carry no meaning in yt-dlp's format-selection language.
pub fn check_format_id(id: &str) -> Result<(), MediaError> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.');
    if ok {
        Ok(())
    } else {
        Err(MediaError::BadChoice(
            "format id must be 1-64 characters of A-Z a-z 0-9 _ - .".into(),
        ))
    }
}

fn kv(flag: &str, value: &str) -> OsString {
    OsString::from(format!("{flag}={value}"))
}

/// Build the complete argument vector (program not included).
pub fn build(
    tools: &ToolPaths,
    url: &MediaUrl,
    action: &Action,
) -> Result<Vec<OsString>, MediaError> {
    let ffmpeg = check_tool_path("ffmpeg", &tools.ffmpeg)?;
    let deno = check_tool_path("deno", &tools.deno)?;
    check_tool_path("yt-dlp", &tools.ytdlp)?;
    let mut a: Vec<OsString> = [
        "--ignore-config",
        "--no-cache-dir",
        "--no-plugin-dirs",
        "--no-remote-components",
        "--no-js-runtimes",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    a.push(kv("--js-runtimes", &format!("deno:{deno}")));
    a.push(kv("--ffmpeg-location", &ffmpeg));
    a.push("--no-update".into());
    a.push(kv("--extractor-args", EJS_JITLESS));
    a.push(kv("--encoding", "utf-8"));
    match action {
        Action::Info => {
            a.push("--dump-single-json".into());
            a.push("--no-playlist".into());
        }
        Action::PlaylistInfo => {
            a.push("--dump-single-json".into());
            a.push("--flat-playlist".into());
            a.push("--yes-playlist".into());
        }
        Action::Download {
            quality,
            limit_rate,
            archive,
        } => {
            a.push("--no-playlist".into());
            a.push("--newline".into());
            a.push(kv("--progress-template", PROGRESS_TEMPLATE));
            a.push(kv("--output", OUTPUT_TEMPLATE));
            a.push("--no-mtime".into());
            match quality {
                Quality::Best => {
                    a.push(kv("--format", "bv*+ba/b"));
                    a.push(kv("--merge-output-format", "mp4"));
                }
                Quality::MaxHeight(h) => {
                    if *h == 0 || *h > MAX_HEIGHT {
                        return Err(MediaError::BadChoice(format!(
                            "height must be 1..={MAX_HEIGHT}"
                        )));
                    }
                    a.push(kv(
                        "--format",
                        &format!("bv*[height<={h}]+ba/b[height<={h}]"),
                    ));
                    a.push(kv("--merge-output-format", "mp4"));
                }
                Quality::AudioMp3 => {
                    a.push(kv("--format", "ba/b"));
                    a.push("--extract-audio".into());
                    a.push(kv("--audio-format", "mp3"));
                    a.push(kv("--audio-quality", "0"));
                }
                Quality::Format(id) => {
                    check_format_id(id)?;
                    a.push(kv("--format", id));
                    a.push(kv("--merge-output-format", "mp4"));
                }
            }
            if let Some(r) = limit_rate {
                if *r == 0 {
                    return Err(MediaError::BadChoice(
                        "limit must be at least 1 byte/s".into(),
                    ));
                }
                a.push(kv("--limit-rate", &r.to_string()));
            }
            if *archive {
                a.push(kv("--download-archive", ARCHIVE_FILE));
            }
        }
    }
    a.push("--".into());
    a.push(url.as_str().into());
    check(&a).map_err(|e| MediaError::BadChoice(format!("internal: {e}")))?;
    Ok(a)
}

/// Structural check of an argument vector (defence in depth, also used by the tests): every
/// argument before the single `--` is an allowed flag in its proper form, none is forbidden,
/// and exactly one argument (the URL) follows `--`.
pub fn check(args: &[OsString]) -> Result<(), String> {
    let sep = args
        .iter()
        .position(|a| a == "--")
        .ok_or("no '--' before the URL")?;
    if args.len() != sep + 2 {
        return Err("exactly one argument must follow '--'".into());
    }
    for a in &args[..sep] {
        let s = a.to_str().ok_or("non-Unicode flag")?;
        let (name, value) = match s.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (s, None),
        };
        if is_forbidden(name) {
            return Err(format!("forbidden flag {name}"));
        }
        let takes = ALLOWED
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, t)| *t)
            .ok_or_else(|| format!("flag {name} is not in the allowlist"))?;
        if takes != value.is_some() {
            return Err(format!("flag {name} has the wrong form"));
        }
    }
    Ok(())
}

/// Names of the flags in an argument vector built by [`build`] (for tests and diagnostics).
pub fn emitted_flags(args: &[OsString]) -> Vec<String> {
    args.iter()
        .take_while(|a| *a != "--")
        .filter_map(|a| a.to_str())
        .map(|s| s.split_once('=').map_or(s, |(n, _)| n).to_string())
        .collect()
}
