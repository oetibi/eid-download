//! Segment table and the byte-reservation handoff protocol (ADR-0009).
//!
//! A segment is `[start, end)`. `next` is the first byte *not yet reserved*; `done` is the
//! first byte not yet *durably completed by the owner* (`start <= done <= next <= end`).
//! A worker reserves bytes it already holds in memory, writes them, then calls `complete`.
//! The splitter may only shrink `end` down to `max(next, midpoint)`: bytes below `next` are
//! reserved by their owner and can never be cut, so no byte is lost or written twice.

use crate::state::SegState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seg {
    pub start: u64,
    pub done: u64,
    pub next: u64,
    pub end: u64,
    pub owned: bool,
}

#[derive(Debug, Default)]
pub struct Table {
    pub segs: Vec<Seg>,
    pub total: u64,
}

impl Table {
    /// Single segment covering everything.
    pub fn new(total: u64) -> Table {
        let mut t = Table {
            segs: Vec::new(),
            total,
        };
        if total > 0 {
            t.segs.push(Seg {
                start: 0,
                done: 0,
                next: 0,
                end: total,
                owned: false,
            });
        }
        t
    }

    /// Rebuild from validated persisted segments (reservations are discarded: `next = done`).
    pub fn from_state(total: u64, segs: &[SegState]) -> Table {
        Table {
            total,
            segs: segs
                .iter()
                .map(|s| Seg {
                    start: s.start,
                    done: s.next,
                    next: s.next,
                    end: s.end,
                    owned: false,
                })
                .collect(),
        }
    }

    pub fn snapshot(&self) -> Vec<SegState> {
        // Segment indices are stable for workers, so new (split) segments are appended; the
        // persisted form must be sorted by offset to describe exact coverage.
        let mut v: Vec<SegState> = self
            .segs
            .iter()
            .map(|s| SegState {
                start: s.start,
                next: s.done,
                end: s.end,
            })
            .collect();
        v.sort_by_key(|s| s.start);
        v
    }

    pub fn done_bytes(&self) -> u64 {
        self.segs.iter().map(|s| s.done - s.start).sum()
    }

    pub fn is_complete(&self) -> bool {
        self.segs.iter().all(|s| s.done >= s.end)
    }

    /// Claim the first unowned segment that still has bytes to fetch.
    pub fn take_pending(&mut self) -> Option<usize> {
        let i = self.segs.iter().position(|s| !s.owned && s.done < s.end)?;
        self.segs[i].owned = true;
        Some(i)
    }

    pub fn release(&mut self, idx: usize) {
        self.segs[idx].owned = false;
        // anything reserved but never completed is simply refetched
        let s = &mut self.segs[idx];
        s.next = s.done;
    }

    /// Current (done-frontier, end) of a segment: where a (re)connect must start.
    pub fn cursor(&self, idx: usize) -> (u64, u64) {
        let s = &self.segs[idx];
        (s.done, s.end)
    }

    /// Reserve up to `want` bytes at the segment frontier. Returns `(offset, granted)`.
    /// `granted` may be < `want` (or 0) if the segment was shortened by a split.
    pub fn reserve(&mut self, idx: usize, want: u64) -> (u64, u64) {
        let s = &mut self.segs[idx];
        let room = s.end.saturating_sub(s.next);
        let n = want.min(room);
        let off = s.next;
        s.next = s.next.saturating_add(n);
        (off, n)
    }

    /// Mark `[off, off+n)` as written. Must be the continuation of `done`.
    pub fn complete(&mut self, idx: usize, off: u64, n: u64) {
        let s = &mut self.segs[idx];
        debug_assert_eq!(off, s.done, "completion must be contiguous");
        debug_assert!(off + n <= s.next);
        s.done = off + n;
    }

    /// IDM-style steal: halve the segment with the most bytes still unreserved. The new
    /// (unowned) tail segment is returned already claimed (`owned = true`).
    /// Only splits when both halves are at least `min_split` bytes.
    pub fn split_largest(&mut self, min_split: u64) -> Option<usize> {
        let min_split = min_split.max(1);
        let (i, rem) = self
            .segs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.owned)
            .map(|(i, s)| (i, s.end.saturating_sub(s.next)))
            .max_by_key(|&(_, r)| r)?;
        if rem < min_split.saturating_mul(2) {
            return None;
        }
        let s = &mut self.segs[i];
        let mid = s.next + rem / 2;
        debug_assert!(mid >= s.next && mid < s.end);
        let tail = Seg {
            start: mid,
            done: mid,
            next: mid,
            end: s.end,
            owned: true,
        };
        s.end = mid;
        self.segs.push(tail);
        Some(self.segs.len() - 1)
    }

    /// Structural invariants; used by tests and debug assertions.
    pub fn check_invariants(&self) -> Result<(), String> {
        let mut v: Vec<&Seg> = self.segs.iter().collect();
        v.sort_by_key(|s| s.start);
        let mut cursor = 0u64;
        for s in v {
            if !(s.start <= s.done && s.done <= s.next && s.next <= s.end) {
                return Err(format!("bad order {s:?}"));
            }
            if s.start != cursor {
                return Err(format!("gap/overlap at {cursor}: {s:?}"));
            }
            cursor = s.end;
        }
        if cursor != self.total {
            return Err(format!("coverage ends at {cursor}, total {}", self.total));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::{Arc, Mutex};

    #[test]
    fn split_never_cuts_below_reserved() {
        let mut t = Table::new(1000);
        let a = t.take_pending().unwrap();
        let (off, n) = t.reserve(a, 400);
        assert_eq!((off, n), (0, 400));
        let b = t.split_largest(10).unwrap();
        assert!(
            t.segs[a].end >= 400,
            "reserved bytes must stay with the owner"
        );
        assert_eq!(t.segs[a].end, t.segs[b].start);
        // worker A tries to reserve a lot: capped at its new end
        let (_, n2) = t.reserve(a, 10_000);
        assert_eq!(t.segs[a].next, t.segs[a].end);
        assert_eq!(n2, t.segs[a].end - 400);
        t.check_invariants().unwrap();
    }

    #[test]
    fn snapshot_is_sorted_and_covers_exactly_after_splits() {
        let mut t = Table::new(10_000);
        let a = t.take_pending().unwrap();
        for _ in 0..6 {
            let _ = t.split_largest(100);
        }
        let _ = a;
        let snap = t.snapshot();
        let mut cursor = 0;
        for s in &snap {
            assert_eq!(s.start, cursor);
            cursor = s.end;
        }
        assert_eq!(cursor, 10_000);
        assert!(snap.len() > 2);
    }

    #[test]
    fn min_split_respected() {
        let mut t = Table::new(100);
        let _ = t.take_pending().unwrap();
        assert!(t.split_largest(60).is_none());
        assert!(t.split_largest(50).is_some());
    }

    #[test]
    fn release_discards_uncompleted_reservation() {
        let mut t = Table::new(100);
        let a = t.take_pending().unwrap();
        let (o, _n) = t.reserve(a, 30);
        t.complete(a, o, 10); // only 10 of the 30 actually written
        t.release(a);
        assert_eq!(t.segs[a].next, 10);
        assert_eq!(t.cursor(a), (10, 100));
    }

    fn content_byte(seed: u64, off: u64) -> u8 {
        let x = off
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(seed.wrapping_mul(0xD6E8_FEB8_6659_FD93));
        (x >> 24) as u8
    }

    /// ADR-0009 arbiter with REAL content. Many threads reserve/write/complete while the pool
    /// keeps stealing (splitting). Each worker writes the true content bytes of its reservation
    /// into a shared buffer; with some probability the "connection drops" MID-RESERVATION: only a
    /// prefix of the reserved bytes is written and completed, then the segment is released and
    /// the rest must be re-reserved by someone else. Checked per seed: every reservation lies
    /// inside the segment's range at that moment (no write outside coverage), no byte is written
    /// twice, the table invariants hold after every step, and the SHA-256 of the assembled
    /// buffer equals the SHA-256 of the expected content.
    #[test]
    fn handoff_randomized_200_seeds() {
        use sha2::{Digest, Sha256};
        for seed in 1..=200u64 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let total = 20_000 + rng.u64(0..200_000);
            let min_split = 1 + rng.u64(0..2000);
            let workers = 2 + rng.usize(0..8);
            let table = Arc::new(Mutex::new(Table::new(total)));
            let writes: Arc<Vec<AtomicU8>> =
                Arc::new((0..total).map(|_| AtomicU8::new(0)).collect());
            let buf: Arc<Vec<AtomicU8>> = Arc::new((0..total).map(|_| AtomicU8::new(0)).collect());
            let mut handles = Vec::new();
            for w in 0..workers {
                let table = table.clone();
                let writes = writes.clone();
                let buf = buf.clone();
                let wseed = seed * 1000 + w as u64;
                handles.push(std::thread::spawn(move || {
                    let mut rng = fastrand::Rng::with_seed(wseed);
                    let mut cur: Option<usize> = None;
                    let mut idle = 0;
                    loop {
                        if cur.is_none() {
                            let mut t = table.lock().unwrap();
                            cur = t.take_pending().or_else(|| t.split_largest(min_split));
                            if cur.is_none() {
                                if t.is_complete() {
                                    return;
                                }
                                drop(t);
                                idle += 1;
                                if idle > 100_000 {
                                    panic!("seed {seed}: stuck");
                                }
                                std::thread::yield_now();
                                continue;
                            }
                        }
                        let idx = cur.unwrap();
                        // "network chunk" of random size
                        let want = 1 + rng.u64(0..4096);
                        let (off, n, seg_end) = {
                            let mut t = table.lock().unwrap();
                            let (next, end) = t.cursor(idx);
                            let (off, n) = t.reserve(idx, want);
                            (off, n, (next, end))
                        };
                        if n == 0 {
                            cur = None; // segment ended (maybe cut by a split)
                            continue;
                        }
                        assert!(
                            off >= seg_end.0 && off + n <= seg_end.1 && off + n <= total,
                            "seed {seed}: reservation {off}+{n} outside segment {seg_end:?}"
                        );
                        if rng.u8(..).is_multiple_of(4) {
                            std::thread::yield_now(); // widen the reserve->write window
                        }
                        // a dropped connection mid-reservation writes only a prefix
                        let drop_conn = rng.u8(..).is_multiple_of(25);
                        let k = if drop_conn && n > 1 { rng.u64(0..n) } else { n };
                        for b in off..off + k {
                            let prev = writes[b as usize].fetch_add(1, Ordering::SeqCst);
                            assert_eq!(prev, 0, "seed {seed}: byte {b} written twice");
                            buf[b as usize].store(content_byte(seed, b), Ordering::SeqCst);
                        }
                        let mut t = table.lock().unwrap();
                        t.complete(idx, off, k);
                        if drop_conn || rng.u8(..).is_multiple_of(50) {
                            // dropped connection: give the segment back
                            t.release(idx);
                            cur = None;
                        }
                        t.check_invariants()
                            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
                    }
                }));
            }
            for h in handles {
                h.join().unwrap_or_else(|_| panic!("seed {seed} failed"));
            }
            let t = table.lock().unwrap();
            assert!(t.is_complete(), "seed {seed}");
            t.check_invariants().unwrap();
            assert_eq!(t.done_bytes(), total, "seed {seed}");
            assert!(
                writes.iter().all(|b| b.load(Ordering::SeqCst) == 1),
                "seed {seed}: some byte not written exactly once"
            );
            let got: Vec<u8> = buf.iter().map(|b| b.load(Ordering::SeqCst)).collect();
            let want: Vec<u8> = (0..total).map(|o| content_byte(seed, o)).collect();
            assert_eq!(
                Sha256::digest(&got),
                Sha256::digest(&want),
                "seed {seed}: assembled content differs"
            );
        }
    }
}
