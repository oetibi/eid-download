//! v0.1: the window's video and playlist commands (docs/V0.1-CONTRACT.md). The work is done by
//! `eid_cli::media_jobs` (child `eid video ...` processes, tested in eid-cli) and, for the
//! one-time agreement and tool install, `eid_cli::video_setup`. Nothing here blocks the window:
//! the probe is async and every other command only touches the in-memory job list.

use crate::app::AppState;
use eid_cli::media_jobs::Kind;
use serde_json::{json, Value};
use std::path::PathBuf;
use tauri::State;

/// A folder the window chose (from the folder picker) or the default one (`None` / empty).
fn folder(st: &AppState, dir: Option<String>) -> Option<PathBuf> {
    dir.filter(|d| !d.is_empty() && *d != st.default_dir)
        .map(PathBuf::from)
}

#[tauri::command]
pub async fn video_probe(st: State<'_, AppState>, url: String) -> Result<Value, String> {
    let v = st.video.clone();
    v.probe(url.trim()).await
}

#[tauri::command]
pub fn tools_status(st: State<'_, AppState>) -> Value {
    st.setup.status()
}

#[tauri::command]
pub fn tools_consent_text(st: State<'_, AppState>, lang: String) -> Result<String, String> {
    st.setup.consent_text(&lang)
}

#[tauri::command]
pub fn tools_install(st: State<'_, AppState>, accept: bool) -> Result<(), String> {
    st.setup.install(accept)
}

#[tauri::command]
pub fn video_add(
    st: State<'_, AppState>,
    url: String,
    quality: String,
    dir: Option<String>,
) -> Result<Value, String> {
    let d = folder(&st, dir);
    let job = st
        .video
        .add(Kind::Video, url.trim(), &quality, None, d.as_deref())?;
    Ok(json!({ "job": job }))
}

#[tauri::command]
pub fn playlist_add(
    st: State<'_, AppState>,
    url: String,
    quality: String,
    items: Option<Vec<u32>>,
    dir: Option<String>,
) -> Result<Value, String> {
    let d = folder(&st, dir);
    let job = st
        .video
        .add(Kind::Playlist, url.trim(), &quality, items, d.as_deref())?;
    Ok(json!({ "job": job }))
}

#[tauri::command]
pub fn video_jobs(st: State<'_, AppState>) -> Vec<Value> {
    st.video.list()
}

#[tauri::command]
pub fn video_pause(st: State<'_, AppState>, job: u64) -> Result<(), String> {
    st.video.pause(job)
}

#[tauri::command]
pub fn video_resume(st: State<'_, AppState>, job: u64) -> Result<(), String> {
    st.video.resume(job)
}

#[tauri::command]
pub fn video_retry(st: State<'_, AppState>, job: u64) -> Result<(), String> {
    st.video.retry(job)
}

#[tauri::command]
pub fn video_remove(st: State<'_, AppState>, job: u64) -> Result<(), String> {
    st.video.remove(job)
}
