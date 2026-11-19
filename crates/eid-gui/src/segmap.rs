//! The segment map of the download details window: which parts of a file are already saved.
//!
//! Read-only and Tauri-free. The map comes from the download's own state file (`.eid-state`,
//! the last checkpoint the engine made durable, at most a few seconds old), read with
//! `eid_engine::state::load`, which never renames, creates or deletes anything, follows no links
//! and refuses files over its size limit. A download that keeps no segments (one stream, the
//! server did not allow ranges) gets a single bar from its live byte count instead.

use eid_engine::download::STATE_SUFFIX;
use eid_engine::queue::{Item, QueueSet, Status};
use eid_engine::sanitize::is_safe_stored_name;
use eid_engine::state::{self, SegState};
use eid_engine::urlsafe;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Cells in the map bar (the window draws one block per cell).
pub const MAP_CELLS: usize = 120;
/// Segment rows listed under the bar (the count of all segments is always given).
pub const MAX_ROWS: usize = 32;

/// One segment as the window lists it (bytes, `end` exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SegRow {
    pub start: u64,
    pub end: u64,
    /// Bytes of this segment already saved.
    pub done: u64,
}

/// What the details window shows. `source` says where the numbers come from:
/// `checkpoint` (the state file), `stream` (one stream, live byte count), `finished`,
/// or `none` (nothing to show yet: not started, no size known, or the state file is unreadable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SegmentMap {
    pub source: &'static str,
    pub total: Option<u64>,
    pub done: u64,
    /// Number of segments in the state file (0 for one stream).
    pub segments: usize,
    /// Per cell: percent saved, 0..=100. Empty when `source == "none"`.
    pub cells: Vec<u8>,
    /// The first [`MAX_ROWS`] segments in file order.
    pub rows: Vec<SegRow>,
}

impl SegmentMap {
    pub fn none() -> Self {
        SegmentMap {
            source: "none",
            total: None,
            done: 0,
            segments: 0,
            cells: vec![],
            rows: vec![],
        }
    }
}

/// Percent saved per cell for a segment table that covers `[0, total)` in order (what
/// `State::validate` guarantees). Cell `k` covers `[k*total/n, (k+1)*total/n)`. A cell reads 100
/// only when every byte of it is saved. `n` is lowered to `total` for tiny files so no cell is
/// empty. O(n + segments); all arithmetic is overflow-free (`u128`).
pub fn cells_from_segments(segs: &[SegState], total: u64, n: usize) -> Vec<u8> {
    if total == 0 || n == 0 {
        return vec![];
    }
    let n = (n as u64).min(total) as usize;
    let bound = |k: usize| ((k as u128 * total as u128) / n as u128) as u64;
    let mut out = Vec::with_capacity(n);
    let mut i = 0usize; // first segment that can still overlap the current cell
    for k in 0..n {
        let (cs, ce) = (bound(k), bound(k + 1));
        let mut saved: u64 = 0;
        while i < segs.len() && segs[i].end <= cs {
            i += 1;
        }
        let mut j = i;
        while j < segs.len() && segs[j].start < ce {
            let s = &segs[j];
            // the saved part of segment j is [start, next)
            let lo = s.start.max(cs);
            let hi = s.next.min(ce);
            if hi > lo {
                saved = saved.saturating_add(hi - lo);
            }
            j += 1;
        }
        let len = ce - cs; // > 0 because n <= total
        out.push(((saved as u128 * 100) / len as u128) as u8);
    }
    out
}

/// The map of a ranged state file.
pub fn from_state(st: &state::State) -> SegmentMap {
    let total = match (st.ranged, st.total) {
        (true, Some(t)) if t > 0 => t,
        _ => return SegmentMap::none(),
    };
    let done = st.segments.iter().fold(0u64, |a, s| {
        a.saturating_add(s.next.saturating_sub(s.start))
    });
    SegmentMap {
        source: "checkpoint",
        total: Some(total),
        done,
        segments: st.segments.len(),
        cells: cells_from_segments(&st.segments, total, MAP_CELLS),
        rows: st
            .segments
            .iter()
            .take(MAX_ROWS)
            .map(|s| SegRow {
                start: s.start,
                end: s.end,
                done: s.next.saturating_sub(s.start),
            })
            .collect(),
    }
}

/// One stream: everything before `done` is saved.
pub fn from_stream(done: u64, total: Option<u64>) -> SegmentMap {
    let Some(t) = total.filter(|t| *t > 0) else {
        return SegmentMap::none();
    };
    let done = done.min(t);
    let seg = [SegState {
        start: 0,
        next: done,
        end: t,
    }];
    SegmentMap {
        source: "stream",
        total: Some(t),
        done,
        segments: 0,
        cells: cells_from_segments(&seg, t, MAP_CELLS),
        rows: vec![],
    }
}

/// The state file of `it` saved in `dir`, if its name is a plain safe name. An item whose name
/// was never recorded is looked up by URL (bounded directory scan, regular files only).
pub fn state_path(dir: &Path, it: &Item) -> Option<PathBuf> {
    match &it.file_name {
        Some(n) if is_safe_stored_name(n) => Some(dir.join(format!("{n}{STATE_SUFFIX}"))),
        Some(_) => None,
        None => {
            let (u, _) = urlsafe::parse_and_strip(&it.url).ok()?;
            state::find_for_url(dir, u.as_str())
        }
    }
}

/// The map for item `id`. `live` = the live byte progress `(done, total)` of the item, used for
/// a single-stream download. Blocking file IO: call it off the UI thread.
pub fn for_item(qs: &QueueSet, id: u64, live: Option<(u64, Option<u64>)>) -> Option<SegmentMap> {
    let (q, it) = qs
        .queues
        .iter()
        .find_map(|q| q.items.iter().find(|i| i.id == id).map(|i| (q, i)))?;
    if it.status == Status::Done {
        let total = live.and_then(|(d, t)| t.or(Some(d)));
        return Some(SegmentMap {
            source: "finished",
            total,
            done: total.unwrap_or(0),
            segments: 0,
            cells: vec![100; MAP_CELLS],
            rows: vec![],
        });
    }
    let dir = crate::core::item_dir(q, it);
    let from_file = state_path(&dir, it)
        .and_then(|p| state::load(&p).ok())
        // the file must belong to this item (same canonical URL), never to another download
        .filter(|st| {
            urlsafe::parse_and_strip(&it.url)
                .map(|(u, _)| u.as_str() == st.url)
                .unwrap_or(false)
        });
    Some(match from_file {
        Some(st) if st.ranged => from_state(&st),
        _ => match live {
            Some((d, t)) => from_stream(d, t),
            None => SegmentMap::none(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: u64, next: u64, end: u64) -> SegState {
        SegState { start, next, end }
    }

    #[test]
    fn cells_follow_the_saved_bytes_of_each_segment() {
        // 4 segments of 25 bytes: full, half, empty, full
        let s = [
            seg(0, 25, 25),
            seg(25, 37, 50),
            seg(50, 50, 75),
            seg(75, 100, 100),
        ];
        let c = cells_from_segments(&s, 100, 4);
        assert_eq!(c, vec![100, 48, 0, 100]);
        // finer cells: 10 bytes each
        let c = cells_from_segments(&s, 100, 10);
        assert_eq!(c, vec![100, 100, 100, 70, 0, 0, 0, 50, 100, 100]);
    }

    #[test]
    fn a_cell_is_full_only_when_every_byte_is_saved() {
        let s = [seg(0, 999, 1000)];
        let c = cells_from_segments(&s, 1000, 1);
        assert_eq!(c, vec![99]);
        let s = [seg(0, 1000, 1000)];
        assert_eq!(cells_from_segments(&s, 1000, 1), vec![100]);
    }

    #[test]
    fn tiny_huge_and_empty_files_never_panic_or_overflow() {
        // fewer bytes than cells: one cell per byte
        let s = [seg(0, 2, 3)];
        assert_eq!(cells_from_segments(&s, 3, MAP_CELLS), vec![100, 100, 0]);
        assert!(cells_from_segments(&[], 0, MAP_CELLS).is_empty());
        assert!(cells_from_segments(&s, 3, 0).is_empty());
        // near u64::MAX (u128 math): half saved
        let t = u64::MAX - 1;
        let h = t / 2;
        let s = [seg(0, h, h), seg(h, h, t)];
        let c = cells_from_segments(&s, t, MAP_CELLS);
        assert_eq!(c.len(), MAP_CELLS);
        assert!(c[..MAP_CELLS / 2 - 1].iter().all(|v| *v == 100), "{c:?}");
        assert!(c[MAP_CELLS / 2 + 1..].iter().all(|v| *v == 0), "{c:?}");
    }

    #[test]
    fn many_segments_stay_linear_and_exact() {
        // 100_000 one-byte-saved segments of 10 bytes (the state file's maximum count)
        let n = state::MAX_SEGMENTS as u64;
        let s: Vec<_> = (0..n)
            .map(|k| seg(k * 10, k * 10 + 1, k * 10 + 10))
            .collect();
        let t0 = std::time::Instant::now();
        let c = cells_from_segments(&s, n * 10, MAP_CELLS);
        assert!(t0.elapsed() < std::time::Duration::from_secs(1));
        // cells are ~8333 bytes, not a multiple of 10: 9 or 10 percent each (rounded down)
        assert!(c.iter().all(|v| (9..=10).contains(v)), "{c:?}");
    }

    #[test]
    fn one_stream_map_is_a_single_growing_bar() {
        let m = from_stream(600, Some(1200));
        assert_eq!(
            (m.source, m.done, m.total, m.segments),
            ("stream", 600, Some(1200), 0)
        );
        assert_eq!(m.cells.len(), MAP_CELLS);
        assert!(m.cells[..60].iter().all(|v| *v == 100));
        assert!(m.cells[60..].iter().all(|v| *v == 0));
        // a byte count above the size (lying server) is clamped
        assert_eq!(from_stream(500, Some(100)).done, 100);
        // unknown size: nothing to draw
        assert_eq!(from_stream(5, None).source, "none");
    }

    // ---- the state file of a real item ----

    use eid_engine::queue::Queue;

    const URL: &str = "http://127.0.0.1:9/f.bin";

    fn item(id: u64, url: &str, name: Option<&str>, status: Status) -> Item {
        Item {
            id,
            url: url.into(),
            subdir: None,
            file_name: name.map(str::to_owned),
            status,
            attempts: 0,
            not_before: None,
            last_error: None,
            limit_kib: None,
        }
    }

    fn set(dir: &Path, items: Vec<Item>) -> QueueSet {
        let mut q = Queue::new("main", &dir.to_string_lossy());
        q.items = items;
        QueueSet {
            queues: vec![q],
            next_id: 100,
        }
    }

    fn write_state(dir: &Path, name: &str, url: &str, segs: Vec<SegState>, total: u64) {
        let mut st = state::State {
            version: state::STATE_VERSION,
            url: url.into(),
            file_name: name.into(),
            total: Some(total),
            etag: None,
            last_modified: None,
            ranged: true,
            segments: segs,
            restarts: 0,
            finalize: None,
            checksum: String::new(),
        };
        st.seal();
        state::save(&dir.join(format!("{name}{STATE_SUFFIX}")), &st).unwrap();
    }

    #[test]
    fn map_of_an_item_comes_from_its_own_state_file_read_only() {
        let d = tempfile::tempdir().unwrap();
        write_state(
            d.path(),
            "f.bin",
            URL,
            vec![seg(0, 600, 600), seg(600, 700, 1000)],
            1000,
        );
        let before: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let qs = set(d.path(), vec![item(1, URL, Some("f.bin"), Status::Running)]);
        let m = for_item(&qs, 1, Some((650, Some(1000)))).unwrap();
        assert_eq!(
            (m.source, m.done, m.total, m.segments),
            ("checkpoint", 700, Some(1000), 2)
        );
        assert_eq!(
            m.rows,
            vec![
                SegRow {
                    start: 0,
                    end: 600,
                    done: 600
                },
                SegRow {
                    start: 600,
                    end: 1000,
                    done: 100
                }
            ]
        );
        assert_eq!(m.cells.len(), MAP_CELLS);
        // nothing in the folder was created, renamed or deleted
        let after: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(before, after);
        // unknown item
        assert!(for_item(&qs, 99, None).is_none());
        // an item whose name was never recorded is found by its URL
        let qs = set(d.path(), vec![item(2, URL, None, Status::Paused)]);
        assert_eq!(for_item(&qs, 2, None).unwrap().source, "checkpoint");
    }

    #[test]
    fn a_state_file_of_another_download_or_an_unsafe_name_is_never_shown() {
        let d = tempfile::tempdir().unwrap();
        write_state(
            d.path(),
            "f.bin",
            "http://127.0.0.1:9/other.bin",
            vec![seg(0, 10, 100)],
            100,
        );
        let qs = set(d.path(), vec![item(1, URL, Some("f.bin"), Status::Paused)]);
        assert_eq!(for_item(&qs, 1, None).unwrap().source, "none");
        // with live progress it falls back to the single bar, still not the other file's table
        let m = for_item(&qs, 1, Some((30, Some(100)))).unwrap();
        assert_eq!((m.source, m.done), ("stream", 30));
        // names that are not plain safe names are never joined to the folder
        for bad in ["../f.bin", "a/b", "CON", "x.bin "] {
            let qs = set(d.path(), vec![item(1, URL, Some(bad), Status::Paused)]);
            assert_eq!(for_item(&qs, 1, None).unwrap().source, "none", "{bad}");
        }
        // a corrupted state file shows nothing (and is left where it is)
        let p = d.path().join(format!("f.bin{STATE_SUFFIX}"));
        std::fs::write(&p, b"{not json").unwrap();
        let qs = set(d.path(), vec![item(1, URL, Some("f.bin"), Status::Paused)]);
        // the .bak (the other URL) still does not match this item
        assert_eq!(for_item(&qs, 1, None).unwrap().source, "none");
        assert!(p.exists());
    }

    #[test]
    fn a_state_file_behind_a_symlink_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        write_state(outside.path(), "f.bin", URL, vec![seg(0, 50, 100)], 100);
        let d = tempfile::tempdir().unwrap();
        eid_platform::fs::symlink_file(
            &outside.path().join(format!("f.bin{STATE_SUFFIX}")),
            &d.path().join(format!("f.bin{STATE_SUFFIX}")),
        )
        .expect("symlink must be creatable for this test (fails loudly, AGENTS §6.1)");
        let qs = set(d.path(), vec![item(1, URL, Some("f.bin"), Status::Paused)]);
        assert_eq!(for_item(&qs, 1, None).unwrap().source, "none");
    }

    #[test]
    fn finished_items_show_a_full_map() {
        let d = tempfile::tempdir().unwrap();
        let qs = set(d.path(), vec![item(1, URL, Some("f.bin"), Status::Done)]);
        let m = for_item(&qs, 1, Some((1000, Some(1000)))).unwrap();
        assert_eq!((m.source, m.done), ("finished", 1000));
        assert!(m.cells.iter().all(|v| *v == 100));
    }
}
