//! The desktop app process: owns the queue scheduler and the per-user IPC endpoint (so `eid add`
//! and the browser's native host hand their links to this window), and exposes the commands the
//! window calls.

use crate::core::{self, AddReport, ItemView, Progress, MAIN_QUEUE};
use crate::listdiff::ListCache;
use eid_cli::media_jobs::VideoJobs;
use eid_cli::remote::ConnArgs;
use eid_cli::serve::{self, ServeArgs, APP_GUI};
use eid_cli::video_setup::VideoSetup;
use eid_engine::queue::Queue;
use eid_engine::queue_runner::EngineRunner;
use eid_engine::scheduler::{NoPower, Scheduler, SchedulerConfig, SystemClock};
use eid_engine::CancellationToken;
use eid_ipc::{Client, ClientError, Command, Endpoint, ServeOptions, StartError};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
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
    task: Mutex<Option<tauri::async_runtime::JoinHandle<Result<(), String>>>>,
}

/// How start-up ended.
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
    let runner = EngineRunner {
        cfg: serve::config_for(&ServeArgs::default()),
        progress: Some({
            let p = progress.clone();
            Arc::new(move |item, done, total| p.record(item, done, total, Instant::now()))
        }),
    };
    let sched = Scheduler::open(
        SchedulerConfig::new(&qfile),
        Arc::new(runner),
        Arc::new(SystemClock),
        Arc::new(NoPower),
        Arc::new(|_| {}),
    )
    .map_err(|e| e.to_string())?;
    let main_dir = default_dir.clone();
    sched
        .edit(|qs| {
            if qs.queue_mut(MAIN_QUEUE).is_none() {
                qs.queues.push(Queue::new(MAIN_QUEUE, &main_dir));
            }
            Ok(())
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
