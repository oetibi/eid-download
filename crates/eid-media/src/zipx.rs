//! Bounded extraction of named entries from a downloaded tool archive (D5).
//!
//! * only the entries the manifest names are read; every other entry (including ones named
//!   `../x` or `C:\x`) is never touched, and entry names are never used as paths: each output
//!   file gets the manifest's plain file name inside our own folder;
//! * each entry is copied with a hard cap of its pinned size: a lying header or a "zip bomb"
//!   is cut at the cap and refused, so disk use is bounded by the manifest;
//! * each extracted file must match its pinned SHA-256, otherwise it is deleted and refused.

use crate::manifest::ToolFile;
use crate::MediaError;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

fn err(m: String) -> MediaError {
    MediaError::Archive(m)
}

/// Extract `files` (all with `entry: Some`) from `archive` into `out_dir`. Returns the created
/// paths. On error every file created by this call is removed.
pub fn extract_named(
    archive: &Path,
    files: &[&ToolFile],
    out_dir: &Path,
) -> Result<Vec<PathBuf>, MediaError> {
    extract_named_cancellable(archive, files, out_dir, &|| false)
}

/// [`extract_named`] that stops with [`MediaError::Cancelled`] as soon as `cancelled()`
/// returns true (checked before every block read), removing what it created.
pub fn extract_named_cancellable(
    archive: &Path,
    files: &[&ToolFile],
    out_dir: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<PathBuf>, MediaError> {
    let mut created: Vec<PathBuf> = vec![];
    let r = extract_inner(archive, files, out_dir, &mut created, cancelled);
    if r.is_err() {
        for p in &created {
            let _ = std::fs::remove_file(p);
        }
    }
    r.map(|()| created)
}

fn extract_inner(
    archive: &Path,
    files: &[&ToolFile],
    out_dir: &Path,
    created: &mut Vec<PathBuf>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), MediaError> {
    let f = eid_platform::fs::open_regular_nofollow_read(archive)?;
    let mut za = zip::ZipArchive::new(f).map_err(|e| err(format!("not a valid zip: {e}")))?;
    for tf in files {
        let entry = tf
            .entry
            .as_deref()
            .ok_or_else(|| err(format!("{}: no archive entry", tf.name)))?;
        if !crate::manifest::is_tool_file_name(&tf.name) {
            return Err(err(format!("{}: not a tool file name", tf.name)));
        }
        let mut e = za
            .by_name(entry)
            .map_err(|_| err(format!("entry {entry} not found")))?;
        if !e.is_file() {
            return Err(err(format!("entry {entry} is not a file")));
        }
        let dest = out_dir.join(&tf.name);
        let mut out = eid_platform::fs::create_new_file(&dest)?;
        created.push(dest.clone());
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut total: u64 = 0;
        loop {
            if cancelled() {
                return Err(MediaError::Cancelled);
            }
            let n = e
                .read(&mut buf)
                .map_err(|x| err(format!("entry {entry}: {x}")))?;
            if n == 0 {
                break;
            }
            total = total.saturating_add(n as u64);
            if total > tf.size {
                return Err(err(format!(
                    "{}: larger than its pinned {} bytes; cut and refused",
                    tf.name, tf.size
                )));
            }
            h.update(&buf[..n]);
            out.write_all(&buf[..n])?;
        }
        if total != tf.size {
            return Err(err(format!(
                "{}: {total} bytes, pinned {}",
                tf.name, tf.size
            )));
        }
        let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != tf.sha256 {
            return Err(MediaError::HashMismatch {
                file: tf.name.clone(),
            });
        }
        out.sync_all()?;
    }
    Ok(())
}
