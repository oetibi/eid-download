//! Queue data model and the PURE scheduling decision (no I/O, no clock, no tasks): what may start
//! now, what must stop, when to look again. The async driver is `scheduler.rs`; the file is
//! `queues.json` (via `store.rs`). Nothing here ever holds a credential: item URLs are
//! credential-free and validated on load.

use crate::sanitize::{is_safe_stored_name, sanitize_file_name};
use eid_platform::time::LocalNow;
use serde::{Deserialize, Serialize};

pub const QUEUES_VERSION: u32 = 1;
pub const MAX_QUEUES: usize = 64;
pub const MAX_ITEMS_TOTAL: usize = 50_000;
pub const MAX_PARALLEL_LIMIT: u32 = 16;
pub const MAX_RETRIES_LIMIT: u32 = 20;
pub const MAX_RETRY_DELAY_SECS: u64 = 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PowerAction {
    #[default]
    None,
    /// Quit the application.
    Exit,
    Sleep,
    Hibernate,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// Minutes since local midnight, 0..1440.
    pub start_min: u16,
    pub end_min: u16,
    /// Bit 0 = Monday ... bit 6 = Sunday; the weekday the window STARTS on.
    pub weekdays: u8,
}

impl Window {
    pub const ALL_DAYS: u8 = 0b0111_1111;

    pub fn validate(&self) -> Result<(), String> {
        if self.start_min >= 1440 || self.end_min >= 1440 {
            return Err("window time must be 0..1440 minutes".into());
        }
        if self.start_min == self.end_min {
            return Err("window start equals end (empty or ambiguous)".into());
        }
        if self.weekdays == 0 || self.weekdays & 0x80 != 0 {
            return Err("window weekdays must be a non-empty 7-bit mask".into());
        }
        Ok(())
    }

    fn day_on(&self, weekday: u8) -> bool {
        self.weekdays & (1 << (weekday % 7)) != 0
    }

    /// Is `now` inside the window? A window with `start > end` runs overnight and belongs to the
    /// weekday it started on.
    pub fn contains(&self, now: &LocalNow) -> bool {
        if self.start_min < self.end_min {
            self.day_on(now.weekday) && (self.start_min..self.end_min).contains(&now.minute)
        } else {
            (self.day_on(now.weekday) && now.minute >= self.start_min)
                || (self.day_on((now.weekday + 6) % 7) && now.minute < self.end_min)
        }
    }

    /// Unix time of the next moment `contains` may change (a boundary), searching up to 8 days.
    pub fn next_change(&self, now: &LocalNow) -> Option<i64> {
        let day_start = now.unix - i64::from(now.minute) * 60 - i64::from(now.second);
        let mut best: Option<i64> = None;
        for d in 0..=8i64 {
            for edge in [self.start_min, self.end_min] {
                let t = day_start.saturating_add(d * 86_400 + i64::from(edge) * 60);
                if t > now.unix {
                    let mut probe = *now;
                    // evaluate at the edge using weekday arithmetic (DST shifts are re-checked
                    // by the caller's next tick, a boundary is only a hint to wake up)
                    probe.unix = t;
                    probe.weekday = ((i64::from(now.weekday) + d).rem_euclid(7)) as u8;
                    probe.minute = edge;
                    probe.second = 0;
                    let before = {
                        let mut p = probe;
                        p.unix = t - 1;
                        p.minute = if edge == 0 { 1439 } else { edge - 1 };
                        if edge == 0 {
                            p.weekday = (probe.weekday + 6) % 7;
                        }
                        self.contains(&p)
                    };
                    if before != self.contains(&probe) {
                        best = Some(best.map_or(t, |b| b.min(t)));
                    }
                }
            }
        }
        best
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Re-queues after the first failure (0 = never retry).
    pub max: u32,
    /// Base delay; attempt n waits `delay_secs * n`.
    pub delay_secs: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max: 3,
            delay_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    /// Was running when the process last persisted; on load it becomes `Pending`.
    Running,
    /// Stopped by the user; never started automatically.
    Paused,
    Done,
    /// Gave up (non-retryable error or retries exhausted).
    Failed,
}

impl Status {
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Done | Status::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    /// Credential-free http(s) URL.
    pub url: String,
    /// Optional relative sub-folder below the queue directory (one plain component).
    #[serde(default)]
    pub subdir: Option<String>,
    /// Requested name, later the real name chosen by the engine (it names the sidecar files).
    #[serde(default)]
    pub file_name: Option<String>,
    pub status: Status,
    #[serde(default)]
    pub attempts: u32,
    /// Do not start before this Unix time (scheduling / retry delay).
    #[serde(default)]
    pub not_before: Option<i64>,
    /// Last error, already redacted.
    #[serde(default)]
    pub last_error: Option<String>,
    /// v0.2: this download's own speed limit in KiB/s, under the global one (`None` = only the
    /// global limit). Takes effect when the download (re)starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_kib: Option<u64>,
}

/// Largest per-download limit accepted (the same nonsense cap as the global one).
pub const MAX_ITEM_LIMIT_KIB: u64 = 12_500_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queue {
    pub name: String,
    /// Download directory (an absolute path chosen by the user/CLI; never read from an import).
    pub dir: String,
    pub max_parallel: u32,
    #[serde(default)]
    pub window: Option<Window>,
    #[serde(default)]
    pub retries: RetryPolicy,
    #[serde(default)]
    pub after_done: PowerAction,
    #[serde(default)]
    pub paused: bool,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QueueSet {
    pub queues: Vec<Queue>,
    pub next_id: u64,
}

impl Queue {
    pub fn new(name: &str, dir: &str) -> Queue {
        Queue {
            name: name.to_string(),
            dir: dir.to_string(),
            max_parallel: 2,
            window: None,
            retries: RetryPolicy::default(),
            after_done: PowerAction::None,
            paused: false,
            items: vec![],
        }
    }

    pub fn running(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.status == Status::Running)
            .count()
    }

    /// Every item is `Done` or `Failed` and there is at least one.
    pub fn is_finished(&self) -> bool {
        !self.items.is_empty() && self.items.iter().all(|i| i.status.is_terminal())
    }
}

impl QueueSet {
    /// Strict validation of an untrusted (loaded / imported) set.
    pub fn validate(&self) -> Result<(), String> {
        if self.queues.len() > MAX_QUEUES {
            return Err("too many queues".into());
        }
        let mut total = 0usize;
        let mut ids = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for q in &self.queues {
            if q.name.is_empty() || q.name.len() > 64 || q.name.chars().any(char::is_control) {
                return Err("bad queue name".into());
            }
            if !names.insert(q.name.clone()) {
                return Err(format!("duplicate queue name {:?}", q.name));
            }
            if q.dir.is_empty() || q.dir.len() > 1024 || q.dir.contains('\0') {
                return Err("bad queue directory".into());
            }
            if q.max_parallel == 0 || q.max_parallel > MAX_PARALLEL_LIMIT {
                return Err("max_parallel out of range".into());
            }
            if q.retries.max > MAX_RETRIES_LIMIT || q.retries.delay_secs > MAX_RETRY_DELAY_SECS {
                return Err("retry policy out of range".into());
            }
            if let Some(w) = &q.window {
                w.validate()?;
            }
            total = total.saturating_add(q.items.len());
            if total > MAX_ITEMS_TOTAL {
                return Err("too many items".into());
            }
            for it in &q.items {
                if !ids.insert(it.id) {
                    return Err(format!("duplicate item id {}", it.id));
                }
                if it.id >= self.next_id {
                    return Err("item id is not below next_id".into());
                }
                let (u, creds) = crate::urlsafe::parse_and_strip(&it.url)?;
                if creds.is_some() || u.to_string() != it.url {
                    return Err("item URL is not in canonical credential-free form".into());
                }
                for n in [&it.subdir, &it.file_name].into_iter().flatten() {
                    if !is_safe_stored_name(n) {
                        return Err(format!("item name {n:?} is not a safe plain name"));
                    }
                }
                if it.attempts > MAX_RETRIES_LIMIT + 1 {
                    return Err("attempt counter out of range".into());
                }
                if let Some(e) = &it.last_error {
                    if e.len() > 2048 {
                        return Err("error text too long".into());
                    }
                }
                if it.not_before.is_some_and(|t| t < 0) {
                    return Err("negative start time".into());
                }
                if it
                    .limit_kib
                    .is_some_and(|l| l == 0 || l > MAX_ITEM_LIMIT_KIB)
                {
                    return Err("item speed limit out of range".into());
                }
            }
        }
        Ok(())
    }

    pub fn queue_mut(&mut self, name: &str) -> Option<&mut Queue> {
        self.queues.iter_mut().find(|q| q.name == name)
    }

    /// Add an item (URL already validated by the caller through `urlsafe`). Returns its id.
    pub fn add_item(
        &mut self,
        queue: &str,
        url: &str,
        subdir: Option<&str>,
        file_name: Option<&str>,
        not_before: Option<i64>,
    ) -> Result<u64, String> {
        let total: usize = self.queues.iter().map(|q| q.items.len()).sum();
        if total >= MAX_ITEMS_TOTAL {
            return Err("too many items".into());
        }
        let (u, _creds) = crate::urlsafe::parse_and_strip(url)?;
        let id = self.next_id;
        let item = Item {
            id,
            url: u.to_string(),
            subdir: subdir.map(sanitize_file_name),
            file_name: file_name.map(sanitize_file_name),
            status: Status::Pending,
            attempts: 0,
            not_before,
            last_error: None,
            limit_kib: None,
        };
        let q = self.queue_mut(queue).ok_or("no such queue")?;
        q.items.push(item);
        self.next_id = id.checked_add(1).ok_or("id space exhausted")?;
        Ok(id)
    }

    /// After a process start nothing is running: `Running` items go back to `Pending` (their
    /// engine sidecars let them resume).
    pub fn demote_running(&mut self) -> usize {
        let mut n = 0;
        for q in &mut self.queues {
            for it in &mut q.items {
                if it.status == Status::Running {
                    it.status = Status::Pending;
                    n += 1;
                }
            }
        }
        n
    }

    /// The decision. `now` is the local time; `already_running` counts what the driver is running
    /// per queue is read from the data itself (`Status::Running`), so there is one source of truth.
    pub fn plan(&self, now: &LocalNow) -> Plan {
        let mut plan = Plan::default();
        let mut wake: Option<i64> = None;
        let mut note = |t: i64| {
            wake = Some(wake.map_or(t, |w| w.min(t)));
        };
        for (qi, q) in self.queues.iter().enumerate() {
            if q.paused {
                continue;
            }
            let open = q.window.as_ref().is_none_or(|w| w.contains(now));
            if let Some(w) = &q.window {
                if let Some(t) = w.next_change(now) {
                    note(t);
                }
            }
            if !open {
                for it in q.items.iter().filter(|i| i.status == Status::Running) {
                    plan.stop.push(StopJob {
                        queue: qi,
                        item: it.id,
                    });
                }
                continue;
            }
            let mut slots = (q.max_parallel as usize).saturating_sub(q.running());
            for it in q.items.iter().filter(|i| i.status == Status::Pending) {
                if slots == 0 {
                    break;
                }
                match it.not_before {
                    Some(t) if t > now.unix => note(t),
                    _ => {
                        plan.start.push(StartJob {
                            queue: qi,
                            item: it.id,
                        });
                        slots -= 1;
                    }
                }
            }
            // Items beyond the free slots are not "scheduled" yet, but their not_before still
            // matters once a slot frees up; the driver re-plans on every completion anyway.
        }
        plan.wake_at = wake;
        plan
    }

    /// Anything left to do anywhere: a running or startable/scheduled item in an unpaused queue.
    /// Used to abort a shutdown countdown ("re-check that nothing new is active").
    pub fn has_pending_work(&self) -> bool {
        self.queues.iter().any(|q| {
            q.items
                .iter()
                .any(|i| matches!(i.status, Status::Running | Status::Pending))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartJob {
    pub queue: usize,
    pub item: u64,
}
pub type StopJob = StartJob;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub start: Vec<StartJob>,
    /// Running jobs that must be cancelled because their queue's window closed.
    pub stop: Vec<StopJob>,
    /// Unix time at which the plan can change without any event (schedule / window edge).
    pub wake_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use eid_platform::time::from_unix;

    /// Monday 2026-11-09 00:00:00 UTC.
    const MON: i64 = 1_794_182_400;
    fn at(day: i64, h: i64, m: i64) -> LocalNow {
        from_unix(MON + day * 86_400 + h * 3600 + m * 60, 0)
    }

    fn set_with(n: usize, max_parallel: u32) -> QueueSet {
        let mut qs = QueueSet::default();
        let mut q = Queue::new("main", "/dl");
        q.max_parallel = max_parallel;
        qs.queues.push(q);
        for i in 0..n {
            qs.add_item("main", &format!("http://h/{i}.bin"), None, None, None)
                .unwrap();
        }
        qs
    }

    #[test]
    fn plan_never_starts_more_than_max_parallel_and_fills_freed_slots() {
        let mut qs = set_with(10, 2);
        let now = at(0, 12, 0);
        let p = qs.plan(&now);
        assert_eq!(p.start.len(), 2);
        assert_eq!(p.start[0].item, 0);
        assert_eq!(p.start[1].item, 1);
        for s in &p.start {
            qs.queues[s.queue]
                .items
                .iter_mut()
                .find(|i| i.id == s.item)
                .unwrap()
                .status = Status::Running;
        }
        assert!(qs.plan(&now).start.is_empty(), "queue is full");
        qs.queues[0].items[0].status = Status::Done;
        let p = qs.plan(&now);
        assert_eq!(p.start, vec![StartJob { queue: 0, item: 2 }]);
        // already running above the limit (e.g. limit lowered): nothing starts, nothing is killed
        qs.queues[0].max_parallel = 1;
        assert!(qs.plan(&now).start.is_empty());
        assert!(qs.plan(&now).stop.is_empty());
    }

    #[test]
    fn plan_respects_not_before_paused_items_and_paused_queues() {
        let mut qs = set_with(3, 5);
        let now = at(0, 12, 0);
        qs.queues[0].items[0].not_before = Some(now.unix + 5);
        qs.queues[0].items[1].status = Status::Paused;
        let p = qs.plan(&now);
        assert_eq!(p.start, vec![StartJob { queue: 0, item: 2 }]);
        assert_eq!(p.wake_at, Some(now.unix + 5));
        // exactly at the time it starts
        let p = qs.plan(&from_unix(now.unix + 5, 0));
        assert!(p.start.contains(&StartJob { queue: 0, item: 0 }));
        qs.queues[0].paused = true;
        assert_eq!(qs.plan(&now), Plan::default());
    }

    #[test]
    fn window_contains_overnight_and_weekday_masks() {
        // 22:00-06:00 on Mon-Fri nights
        let w = Window {
            start_min: 22 * 60,
            end_min: 6 * 60,
            weekdays: 0b0001_1111,
        };
        w.validate().unwrap();
        assert!(w.contains(&at(0, 23, 0)), "Mon 23:00");
        assert!(
            w.contains(&at(1, 5, 59)),
            "Tue 05:59 belongs to Monday's window"
        );
        assert!(!w.contains(&at(1, 6, 0)), "Tue 06:00 is closed");
        assert!(!w.contains(&at(1, 12, 0)));
        assert!(w.contains(&at(4, 23, 30)), "Fri night");
        assert!(
            w.contains(&at(5, 3, 0)),
            "Sat 03:00 belongs to Friday's window"
        );
        assert!(!w.contains(&at(5, 23, 0)), "Sat night is not allowed");
        assert!(
            !w.contains(&at(6, 3, 0)),
            "Sun 03:00 would be Saturday's window"
        );
        // daytime window
        let d = Window {
            start_min: 9 * 60,
            end_min: 17 * 60,
            weekdays: 0b0000_0001,
        };
        assert!(d.contains(&at(0, 9, 0)) && d.contains(&at(0, 16, 59)));
        assert!(!d.contains(&at(0, 17, 0)) && !d.contains(&at(1, 12, 0)));
        for bad in [
            Window {
                start_min: 5,
                end_min: 5,
                weekdays: 1,
            },
            Window {
                start_min: 1440,
                end_min: 5,
                weekdays: 1,
            },
            Window {
                start_min: 5,
                end_min: 6,
                weekdays: 0,
            },
            Window {
                start_min: 5,
                end_min: 6,
                weekdays: 0xFF,
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn window_next_change_finds_the_next_boundary() {
        let d = Window {
            start_min: 9 * 60,
            end_min: 17 * 60,
            weekdays: Window::ALL_DAYS,
        };
        let now = at(0, 8, 30);
        assert_eq!(d.next_change(&now), Some(at(0, 9, 0).unix));
        let now = at(0, 12, 0);
        assert_eq!(d.next_change(&now), Some(at(0, 17, 0).unix));
        let now = at(0, 20, 0);
        assert_eq!(d.next_change(&now), Some(at(1, 9, 0).unix));
        // weekdays only (Mon): from Monday 18:00 the next opening is NEXT Monday
        let m = Window {
            start_min: 9 * 60,
            end_min: 17 * 60,
            weekdays: 0b0000_0001,
        };
        assert_eq!(m.next_change(&at(0, 18, 0)), Some(at(7, 9, 0).unix));
        // overnight, from inside it: closes at 06:00 next day
        let o = Window {
            start_min: 22 * 60,
            end_min: 6 * 60,
            weekdays: Window::ALL_DAYS,
        };
        assert_eq!(o.next_change(&at(0, 23, 0)), Some(at(1, 6, 0).unix));
    }

    #[test]
    fn plan_closed_window_starts_nothing_and_stops_running_jobs() {
        let mut qs = set_with(3, 3);
        qs.queues[0].window = Some(Window {
            start_min: 9 * 60,
            end_min: 17 * 60,
            weekdays: Window::ALL_DAYS,
        });
        qs.queues[0].items[0].status = Status::Running;
        let closed = at(0, 20, 0);
        let p = qs.plan(&closed);
        assert!(p.start.is_empty());
        assert_eq!(p.stop, vec![StopJob { queue: 0, item: 0 }]);
        assert_eq!(p.wake_at, Some(at(1, 9, 0).unix));
        let open = at(1, 9, 0);
        let p = qs.plan(&open);
        assert!(p.stop.is_empty());
        assert_eq!(p.start.len(), 2); // 3 slots, one item already running
    }

    #[test]
    fn queue_set_validation_rejects_hostile_content() {
        let good = set_with(2, 2);
        good.validate().unwrap();
        let mut v: Vec<(&str, QueueSet)> = vec![];
        let mut x = good.clone();
        x.queues[0].items[0].url = "http://u:p@h/a".into();
        v.push(("credentials in URL", x));
        let mut x = good.clone();
        x.queues[0].items[0].url = "ftp://h/a".into();
        v.push(("scheme", x));
        let mut x = good.clone();
        x.queues[0].items[0].subdir = Some("../x".into());
        v.push(("subdir traversal", x));
        let mut x = good.clone();
        x.queues[0].items[0].file_name = Some("CON".into());
        v.push(("reserved name", x));
        let mut x = good.clone();
        x.queues[0].max_parallel = 0;
        v.push(("zero parallel", x));
        let mut x = good.clone();
        x.queues[0].max_parallel = 10_000;
        v.push(("huge parallel", x));
        let mut x = good.clone();
        x.queues[0].items[1].id = 0;
        v.push(("duplicate id", x));
        let mut x = good.clone();
        x.next_id = 1;
        v.push(("id >= next_id", x));
        let mut x = good.clone();
        x.queues.push(x.queues[0].clone());
        v.push(("duplicate queue name", x));
        let mut x = good.clone();
        x.queues[0].dir = String::new();
        v.push(("empty dir", x));
        let mut x = good.clone();
        x.queues[0].window = Some(Window {
            start_min: 1,
            end_min: 1,
            weekdays: 1,
        });
        v.push(("bad window", x));
        let mut x = good.clone();
        x.queues[0].retries.max = 1000;
        v.push(("retry range", x));
        for (why, q) in v {
            assert!(q.validate().is_err(), "{why} must be rejected");
        }
    }

    #[test]
    fn add_item_strips_credentials_and_sanitizes_names() {
        let mut qs = set_with(0, 2);
        let id = qs
            .add_item(
                "main",
                "http://bob:SECRET@h/p?q=1#frag",
                Some("../evil"),
                Some("a/b.txt"),
                None,
            )
            .unwrap();
        let it = &qs.queues[0].items[0];
        assert_eq!(it.id, id);
        assert_eq!(it.url, "http://h/p?q=1");
        assert!(!format!("{qs:?}").contains("SECRET"));
        assert_eq!(it.subdir.as_deref(), Some("evil"));
        assert_eq!(it.file_name.as_deref(), Some("b.txt"));
        qs.validate().unwrap();
        assert!(qs.add_item("nope", "http://h/", None, None, None).is_err());
        assert!(qs
            .add_item("main", "javascript:alert(1)", None, None, None)
            .is_err());
    }

    #[test]
    fn demote_running_after_a_restart() {
        let mut qs = set_with(3, 3);
        qs.queues[0].items[0].status = Status::Running;
        qs.queues[0].items[1].status = Status::Done;
        assert_eq!(qs.demote_running(), 1);
        assert_eq!(qs.queues[0].items[0].status, Status::Pending);
        assert_eq!(qs.queues[0].items[1].status, Status::Done);
    }
}
