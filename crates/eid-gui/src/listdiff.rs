//! v0.1 lightness: the window's `list` sends only what changed since the revision it already
//! has (docs/V0.1-CONTRACT.md). With hundreds of downloads and nothing moving, a poll then
//! carries no rows at all, and the window touches no DOM. Pure std (tested without Tauri).

use std::collections::HashMap;
use std::hash::Hash;

/// What one `list` answers.
#[derive(Debug, Clone, PartialEq)]
pub struct Delta<K, V> {
    pub rev: u64,
    /// `true`: `items` is the whole list (the window replaces everything it has).
    pub full: bool,
    /// Every row when `full`, else only rows that are new or changed since `since`.
    pub items: Vec<V>,
    /// Keys of rows gone since `since` (always empty when `full`).
    pub removed: Vec<K>,
}

/// The last list sent to the window (one window: one cache).
#[derive(Debug)]
pub struct ListCache<K, V> {
    rev: u64,
    last: HashMap<K, V>,
    order: Vec<K>,
}

impl<K: Eq + Hash + Clone, V: Clone + PartialEq> Default for ListCache<K, V> {
    fn default() -> Self {
        ListCache {
            rev: 0,
            last: HashMap::new(),
            order: vec![],
        }
    }
}

impl<K: Eq + Hash + Clone, V: Clone + PartialEq> ListCache<K, V> {
    /// Compare the current rows (`now`, in display order, keyed by `key`) with the last ones.
    /// `since` = the revision the window holds (`None`: it holds nothing, e.g. first load).
    /// A `since` other than the current revision (a lost answer, a reloaded window, a second
    /// window) gets the full list, so the window can never drift from the truth.
    pub fn update(
        &mut self,
        now: Vec<V>,
        key: impl Fn(&V) -> K,
        since: Option<u64>,
    ) -> Delta<K, V> {
        let mut changed = vec![];
        let mut next: HashMap<K, V> = HashMap::with_capacity(now.len());
        let mut order = Vec::with_capacity(now.len());
        for v in &now {
            let k = key(v);
            if self.last.get(&k) != Some(v) {
                changed.push(v.clone());
            }
            order.push(k.clone());
            next.insert(k, v.clone());
        }
        let removed: Vec<K> = self
            .order
            .iter()
            .filter(|k| !next.contains_key(*k))
            .cloned()
            .collect();
        let moved = order != self.order;
        if !changed.is_empty() || !removed.is_empty() || moved {
            self.rev += 1;
        }
        let full =
            since != Some(self.rev_before(changed.is_empty() && removed.is_empty() && !moved));
        self.last = next;
        self.order = order;
        if full {
            Delta {
                rev: self.rev,
                full: true,
                items: now,
                removed: vec![],
            }
        } else {
            Delta {
                rev: self.rev,
                full: false,
                items: changed,
                removed,
            }
        }
    }

    /// The revision the window must hold for a delta to apply: the one before this update.
    fn rev_before(&self, unchanged: bool) -> u64 {
        if unchanged {
            self.rev
        } else {
            self.rev - 1
        }
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Row {
        id: u64,
        pct: u32,
    }

    fn rows(v: &[(u64, u32)]) -> Vec<Row> {
        v.iter().map(|&(id, pct)| Row { id, pct }).collect()
    }

    /// The window side: apply a delta to what it holds.
    fn apply(have: &mut Vec<Row>, d: &Delta<u64, Row>, order: &[u64]) {
        if d.full {
            *have = d.items.clone();
            return;
        }
        let mut m: HashMap<u64, Row> = have.drain(..).map(|r| (r.id, r)).collect();
        for k in &d.removed {
            m.remove(k);
        }
        for r in &d.items {
            m.insert(r.id, r.clone());
        }
        *have = order.iter().filter_map(|k| m.remove(k)).collect();
    }

    #[test]
    fn first_call_is_full_then_only_changes_then_nothing() {
        let mut c = ListCache::default();
        let d = c.update(rows(&[(2, 0), (1, 50)]), |r| r.id, None);
        assert!(d.full);
        assert_eq!(d.items.len(), 2);
        let r1 = d.rev;
        let d = c.update(rows(&[(2, 10), (1, 50)]), |r| r.id, Some(r1));
        assert!(!d.full);
        assert_eq!(d.items, rows(&[(2, 10)]));
        assert!(d.removed.is_empty());
        let r2 = d.rev;
        assert!(r2 > r1);
        let d = c.update(rows(&[(2, 10), (1, 50)]), |r| r.id, Some(r2));
        assert!(!d.full && d.items.is_empty() && d.removed.is_empty());
        assert_eq!(d.rev, r2, "nothing changed: same revision");
    }

    #[test]
    fn removed_rows_are_named_and_a_stale_or_missing_revision_gets_everything() {
        let mut c = ListCache::default();
        let r = c
            .update(rows(&[(3, 0), (2, 0), (1, 0)]), |r| r.id, None)
            .rev;
        let d = c.update(rows(&[(3, 0), (1, 0)]), |r| r.id, Some(r));
        assert_eq!(d.removed, vec![2]);
        assert!(d.items.is_empty());
        // the window missed that answer and still holds `r`: it must get the full list
        let d = c.update(rows(&[(3, 0), (1, 5)]), |r| r.id, Some(r));
        assert!(d.full);
        assert_eq!(d.items, rows(&[(3, 0), (1, 5)]));
        let d = c.update(rows(&[(3, 0), (1, 5)]), |r| r.id, None);
        assert!(d.full);
        let d = c.update(rows(&[(3, 0), (1, 5)]), |r| r.id, Some(999));
        assert!(d.full);
    }

    #[test]
    fn a_window_applying_deltas_always_equals_the_truth() {
        // a deterministic pseudo-random walk over adds, removes, progress and reorders
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut c = ListCache::default();
        let mut truth: Vec<Row> = vec![];
        let mut have: Vec<Row> = vec![];
        let mut rev = None;
        let mut next_id = 1;
        for step in 0..5000 {
            match rnd(10) {
                0..=2 => {
                    truth.insert(
                        0,
                        Row {
                            id: next_id,
                            pct: 0,
                        },
                    );
                    next_id += 1;
                }
                3 if !truth.is_empty() => {
                    let i = rnd(truth.len() as u64) as usize;
                    truth.remove(i);
                }
                4 if truth.len() > 1 => {
                    let i = rnd(truth.len() as u64) as usize;
                    let r = truth.remove(i);
                    truth.push(r);
                }
                _ if !truth.is_empty() => {
                    let i = rnd(truth.len() as u64) as usize;
                    truth[i].pct = (truth[i].pct + 1) % 101;
                }
                _ => {}
            }
            // sometimes the window "loses" an answer
            let lost = rnd(50) == 0;
            let d = c.update(truth.clone(), |r| r.id, rev);
            if lost {
                continue;
            }
            let order: Vec<u64> = truth.iter().map(|r| r.id).collect();
            apply(&mut have, &d, &order);
            rev = Some(d.rev);
            assert_eq!(have, truth, "step {step}");
        }
    }
}
