//! The desktop app's logic, kept free of Tauri so it is unit tested on every platform: the view
//! of the queue the window shows, byte progress with speed and time left, adding links into the
//! folder the user picked, removing items, and plain-language error categories.

use eid_engine::queue::{Item, Queue, QueueSet, Status};
use eid_engine::scheduler::Scheduler;
use eid_engine::urlsafe;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The queue every download goes to unless the user picks another folder.
pub const MAIN_QUEUE: &str = "main";
/// Links accepted in one "add" (one per line).
pub const MAX_LINKS_PER_ADD: usize = 200;
/// Speed is averaged over this window.
const SPEED_WINDOW: Duration = Duration::from_secs(3);
/// One sample at most this often (progress events arrive per received chunk).
const SAMPLE_EVERY: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy)]
struct Sample {
    at: Instant,
    done: u64,
}

#[derive(Debug, Default)]
struct Track {
    done: u64,
    total: Option<u64>,
    samples: VecDeque<Sample>,
}

/// Byte progress of the items, fed by the engine (`EngineRunner::progress`), read by the window.
#[derive(Debug, Default)]
pub struct Progress {
    map: Mutex<HashMap<u64, Track>>,
}

/// What the window shows for the transfer of one item.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rate {
    pub done: u64,
    pub total: Option<u64>,
    /// Bytes per second over the last few seconds (0 when stalled).
    pub speed: u64,
    /// Seconds left at that speed (`None` if the size or the speed is unknown).
    pub eta: Option<u64>,
}

impl Progress {
    pub fn record(&self, item: u64, done: u64, total: Option<u64>, now: Instant) {
        let mut m = self.map.lock().unwrap_or_else(|p| p.into_inner());
        let t = m.entry(item).or_default();
        // a restart from zero (server changed, no ranges) starts a new speed window
        if done < t.done {
            t.samples.clear();
        }
        t.done = done;
        if total.is_some() {
            t.total = total;
        }
        let push = t
            .samples
            .back()
            .is_none_or(|s| now.saturating_duration_since(s.at) >= SAMPLE_EVERY);
        if push {
            t.samples.push_back(Sample { at: now, done });
        } else if let Some(last) = t.samples.back_mut() {
            last.done = done;
        }
        while t.samples.len() > 2
            && t.samples
                .get(1)
                .is_some_and(|s| now.saturating_duration_since(s.at) > SPEED_WINDOW)
        {
            t.samples.pop_front();
        }
    }

    /// Progress of `item` at `now`. `running` = the item is transferring right now (otherwise
    /// the speed and time left are not shown).
    pub fn rate(&self, item: u64, running: bool, now: Instant) -> Option<Rate> {
        let m = self.map.lock().unwrap_or_else(|p| p.into_inner());
        let t = m.get(&item)?;
        let speed = if running {
            speed_of(&t.samples, now)
        } else {
            0
        };
        let eta = match t.total {
            Some(total) if speed > 0 => Some(total.saturating_sub(t.done).div_ceil(speed)),
            _ => None,
        };
        Some(Rate {
            done: t.done,
            total: t.total,
            speed,
            eta,
        })
    }

    pub fn forget(&self, item: u64) {
        self.map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&item);
    }
}

/// Bytes per second from the oldest sample still inside the window up to `now`: a transfer that
/// stops sending decays to 0 instead of showing its last speed for ever.
fn speed_of(samples: &VecDeque<Sample>, now: Instant) -> u64 {
    let (Some(first), Some(last)) = (
        samples
            .iter()
            .find(|s| now.saturating_duration_since(s.at) <= SPEED_WINDOW),
        samples.back(),
    ) else {
        return 0;
    };
    let span = now.saturating_duration_since(first.at);
    if span < Duration::from_millis(300) {
        return 0;
    }
    let bytes = last.done.saturating_sub(first.done) as u128;
    u64::try_from(bytes * 1000 / span.as_millis().max(1)).unwrap_or(u64::MAX)
}

/// One row of the download list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemView {
    pub id: u64,
    /// pending | waiting | running | paused | done | failed
    pub status: &'static str,
    pub name: String,
    /// Without credentials or query string (it is shown on screen).
    pub url: String,
    pub folder: String,
    pub attempts: u32,
    /// Plain-language category of the last error (translated by the window).
    pub error_kind: Option<&'static str>,
    /// The engine's own text, for the details line.
    pub error: Option<String>,
    pub rate: Option<Rate>,
}

/// Name shown for an item whose file name is not known yet: the last path segment of the URL.
fn display_name(it: &Item) -> String {
    if let Some(n) = &it.file_name {
        return n.clone();
    }
    url::Url::parse(&it.url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut s| s.next_back().map(str::to_owned))
        })
        .map(|s| {
            percent_encoding::percent_decode_str(&s)
                .decode_utf8_lossy()
                .into_owned()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            url::Url::parse(&it.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default()
        })
}

/// The list, newest first. `unix_now` decides whether a pending item is waiting for a retry.
pub fn view(qs: &QueueSet, progress: &Progress, now: Instant, unix_now: i64) -> Vec<ItemView> {
    let mut out: Vec<ItemView> = qs
        .queues
        .iter()
        .flat_map(|q| q.items.iter().map(move |i| (q, i)))
        .map(|(q, it)| {
            let status = match it.status {
                Status::Pending if it.not_before.is_some_and(|t| t > unix_now) => "waiting",
                Status::Pending => "pending",
                Status::Running => "running",
                Status::Paused => "paused",
                Status::Done => "done",
                Status::Failed => "failed",
            };
            let mut rate = progress.rate(it.id, it.status == Status::Running, now);
            if it.status == Status::Done {
                rate = rate.map(|r| Rate {
                    done: r.total.unwrap_or(r.done),
                    speed: 0,
                    eta: None,
                    ..r
                });
            }
            let error = match it.status {
                Status::Done => None,
                _ => it.last_error.as_deref().map(urlsafe::redact_text),
            };
            ItemView {
                id: it.id,
                status,
                name: display_name(it),
                url: urlsafe::redact_text(&it.url),
                folder: item_dir(q, it).to_string_lossy().into_owned(),
                attempts: it.attempts,
                error_kind: error.as_deref().map(error_kind),
                error,
                rate,
            }
        })
        .collect();
    out.sort_by_key(|v| std::cmp::Reverse(v.id));
    out
}

/// The folder an item is saved in.
pub fn item_dir(q: &Queue, it: &Item) -> PathBuf {
    let base = PathBuf::from(&q.dir);
    match &it.subdir {
        Some(s) => base.join(s),
        None => base,
    }
}

/// The saved file of a finished item (`None` if it is not finished or its name is unknown).
pub fn finished_file(qs: &QueueSet, id: u64) -> Option<PathBuf> {
    qs.queues.iter().find_map(|q| {
        q.items
            .iter()
            .find(|i| i.id == id && i.status == Status::Done)
            .and_then(|i| i.file_name.as_ref().map(|n| item_dir(q, i).join(n)))
    })
}

/// Plain-language category of an engine error text (the window translates it). Matches the
/// `Display` texts of `eid_engine::Error` and of the URL checks.
pub fn error_kind(msg: &str) -> &'static str {
    let m = msg.to_ascii_lowercase();
    let has = |s: &str| m.contains(s);
    if has("os error 112") || has("not enough space") || has("disk full") || has("no space") {
        "disk_full"
    } else if has("i/o error")
        && (has("os error 5)") || has("access is denied") || has("permission denied"))
    {
        "disk_denied"
    } else if has("i/o error") {
        "disk"
    } else if has("http 404") || has("http 410") {
        "not_found"
    } else if has("access denied by server") || has("http 401") || has("http 403") {
        "forbidden"
    } else if has("server error: http") {
        "server"
    } else if has("network error") {
        "network"
    } else if has("made no progress") || has("short body") {
        "stalled"
    } else if has("changed on the server") {
        "changed"
    } else if has("byte ranges") {
        "no_resume"
    } else if has("checksum") {
        "checksum"
    } else if has("already running") {
        "busy"
    } else if has("state file") {
        "state"
    } else if has("invalid response") {
        "bad_response"
    } else if has("not a valid url")
        || has("unsupported scheme")
        || has("has no host")
        || has("url too long")
        || has("invalid url")
    {
        "bad_url"
    } else if has("too many") {
        "too_many"
    } else {
        "unknown"
    }
}

/// The links of an "add" text: one per line (or separated by spaces), trimmed, without
/// duplicates, in order. More than [`MAX_LINKS_PER_ADD`] is refused.
pub fn parse_links(text: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = vec![];
    for l in text.split_whitespace() {
        if !out.iter().any(|x| x == l) {
            out.push(l.to_owned());
        }
    }
    if out.is_empty() {
        return Err("no_links".into());
    }
    if out.len() > MAX_LINKS_PER_ADD {
        return Err("too_many_links".into());
    }
    Ok(out)
}

/// A folder the user picked: absolute, an existing real directory.
pub fn check_folder(dir: &str) -> Result<PathBuf, String> {
    let p = Path::new(dir);
    if dir.is_empty() || dir.len() > 1024 || dir.contains('\0') || !p.is_absolute() {
        return Err("bad_folder".into());
    }
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => Ok(p.to_path_buf()),
        _ => Err("bad_folder".into()),
    }
}

/// The queue that saves into `dir`, created (with the main queue's settings) when none does.
/// Each folder is its own queue, so an item never changes folder after it started.
pub fn queue_for_dir(qs: &mut QueueSet, dir: &str) -> Result<String, String> {
    if let Some(q) = qs.queues.iter().find(|q| q.dir == dir) {
        return Ok(q.name.clone());
    }
    let parallel = qs
        .queues
        .iter()
        .find(|q| q.name == MAIN_QUEUE)
        .map_or(2, |q| q.max_parallel);
    let name = (1..)
        .map(|n| format!("folder-{n}"))
        .find(|n| qs.queues.iter().all(|q| &q.name != n))
        .expect("unbounded");
    let mut q = Queue::new(&name, dir);
    q.max_parallel = parallel;
    qs.queues.push(q);
    Ok(name)
}

/// Result of one "add".
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct AddReport {
    pub added: Vec<u64>,
    /// `(link as shown, error category)`; the link is redacted.
    pub refused: Vec<(String, &'static str)>,
}

/// Add links. `dir` = a folder the user picked (`None` = the main queue's folder); `name` = a
/// file name for a single link.
pub fn add_links(
    sched: &Scheduler,
    text: &str,
    dir: Option<&str>,
    name: Option<&str>,
) -> Result<AddReport, String> {
    let links = parse_links(text)?;
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    if name.is_some() && links.len() != 1 {
        return Err("name_needs_one_link".into());
    }
    let queue = match dir {
        None => MAIN_QUEUE.to_owned(),
        Some(d) => {
            let d = check_folder(d)?;
            let d = d.to_string_lossy().into_owned();
            sched
                .edit(|qs| queue_for_dir(qs, &d))
                .map_err(|e| e.to_string())?
        }
    };
    let mut r = AddReport::default();
    for l in links {
        match sched.add_item(&queue, &l, None, name, None) {
            Ok(id) => r.added.push(id),
            Err(e) => r
                .refused
                .push((urlsafe::redact_text(&l), error_kind(&e.to_string()))),
        }
    }
    Ok(r)
}

/// Take an item off the list (its downloaded file, if any, stays on disk). A running item must
/// be paused first.
pub fn remove_item(sched: &Scheduler, id: u64) -> Result<(), String> {
    sched
        .edit(|qs| {
            for q in &mut qs.queues {
                if let Some(pos) = q.items.iter().position(|i| i.id == id) {
                    if q.items[pos].status == Status::Running {
                        return Err("pause_first".into());
                    }
                    q.items.remove(pos);
                    return Ok(());
                }
            }
            Err("no_such_item".into())
        })
        .map_err(|e| invalid_input_text(&e.to_string()))
}

/// Take every finished item off the list. Returns how many.
pub fn clear_finished(sched: &Scheduler) -> Result<usize, String> {
    sched
        .edit(|qs| {
            let mut n = 0;
            for q in &mut qs.queues {
                let before = q.items.len();
                q.items.retain(|i| i.status != Status::Done);
                n += before - q.items.len();
            }
            Ok(n)
        })
        .map_err(|e| e.to_string())
}

/// `edit` wraps the closure's error as "invalid input: X"; give the window just X.
fn invalid_input_text(s: &str) -> String {
    s.strip_prefix("invalid input: ").unwrap_or(s).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use eid_engine::config::Config;
    use eid_engine::queue_runner::EngineRunner;
    use eid_engine::scheduler::{NoPower, SchedulerConfig, SystemClock};
    use std::sync::Arc;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn speed_and_eta_from_samples() {
        let p = Progress::default();
        let t0 = Instant::now();
        p.record(1, 0, Some(10_000), t0);
        for k in 1..=10u64 {
            p.record(1, k * 100, None, t0 + ms(k * 250)); // 400 B/s
        }
        let r = p.rate(1, true, t0 + ms(2500)).unwrap();
        assert_eq!((r.done, r.total), (1000, Some(10_000)));
        assert!((380..=420).contains(&r.speed), "speed {}", r.speed);
        let eta = r.eta.unwrap();
        assert!((21..=24).contains(&eta), "eta {eta}");
        // not running: no speed, no time left
        let r = p.rate(1, false, t0 + ms(2500)).unwrap();
        assert_eq!((r.speed, r.eta), (0, None));
        assert_eq!(p.rate(2, true, t0), None);
    }

    #[test]
    fn a_stalled_transfer_decays_to_zero_and_a_restart_resets_the_window() {
        let p = Progress::default();
        let t0 = Instant::now();
        p.record(1, 0, None, t0);
        p.record(1, 5000, None, t0 + ms(1000));
        assert!(p.rate(1, true, t0 + ms(1000)).unwrap().speed > 0);
        // nothing for 10 s
        let r = p.rate(1, true, t0 + ms(11_000)).unwrap();
        assert_eq!(r.speed, 0);
        assert_eq!(r.eta, None, "unknown total and no speed");
        // restart from zero
        p.record(1, 100, Some(1000), t0 + ms(12_000));
        p.record(1, 600, None, t0 + ms(13_000));
        let r = p.rate(1, true, t0 + ms(13_000)).unwrap();
        assert!((450..=550).contains(&r.speed), "speed {}", r.speed);
        assert_eq!(r.eta, Some(1));
        p.forget(1);
        assert_eq!(p.rate(1, true, t0), None);
    }

    #[test]
    fn progress_numbers_never_overflow() {
        let p = Progress::default();
        let t0 = Instant::now();
        p.record(1, 0, Some(u64::MAX), t0);
        p.record(1, u64::MAX, None, t0 + ms(1000));
        let r = p.rate(1, true, t0 + ms(1000)).unwrap();
        assert_eq!(r.eta, Some(0));
        p.record(1, u64::MAX, Some(1), t0 + ms(2000)); // done > total
        assert_eq!(p.rate(1, true, t0 + ms(2000)).unwrap().eta.unwrap_or(0), 0);
    }

    #[test]
    fn error_kinds_cover_the_engine_texts() {
        let cases = [
            ("network error: connection refused", "network"),
            ("server error: HTTP 404 for http://h/x", "not_found"),
            ("server error: HTTP 503 for http://h/x", "server"),
            ("access denied by server (HTTP 403)", "forbidden"),
            (
                "the file changed on the server while downloading",
                "changed",
            ),
            ("server does not honour byte ranges", "no_resume"),
            ("download made no progress after 5 attempts", "stalled"),
            (
                "transfer ended before the full length arrived (short body)",
                "stalled",
            ),
            (
                "I/O error (write): There is not enough space on the disk. (os error 112)",
                "disk_full",
            ),
            (
                "I/O error (create): Access is denied. (os error 5)",
                "disk_denied",
            ),
            ("I/O error (rename): something", "disk"),
            ("checksum mismatch: expected a, got b", "checksum"),
            (
                "this download is already running (its job lock file is held)",
                "busy",
            ),
            ("bad or untrusted state file: x", "state"),
            ("server sent an invalid response: x", "bad_response"),
            ("invalid input: not a valid URL", "bad_url"),
            ("invalid input: unsupported scheme 'ftp'", "bad_url"),
            ("invalid input: too many items", "too_many"),
            ("something new", "unknown"),
        ];
        for (msg, kind) in cases {
            assert_eq!(error_kind(msg), kind, "{msg}");
        }
    }

    #[test]
    fn links_are_split_deduplicated_and_bounded() {
        assert_eq!(
            parse_links("  http://a/1 \r\n\nhttp://a/2 http://a/1\t").unwrap(),
            vec!["http://a/1", "http://a/2"]
        );
        assert_eq!(parse_links(" \n ").unwrap_err(), "no_links");
        let many: String = (0..=MAX_LINKS_PER_ADD)
            .map(|i| format!("http://a/{i}\n"))
            .collect();
        assert_eq!(parse_links(&many).unwrap_err(), "too_many_links");
    }

    #[test]
    fn folders_must_be_absolute_existing_directories() {
        let d = tempfile::tempdir().unwrap();
        assert!(check_folder(&d.path().to_string_lossy()).is_ok());
        let f = d.path().join("file.txt");
        std::fs::write(&f, b"x").unwrap();
        for bad in [
            String::new(),
            "relative/dir".into(),
            f.to_string_lossy().into_owned(),
            d.path().join("missing").to_string_lossy().into_owned(),
            format!("{}\0x", d.path().display()),
        ] {
            assert_eq!(check_folder(&bad).unwrap_err(), "bad_folder", "{bad:?}");
        }
    }

    fn sched(dir: &Path) -> Scheduler {
        let s = Scheduler::open(
            SchedulerConfig::new(&dir.join("queues.json")),
            Arc::new(EngineRunner::new(Config::default())),
            Arc::new(SystemClock),
            Arc::new(NoPower),
            Arc::new(|_| {}),
        )
        .unwrap();
        let main = dir.join("dl").to_string_lossy().into_owned();
        s.edit(|qs| {
            qs.queues.push(Queue::new(MAIN_QUEUE, &main));
            Ok(())
        })
        .unwrap();
        s
    }

    #[test]
    fn add_into_main_or_a_picked_folder_and_refuse_bad_links() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(d.path());
        let r = add_links(
            &s,
            "http://h/a.zip\nftp://h/b\nnot-a-url\nhttps://user:secret@h/c.iso?token=1",
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.added.len(), 2);
        assert_eq!(r.refused.len(), 2);
        assert!(r.refused.iter().all(|(_, k)| *k == "bad_url"), "{r:?}");
        let other = tempfile::tempdir().unwrap();
        let od = other.path().to_string_lossy().into_owned();
        let r2 = add_links(&s, "http://h/x.bin", Some(&od), Some("  my file.bin ")).unwrap();
        let r3 = add_links(&s, "http://h/y.bin", Some(&od), None).unwrap();
        let qs = s.snapshot();
        assert_eq!(qs.queues.len(), 2, "one queue per folder, reused");
        let q = qs.queues.iter().find(|q| q.dir == od).unwrap();
        assert_eq!(q.items.len(), 2);
        assert_eq!(q.items[0].file_name.as_deref(), Some("my file.bin"));
        assert_eq!(q.items[0].id, r2.added[0]);
        assert_eq!(q.items[1].id, r3.added[0]);
        // the secret never reaches the list
        let v = view(&qs, &Progress::default(), Instant::now(), 0);
        let text = serde_json::to_string(&v).unwrap();
        assert!(
            !text.contains("secret") && !text.contains("token"),
            "{text}"
        );
        assert_eq!(v[0].id, r3.added[0], "newest first");
        assert_eq!(v.last().unwrap().name, "a.zip");
        // a name with two links, a bad folder, nothing at all
        assert_eq!(
            add_links(&s, "http://h/1 http://h/2", None, Some("n")).unwrap_err(),
            "name_needs_one_link"
        );
        assert_eq!(
            add_links(&s, "http://h/1", Some("relative"), None).unwrap_err(),
            "bad_folder"
        );
        assert_eq!(add_links(&s, "", None, None).unwrap_err(), "no_links");
    }

    #[test]
    fn unsafe_file_names_are_sanitized_by_the_engine() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(d.path());
        for n in ["..\\..\\evil.exe", "../x", "CON", "a\u{202e}gpj.exe"] {
            add_links(&s, "http://h/z", None, Some(n)).unwrap();
        }
        for it in &s.snapshot().queues[0].items {
            let n = it.file_name.as_deref().unwrap();
            assert!(eid_engine::sanitize::is_safe_stored_name(n), "{n:?}");
            assert!(!n.contains(['/', '\\']), "{n:?}");
        }
    }

    #[test]
    fn remove_refuses_running_items_and_clear_keeps_unfinished_ones() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(d.path());
        let ids = add_links(&s, "http://h/1 http://h/2 http://h/3", None, None)
            .unwrap()
            .added;
        s.edit(|qs| {
            qs.queues[0].items[0].status = Status::Running;
            qs.queues[0].items[1].status = Status::Done;
            Ok(())
        })
        .unwrap();
        assert_eq!(remove_item(&s, ids[0]).unwrap_err(), "pause_first");
        assert_eq!(remove_item(&s, 999).unwrap_err(), "no_such_item");
        assert_eq!(clear_finished(&s).unwrap(), 1);
        remove_item(&s, ids[2]).unwrap();
        let left: Vec<u64> = s.snapshot().queues[0].items.iter().map(|i| i.id).collect();
        assert_eq!(left, vec![ids[0]]);
    }

    #[test]
    fn view_statuses_errors_and_finished_files() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(d.path());
        let ids = add_links(
            &s,
            "http://h/1 http://h/2 http://h/3 http://h/4",
            None,
            None,
        )
        .unwrap()
        .added;
        s.edit(|qs| {
            let it = &mut qs.queues[0].items;
            it[0].not_before = Some(2000);
            it[0].last_error = Some("network error: reset".into());
            it[1].status = Status::Failed;
            it[1].last_error = Some("server error: HTTP 404 for http://h/2?sig=abc".into());
            it[2].status = Status::Done;
            it[2].file_name = Some("three.bin".into());
            it[2].last_error = Some("network error: old".into());
            it[3].status = Status::Paused;
            Ok(())
        })
        .unwrap();
        let p = Progress::default();
        let t = Instant::now();
        p.record(ids[2], 50, Some(100), t);
        let v = view(&s.snapshot(), &p, t, 1000);
        let by = |id: u64| v.iter().find(|x| x.id == id).unwrap();
        assert_eq!(by(ids[0]).status, "waiting");
        assert_eq!(by(ids[0]).error_kind, Some("network"));
        assert_eq!(by(ids[1]).status, "failed");
        assert_eq!(by(ids[1]).error_kind, Some("not_found"));
        assert!(!by(ids[1]).error.as_ref().unwrap().contains("sig=abc"));
        assert_eq!(by(ids[2]).status, "done");
        assert_eq!(by(ids[2]).error, None, "a finished item shows no old error");
        assert_eq!(by(ids[2]).rate.unwrap().done, 100);
        assert_eq!(by(ids[3]).status, "paused");
        assert_eq!(
            finished_file(&s.snapshot(), ids[2]).unwrap(),
            d.path().join("dl").join("three.bin")
        );
        assert_eq!(finished_file(&s.snapshot(), ids[3]), None);
        // later the retry time has passed
        let v = view(&s.snapshot(), &p, t, 3000);
        assert_eq!(v.iter().find(|x| x.id == ids[0]).unwrap().status, "pending");
    }

    #[test]
    fn display_names_fall_back_to_the_url() {
        let it = |u: &str| Item {
            id: 1,
            url: u.into(),
            subdir: None,
            file_name: None,
            status: Status::Pending,
            attempts: 0,
            not_before: None,
            last_error: None,
        };
        assert_eq!(
            display_name(&it("http://h/dir/%D9%85%D9%84%D9%81.zip")),
            "ملف.zip"
        );
        assert_eq!(display_name(&it("http://example.com/")), "example.com");
    }
}
