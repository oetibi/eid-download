//! The desktop app process: owns the queue scheduler and the per-user IPC endpoint (so `eid add`
//! and the browser's native host hand their links to this window), and exposes the commands the
//! window calls.

use crate::core::{self, AddReport, ItemView, Progress, MAIN_QUEUE};
use crate::listdiff::ListCache;
use eid_cli::gui_settings::{self, GuiSettings};
use eid_cli::media_jobs::VideoJobs;
use eid_cli::remote::ConnArgs;
use eid_cli::serve::{self, ServeArgs, APP_GUI};
use eid_cli::video_setup::VideoSetup;
use eid_engine::queue::Queue;
use eid_engine::queue_runner::EngineRunner;
use eid_engine::scheduler::{Scheduler, SchedulerConfig, SystemClock};
use eid_engine::{CancellationToken, Limiter};
use eid_ipc::{Client, ClientError, Command, Endpoint, ServeOptions, StartError};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Notify;

/// Everything the window's commands need.
pub struct AppState {
    pub sched: Scheduler,
    pub progress: Arc<Progress>,
    pub cancel: CancellationToken,
    pub default_dir: String,
    /// Signalled when a second launch of the app asks this window to come forward.
    pub show: Arc<Notify>,
    /// v0.1: video and playlist jobs (also served to the browser over IPC).
    pub video: VideoJobs,
    /// v0.1: the window-only agreement + tool install.
    pub setup: VideoSetup,
    /// v0.1: what the window was last sent (`list` answers with changes only).
    pub list_cache: Mutex<ListCache<u64, ItemView>>,
    /// v0.1: download settings (connections, simultaneous downloads, speed limit, scheduler).
    pub settings: Mutex<GuiSettings>,
    /// v0.2: the tray menu texts (the page sends them in its language).
    pub tray_texts: Mutex<crate::desktop::TrayTexts>,
    settings_file: Option<PathBuf>,
    limiter: Arc<Limiter>,
    connections: Arc<AtomicUsize>,
    /// v0.2: "when the downloads are finished" (one-shot, with a cancellable countdown).
    pub power: Arc<crate::power::GuiPower>,
    pub power_view: Arc<Mutex<crate::power::PowerView>>,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<Result<(), String>>>>,
}

/// How start-up ended.
// built once at start-up and consumed at once: its size does not matter
#[allow(clippy::large_enum_variant)]
pub enum Boot {
    /// This process owns the queue: open the window.
    Owner(AppState),
    /// Another Eid Download window is already open; `shown` = it was asked to come forward.
    AlreadyOpen { shown: bool },
}

/// How long a background `eid serve` gets to hand the queue over.
const TAKEOVER: Duration = Duration::from_secs(15);

async fn existing_app(ep: &Endpoint) -> Result<Option<String>, String> {
    match Client::connect(ep).await {
        Ok(mut c) => {
            let r = c.call(Command::Ping {}).await.map_err(|e| e.to_string())?;
            Ok(Some(
                r["app"].as_str().unwrap_or(serve::APP_SERVE).to_owned(),
            ))
        }
        // someone owns the endpoint but our token does not fit: treat it as an open app
        Err(ClientError::Auth(_)) => Ok(Some(APP_GUI.to_owned())),
        Err(_) => Ok(None),
    }
}

/// A background `eid serve` (started by `eid` or by the browser's native host) owns the queue:
/// ask it to stop and wait until its endpoint is gone (it writes the queue file first).
async fn take_over(ep: &Endpoint) -> Result<(), String> {
    let mut c = Client::connect(ep).await.map_err(|e| e.to_string())?;
    c.call(Command::Shutdown {})
        .await
        .map_err(|e| e.to_string())?;
    drop(c);
    let deadline = Instant::now() + TAKEOVER;
    while Instant::now() < deadline {
        if matches!(Client::connect(ep).await, Err(ClientError::NotRunning)) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("the background Eid Download did not stop in time".into())
}

/// Ask the open window to come forward (a second double-click on the app).
async fn already_open(ep: &Endpoint) -> Boot {
    let shown = match Client::connect(ep).await {
        Ok(mut c) => c.call(Command::Show {}).await.is_ok(),
        Err(_) => false,
    };
    Boot::AlreadyOpen { shown }
}

/// Start-up: become the single owner of the queue, or find the window that already is.
pub async fn boot(endpoint: Option<String>) -> Result<Boot, String> {
    let conn = ConnArgs {
        endpoint,
        token_file: None,
    };
    let ep = conn.endpoint().map_err(|e| e.to_string())?;
    match existing_app(&ep).await? {
        Some(kind) if kind == APP_GUI => return Ok(already_open(&ep).await),
        Some(_) => take_over(&ep).await?,
        None => {}
    }
    let dir =
        eid_platform::dirs::default_download_dir().ok_or("cannot find your Downloads folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let default_dir = std::path::absolute(&dir)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    let qfile = serve::default_queue_file()?;
    if let Some(parent) = qfile.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let progress = Arc::new(Progress::default());
    let settings_file = gui_settings::default_file();
    let settings = settings_file
        .as_deref()
        .map(gui_settings::load)
        .unwrap_or_default();
    let limiter = Limiter::new(settings.limit_bps());
    let connections = Arc::new(AtomicUsize::new(settings.connections as usize));
    let mut cfg = serve::config_for(&ServeArgs::default());
    cfg.global_limiter = Some(limiter.clone());
    let runner = EngineRunner {
        connections_live: Some(connections.clone()),
        cfg,
        progress: Some({
            let p = progress.clone();
            Arc::new(move |item, done, total| p.record(item, done, total, Instant::now()))
        }),
    };
    let power = Arc::new(crate::power::GuiPower::default());
    power
        .allowed
        .store(settings.power_actions, Ordering::Relaxed);
    let power_view = Arc::new(Mutex::new(crate::power::PowerView::default()));
    let sched_cell = Arc::new(std::sync::OnceLock::new());
    let sched = Scheduler::open(
        SchedulerConfig::new(&qfile),
        Arc::new(runner),
        Arc::new(SystemClock),
        power.clone(),
        crate::power::sink(power_view.clone(), sched_cell.clone()),
    )
    .map_err(|e| e.to_string())?;
    let _ = sched_cell.set(sched.clone());
    let _ = power.sched.set(sched.clone());
    let main_dir = default_dir.clone();
    sched
        .edit(|qs| {
            if qs.queue_mut(MAIN_QUEUE).is_none() {
                qs.queues.push(Queue::new(MAIN_QUEUE, &main_dir));
            }
            settings.apply_to_queues(qs, MAIN_QUEUE)
        })
        .map_err(|e| e.to_string())?;
    let cancel = CancellationToken::new();
    let show = Arc::new(Notify::new());
    // video jobs run the `eid` program shipped next to this one
    let eid_exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name(format!("eid{}", std::env::consts::EXE_SUFFIX));
    let video = serve::start_video_jobs(eid_exe, PathBuf::from(&default_dir))?;
    let setup = VideoSetup::new(video.clone());
    let handler = Arc::new(core::with_show(
        serve::handler_with_video(
            sched.clone(),
            MAIN_QUEUE.to_owned(),
            cancel.clone(),
            Arc::new(AtomicBool::new(false)),
            APP_GUI,
            Some(video.clone()),
        ),
        show.clone(),
    ));
    let running = match eid_ipc::start(&ep, handler, ServeOptions::default()) {
        Ok(r) => r,
        Err(StartError::AlreadyRunning) => return Ok(already_open(&ep).await),
        Err(e) => return Err(e.to_string()),
    };
    let (s2, c2) = (sched.clone(), cancel.clone());
    let task = tauri::async_runtime::spawn(async move {
        let r = s2.run(c2, false).await;
        let first = s2.flush().await;
        running.stop().await;
        r.map_err(|e| e.to_string())?;
        first.map_err(|e| e.to_string())
    });
    Ok(Boot::Owner(AppState {
        sched,
        progress,
        cancel,
        default_dir,
        show,
        video,
        setup,
        list_cache: Mutex::new(ListCache::default()),
        settings: Mutex::new(settings),
        tray_texts: Mutex::new(crate::desktop::TrayTexts::default()),
        settings_file,
        limiter,
        connections,
        power,
        power_view,
        task: Mutex::new(Some(task)),
    }))
}

impl AppState {
    /// Stop the downloads (they stay resumable) and write the queue. Called once, on exit.
    pub fn shutdown_blocking(&self) {
        self.cancel.cancel();
        // video children stop like a pause (their partial data is kept for the next run)
        self.setup.stop();
        let v = self.video.clone();
        let _ = tauri::async_runtime::block_on(async move {
            tokio::time::timeout(Duration::from_secs(16), v.shutdown()).await
        });
        let task = self.task.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(t) = task {
            let _ = tauri::async_runtime::block_on(async {
                tokio::time::timeout(Duration::from_secs(10), t).await
            });
        }
    }
}

/// What `list` answers (docs/V0.1-CONTRACT.md): with `since` = the `rev` the window holds,
/// only the rows that changed and the ids that are gone; otherwise (`full`) every row.
#[derive(Serialize)]
pub struct Snapshot {
    rev: u64,
    full: bool,
    items: Vec<ItemView>,
    removed: Vec<u64>,
    default_dir: String,
    /// v0.2: the "when finished" countdown and its last outcome.
    power: crate::power::PowerView,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[tauri::command]
pub fn list(st: State<'_, AppState>, since: Option<u64>) -> Snapshot {
    let rows = core::view(
        &st.sched.snapshot(),
        &st.progress,
        Instant::now(),
        unix_now(),
    );
    let d = st
        .list_cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .update(rows, |r| r.id, since);
    Snapshot {
        rev: d.rev,
        full: d.full,
        items: d.items,
        removed: d.removed,
        default_dir: st.default_dir.clone(),
        power: {
            // the outcome message is sent once
            let mut g = st.power_view.lock().unwrap_or_else(|p| p.into_inner());
            let v = g.clone();
            g.last = None;
            v
        },
    }
}

#[tauri::command]
pub fn add(
    st: State<'_, AppState>,
    links: String,
    folder: Option<String>,
    name: Option<String>,
) -> Result<AddReport, String> {
    let folder = folder.filter(|f| !f.is_empty() && *f != st.default_dir);
    core::add_links(&st.sched, &links, folder.as_deref(), name.as_deref())
}

#[tauri::command]
pub fn pause(st: State<'_, AppState>, id: u64) -> Result<(), String> {
    st.sched.pause_item(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn resume(st: State<'_, AppState>, id: u64) -> Result<(), String> {
    st.sched.resume_item(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove(st: State<'_, AppState>, id: u64) -> Result<(), String> {
    core::remove_item(&st.sched, id)?;
    st.progress.forget(id);
    Ok(())
}

#[tauri::command]
pub fn clear_finished(st: State<'_, AppState>) -> Result<usize, String> {
    core::clear_finished(&st.sched)
}

#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |p| {
        let _ = tx.send(p.and_then(|p| p.into_path().ok()));
    });
    rx.await
        .ok()
        .flatten()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Show a finished file in Explorer (it is never opened or run).
#[tauri::command]
pub fn reveal(app: AppHandle, st: State<'_, AppState>, id: u64) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let path = core::finished_file(&st.sched.snapshot(), id).ok_or("not_finished")?;
    if !path.is_file() {
        return Err("file_missing".into());
    }
    app.opener()
        .reveal_item_in_dir(path)
        .map_err(|e| e.to_string())
}

/// The segment map of one download for the details window (`None`: no such item). Reads the
/// download's state file read-only, off the UI thread.
#[tauri::command]
pub async fn segments(
    st: State<'_, AppState>,
    id: u64,
) -> Result<Option<crate::segmap::SegmentMap>, String> {
    let qs = st.sched.snapshot();
    let live = st
        .progress
        .rate(id, true, Instant::now())
        .map(|r| (r.done, r.total));
    tauri::async_runtime::spawn_blocking(move || crate::segmap::for_item(&qs, id, live))
        .await
        .map_err(|e| e.to_string())
}

/// v0.2: the queues (main first) with their counts and settings.
#[tauri::command]
pub fn queues(st: State<'_, AppState>) -> Vec<core::QueueView> {
    core::queues_view(&st.sched.snapshot())
}

/// v0.2: change one queue (simultaneous downloads, stop/start, "when finished"). Stopping a
/// queue pauses its running downloads (they stay resumable); starting it resumes its paused ones.
#[tauri::command]
pub fn queue_set(st: State<'_, AppState>, patch: core::QueuePatch) -> Result<(), String> {
    let (stop, start) = st
        .sched
        .edit(|qs| {
            let allowed = st
                .settings
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .power_actions;
            core::apply_queue_patch(qs, &patch, allowed)
        })
        .map_err(|e| e.to_string())?;
    // the main queue's simultaneous downloads is also the Settings value: keep them equal
    if patch.name == MAIN_QUEUE {
        if let Some(n) = patch.max_parallel {
            let mut s = st.settings.lock().unwrap_or_else(|p| p.into_inner());
            s.max_parallel = n;
            if let Some(f) = &st.settings_file {
                gui_settings::save(f, &s)?;
            }
        }
    }
    for id in stop {
        let _ = st.sched.pause_item(id);
    }
    for id in start {
        let _ = st.sched.resume_item(id);
    }
    Ok(())
}

/// v0.2: one download's own speed limit in KiB/s (`null` = none). A running download is
/// paused and resumed so the new limit applies at once (it continues where it was).
#[tauri::command]
pub fn item_limit(st: State<'_, AppState>, id: u64, limit_kib: Option<u64>) -> Result<(), String> {
    if limit_kib.is_some_and(|l| l == 0 || l > eid_engine::queue::MAX_ITEM_LIMIT_KIB) {
        return Err("invalid input: speed limit".into());
    }
    let running = st
        .sched
        .edit(|qs| {
            let it = qs
                .queues
                .iter_mut()
                .flat_map(|q| q.items.iter_mut())
                .find(|i| i.id == id)
                .ok_or("no_such_item")?;
            it.limit_kib = limit_kib;
            Ok(it.status == eid_engine::queue::Status::Running)
        })
        .map_err(|e| e.to_string())?;
    if running {
        st.sched.pause_item(id).map_err(|e| e.to_string())?;
        st.sched.resume_item(id).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// v0.2: stop the "when finished" countdown (the choice is reset, nothing happens).
#[tauri::command]
pub fn cancel_countdown(st: State<'_, AppState>) -> bool {
    st.sched.cancel_countdown()
}

/// v0.1: the download settings the window shows.
#[tauri::command]
pub fn settings_get(st: State<'_, AppState>) -> GuiSettings {
    st.settings
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// v0.1: validate, save and apply new download settings at once (speed limit and
/// simultaneous downloads take effect immediately, connections for downloads that start next).
#[tauri::command]
pub fn settings_set(
    app: AppHandle,
    st: State<'_, AppState>,
    settings: GuiSettings,
) -> Result<GuiSettings, String> {
    settings.validate()?;
    if let Some(f) = &st.settings_file {
        gui_settings::save(f, &settings)?;
    }
    st.sched
        .edit(|qs| {
            settings.apply_to_queues(qs, MAIN_QUEUE)?;
            // computer actions turned off: no queue may keep one
            if !settings.power_actions {
                for q in &mut qs.queues {
                    if core::is_machine_action(q.after_done) {
                        q.after_done = eid_engine::queue::PowerAction::None;
                    }
                }
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    st.power
        .allowed
        .store(settings.power_actions, Ordering::Relaxed);
    st.limiter.set_rate(settings.limit_bps());
    st.connections
        .store(settings.connections as usize, Ordering::Relaxed);
    *st.settings.lock().unwrap_or_else(|p| p.into_inner()) = settings.clone();
    // v0.2: the tray icon follows the setting at once (clipboard monitoring reads it live)
    crate::desktop::set_tray(&app, settings.tray)?;
    Ok(settings)
}

/// v0.2: the tray menu in the window's language.
#[tauri::command]
pub fn tray_texts(app: AppHandle, texts: crate::desktop::TrayTexts) -> Result<(), String> {
    crate::desktop::retext_tray(&app, texts)
}

/// v0.1: open a finished file with its default program. Programs and scripts are never
/// started from here (they are only shown in their folder): `refused_executable`.
#[tauri::command]
pub fn open_file(app: AppHandle, st: State<'_, AppState>, id: u64) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let path = core::finished_file(&st.sched.snapshot(), id).ok_or("not_finished")?;
    if !path.is_file() {
        return Err("file_missing".into());
    }
    if core::is_executable_name(&path) {
        // Never run a downloaded program from here: show it in its folder instead.
        let _ = app.opener().reveal_item_in_dir(&path);
        return Err("refused_executable".into());
    }
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Stop the app when an `eid stop` arrives over IPC.
pub fn exit_on_cancel(app: &AppHandle) {
    let st = app.state::<AppState>();
    let (c, h) = (st.cancel.clone(), app.clone());
    tauri::async_runtime::spawn(async move {
        c.cancelled().await;
        h.exit(0);
    });
}

/// Bring the window forward each time a second launch of the app asks for it.
pub fn focus_on_show(app: &AppHandle) {
    let st = app.state::<AppState>();
    let (show, h) = (st.show.clone(), app.clone());
    tauri::async_runtime::spawn(async move {
        loop {
            show.notified().await;
            if let Some(w) = h.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
    });
}
