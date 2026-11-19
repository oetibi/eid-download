//! Staging folders and publishing (shared by `eid video` and the job layer, [`crate::jobs`]).
//!
//! yt-dlp works in a staging folder we created (its working directory, constant output
//! template); we publish the finished file ourselves under a sanitized name, never
//! overwriting, after checking it through the handle that is moved (review M4, n3, r2).

use crate::args::{MediaUrl, Quality};
use std::path::{Path, PathBuf};

/// Control and bidi characters become '?' (text from disk or the tool, shown in one line).
pub fn printable(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') {
                '?'
            } else {
                c
            }
        })
        .collect()
}

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
    // a reused folder (resume) must contain nothing but plain files and folders, at any depth
    // (review m7); the walk is bounded and never enters a link
    let mut budget = STAGING_MAX_ENTRIES;
    check_staging_tree(&p, &p, 0, &mut budget)?;
    Ok(p)
}

/// Bounds for walking a reused staging folder. yt-dlp with our template writes only files at
/// its top level, so anything near these bounds is not ours.
pub const STAGING_MAX_DEPTH: usize = 8;
pub const STAGING_MAX_ENTRIES: usize = 10_000;

fn check_staging_tree(
    root: &Path,
    dir: &Path,
    depth: usize,
    budget: &mut usize,
) -> Result<(), String> {
    let refuse = |why: String| format!("refusing to use {}: {why}", root.display());
    let unreadable = |e: std::io::Error| {
        refuse(format!(
            "cannot read {}: {e}",
            printable(&dir.to_string_lossy())
        ))
    };
    for e in std::fs::read_dir(dir).map_err(unreadable)? {
        let e = e.map_err(unreadable)?;
        *budget = budget.checked_sub(1).ok_or_else(|| {
            refuse(format!(
                "it holds more than {STAGING_MAX_ENTRIES} entries; delete that folder to \
                 download this item again from the start"
            ))
        })?;
        let path = e.path();
        let rel = || printable(&path.strip_prefix(root).unwrap_or(&path).to_string_lossy());
        let md = std::fs::symlink_metadata(&path).map_err(unreadable)?;
        if is_link_or_odd(&md) {
            return Err(refuse(format!("it contains a link ({})", rel())));
        }
        // a hard link to a file elsewhere would let the tool append to it when it resumes
        // (review n3): every file is opened without following links and must have one name
        if md.is_file() {
            if let Err(e) = eid_platform::fs::open_regular_nofollow_read(&path) {
                return Err(refuse(if e.kind() == std::io::ErrorKind::InvalidInput {
                    format!("it contains a hard link or reparse point ({}): {e}", rel())
                } else {
                    format!("cannot check {}: {e}", rel())
                }));
            }
        }
        if md.is_dir() {
            if depth + 1 >= STAGING_MAX_DEPTH {
                return Err(refuse(format!(
                    "its folders are nested more than {STAGING_MAX_DEPTH} deep"
                )));
            }
            check_staging_tree(root, &path, depth + 1, budget)?;
        }
    }
    Ok(())
}

/// Remove a staging folder that is still empty after a cancel. The killed tree dies
/// asynchronously (Windows `TerminateJobObject`), and until it has, the tool's working
/// directory cannot be removed, so the removal is retried for up to 2 s (AGENTS 6.9). A folder
/// that is not empty (partial data, for resume) is never touched.
pub async fn remove_empty_staging(s: &Path) {
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

/// The finished file in the staging folder: exactly one plain file with the expected extension
/// (an `.mp4` must also be a whole MP4 container). When the tool produced something else (for
/// example a WebM it could not remux), the error names it; the file stays in staging. A
/// leftover partial conversion (`<id>.temp.mp4`) is never taken: next to the result it makes
/// two candidates (refused), alone it fails the container check.
pub fn finished_file(staging: &Path, ext: &str) -> Result<PathBuf, String> {
    let mut found = vec![];
    let mut other = vec![];
    for e in std::fs::read_dir(staging).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let md = std::fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
        let n = e.file_name().to_string_lossy().into_owned();
        if !md.is_file() {
            continue;
        }
        if n.ends_with(&format!(".{ext}")) {
            found.push(e.path());
        } else {
            other.push(n);
        }
    }
    match found.as_slice() {
        [one] if ext == "mp4" => {
            // checked here (a playlist then counts it as a failed item and retries it) and
            // again through the handle that is published
            let f = eid_platform::fs::open_regular_nofollow_read(one)
                .map_err(|e| format!("refusing the staged file: {e}"))?;
            let mut r = &f;
            crate::container::check_mp4(&mut r).map_err(|e| {
                format!(
                    "the tool's {} is {e}; nothing was published, it is kept in {}",
                    one.file_name().unwrap_or_default().to_string_lossy(),
                    staging.display()
                )
            })?;
            Ok(one.clone())
        }
        [one] => Ok(one.clone()),
        [] if !other.is_empty() => Err(format!(
            "the tool finished but left no .{ext} file (it left {}); the file is kept in {}",
            other.join(", "),
            staging.display()
        )),
        [] => Err(format!("the tool finished but left no .{ext} file")),
        _ => Err(format!(
            "the tool left {} .{ext} files; refusing to guess",
            found.len()
        )),
    }
}

/// Longest staging path we allow for yt-dlp's own files (`<id>.f137.mp4.part` and friends).
pub const TOOL_NAME_RESERVE: usize = 96;

pub fn units(p: &Path) -> usize {
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

/// Why [`publish`] published nothing.
#[derive(Debug)]
pub enum PublishError {
    /// The staged file is not a plain file we can open without following links, or it or the
    /// destination was swapped while publishing (something was planted in our folders): the
    /// run stops, fail closed. The next run is not told about it (known limit, Round 17).
    Refused(String),
    /// The staged file is in use by another program (for example an antivirus): retried by
    /// [`publish_retrying`] (AGENTS §6.9), then an [`PublishError::Item`] failure.
    Locked(String),
    /// This file could not be published (bad container, I/O error, no free name). In a
    /// playlist this is a failed item, tried again at the next run (review m3).
    Item(String),
    /// Ctrl-C while waiting for a locked file (review r3): nothing was published.
    Cancelled,
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishError::Refused(m) | PublishError::Locked(m) | PublishError::Item(m) => {
                f.write_str(m)
            }
            PublishError::Cancelled => f.write_str("stopped while waiting for a locked file"),
        }
    }
}

/// How often and how long a locked staged file is retried before the item fails.
pub const PUBLISH_LOCK_TRIES: u32 = 10;
pub const PUBLISH_LOCK_PAUSE: std::time::Duration = std::time::Duration::from_millis(200);

/// [`publish`], retried while the staged file is locked (review n4, AGENTS §6.9). The pause is
/// an async sleep, never a blocking one (§6.7), and Ctrl-C ends it at once (review r3).
pub async fn publish_retrying(
    staged: &Path,
    out_dir: &Path,
    name: &str,
    cancel: &eid_engine::CancellationToken,
    fault: &PublishFault,
) -> Result<PathBuf, PublishError> {
    retry_locked(PUBLISH_LOCK_TRIES, PUBLISH_LOCK_PAUSE, cancel, || {
        publish_with_fault(staged, out_dir, name, fault)
    })
    .await
}

pub async fn retry_locked<T>(
    tries: u32,
    pause: std::time::Duration,
    cancel: &eid_engine::CancellationToken,
    mut attempt: impl FnMut() -> Result<T, PublishError>,
) -> Result<T, PublishError> {
    let mut left = tries.max(1);
    loop {
        match attempt() {
            Err(PublishError::Locked(m)) => {
                left -= 1;
                if left == 0 {
                    return Err(PublishError::Item(format!(
                        "{m}; still locked after {tries} tries, it is tried again at the next run"
                    )));
                }
                tokio::select! {
                    _ = tokio::time::sleep(pause) => {}
                    _ = cancel.cancelled() => return Err(PublishError::Cancelled),
                }
            }
            other => return other,
        }
    }
}

/// Publish `staged` (opened without following links and checked to be a plain file) into
/// `out_dir` under `name`, or `name (n)` if taken; never overwrites anything. An `.mp4` must
/// be a whole MP4 container, checked through the same handle that is published (review M4);
/// otherwise nothing is published and the file stays in staging.
pub fn publish(staged: &Path, out_dir: &Path, name: &str) -> Result<PathBuf, PublishError> {
    publish_with_fault(staged, out_dir, name, &|_| None)
}

/// A test fault for a candidate name: `Some(kind)` makes publishing under that name fail with
/// an I/O error of that kind (`InvalidData` = a detected swap).
pub type PublishFault = dyn Fn(&str) -> Option<std::io::ErrorKind> + Send + Sync;

/// [`publish`] with a fault injector (tests).
pub fn publish_with_fault(
    staged: &Path,
    out_dir: &Path,
    name: &str,
    fault: &PublishFault,
) -> Result<PathBuf, PublishError> {
    let f = eid_platform::fs::open_regular_nofollow(staged).map_err(|e| {
        // only "in use by another program" is retried; access denied (a folder or junction
        // swapped in, a delete-pending file) is a refusal like any other (review r2)
        if eid_platform::fs::is_file_in_use(&e) {
            PublishError::Locked(format!(
                "the staged file {} is in use by another program: {e}",
                staged.display()
            ))
        } else {
            PublishError::Refused(format!("refusing the staged file: {e}"))
        }
    })?;
    if name.ends_with(".mp4") {
        let mut r = &f;
        crate::container::check_mp4(&mut r).map_err(|e| {
            PublishError::Item(format!(
                "refusing to publish {}: {e}; it is kept in the staging folder",
                staged.display()
            ))
        })?;
    }
    for n in 0..1000u32 {
        let cand = if n == 0 {
            name.to_string()
        } else {
            eid_engine::sanitize::numbered_name(name, n)
        };
        let dest = out_dir.join(&cand);
        // tests only: an injected I/O error at this name, through the same classifier as a
        // real error (the CLI's `test-hooks` build wires `EID_TEST_PUBLISH_FAIL` here)
        if let Some(kind) = fault(&cand) {
            return Err(publish_failure(
                &dest,
                std::io::Error::new(kind, "injected test fault"),
            ));
        }
        match eid_platform::fs::publish_verified(&f, staged, &dest) {
            Ok(()) => return Ok(dest),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(publish_failure(&dest, e)),
        }
    }
    Err(PublishError::Item(
        "no free file name (1000 taken); it is kept in the staging folder".into(),
    ))
}

/// A failed `publish_verified`. A swap of the staged file or of the destination (kind
/// `InvalidData`, eid-platform's contract) stops this run, so nothing more is published while
/// something is changing our files (review of m3). The next run is NOT protected: it finds
/// whatever is then in staging and is not told about the swap (known limit, Round 17).
/// Anything else fails only this item. The file may or may not still be in staging at that
/// point (a post-check can fail after the move), so the message does not say where it is.
pub fn publish_failure(dest: &Path, e: std::io::Error) -> PublishError {
    if e.kind() == std::io::ErrorKind::InvalidData {
        PublishError::Refused(format!(
            "stopped: the file to publish as {} changed while it was being published ({e}); \
             nothing more is published in this run. Check the staging folder and that file \
             before running again",
            dest.display()
        ))
    } else {
        PublishError::Item(format!(
            "cannot publish {}: {e}; the item is tried again at the next run",
            dest.display()
        ))
    }
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

/// Items yt-dlp's listing reports as not downloadable without signing in (skipped, number
/// kept, never worked around).
pub fn listed_unavailable(availability: Option<&str>) -> bool {
    matches!(
        availability,
        Some("private" | "needs_auth" | "subscriber_only" | "premium_only")
    )
}
