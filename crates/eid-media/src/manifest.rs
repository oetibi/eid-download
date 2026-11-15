//! The pinned tool manifest (`tools/manifest.json`, compiled into the program).
//!
//! It is the only source of what may be downloaded and run: exact HTTPS URLs of immutable
//! release assets on GitHub (D14), their sizes and SHA-256, and for archives the exact entries
//! to extract with their own sizes and SHA-256. "Updating" the tools means installing what the
//! manifest of the running program pins; there is no other update path.

use crate::MediaError;
use serde::Deserialize;

/// The manifest shipped with this build.
pub const EMBEDDED: &str = include_str!("../../../tools/manifest.json");

/// Hosts a manifest may name at all. A manifest's own `allowed_hosts` can only narrow this.
pub const TOOL_HOSTS: &[&str] = &[
    "github.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
];

/// Largest download or extracted file a manifest may declare.
pub const MAX_TOOL_BYTES: u64 = 512 * 1024 * 1024;

/// The tools eid needs (each exactly once).
pub const REQUIRED_FILES: &[&str] = &["yt-dlp.exe", "ffmpeg.exe", "ffprobe.exe", "deno.exe"];

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    /// Identifies the pinned set (part of the install folder name).
    pub id: String,
    pub allowed_hosts: Vec<String>,
    pub tools: Vec<Tool>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    pub version: String,
    pub license: String,
    /// Human-readable release page (shown in the consent text).
    pub source: String,
    /// The asset to download.
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub files: Vec<ToolFile>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolFile {
    /// Archive entry to extract; `None` when the asset itself is the file.
    pub entry: Option<String>,
    /// File name in the tools folder.
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

fn bad(m: String) -> MediaError {
    MediaError::BadManifest(m)
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A tool file name: one plain component ending in `.exe` (no `.bat`/`.cmd`/scripts, no
/// separators, no reserved or odd characters).
pub fn is_tool_file_name(n: &str) -> bool {
    let stem = n.strip_suffix(".exe").unwrap_or("");
    !stem.is_empty()
        && n.len() <= 64
        && stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        && !stem.starts_with('.')
        && !stem.starts_with('-')
}

/// An archive entry: a relative path of plain components joined by `/`.
fn is_safe_entry(e: &str) -> bool {
    !e.is_empty()
        && e.len() <= 512
        && !e.starts_with('/')
        && !e.contains('\\')
        && !e.contains(':')
        && e.split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.chars().any(|ch| ch.is_control()))
}

/// HTTPS, an allowed host, and an immutable GitHub release asset path:
/// `/<owner>/<repo>/releases/download/<tag>/<asset>` with a fixed tag (never `latest`).
pub fn check_asset_url(url: &str, allowed: &[String]) -> Result<(), MediaError> {
    let u = url::Url::parse(url).map_err(|_| bad(format!("not a URL: {url}")))?;
    if u.scheme() != "https" {
        return Err(bad(format!("not HTTPS: {url}")));
    }
    if !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(bad(format!(
            "URL with credentials, query or fragment: {url}"
        )));
    }
    if u.port().is_some() {
        return Err(bad(format!("URL with an explicit port: {url}")));
    }
    let host = u.host_str().unwrap_or("");
    if !allowed.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        return Err(bad(format!("host {host} is not allowed")));
    }
    let segs: Vec<&str> = u.path_segments().map(|s| s.collect()).unwrap_or_default();
    let ok = segs.len() == 6
        && segs[2] == "releases"
        && segs[3] == "download"
        && segs
            .iter()
            .all(|s| !s.is_empty() && *s != "." && *s != "..");
    if !ok || !host.eq_ignore_ascii_case("github.com") {
        return Err(bad(format!(
            "not a pinned GitHub release asset (github.com/<owner>/<repo>/releases/download/<tag>/<asset>): {url}"
        )));
    }
    let tag = segs[4].to_ascii_lowercase();
    if tag == "latest" || tag.contains("nightly") || tag.contains("master") {
        return Err(bad(format!("tag '{}' is not immutable", segs[4])));
    }
    Ok(())
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, MediaError> {
        if text.len() > 64 * 1024 {
            return Err(bad("manifest larger than 64 KiB".into()));
        }
        let m: Manifest = serde_json::from_str(text).map_err(|e| bad(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    /// The manifest compiled into this build.
    pub fn embedded() -> Manifest {
        Manifest::parse(EMBEDDED).expect("the embedded tool manifest is valid (tested)")
    }

    pub fn validate(&self) -> Result<(), MediaError> {
        if self.schema != 1 {
            return Err(bad(format!("unknown schema {}", self.schema)));
        }
        if self.id.is_empty()
            || self.id.len() > 32
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(bad("id must be 1-32 of [A-Za-z0-9-]".into()));
        }
        for h in &self.allowed_hosts {
            if !TOOL_HOSTS.iter().any(|t| t.eq_ignore_ascii_case(h)) {
                return Err(bad(format!(
                    "host {h} is not one of the program's tool hosts"
                )));
            }
        }
        let mut names = vec![];
        for t in &self.tools {
            check_asset_url(&t.url, &self.allowed_hosts)?;
            if !is_sha256_hex(&t.sha256) {
                return Err(bad(format!("{}: sha256 must be 64 lowercase hex", t.name)));
            }
            if t.size == 0 || t.size > MAX_TOOL_BYTES {
                return Err(bad(format!("{}: implausible size {}", t.name, t.size)));
            }
            if t.files.is_empty() {
                return Err(bad(format!("{}: no files", t.name)));
            }
            let is_archive = t.files.iter().any(|f| f.entry.is_some());
            if is_archive && !t.url.ends_with(".zip") {
                return Err(bad(format!(
                    "{}: entries are only supported in .zip assets",
                    t.name
                )));
            }
            for f in &t.files {
                if !is_tool_file_name(&f.name) {
                    return Err(bad(format!(
                        "{}: '{}' is not an allowed tool file name (plain *.exe only)",
                        t.name, f.name
                    )));
                }
                if !is_sha256_hex(&f.sha256) {
                    return Err(bad(format!("{}: sha256 must be 64 lowercase hex", f.name)));
                }
                if f.size == 0 || f.size > MAX_TOOL_BYTES {
                    return Err(bad(format!("{}: implausible size {}", f.name, f.size)));
                }
                match &f.entry {
                    Some(e) if !is_safe_entry(e) => {
                        return Err(bad(format!("{}: unsafe archive entry {e:?}", f.name)))
                    }
                    Some(_) => {}
                    None => {
                        if is_archive
                            || t.files.len() != 1
                            || f.sha256 != t.sha256
                            || f.size != t.size
                        {
                            return Err(bad(format!(
                                "{}: a direct file must be the whole asset (same size and sha256)",
                                f.name
                            )));
                        }
                    }
                }
                let lower = f.name.to_ascii_lowercase();
                if names.contains(&lower) {
                    return Err(bad(format!("{} appears twice", f.name)));
                }
                names.push(lower);
            }
        }
        for r in REQUIRED_FILES {
            if !names.iter().any(|n| n == r) {
                return Err(bad(format!("{r} is missing")));
            }
        }
        Ok(())
    }

    /// Every file the manifest installs.
    pub fn files(&self) -> impl Iterator<Item = &ToolFile> {
        self.tools.iter().flat_map(|t| t.files.iter())
    }

    pub fn file(&self, name: &str) -> Option<&ToolFile> {
        self.files().find(|f| f.name.eq_ignore_ascii_case(name))
    }
}
