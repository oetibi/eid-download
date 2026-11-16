//! Playlists (F11a, numbering rules D8 in docs/plans/P4.md section 4).
//!
//! The state lives inside the playlist folder in `.eid-playlist.json` (private, checksummed,
//! bounded, validated and confined on load; D13). Rules:
//!
//! 1. the number width is fixed at the first download: 2 if the playlist has at most 99 items,
//!    otherwise 3; it never changes;
//! 2. an item's number is its position when it is first seen; it never changes and is never
//!    reused (also when the item later disappears or becomes private);
//! 3. on sync, new items get `max + 1`, in the order they appear (items added at the top do
//!    not shift the old ones);
//! 4. a number wider than the width is written with its natural digits (`100 - ...`); no old
//!    file is ever renamed or cut;
//! 5. a video listed twice is one item, with the number of its first appearance;
//! 6. names are `<digits> - <title>.<ext>`; numbers are unique, so names are unique however
//!    the titles look; a foreign file with the same name gets the collision suffix at publish
//!    time and the real name is recorded.

use crate::info::Entry;
use crate::MediaError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

pub const STATE_FILE: &str = ".eid-playlist.json";
pub const LOCK_FILE: &str = ".eid-lock";
pub const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_ITEMS: usize = 100_000;
pub const MAX_NUMBER: u32 = 1_000_000;
pub const MAX_ID: usize = 256;
/// Schema written by this version. Schema 1 (before the P4 review, M2) is still read.
pub const SCHEMA: u32 = 2;
/// Failed runs of one item before automatic retries stop (owner decision M2, 2026-11-16).
pub const MAX_ATTEMPTS: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// To download (also after a temporary failure, while `attempts < MAX_ATTEMPTS`).
    Pending,
    Done,
    /// An explicit access restriction (private, removed, sign-in, membership, country) or DRM:
    /// not retried automatically, number kept; `--retry-failed` lets the user try again.
    Unavailable,
    /// `MAX_ATTEMPTS` temporary failures in a row: not retried automatically, number kept,
    /// shown at every run; `--retry-failed` starts over.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub number: u32,
    pub status: Status,
    /// Published file name (one component inside the playlist folder), once done.
    pub file: Option<String>,
    /// Temporary failures in a row (`MAX_ATTEMPTS` exactly when `Failed`, less otherwise).
    pub attempts: u32,
}

impl Item {
    /// Record one temporary failure; returns true when automatic retries stop now.
    pub fn record_failure(&mut self) -> bool {
        self.attempts = self.attempts.saturating_add(1).min(MAX_ATTEMPTS);
        if self.attempts >= MAX_ATTEMPTS {
            self.status = Status::Failed;
            true
        } else {
            self.status = Status::Pending;
            false
        }
    }

    /// `--retry-failed`: a failed or unavailable item is tried again from zero. Returns false
    /// (and changes nothing) for any other item.
    pub fn reset_for_retry(&mut self) -> bool {
        if matches!(self.status, Status::Failed | Status::Unavailable) {
            self.status = Status::Pending;
            self.attempts = 0;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub playlist_id: String,
    pub width: u8,
    pub items: Vec<Item>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OnDisk {
    schema: u32,
    state: State,
    sha256: String,
}

/// Schema 1, exactly as written before the M2 change (field order matters for the checksum).
mod v1 {
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Status {
        Pending,
        Done,
        Unavailable,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Item {
        pub id: String,
        pub number: u32,
        pub status: Status,
        pub file: Option<String>,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct State {
        pub playlist_id: String,
        pub width: u8,
        pub items: Vec<Item>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct OnDisk {
        #[allow(dead_code)]
        pub schema: u32,
        pub state: State,
        pub sha256: String,
    }
}

/// Upgrade a verified schema-1 state. Numbers, ids, files, width and `done` are kept as they
/// are. A schema-1 `unavailable` was set by the old classifier, which also matched warnings
/// and vague messages (review M2), so it becomes `pending` again: an explicit restriction in
/// the listing marks it again before any tool runs, an explicit tool error after one try.
fn upgrade_v1(s: v1::State) -> State {
    State {
        playlist_id: s.playlist_id,
        width: s.width,
        items: s
            .items
            .into_iter()
            .map(|i| Item {
                id: i.id,
                number: i.number,
                status: match i.status {
                    v1::Status::Done => Status::Done,
                    v1::Status::Pending | v1::Status::Unavailable => Status::Pending,
                },
                file: i.file,
                attempts: 0,
            })
            .collect(),
    }
}

fn bad(m: impl Into<String>) -> MediaError {
    MediaError::Playlist(m.into())
}

fn sha_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn digest(s: &State) -> String {
    sha_hex(&serde_json::to_vec(s).expect("state serializes"))
}

fn is_ok_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID && !id.chars().any(|c| c.is_control())
}

impl State {
    /// Every rule a state file must satisfy before it is trusted.
    pub fn validate(&self) -> Result<(), MediaError> {
        if !is_ok_id(&self.playlist_id) {
            return Err(bad("bad playlist id"));
        }
        if !(2..=7).contains(&self.width) {
            return Err(bad(format!("bad width {}", self.width)));
        }
        if self.items.len() > MAX_ITEMS {
            return Err(bad("too many items"));
        }
        let mut ids = std::collections::HashSet::new();
        let mut numbers = std::collections::HashSet::new();
        let mut files = std::collections::HashSet::new();
        for it in &self.items {
            if !is_ok_id(&it.id) || !ids.insert(it.id.as_str()) {
                return Err(bad("bad or duplicate item id"));
            }
            if it.number == 0 || it.number > MAX_NUMBER || !numbers.insert(it.number) {
                return Err(bad(format!("bad or duplicate number {}", it.number)));
            }
            let attempts_ok = if it.status == Status::Failed {
                it.attempts == MAX_ATTEMPTS
            } else {
                it.attempts < MAX_ATTEMPTS
            };
            if !attempts_ok {
                return Err(bad(format!(
                    "bad attempt count {} for a {:?} item",
                    it.attempts, it.status
                )));
            }
            match (&it.status, &it.file) {
                (Status::Done, Some(f)) => {
                    // one plain, already-sanitized component: confined to the playlist folder
                    if !eid_engine::sanitize::is_safe_stored_name(f)
                        || f.starts_with('.')
                        || !files.insert(f.to_lowercase())
                    {
                        return Err(bad(format!("unsafe or duplicate file name {f:?}")));
                    }
                }
                // done earlier, known only from the download archive (state was lost)
                (Status::Done, None) => {}
                (_, Some(_)) => return Err(bad("file on an item that is not done")),
                _ => {}
            }
        }
        Ok(())
    }

    pub fn max_number(&self) -> u32 {
        self.items.iter().map(|i| i.number).max().unwrap_or(0)
    }

    pub fn get(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Item> {
        self.items.iter_mut().find(|i| i.id == id)
    }
}

/// The listing's distinct entries in playlist order, each with its number.
pub type Planned<'a> = Vec<(u32, &'a Entry)>;

/// Merge the current listing into the state (rules 1, 2, 3, 5). Returns the state and the
/// listing's distinct entries in playlist order, each with its number.
pub fn plan<'a>(
    old: Option<State>,
    playlist_id: &str,
    entries: &'a [Entry],
) -> Result<(State, Planned<'a>), MediaError> {
    let mut seen = std::collections::HashSet::new();
    let distinct: Vec<&Entry> = entries
        .iter()
        .filter(|e| is_ok_id(&e.id) && seen.insert(e.id.clone()))
        .collect();
    let mut st = match old {
        Some(s) => {
            if s.playlist_id != playlist_id {
                return Err(bad(
                    "this folder belongs to a different playlist; choose another folder",
                ));
            }
            s
        }
        None => State {
            playlist_id: playlist_id.to_string(),
            width: if distinct.len() <= 99 { 2 } else { 3 },
            items: vec![],
        },
    };
    let mut next = st.max_number();
    // first run: numbers are positions (1..); later runs: new items get max+1 in order
    for e in &distinct {
        if st.get(&e.id).is_none() {
            next = next
                .checked_add(1)
                .filter(|n| *n <= MAX_NUMBER)
                .ok_or_else(|| bad("too many numbers"))?;
            st.items.push(Item {
                id: e.id.clone(),
                number: next,
                status: Status::Pending,
                file: None,
                attempts: 0,
            });
        }
    }
    if st.items.len() > MAX_ITEMS {
        return Err(bad("too many items"));
    }
    let out = distinct
        .into_iter()
        .map(|e| (st.get(&e.id).expect("just added").number, e))
        .collect();
    Ok((st, out))
}

/// `<digits> - <title>.<ext>` (rules 4 and 6), sanitized like every file name we create.
pub fn item_name(width: u8, number: u32, title: &str, ext: &str) -> String {
    let title = if title.trim().is_empty() {
        "video"
    } else {
        title
    };
    // The title is made safe on its own first: sanitizing "<number> - <title>" as a whole would
    // let a title like "../../x" cut the number off with the "directory" part, and the names
    // would stop being unique (rule 6). The number prefix is digits and " - " only.
    let safe = eid_engine::sanitize::sanitize_file_name(&format!("{title}.{ext}"));
    eid_engine::sanitize::sanitize_file_name(&format!("{number:0w$} - {safe}", w = width as usize))
}

/// Load the state of a playlist folder. Missing = `None`. A link, an oversized, malformed,
/// tampered (checksum) or invalid file is an error: the run stops instead of renumbering.
pub fn load(dir: &Path) -> Result<Option<State>, MediaError> {
    let p = dir.join(STATE_FILE);
    let f = match eid_platform::fs::open_regular_nofollow_read(&p) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(bad(format!("cannot open {STATE_FILE}: {e}"))),
    };
    let mut buf = vec![];
    f.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| bad(e.to_string()))?;
    if buf.len() as u64 > MAX_STATE_BYTES {
        return Err(bad(format!("{STATE_FILE} is larger than 8 MiB")));
    }
    let invalid = |e: serde_json::Error| bad(format!("{STATE_FILE} is invalid: {e}"));
    let mismatch = || {
        bad(format!(
            "{STATE_FILE} checksum mismatch (damaged or edited)"
        ))
    };
    // the schema decides the exact shape; each shape is strict (`deny_unknown_fields`)
    let schema = serde_json::from_slice::<serde_json::Value>(&buf)
        .map_err(invalid)?
        .get("schema")
        .and_then(serde_json::Value::as_u64);
    let state = match schema {
        Some(1) => {
            let d: v1::OnDisk = serde_json::from_slice(&buf).map_err(invalid)?;
            let raw = serde_json::to_vec(&d.state).expect("state serializes");
            if sha_hex(&raw) != d.sha256 {
                return Err(mismatch());
            }
            upgrade_v1(d.state)
        }
        Some(2) => {
            let d: OnDisk = serde_json::from_slice(&buf).map_err(invalid)?;
            if digest(&d.state) != d.sha256 {
                return Err(mismatch());
            }
            d.state
        }
        other => {
            return Err(bad(format!(
                "unknown {STATE_FILE} schema {}",
                other.map_or("(none)".to_string(), |s| s.to_string())
            )))
        }
    };
    state.validate()?;
    Ok(Some(state))
}

/// Atomic, private write of the state (always the current [`SCHEMA`]).
pub fn save(dir: &Path, s: &State) -> Result<(), MediaError> {
    s.validate()?;
    let d = OnDisk {
        schema: SCHEMA,
        sha256: digest(s),
        state: s.clone(),
    };
    let b = serde_json::to_vec_pretty(&d).map_err(|e| bad(e.to_string()))?;
    eid_platform::ipc::write_private_atomic(&dir.join(STATE_FILE), &b)
        .map_err(|e| bad(format!("cannot write {STATE_FILE}: {e}")))
}

/// Exclusive lock on the playlist folder (`.eid-lock`), held for the whole run.
pub struct FolderLock {
    _f: std::fs::File,
}

pub fn lock_folder(dir: &Path) -> Result<FolderLock, MediaError> {
    let f = eid_platform::fs::open_or_create_lock_file(&dir.join(LOCK_FILE))
        .map_err(|e| bad(format!("cannot open {LOCK_FILE}: {e}")))?;
    match eid_platform::fs::try_lock_exclusive(&f) {
        Ok(true) => Ok(FolderLock { _f: f }),
        Ok(false) => Err(bad(
            "another eid run is already working on this playlist folder",
        )),
        Err(e) => Err(bad(format!("cannot lock {LOCK_FILE}: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str) -> Entry {
        Entry {
            id: id.into(),
            title: format!("t-{id}"),
            url: Some(format!("https://fake.test/video/{id}")),
            availability: None,
        }
    }

    fn numbers(st: &State) -> Vec<(String, u32)> {
        st.items.iter().map(|i| (i.id.clone(), i.number)).collect()
    }

    #[test]
    fn numbering_follows_the_d8_rules() {
        let list: Vec<Entry> = ["a", "b", "a", "c"].iter().map(|x| e(x)).collect();
        let (st, planned) = plan(None, "pl", &list).unwrap();
        assert_eq!(st.width, 2);
        assert_eq!(
            numbers(&st),
            [("a".into(), 1), ("b".into(), 2), ("c".into(), 3)]
        );
        assert_eq!(planned.iter().map(|p| p.0).collect::<Vec<_>>(), [1, 2, 3]);
        // prepended and appended items take max+1 in order of appearance
        let list: Vec<Entry> = ["x", "a", "b", "c", "y"].iter().map(|x| e(x)).collect();
        let (st2, planned) = plan(Some(st.clone()), "pl", &list).unwrap();
        assert_eq!(
            planned
                .iter()
                .map(|p| (p.1.id.as_str(), p.0))
                .collect::<Vec<_>>(),
            [("x", 4), ("a", 1), ("b", 2), ("c", 3), ("y", 5)]
        );
        // a vanished item keeps its number and it is never reused
        let list: Vec<Entry> = ["a", "z"].iter().map(|x| e(x)).collect();
        let (st3, _) = plan(Some(st2), "pl", &list).unwrap();
        assert_eq!(st3.get("b").unwrap().number, 2);
        assert_eq!(st3.get("z").unwrap().number, 6);
        assert!(plan(Some(st3), "other", &list).is_err());
        let many: Vec<Entry> = (0..100).map(|i| e(&format!("i{i}"))).collect();
        assert_eq!(plan(None, "pl", &many).unwrap().0.width, 3);
        assert_eq!(item_name(2, 7, "x", "mp4"), "07 - x.mp4");
        assert_eq!(item_name(2, 100, "x", "mp4"), "100 - x.mp4");
        assert_eq!(item_name(3, 7, "", "mp3"), "007 - video.mp3");
    }

    /// Written by the pre-M2 code (`save` of schema 1), kept byte for byte.
    const V1: &[u8] = include_bytes!("../tests/fixtures/playlist-state-v1.json");

    #[test]
    fn playlist_state_schema_1_is_read_and_upgraded_without_losing_data() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(STATE_FILE), V1).unwrap();
        let st = load(d.path()).unwrap().unwrap();
        assert_eq!(st.playlist_id, "pl-v1");
        assert_eq!(st.width, 2);
        let got: Vec<(&str, u32, Status, Option<&str>, u32)> = st
            .items
            .iter()
            .map(|i| {
                (
                    i.id.as_str(),
                    i.number,
                    i.status,
                    i.file.as_deref(),
                    i.attempts,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("a", 1, Status::Done, Some("01 - Alpha.mp4"), 0),
                // the old classifier's "unavailable" is tried again
                ("b", 2, Status::Pending, None, 0),
                ("c", 3, Status::Pending, None, 0),
                ("d", 7, Status::Done, None, 0),
            ]
        );
        // loading never writes: the old file is untouched until a run saves
        assert_eq!(std::fs::read(d.path().join(STATE_FILE)).unwrap(), V1);
        // saving writes schema 2, which loads back to the same state
        save(d.path(), &st).unwrap();
        let t: serde_json::Value =
            serde_json::from_slice(&std::fs::read(d.path().join(STATE_FILE)).unwrap()).unwrap();
        assert_eq!(t["schema"], SCHEMA);
        assert_eq!(load(d.path()).unwrap().unwrap(), st);
        // a schema-1 file is still checked: an edit breaks its checksum, an unknown field or
        // an `attempts` field (schema 2 only) is refused
        // line endings depend on the checkout (Windows CI may convert them): edit an LF copy
        let text = std::str::from_utf8(V1).unwrap().replace("\r\n", "\n");
        for bad in [
            text.replace("\"number\": 7", "\"number\": 8"),
            text.replace(
                "\"file\": null\n      },\n      {\n        \"id\": \"c\"",
                "\"file\": null, \"x\": 1\n      },\n      {\n        \"id\": \"c\"",
            ),
            text.replace(
                "\"status\": \"pending\",",
                "\"status\": \"pending\", \"attempts\": 0,",
            ),
            text.replace("\"schema\": 1", "\"schema\": 3"),
        ] {
            assert_ne!(bad, text, "the edit did not apply");
            std::fs::write(d.path().join(STATE_FILE), &bad).unwrap();
            assert!(load(d.path()).is_err(), "accepted: {bad}");
        }
    }

    #[test]
    fn failed_attempts_stop_at_three_and_retry_resets_only_failed_or_unavailable() {
        let mut i = Item {
            id: "x".into(),
            number: 4,
            status: Status::Pending,
            file: None,
            attempts: 0,
        };
        assert!(!i.record_failure());
        assert_eq!((i.status, i.attempts), (Status::Pending, 1));
        assert!(!i.record_failure());
        assert_eq!((i.status, i.attempts), (Status::Pending, 2));
        assert!(
            i.record_failure(),
            "the third failure stops automatic retries"
        );
        assert_eq!((i.status, i.attempts), (Status::Failed, MAX_ATTEMPTS));
        assert!(!i.record_failure() || i.status == Status::Failed);
        assert_eq!(i.attempts, MAX_ATTEMPTS, "never counts past the limit");
        assert!(i.reset_for_retry());
        assert_eq!((i.status, i.attempts, i.number), (Status::Pending, 0, 4));
        // only failed or unavailable items are reset
        let mut done = Item {
            status: Status::Done,
            file: Some("04 - x.mp4".into()),
            ..i.clone()
        };
        assert!(!done.reset_for_retry());
        assert_eq!(done.status, Status::Done);
        let mut gone = Item {
            status: Status::Unavailable,
            ..i.clone()
        };
        assert!(gone.reset_for_retry());
        assert_eq!(gone.status, Status::Pending);
        let mut pending = Item {
            attempts: 2,
            ..i.clone()
        };
        assert!(!pending.reset_for_retry());
        assert_eq!(pending.attempts, 2);
        // a state file must agree with the counter
        for (status, attempts) in [
            (Status::Failed, 2),
            (Status::Pending, 3),
            (Status::Done, 3),
            (Status::Unavailable, 9),
        ] {
            let st = State {
                playlist_id: "pl".into(),
                width: 2,
                items: vec![Item {
                    id: "x".into(),
                    number: 1,
                    status,
                    file: None,
                    attempts,
                }],
            };
            assert!(
                st.validate().is_err(),
                "{status:?} with {attempts} attempts"
            );
        }
    }

    #[test]
    fn hostile_titles_never_remove_the_number_prefix() {
        for t in [
            "../../evil",
            "..\\..\\evil",
            "/abs/path",
            "C:\\x\\y",
            "a\u{202E}4pm.exe",
            "x:ads",
            "CON",
            "...",
            "   ",
            &"ع".repeat(300),
        ] {
            let n = item_name(2, 4, t, "mp4");
            assert!(n.starts_with("04 - "), "{t:?} -> {n:?}");
            assert!(n.ends_with(".mp4"), "{t:?} -> {n:?}");
            assert!(!n.contains(['/', '\\', ':']), "{t:?} -> {n:?}");
            assert!(n.len() <= 255, "{t:?} -> {} bytes", n.len());
        }
    }
}
