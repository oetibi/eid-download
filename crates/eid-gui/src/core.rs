//! The desktop app's logic, kept free of Tauri so it is unit tested on every platform: the view
//! of the queue the window shows, byte progress with speed and time left, adding links into the
//! folder the user picked, removing items, and plain-language error categories.

use eid_engine::queue::{Item, PowerAction, Queue, QueueSet, Status};
use eid_engine::scheduler::Scheduler;
use eid_engine::urlsafe;
use eid_ipc::Command;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

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

/// File types that run code when "opened" on Windows: `open_file` never starts these.
const EXECUTABLE_EXT: &[&str] = &[
    "exe",
    "msi",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "bat",
    "cmd",
    "com",
    "scr",
    "pif",
    "cpl",
    "ps1",
    "psm1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "hta",
    "lnk",
    "url",
    "reg",
    "jar",
    "msc",
    "application",
    "appref-ms",
    "gadget",
    "inf",
    "sys",
    "dll",
    "ocx",
    "chm",
    "settingcontent-ms",
    "library-ms",
    "iso",
    "img",
    "vhd",
    "vhdx",
];

/// Is `p` a program, script or shortcut (by extension, case-insensitive)?
pub fn is_executable_name(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXECUTABLE_EXT.iter().any(|x| x.eq_ignore_ascii_case(e)))
        || p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with('.') || n.ends_with(' '))
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
    /// v0.2: this download's own speed limit (KiB/s), if any.
    pub limit_kib: Option<u64>,
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
                limit_kib: it.limit_kib,
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

/// v0.2 queue management: one queue as the window lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueueView {
    pub name: String,
    pub dir: String,
    pub max_parallel: u32,
    pub paused: bool,
    /// none | exit | sleep | hibernate | shutdown
    pub after_done: &'static str,
    pub total: usize,
    pub done: usize,
    pub running: usize,
    pub failed: usize,
}

pub const AFTER_DONE: &[&str] = &["none", "exit", "sleep", "hibernate", "shutdown"];

pub fn power_name(a: PowerAction) -> &'static str {
    match a {
        PowerAction::None => "none",
        PowerAction::Exit => "exit",
        PowerAction::Sleep => "sleep",
        PowerAction::Hibernate => "hibernate",
        PowerAction::Shutdown => "shutdown",
    }
}

pub fn power_from_name(s: &str) -> Result<PowerAction, String> {
    Ok(match s {
        "none" => PowerAction::None,
        "exit" => PowerAction::Exit,
        "sleep" => PowerAction::Sleep,
        "hibernate" => PowerAction::Hibernate,
        "shutdown" => PowerAction::Shutdown,
        _ => {
            return Err(format!(
                "after_done must be one of {}",
                AFTER_DONE.join(", ")
            ))
        }
    })
}

/// Sleep, hibernate and shut down act on the computer (off unless allowed in Settings).
pub fn is_machine_action(a: PowerAction) -> bool {
    matches!(
        a,
        PowerAction::Sleep | PowerAction::Hibernate | PowerAction::Shutdown
    )
}

/// Every queue, main first, then in creation order.
pub fn queues_view(qs: &QueueSet) -> Vec<QueueView> {
    let mut v: Vec<QueueView> = qs
        .queues
        .iter()
        .map(|q| QueueView {
            name: q.name.clone(),
            dir: q.dir.clone(),
            max_parallel: q.max_parallel,
            paused: q.paused,
            after_done: power_name(q.after_done),
            total: q.items.len(),
            done: q.items.iter().filter(|i| i.status == Status::Done).count(),
            running: q
                .items
                .iter()
                .filter(|i| i.status == Status::Running)
                .count(),
            failed: q
                .items
                .iter()
                .filter(|i| i.status == Status::Failed)
                .count(),
        })
        .collect();
    v.sort_by_key(|q| q.name != MAIN_QUEUE);
    v
}

/// A change to one queue from the window. Absent fields are kept.
#[derive(Debug, Clone, Default, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QueuePatch {
    pub name: String,
    #[serde(default)]
    pub max_parallel: Option<u32>,
    #[serde(default)]
    pub paused: Option<bool>,
    #[serde(default)]
    pub after_done: Option<String>,
}

/// Validate and apply `p` (all or nothing). Returns the ids of the queue's running items when
/// the queue was just stopped (the caller pauses them: a stopped queue starts nothing and keeps
/// nothing running) and of its paused items when it was just started (the caller resumes them).
pub fn apply_queue_patch(
    qs: &mut QueueSet,
    p: &QueuePatch,
    power_allowed: bool,
) -> Result<(Vec<u64>, Vec<u64>), String> {
    let max = eid_cli::gui_settings::MAX_PARALLEL;
    if let Some(n) = p.max_parallel {
        if !(1..=max).contains(&n) {
            return Err(format!("simultaneous downloads must be 1..={max}"));
        }
    }
    let after = p.after_done.as_deref().map(power_from_name).transpose()?;
    if after.is_some_and(is_machine_action) && !power_allowed {
        return Err("power_actions_off".into());
    }
    let q = qs
        .queues
        .iter_mut()
        .find(|q| q.name == p.name)
        .ok_or_else(|| "no_such_queue".to_string())?;
    if let Some(n) = p.max_parallel {
        q.max_parallel = n;
    }
    if let Some(a) = after {
        q.after_done = a;
    }
    let (mut stop, mut start) = (vec![], vec![]);
    if let Some(pz) = p.paused {
        if pz && !q.paused {
            stop = q
                .items
                .iter()
                .filter(|i| i.status == Status::Running)
                .map(|i| i.id)
                .collect();
        }
        if !pz && q.paused {
            start = q
                .items
                .iter()
                .filter(|i| i.status == Status::Paused)
                .map(|i| i.id)
                .collect();
        }
        q.paused = pz;
    }
    Ok((stop, start))
}

/// v0.2 import: what happened to a list file.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct ImportReport {
    pub added: usize,
    /// Entries the file had that could not be added (bad links, refused by the queue).
    pub refused: usize,
    pub duplicates: usize,
    /// Links whose `user:pass@` was removed (never stored).
    pub credentials_dropped: usize,
}

/// Import a list file's bytes (untrusted: size-, count- and content-checked by
/// `eid_engine::porting`, names and sub-folders sanitized) into the main queue or the queue of
/// `dir`. Format from the file extension (`.txt`/`.list`, `.json`, `.csv`).
pub fn import_into(
    sched: &Scheduler,
    bytes: &[u8],
    file_name: &str,
    dir: Option<&str>,
) -> Result<ImportReport, String> {
    let fmt = eid_engine::porting::Format::from_extension(file_name)
        .ok_or_else(|| "bad_list_format".to_string())?;
    let imp = eid_engine::porting::import(bytes, fmt).map_err(|e| e.to_string())?;
    let queue = match dir {
        None => MAIN_QUEUE.to_owned(),
        Some(d) => {
            let d = check_folder(d)?.to_string_lossy().into_owned();
            sched
                .edit(|qs| queue_for_dir(qs, &d))
                .map_err(|e| e.to_string())?
        }
    };
    let mut r = ImportReport {
        refused: imp.rejected_total,
        duplicates: imp.duplicates,
        credentials_dropped: imp.credentials_dropped,
        ..ImportReport::default()
    };
    for it in imp.items {
        match sched.add_item(
            &queue,
            &it.url,
            it.subdir.as_deref(),
            it.name.as_deref(),
            None,
        ) {
            Ok(_) => r.added += 1,
            Err(_) => r.refused += 1,
        }
    }
    Ok(r)
}

/// v0.2 export: every item of every queue (credential-free URL, file name, sub-folder).
pub fn export_list(qs: &QueueSet, file_name: &str) -> Result<String, String> {
    let fmt = eid_engine::porting::Format::from_extension(file_name)
        .ok_or_else(|| "bad_list_format".to_string())?;
    let items: Vec<eid_engine::porting::ListItem> = qs
        .queues
        .iter()
        .flat_map(|q| q.items.iter())
        .map(|i| eid_engine::porting::ListItem {
            url: i.url.clone(),
            name: i.file_name.clone(),
            subdir: i.subdir.clone(),
        })
        .collect();
    Ok(eid_engine::porting::export(&items, fmt))
}

/// v0.2 clipboard monitoring: the copied text, if it is exactly one http(s) link worth
/// offering: a file whose name has a known download extension (archives, documents, music,
/// video, pictures, programs) or a YouTube page. Anything else (plain text, links to ordinary
/// pages, links with a user name or password, very long text) is ignored. Returns
/// `(link, "file" | "video")`.
pub fn clipboard_link(text: &str) -> Option<(String, &'static str)> {
    const MAX: usize = 2048;
    let t = text.trim();
    if t.is_empty() || t.len() > MAX || t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let u = url::Url::parse(t).ok()?;
    if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some()
    {
        return None;
    }
    if eid_engine::links::video_page(t).is_some() {
        return Some((u.to_string(), "video"));
    }
    let last = u.path_segments()?.next_back()?.to_string();
    let name = percent_encoding::percent_decode_str(&last).decode_utf8_lossy();
    let cat = eid_engine::category::Categorizer::default();
    (cat.classify(&name, None).name != "Other").then(|| (u.to_string(), "file"))
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
    } else if has("opens a web page") {
        "web_page"
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
    add_links_with(sched, text, dir, name, "none")
}

/// v0.2 category folders: `none` (the folder itself), `auto` (a sub-folder named after the
/// file's type: Compressed, Documents, Music, Video, Programs, Pictures; nothing for other or
/// unknown types) or one category's name. Returns the sub-folder (a plain, sanitized name).
pub fn category_subdir(
    url: &str,
    name: Option<&str>,
    mode: &str,
) -> Result<Option<String>, String> {
    let cat = eid_engine::category::Categorizer::default();
    if mode == "none" {
        return Ok(None);
    }
    if mode != "auto" {
        let c = cat
            .rules
            .iter()
            .find(|c| c.name == mode)
            .ok_or_else(|| "bad_category".to_string())?;
        return Ok(Some(eid_engine::sanitize::sanitize_file_name(&c.folder)));
    }
    let file = match name {
        Some(n) => n.to_string(),
        None => url::Url::parse(url)
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
            .unwrap_or_default(),
    };
    let c = cat.classify(&file, None);
    if c.name == cat.fallback.name {
        return Ok(None);
    }
    Ok(Some(eid_engine::sanitize::sanitize_file_name(&c.folder)))
}

/// The category choices the add dialog offers (besides `none` and `auto`).
pub fn category_names() -> Vec<String> {
    eid_engine::category::Categorizer::default()
        .rules
        .iter()
        .map(|c| c.name.clone())
        .collect()
}

/// [`add_links`] with a category folder mode (see [`category_subdir`]).
pub fn add_links_with(
    sched: &Scheduler,
    text: &str,
    dir: Option<&str>,
    name: Option<&str>,
    category: &str,
) -> Result<AddReport, String> {
    if category != "none" && category != "auto" && !category_names().iter().any(|c| c == category) {
        return Err("bad_category".into());
    }
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
        // v0.2: a video page is never a file (the window opens the video dialog for it).
        // The link goes back WHOLE: the window needs `?v=...` (redaction drops the query; the
        // real-window test caught it), and `video_page` never matches a link with credentials.
        if eid_engine::links::video_page(&l).is_some() {
            r.refused.push((l.clone(), "video_page"));
            continue;
        }
        let sub = category_subdir(&l, name, category)?;
        match sched.add_item(&queue, &l, sub.as_deref(), name, None) {
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

/// The desktop app's IPC handler: the one `eid serve` uses, plus `show` (a second launch of the
/// app asks the open one to bring its window forward instead of showing a second window).
pub fn with_show<H>(
    inner: H,
    show: Arc<Notify>,
) -> impl Fn(Command) -> Result<serde_json::Value, String> + Send + Sync + 'static
where
    H: Fn(Command) -> Result<serde_json::Value, String> + Send + Sync + 'static,
{
    with_window(inner, show, Arc::default())
}

/// v0.2: links the browser asked the window to open (`open_video`), taken by the next `list`.
/// Bounded: a page cannot queue an unbounded number of windows.
pub type OpenRequests = Arc<std::sync::Mutex<Vec<OpenRequest>>>;

/// One `open_video` for the window: the page and the quality chosen in the browser, if any.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct OpenRequest {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
}
const MAX_OPEN_REQUESTS: usize = 8;

/// [`with_show`] plus `open_video`: the link is queued for the window, which opens its Video
/// window with it (the user chooses there), and the window comes to the front.
pub fn with_window<H>(
    inner: H,
    show: Arc<Notify>,
    open: OpenRequests,
) -> impl Fn(Command) -> Result<serde_json::Value, String> + Send + Sync + 'static
where
    H: Fn(Command) -> Result<serde_json::Value, String> + Send + Sync + 'static,
{
    move |cmd| match cmd {
        Command::Show {} => {
            // stores a permit when the window task is not waiting yet: never lost
            show.notify_one();
            Ok(serde_json::json!({}))
        }
        Command::OpenVideo { url, quality } => {
            let mut q = open.lock().unwrap_or_else(|p| p.into_inner());
            if q.len() >= MAX_OPEN_REQUESTS {
                return Err("too many open requests; finish the ones in the window first".into());
            }
            q.push(OpenRequest { url, quality });
            drop(q);
            show.notify_one();
            Ok(serde_json::json!({ "opened": true }))
        }
        other => inner(other),
    }
}

/// v0.2 startup self-check (owner's Windows test, 2026-11-19): what is wrong with where the app
/// runs, as codes the window explains. `exe` is this program; `exists` tells whether a file is
/// there (the real disk in the app, a list in the tests).
/// - `from_zip`: started from inside a ZIP (Explorer runs it from `%TEMP%\Temp1_<name>.zip\`,
///   7-Zip from `%TEMP%\7zO...`, WinRAR from `%TEMP%\Rar$EX...`): only the clicked file is
///   there, so the browser link cannot work and Windows may delete the folder at any time.
/// - `no_native_host`: `eid-native-host` is not next to the app, so the browser extension
///   cannot reach it (downloads in the window still work).
pub fn install_problems(
    exe: &std::path::Path,
    exists: &dyn Fn(&std::path::Path) -> bool,
) -> Vec<&'static str> {
    let mut v = Vec::new();
    let from_zip = exe
        .parent()
        .into_iter()
        .flat_map(|p| p.ancestors())
        .filter_map(|a| a.file_name())
        .any(|n| {
            let n = n.to_string_lossy().to_lowercase();
            n.ends_with(".zip") || n.starts_with("7zo") || n.starts_with("rar$ex")
        });
    if from_zip {
        v.push("from_zip");
    }
    let host = exe.with_file_name(format!("eid-native-host{}", std::env::consts::EXE_SUFFIX));
    if !exists(&host) {
        v.push("no_native_host");
    }
    v
}

/// `edit` wraps the closure's error as "invalid input: X"; give the window just X.
fn invalid_input_text(s: &str) -> String {
    s.strip_prefix("invalid input: ").unwrap_or(s).to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_startup_check_sees_a_zip_folder_and_a_missing_native_host() {
        use super::install_problems;
        use std::path::{Path, PathBuf};
        let host = |dir: &str| {
            PathBuf::from(dir).join(format!("eid-native-host{}", std::env::consts::EXE_SUFFIX))
        };
        let only = |p: PathBuf| move |q: &Path| q == p;
        let sep = std::path::MAIN_SEPARATOR;
        let d = |parts: &[&str]| parts.join(&sep.to_string());
        // extracted properly, with everything next to it: nothing to say
        let ok = d(&["", "Users", "me", "Apps", "EidDownload"]);
        assert!(
            install_problems(&Path::new(&ok).join("EidDownload.exe"), &only(host(&ok))).is_empty()
        );
        // Explorer's "run from inside the ZIP" folders (any case), 7-Zip and WinRAR
        for zipdir in [
            d(&[
                "",
                "Users",
                "me",
                "AppData",
                "Local",
                "Temp",
                "Temp1_EidDownload-windows-x64.zip",
                "EidDownload",
            ]),
            d(&[
                "",
                "Users",
                "me",
                "AppData",
                "Local",
                "Temp",
                "Temp2_EIDDOWNLOAD.ZIP",
            ]),
            d(&["", "tmp", "7zO8A1B2C3D"]),
            d(&["", "tmp", "Rar$EXa1234.5678"]),
        ] {
            let exe = Path::new(&zipdir).join("EidDownload.exe");
            assert_eq!(
                install_problems(&exe, &|_| false),
                ["from_zip", "no_native_host"],
                "{zipdir}"
            );
            assert_eq!(
                install_problems(&exe, &only(host(&zipdir))),
                ["from_zip"],
                "{zipdir}"
            );
        }
        // a folder merely named like a zip file further away is still a zip folder; a file
        // name is not looked at, and "zip" inside a name is fine
        let near = d(&["", "Users", "me", "zipper", "my.zip.tools"]);
        assert!(install_problems(
            &Path::new(&near).join("EidDownload.zip.exe"),
            &only(host(&near))
        )
        .is_empty());
        // missing native host alone
        assert_eq!(
            install_problems(&Path::new(&ok).join("EidDownload.exe"), &|_| false),
            ["no_native_host"]
        );
    }

    #[test]
    fn executables_are_never_opened() {
        use std::path::Path;
        for n in [
            "a.exe",
            "B.EXE",
            "x.msi",
            "s.ps1",
            "l.lnk",
            "d.iso",
            "t.bat",
            "trailing.pdf.",
            "sp.pdf ",
        ] {
            assert!(super::is_executable_name(Path::new(n)), "{n}");
        }
        for n in ["a.pdf", "v.mp4", "z.zip", "noext", "exe.txt"] {
            assert!(!super::is_executable_name(Path::new(n)), "{n}");
        }
    }

    use super::*;
    use eid_engine::config::Config;
    use eid_engine::queue_runner::EngineRunner;
    use eid_engine::scheduler::{NoPower, SchedulerConfig, SystemClock};
    use std::sync::Arc;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[tokio::test]
    async fn show_wakes_the_window_and_everything_else_goes_to_the_shared_handler() {
        let show = Arc::new(Notify::new());
        let seen = Arc::new(Mutex::new(vec![]));
        let s2 = seen.clone();
        let h = with_show(
            move |c: Command| {
                s2.lock().unwrap().push(c);
                Ok(serde_json::json!({"inner": true}))
            },
            show.clone(),
        );
        // sent before anyone waits: the permit is kept
        assert_eq!(h(Command::Show {}).unwrap(), serde_json::json!({}));
        tokio::time::timeout(Duration::from_secs(5), show.notified())
            .await
            .expect("show was not signalled");
        assert_eq!(h(Command::List {}).unwrap()["inner"], true);
        assert_eq!(*seen.lock().unwrap(), vec![Command::List {}]);
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

    #[tokio::test]
    async fn the_background_server_refuses_show_and_the_app_answers_it() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(d.path());
        let mk = || {
            eid_cli::serve::handler(
                s.clone(),
                MAIN_QUEUE.into(),
                eid_engine::CancellationToken::new(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                eid_cli::serve::APP_GUI,
            )
        };
        assert!(mk()(Command::Show {}).is_err());
        let app = with_show(mk(), Arc::new(Notify::new()));
        assert!(app(Command::Show {}).is_ok());
        assert_eq!(app(Command::Ping {}).unwrap()["app"], "gui");
    }

    /// v0.2: the browser's "Open in Eid Download": the link is queued for the window (taken
    /// once by `list`), the window is asked to come forward, the queue is bounded, and every
    /// other command still reaches the real handler.
    #[test]
    fn open_video_queues_the_link_for_the_window_and_brings_it_forward() {
        use eid_ipc::Command;
        let show = Arc::new(Notify::new());
        let open: OpenRequests = Arc::default();
        let h = with_window(
            |_c| Ok(serde_json::json!({"inner": true})),
            show.clone(),
            open.clone(),
        );
        let u = "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PLtestlist01";
        assert_eq!(
            h(Command::OpenVideo {
                url: u.into(),
                quality: Some("720p".into())
            })
            .unwrap()["opened"],
            true
        );
        assert_eq!(
            *open.lock().unwrap(),
            [OpenRequest {
                url: u.into(),
                quality: Some("720p".into())
            }]
        );
        assert_eq!(
            serde_json::to_value(&open.lock().unwrap()[0]).unwrap(),
            serde_json::json!({"url": u, "quality": "720p"})
        );
        // the window got a "come forward" permit
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(1), show.notified())
                .await
                .expect("no come-forward permit")
        });
        for i in 1..8 {
            h(Command::OpenVideo {
                url: format!("https://h/{i}"),
                quality: None,
            })
            .unwrap();
        }
        assert!(
            h(Command::OpenVideo {
                url: "https://h/9".into(),
                quality: None
            })
            .is_err(),
            "bounded"
        );
        assert_eq!(h(Command::Ping {}).unwrap()["inner"], true);
    }

    /// v0.2 (owner's Windows test): a YouTube page in the add dialog is never queued as a
    /// file; it comes back as `video_page` (the window opens the video dialog for it) while the
    /// file links next to it are added.
    #[test]
    fn video_pages_are_not_queued_as_files() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(d.path());
        let r = add_links(
            &s,
            "https://www.youtube.com/watch?v=abc\nhttp://h/a.zip\nhttps://youtu.be/xyz\nhttps://www.youtube.com/playlist?list=PL1",
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.added.len(), 1, "{r:?}");
        assert_eq!(r.refused.len(), 3, "{r:?}");
        assert!(r.refused.iter().all(|(_, k)| *k == "video_page"), "{r:?}");
        // the window opens the video dialog with exactly this link (query included)
        assert_eq!(r.refused[0].0, "https://www.youtube.com/watch?v=abc");
        assert_eq!(r.refused[2].0, "https://www.youtube.com/playlist?list=PL1");
        let urls: Vec<String> = s
            .snapshot()
            .queues
            .iter()
            .flat_map(|q| q.items.iter().map(|i| i.url.clone()))
            .collect();
        assert_eq!(urls, ["http://h/a.zip"]);
        assert_eq!(
            error_kind(&eid_engine::Error::WebPage("text/html".into()).to_string()),
            "web_page"
        );
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
    fn clipboard_offers_only_one_download_link_and_ignores_everything_else() {
        let ok = |t: &str| clipboard_link(t).map(|(_, k)| k);
        assert_eq!(ok("https://example.com/files/setup.exe"), Some("file"));
        assert_eq!(
            ok("  http://h.test/a/b/%D9%85%D9%84%D9%81.ZIP?x=1  "),
            Some("file")
        );
        assert_eq!(ok("https://www.youtube.com/watch?v=abc"), Some("video"));
        assert_eq!(ok("https://youtu.be/abc"), Some("video"));
        assert_eq!(ok("https://m.youtube.com/playlist?list=PL1"), Some("video"));
        for no in [
            "",
            "hello world",
            "https://example.com/",
            "https://example.com/page.html",
            "https://www.youtube.com/",
            "https://user:pass@example.com/a.zip",
            "https://example.com/a.zip https://example.com/b.zip",
            "ftp://example.com/a.zip",
            "file:///C:/a.zip",
            "javascript:alert(1)//a.zip",
            "https://example.com/a.zip\u{202e}",
        ] {
            assert_eq!(ok(no), None, "{no:?}");
        }
        let long = format!("https://example.com/{}.zip", "a".repeat(3000));
        assert_eq!(ok(&long), None);
    }

    #[test]
    fn queues_are_listed_main_first_and_patches_are_validated_all_or_nothing() {
        let it = |id: u64, status: Status| Item {
            id,
            url: format!("http://h/{id}.zip"),
            subdir: None,
            file_name: None,
            status,
            attempts: 0,
            not_before: None,
            last_error: None,
            limit_kib: None,
        };
        let mut a = Queue::new("folder-1", "/d1");
        a.items = vec![
            it(1, Status::Running),
            it(2, Status::Paused),
            it(3, Status::Done),
        ];
        let mut m = Queue::new(MAIN_QUEUE, "/m");
        m.items = vec![it(4, Status::Failed)];
        let mut qs = QueueSet {
            queues: vec![a, m],
            next_id: 10,
        };
        let v = queues_view(&qs);
        assert_eq!(v[0].name, MAIN_QUEUE);
        assert_eq!(
            (v[1].total, v[1].done, v[1].running, v[1].failed),
            (3, 1, 1, 0)
        );
        assert_eq!(v[0].after_done, "none");
        // stop: running items are reported to be paused
        let p = QueuePatch {
            name: "folder-1".into(),
            paused: Some(true),
            ..Default::default()
        };
        assert_eq!(
            apply_queue_patch(&mut qs, &p, true).unwrap(),
            (vec![1], vec![])
        );
        assert!(qs.queues[0].paused);
        // start: its paused items are reported to be resumed
        let p = QueuePatch {
            name: "folder-1".into(),
            paused: Some(false),
            max_parallel: Some(4),
            after_done: Some("shutdown".into()),
        };
        assert_eq!(
            apply_queue_patch(&mut qs, &p, true).unwrap(),
            (vec![], vec![2])
        );
        assert_eq!(
            (qs.queues[0].max_parallel, qs.queues[0].after_done),
            (4, PowerAction::Shutdown)
        );
        // invalid: nothing changes
        let before = qs.clone();
        for bad in [
            QueuePatch {
                name: "folder-1".into(),
                max_parallel: Some(0),
                paused: Some(true),
                ..Default::default()
            },
            QueuePatch {
                name: "folder-1".into(),
                max_parallel: Some(99),
                ..Default::default()
            },
            QueuePatch {
                name: "folder-1".into(),
                after_done: Some("reboot".into()),
                paused: Some(true),
                ..Default::default()
            },
            QueuePatch {
                name: "nope".into(),
                paused: Some(true),
                ..Default::default()
            },
        ] {
            assert!(apply_queue_patch(&mut qs, &bad, true).is_err(), "{bad:?}");
            assert_eq!(qs, before);
        }
        for n in AFTER_DONE {
            assert_eq!(power_name(power_from_name(n).unwrap()), *n);
        }
        // owner decision: computer actions are refused unless allowed in Settings; quitting is fine
        let before = qs.clone();
        for a in ["sleep", "hibernate", "shutdown"] {
            let p = QueuePatch {
                name: "folder-1".into(),
                after_done: Some(a.into()),
                ..Default::default()
            };
            assert_eq!(
                apply_queue_patch(&mut qs, &p, false).unwrap_err(),
                "power_actions_off"
            );
            assert_eq!(qs, before);
        }
        for a in ["none", "exit"] {
            let p = QueuePatch {
                name: "folder-1".into(),
                after_done: Some(a.into()),
                ..Default::default()
            };
            assert!(apply_queue_patch(&mut qs, &p, false).is_ok(), "{a}");
        }
    }

    #[test]
    fn category_folders_auto_fixed_and_none() {
        let sub = |u: &str, m: &str| category_subdir(u, None, m).unwrap();
        assert_eq!(
            sub("http://h/a/setup.ZIP", "auto").as_deref(),
            Some("Compressed")
        );
        assert_eq!(
            sub("http://h/%D8%AA%D9%82%D8%B1%D9%8A%D8%B1.pdf", "auto").as_deref(),
            Some("Documents")
        );
        assert_eq!(
            sub("http://h/page", "auto"),
            None,
            "unknown type: no sub-folder"
        );
        assert_eq!(sub("http://h/x.weird", "auto"), None);
        assert_eq!(sub("http://h/x.zip", "none"), None);
        assert_eq!(sub("http://h/x.zip", "Music").as_deref(), Some("Music"));
        // the name given by the user wins over the link
        assert_eq!(
            category_subdir("http://h/x", Some("song.mp3"), "auto")
                .unwrap()
                .as_deref(),
            Some("Music")
        );
        assert!(category_subdir("http://h/x", None, "../evil").is_err());
        let d = tempfile::tempdir().unwrap();
        let s = sched(d.path());
        assert_eq!(
            add_links_with(&s, "http://h/a.zip", None, None, "evil").unwrap_err(),
            "bad_category"
        );
        add_links_with(
            &s,
            "http://h/a.zip\nhttp://h/b.mp4\nhttp://h/c",
            None,
            None,
            "auto",
        )
        .unwrap();
        let subs: Vec<_> = s.snapshot().queues[0]
            .items
            .iter()
            .map(|i| i.subdir.clone())
            .collect();
        assert_eq!(
            subs,
            vec![Some("Compressed".into()), Some("Video".into()), None]
        );
    }

    #[test]
    fn import_and_export_lists_are_untrusted_bounded_and_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(d.path());
        let txt = b"http://h/a.zip\nhttp://user:pw@h/b.iso\njavascript:alert(1)\nhttp://h/a.zip\nftp://h/c\n";
        let r = import_into(&s, txt, "links.txt", None).unwrap();
        assert_eq!(
            (r.added, r.credentials_dropped, r.duplicates),
            (2, 1, 1),
            "{r:?}"
        );
        assert!(r.refused >= 2, "{r:?}");
        let qs = s.snapshot();
        assert!(
            !serde_json::to_string(&qs).unwrap().contains("pw@"),
            "credentials stored"
        );
        // hostile names and sub-folders are sanitized, never a path
        let json = br#"{"version":1,"items":[{"url":"http://h/x.bin","name":"../../evil.exe","subdir":"..\\up"}]}"#;
        let r = import_into(&s, json, "l.json", None).unwrap();
        assert_eq!(r.added, 1, "{r:?}");
        let it = s.snapshot().queues[0].items.last().cloned().unwrap();
        for n in [it.file_name.unwrap(), it.subdir.unwrap()] {
            assert!(!n.contains("..") || !n.contains(['/', '\\']), "{n}");
            assert!(eid_engine::sanitize::is_safe_stored_name(&n), "{n}");
        }
        // unknown format, too large
        assert_eq!(
            import_into(&s, b"x", "l.exe", None).unwrap_err(),
            "bad_list_format"
        );
        let big = vec![b'a'; eid_engine::porting::MAX_IMPORT_BYTES + 1];
        assert!(import_into(&s, &big, "l.txt", None).is_err());
        // export then import into a fresh queue file gives the same links
        for f in ["out.txt", "out.json", "out.csv"] {
            let text = export_list(&s.snapshot(), f).unwrap();
            let d2 = tempfile::tempdir().unwrap();
            let s2 = sched(d2.path());
            let r = import_into(&s2, text.as_bytes(), f, None).unwrap();
            assert_eq!(r.added, 3, "{f}: {r:?}");
            let urls = |s: &Scheduler| -> Vec<String> {
                let mut v: Vec<_> = s
                    .snapshot()
                    .queues
                    .iter()
                    .flat_map(|q| q.items.iter().map(|i| i.url.clone()))
                    .collect();
                v.sort();
                v
            };
            assert_eq!(urls(&s), urls(&s2), "{f}");
        }
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
            limit_kib: None,
        };
        assert_eq!(
            display_name(&it("http://h/dir/%D9%85%D9%84%D9%81.zip")),
            "ملف.zip"
        );
        assert_eq!(display_name(&it("http://example.com/")), "example.com");
    }
}
