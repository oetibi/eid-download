//! Persistent job state: one JSON file, atomic replace, `.bak` fallback, strict validation.
//! The file is created owner-only and never contains credentials or cookies.

use crate::config::MAX_TOTAL_BYTES;
use crate::error::{Error, Result};
use crate::fault;
use crate::sanitize::is_safe_stored_name;
use eid_platform::fs as pfs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const STATE_VERSION: u32 = 1;
pub const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_SEGMENTS: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegState {
    pub start: u64,
    /// First byte NOT yet durably written (the resume frontier).
    pub next: u64,
    pub end: u64,
}

/// Written (and synced) just before the final rename, so a crash after the rename can be
/// recognised as "done" by content identity, never by file size alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finalize {
    /// Exact final file name chosen in the download directory.
    pub name: String,
    /// SHA-256 (lowercase hex) of the complete data.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// Credential-free URL (query kept: it may be required for the request).
    pub url: String,
    /// Sanitized final file name (no directory: the directory is the state file's parent).
    pub file_name: String,
    pub total: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// `true` => `segments` describe a resumable ranged download.
    pub ranged: bool,
    pub segments: Vec<SegState>,
    pub restarts: u32,
    #[serde(default)]
    pub finalize: Option<Finalize>,
    /// SHA-256 (hex) of this struct serialized with `checksum == ""`.
    pub checksum: String,
}

impl State {
    fn compute_checksum(&self) -> String {
        let mut c = self.clone();
        c.checksum = String::new();
        let bytes = serde_json::to_vec(&c).expect("state serializes");
        hex(&Sha256::digest(bytes))
    }

    pub fn seal(&mut self) {
        self.checksum = self.compute_checksum();
    }

    /// Strict validation of an untrusted (read-back) state.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.version != STATE_VERSION {
            return Err(format!("unsupported version {}", self.version));
        }
        if self.checksum != self.compute_checksum() {
            return Err("checksum mismatch (file corrupted or edited)".into());
        }
        let (u, creds) = crate::urlsafe::parse_and_strip(&self.url)?;
        if creds.is_some() || u.to_string() != self.url {
            return Err("URL in state is not in canonical credential-free form".into());
        }
        if !is_safe_stored_name(&self.file_name) {
            return Err("file name in state is not a safe plain name".into());
        }
        if let Some(t) = self.total {
            if t > MAX_TOTAL_BYTES {
                return Err("total size absurd".into());
            }
        }
        if let Some(f) = &self.finalize {
            if !is_safe_stored_name(&f.name) {
                return Err("finalize name is not a safe plain name".into());
            }
            if f.sha256.len() != 64
                || !f
                    .sha256
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            {
                return Err("finalize digest malformed".into());
            }
        }
        if self.segments.len() > MAX_SEGMENTS {
            return Err("too many segments".into());
        }
        if self.ranged {
            let total = self.total.ok_or("ranged state without total")?;
            if total == 0 {
                return Err("ranged state with zero total".into());
            }
            let mut cursor = 0u64;
            for s in &self.segments {
                if s.start != cursor || !(s.start <= s.next && s.next <= s.end) {
                    return Err(format!("segment table inconsistent at {}", s.start));
                }
                cursor = s.end;
            }
            if cursor != total {
                return Err("segment table does not cover the file exactly".into());
            }
        } else if !self.segments.is_empty() {
            return Err("non-ranged state with segments".into());
        }
        Ok(())
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn aux(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

pub fn bak_path(p: &Path) -> PathBuf {
    aux(p, ".bak")
}
pub fn tmp_path(p: &Path) -> PathBuf {
    aux(p, ".tmp")
}
pub fn broken_path(p: &Path) -> PathBuf {
    aux(p, ".broken")
}

/// Atomically persist `state` at `path`. Caller must already have `sync_data`-ed the data file
/// for every byte the state claims is done.
pub fn save(path: &Path, state: &State) -> Result<()> {
    let mut st = state.clone();
    st.seal();
    let json = serde_json::to_vec(&st).map_err(|e| Error::BadState(e.to_string()))?;
    let tmp = tmp_path(path);
    // a stale tmp from a crash may exist; remove (never follows a symlink: removes the link)
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = pfs::create_new_private(&tmp).map_err(Error::io("create state tmp"))?;
        f.write_all(&json).map_err(Error::io("write state"))?;
        f.sync_all().map_err(Error::io("sync state"))?;
    }
    fault::hit("tmp_written");
    let bak = bak_path(path);
    if path.exists() {
        pfs::replace_atomic(path, &bak).map_err(Error::io("rotate state to .bak"))?;
        fault::hit("dest_moved_to_bak");
    }
    pfs::replace_atomic(&tmp, path).map_err(Error::io("replace state"))?;
    fault::hit("state_renamed");
    if let Some(dir) = path.parent() {
        let _ = pfs::sync_dir(dir);
    }
    Ok(())
}

/// Why a sidecar could not be read. Only `Invalid` (the file was opened and read in full and its
/// CONTENT is wrong) is evidence of corruption; `Unreadable` (missing, refused by the no-follow /
/// single-link checks, a transient sharing violation on Windows, ...) says nothing about the
/// bytes, so it must never lead to the file being moved.
struct ReadFailure {
    msg: String,
    invalid_content: bool,
}

fn unreadable(msg: String) -> ReadFailure {
    ReadFailure {
        msg,
        invalid_content: false,
    }
}

fn invalid(msg: String) -> ReadFailure {
    ReadFailure {
        msg,
        invalid_content: true,
    }
}

fn read_classified(path: &Path) -> std::result::Result<State, ReadFailure> {
    let f = pfs::open_regular_nofollow_read(path)
        .map_err(|e| unreadable(format!("{}: {e}", path.display())))?;
    let len = f.metadata().map_err(|e| unreadable(e.to_string()))?.len();
    if len > MAX_STATE_BYTES {
        return Err(invalid("state file too large".into()));
    }
    let mut buf = Vec::with_capacity(len as usize);
    f.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| unreadable(e.to_string()))?;
    if buf.len() as u64 > MAX_STATE_BYTES {
        return Err(invalid("state file too large".into()));
    }
    let st: State =
        serde_json::from_slice(&buf).map_err(|e| invalid(format!("not valid state JSON: {e}")))?;
    st.validate().map_err(invalid)?;
    Ok(st)
}

fn read_validate(path: &Path) -> std::result::Result<State, String> {
    read_classified(path).map_err(|e| e.msg)
}

/// Load `path`, falling back to `path.bak`. **Read-only**: it never renames, creates or deletes
/// anything, so it is safe to call on a live sidecar while the job's checkpoint task rotates it
/// (PLAN P1 open item (a)). A corrupt primary is left exactly where it is; the error (or the
/// backup's state) is all the caller gets. Quarantining is a separate, explicit act:
/// [`load_repairing`].
pub fn load(path: &Path) -> Result<State> {
    match read_validate(path) {
        Ok(s) => Ok(s),
        Err(primary) => from_backup(path, primary),
    }
}

/// [`load`] for the one caller that owns the job: it moves a primary whose CONTENT is invalid
/// aside as `.broken` (so the next [`save`] cannot rotate the corrupt file over a good `.bak`)
/// before falling back to the backup.
///
/// The caller MUST hold the job's lock (`JobLock`) and have no checkpoint task running: that is
/// what makes the rename safe against the rotation race. A primary that merely could not be read
/// (missing, symlink or hard link refused, sharing violation) is never moved.
pub fn load_repairing(path: &Path) -> Result<State> {
    match read_classified(path) {
        Ok(s) => Ok(s),
        Err(primary) => {
            if primary.invalid_content {
                let _ = std::fs::rename(path, broken_path(path));
            }
            from_backup(path, primary.msg)
        }
    }
}

fn from_backup(path: &Path, primary: String) -> Result<State> {
    match read_validate(&bak_path(path)) {
        Ok(s) => Ok(s),
        Err(bak) => Err(Error::BadState(format!("{primary}; backup: {bak}"))),
    }
}

/// Read-only validation of the primary sidecar alone (no `.bak` fallback). Like [`load`], it
/// never renames or deletes anything.
pub fn peek(path: &Path) -> std::result::Result<State, String> {
    read_validate(path)
}

/// The `.eid-state` file in `dir` that belongs to `url` (credential-free, canonical form), if any.
/// Read-only; looks at regular files only and at most `MAX_SCAN` directory entries, so a hostile
/// directory cannot make it run for long. Used to find the sidecar of a job whose file name was
/// never recorded (killed right after the claim).
pub fn find_for_url(dir: &Path, url: &str) -> Option<PathBuf> {
    const MAX_SCAN: usize = 10_000;
    let rd = std::fs::read_dir(dir).ok()?;
    for entry in rd.take(MAX_SCAN).flatten() {
        let name = entry.file_name();
        let Some(n) = name.to_str() else { continue };
        if !n.ends_with(".eid-state") {
            continue;
        }
        let p = entry.path();
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_file() => {}
            _ => continue,
        }
        if peek(&p).is_ok_and(|s| s.url == url) {
            return Some(p);
        }
    }
    None
}

/// Delete only the `.bak` generation (used when a restart replaces the state, so the backup can
/// never certify data of a previous representation) and make the deletion durable.
///
/// Review B1: this MUST NOT fail silently. A `.bak` that survives (an antivirus/indexer holds it
/// open on Windows) would later be picked up by `load` as a valid older generation next to
/// new-generation data. Bounded retries on any error (AGENTS §6.9), then `sync_dir`; the caller
/// aborts the restart BEFORE truncating when this returns `Err`.
pub fn remove_backup(path: &Path) -> Result<()> {
    remove_backup_with(path, 8, std::time::Duration::from_millis(20))
}

pub fn remove_backup_with(path: &Path, attempts: u32, step: std::time::Duration) -> Result<()> {
    let bak = bak_path(path);
    let mut last: Option<std::io::Error> = None;
    for attempt in 0..attempts.max(1) {
        let r =
            crate::fault::fail("remove_backup").and_then(|_| match std::fs::remove_file(&bak) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            });
        match r {
            Ok(()) => {
                if let Some(dir) = path.parent() {
                    pfs::sync_dir(dir).map_err(Error::io("sync dir after removing .bak"))?;
                }
                return Ok(());
            }
            Err(e) => {
                last = Some(e);
                std::thread::sleep(step * (attempt + 1));
            }
        }
    }
    Err(Error::io("remove stale .bak")(
        last.expect("at least one attempt"),
    ))
}

pub fn remove_all(path: &Path) {
    for p in [path.to_path_buf(), bak_path(path), tmp_path(path)] {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> State {
        let mut s = State {
            version: 1,
            url: "https://h.example/f.bin?sig=abc".into(),
            file_name: "f.bin".into(),
            total: Some(100),
            etag: Some("\"e\"".into()),
            last_modified: None,
            ranged: true,
            segments: vec![
                SegState {
                    start: 0,
                    next: 10,
                    end: 60,
                },
                SegState {
                    start: 60,
                    next: 60,
                    end: 100,
                },
            ],
            restarts: 0,
            finalize: None,
            checksum: String::new(),
        };
        s.seal();
        s
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eid-state-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn state_roundtrip_and_private() {
        let d = dir("rt");
        let p = d.join("f.bin.eid-state");
        save(&p, &sample()).unwrap();
        save(&p, &sample()).unwrap(); // second save rotates to .bak
        assert!(bak_path(&p).exists());
        let s = load(&p).unwrap();
        assert_eq!(s.segments.len(), 2);
        // Unix: proven owner-only (Some(true)). Windows: ACLs are not inspected yet (P3) => None,
        // which is "not verified", never counted as proof.
        let want = (!eid_platform::IS_WINDOWS).then_some(true);
        assert_eq!(eid_platform::fs::owner_only(&p).unwrap(), want);
        assert_eq!(
            eid_platform::fs::owner_only(&bak_path(&p)).unwrap(),
            want,
            "the .bak must be private too"
        );
    }

    #[test]
    fn corrupt_or_hostile_state_file_is_rejected() {
        let d = dir("bad");
        let p = d.join("x.eid-state");
        let good = sample();
        let mut cases: Vec<(&str, State)> = Vec::new();
        let mut c = good.clone();
        c.file_name = "../../evil".into();
        cases.push(("traversal name", c));
        let mut c = good.clone();
        c.file_name = "C:\\evil".into();
        cases.push(("drive name", c));
        let mut c = good.clone();
        c.file_name = "CON".into();
        cases.push(("reserved", c));
        let mut c = good.clone();
        c.url = "https://u:pw@h.example/f".into();
        cases.push(("creds in url", c));
        let mut c = good.clone();
        c.url = "file:///etc/passwd".into();
        cases.push(("file scheme", c));
        let mut c = good.clone();
        c.total = Some(u64::MAX);
        cases.push(("absurd total", c));
        let mut c = good.clone();
        c.segments[0].end = 70;
        cases.push(("overlap", c));
        let mut c = good.clone();
        c.segments[1].end = 99;
        cases.push(("short coverage", c));
        let mut c = good.clone();
        c.segments[0].next = 61;
        cases.push(("next beyond end", c));
        let mut c = good.clone();
        c.version = 99;
        cases.push(("version", c));
        for (name, mut s) in cases {
            s.seal(); // checksum valid: the semantic validation must reject it
            let e = s.validate();
            assert!(e.is_err(), "{name} should be rejected");
        }
        // truncated / garbage / checksum edited / oversized
        let json = serde_json::to_vec(&good).unwrap();
        for (name, bytes) in [
            ("truncated", json[..json.len() / 2].to_vec()),
            ("garbage", b"\xff\xfe\x00{{{".to_vec()),
            ("empty", vec![]),
            (
                "edited",
                String::from_utf8(json.clone())
                    .unwrap()
                    .replace("\"next\":10", "\"next\":11")
                    .into_bytes(),
            ),
        ] {
            std::fs::write(&p, bytes).unwrap();
            let _ = std::fs::remove_file(bak_path(&p));
            assert!(load(&p).is_err(), "{name} must be rejected");
            // `load` is read-only: the corrupt file stays exactly where and what it was
            assert!(
                !broken_path(&p).exists(),
                "{name}: load must not quarantine"
            );
            assert_eq!(std::fs::read(&p).unwrap(), bytes_of(&p));
            // only the explicit, lock-holding repair moves it aside
            assert!(load_repairing(&p).is_err(), "{name} must be rejected");
            assert!(
                broken_path(&p).exists() && !p.exists(),
                "{name}: load_repairing quarantines invalid content"
            );
            let _ = std::fs::remove_file(broken_path(&p));
        }
        std::fs::write(&p, vec![b' '; (MAX_STATE_BYTES + 10) as usize]).unwrap();
        assert!(load(&p).is_err());
        assert!(p.exists() && !broken_path(&p).exists());
    }

    fn bytes_of(p: &Path) -> Vec<u8> {
        std::fs::read(p).unwrap()
    }

    /// Every file in `d` with its bytes, to prove a call changed nothing.
    fn snapshot(d: &Path) -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<_> = std::fs::read_dir(d)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).unwrap_or_default(),
                )
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn load_never_changes_anything_on_disk_in_any_state_of_the_pair() {
        let d = dir("ro");
        let p = d.join("r.eid-state");
        let good = serde_json::to_vec(&sample()).unwrap();
        let bad: &[u8] = b"garbage";
        // (primary, backup): each present-good / present-bad / absent
        let kinds: [Option<&[u8]>; 3] = [Some(&good), Some(bad), None];
        for prim in kinds {
            for bak in kinds {
                for f in [&p, &bak_path(&p), &broken_path(&p), &tmp_path(&p)] {
                    let _ = std::fs::remove_file(f);
                }
                if let Some(b) = prim {
                    std::fs::write(&p, b).unwrap();
                }
                if let Some(b) = bak {
                    std::fs::write(bak_path(&p), b).unwrap();
                }
                let before = snapshot(&d);
                let r = load(&p);
                assert_eq!(
                    r.is_ok(),
                    prim == Some(&good[..]) || bak == Some(&good[..]),
                    "primary {prim:?} backup {bak:?}"
                );
                assert_eq!(snapshot(&d), before, "load changed files");
            }
        }
    }

    #[test]
    fn corrupt_primary_with_good_bak_is_served_from_the_bak_without_touching_the_primary() {
        let d = dir("robak");
        let p = d.join("b.eid-state");
        save(&p, &sample()).unwrap();
        save(&p, &sample()).unwrap();
        std::fs::write(&p, b"garbage").unwrap();
        let before = snapshot(&d);
        assert_eq!(load(&p).unwrap().file_name, "f.bin");
        assert_eq!(snapshot(&d), before);
        // the repairing load serves the same state but sets the corrupt file aside
        assert_eq!(load_repairing(&p).unwrap().file_name, "f.bin");
        assert_eq!(bytes_of(&broken_path(&p)), b"garbage");
        assert!(!p.exists());
    }

    #[test]
    fn load_repairing_never_moves_a_primary_that_was_merely_unreadable() {
        let d = dir("unread");
        let p = d.join("u.eid-state");
        // a DIRECTORY where the sidecar should be: the open is refused (not a regular file), which
        // says nothing about content, exactly like a transient sharing violation on Windows
        std::fs::create_dir(&p).unwrap();
        assert!(load_repairing(&p).is_err());
        assert!(p.is_dir() && !broken_path(&p).exists());
        // and a missing primary is not "broken" either
        std::fs::remove_dir(&p).unwrap();
        assert!(load_repairing(&p).is_err());
        assert!(!broken_path(&p).exists());
    }

    #[test]
    fn corrupt_primary_recovers_from_bak() {
        let d = dir("bak");
        let p = d.join("y.eid-state");
        save(&p, &sample()).unwrap();
        save(&p, &sample()).unwrap();
        std::fs::write(&p, b"garbage").unwrap();
        let s = load(&p).unwrap();
        assert_eq!(s.file_name, "f.bin");
    }

    #[test]
    fn security_state_never_contains_secrets_on_disk() {
        let d = dir("sec");
        let p = d.join("z.eid-state");
        let (u, _) =
            crate::urlsafe::parse_and_strip("https://alice:hunter2@h.example/f.bin").unwrap();
        let mut s = sample();
        s.url = u.to_string();
        s.seal();
        save(&p, &s).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(!raw.contains("hunter2") && !raw.contains("alice") && !raw.contains('@'));
    }

    #[test]
    fn symlinked_state_path_is_not_followed() {
        let d = dir("sym");
        let outside = d.join("outside");
        std::fs::write(&outside, b"SENTINEL").unwrap();
        let p = d.join("l.eid-state");
        eid_platform::fs::symlink_file(&outside, &p).unwrap();
        assert!(load(&p).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"SENTINEL");
    }
}
