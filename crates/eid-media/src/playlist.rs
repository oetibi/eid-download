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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Done,
    /// Private, removed, sign-in only or DRM: skipped, number kept.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub number: u32,
    pub status: Status,
    /// Published file name (one component inside the playlist folder), once done.
    pub file: Option<String>,
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

fn bad(m: impl Into<String>) -> MediaError {
    MediaError::Playlist(m.into())
}

fn digest(s: &State) -> String {
    let b = serde_json::to_vec(s).expect("state serializes");
    Sha256::digest(&b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
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

/// Merge the current listing into the state (rules 1, 2, 3, 5). Returns the state and the
/// listing's distinct entries in playlist order, each with its number.
pub fn plan<'a>(
    old: Option<State>,
    playlist_id: &str,
    entries: &'a [Entry],
) -> Result<(State, Vec<(u32, &'a Entry)>), MediaError> {
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
    eid_engine::sanitize::sanitize_file_name(&format!(
        "{number:0w$} - {title}.{ext}",
        w = width as usize
    ))
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
    let d: OnDisk =
        serde_json::from_slice(&buf).map_err(|e| bad(format!("{STATE_FILE} is invalid: {e}")))?;
    if d.schema != 1 {
        return Err(bad(format!("unknown {STATE_FILE} schema {}", d.schema)));
    }
    if digest(&d.state) != d.sha256 {
        return Err(bad(format!(
            "{STATE_FILE} checksum mismatch (damaged or edited)"
        )));
    }
    d.state.validate()?;
    Ok(Some(d.state))
}

/// Atomic, private write of the state.
pub fn save(dir: &Path, s: &State) -> Result<(), MediaError> {
    s.validate()?;
    let d = OnDisk {
        schema: 1,
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
}
