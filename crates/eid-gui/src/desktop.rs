//! v0.2 desktop integration: the notification-area (tray) icon, close-to-tray and clipboard
//! monitoring. All three are switched in Settings (`GuiSettings::{tray, close_to_tray,
//! clipboard}`); the icon is on by default, the other two are off.
//!
//! Lightness: the icon is one native menu (no extra window or process). The clipboard watcher
//! wakes once a second and only asks Windows for the clipboard's change counter; the text is
//! read only after a change, at most 4096 characters, kept in memory just long enough to find
//! one link in it, and never written anywhere.

use crate::app::AppState;
use serde::Deserialize;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, UserAttentionType};

pub const TRAY_ID: &str = "eid-tray";
const MAX_TEXT: usize = 64;

/// Menu texts of the icon, in the window's language (sent by the page).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrayTexts {
    pub show: String,
    pub pause_all: String,
    pub resume_all: String,
    pub quit: String,
}

impl Default for TrayTexts {
    fn default() -> Self {
        TrayTexts {
            show: "إظهار عيد للتحميل".into(),
            pause_all: "إيقاف الكل".into(),
            resume_all: "استكمال الكل".into(),
            quit: "خروج".into(),
        }
    }
}

impl TrayTexts {
    /// Short, single-line, no control or direction-override characters (they come from the
    /// page; a menu entry is never a place for them).
    pub fn cleaned(self) -> Result<TrayTexts, String> {
        let c = |s: String| -> Result<String, String> {
            let t: String = s
                .chars()
                .filter(|c| {
                    !c.is_control()
                        && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
                .take(MAX_TEXT)
                .collect();
            let t = t.trim().to_string();
            if t.is_empty() {
                Err("empty menu text".into())
            } else {
                Ok(t)
            }
        };
        Ok(TrayTexts {
            show: c(self.show)?,
            pause_all: c(self.pause_all)?,
            resume_all: c(self.resume_all)?,
            quit: c(self.quit)?,
        })
    }
}

/// Bring the main window back (from the tray, minimized or behind other windows).
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Show or remove the icon (idempotent). Texts come from [`AppState::tray_texts`].
pub fn set_tray(app: &AppHandle, on: bool) -> Result<(), String> {
    if !on {
        let _ = app.remove_tray_by_id(TRAY_ID);
        return Ok(());
    }
    if app.tray_by_id(TRAY_ID).is_some() {
        return Ok(());
    }
    let texts = app
        .state::<AppState>()
        .tray_texts
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let e = |x: tauri::Error| x.to_string();
    let show = MenuItem::with_id(app, "show", &texts.show, true, None::<&str>).map_err(e)?;
    let pause =
        MenuItem::with_id(app, "pause_all", &texts.pause_all, true, None::<&str>).map_err(e)?;
    let resume =
        MenuItem::with_id(app, "resume_all", &texts.resume_all, true, None::<&str>).map_err(e)?;
    let quit = MenuItem::with_id(app, "quit", &texts.quit, true, None::<&str>).map_err(e)?;
    let s1 = PredefinedMenuItem::separator(app).map_err(e)?;
    let s2 = PredefinedMenuItem::separator(app).map_err(e)?;
    let menu = Menu::with_items(app, &[&show, &s1, &pause, &resume, &s2, &quit]).map_err(e)?;
    let mut b = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Eid Download")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id().as_ref() {
            "show" => show_main(app),
            // the page owns these commands (the same as its menu items)
            a @ ("pause_all" | "resume_all") => {
                let _ = app.emit("tray", a);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.build(app).map(|_| ()).map_err(e)
}

/// New menu texts: stored, and the icon rebuilt if it is shown.
pub fn retext_tray(app: &AppHandle, texts: TrayTexts) -> Result<(), String> {
    let texts = texts.cleaned()?;
    let st = app.state::<AppState>();
    *st.tray_texts.lock().unwrap_or_else(|p| p.into_inner()) = texts;
    if app.tray_by_id(TRAY_ID).is_some() {
        let _ = app.remove_tray_by_id(TRAY_ID);
        set_tray(app, true)?;
    }
    Ok(())
}

/// Whether closing the window should hide it instead (the icon must be there to bring it back).
pub fn hide_on_close(app: &AppHandle) -> bool {
    let st = app.state::<AppState>();
    let s = st.settings.lock().unwrap_or_else(|p| p.into_inner());
    s.tray && s.close_to_tray && app.tray_by_id(TRAY_ID).is_some()
}

/// The clipboard watcher (runs for the life of the app; idle while the setting is off).
/// A link that was already on the clipboard when monitoring was switched on is not offered.
pub fn watch_clipboard(app: &AppHandle) {
    let h = app.clone();
    let cancel = app.state::<AppState>().cancel.clone();
    tauri::async_runtime::spawn(async move {
        let mut last_seq: Option<u32> = None;
        let mut last_link: Option<String> = None;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            let on = h
                .state::<AppState>()
                .settings
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clipboard;
            if !on {
                last_seq = None;
                continue;
            }
            let Ok(seq) =
                tauri::async_runtime::spawn_blocking(eid_platform::clipboard::sequence).await
            else {
                continue;
            };
            let Some(seq) = seq else { continue };
            let changed = last_seq.is_some_and(|l| l != seq);
            last_seq = Some(seq);
            if !changed {
                continue;
            }
            let Ok(Some(text)) =
                tauri::async_runtime::spawn_blocking(|| eid_platform::clipboard::read_text(4096))
                    .await
            else {
                continue;
            };
            let Some((url, kind)) = crate::core::clipboard_link(&text) else {
                continue;
            };
            if last_link.as_deref() == Some(url.as_str()) {
                continue;
            }
            last_link = Some(url.clone());
            let _ = h.emit(
                "clipboard-link",
                serde_json::json!({ "url": url, "kind": kind }),
            );
            if let Some(w) = h.get_webview_window("main") {
                if !w.is_focused().unwrap_or(false) {
                    let _ = w.request_user_attention(Some(UserAttentionType::Informational));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_texts_are_short_single_line_and_never_empty() {
        let t = TrayTexts {
            show: "  Show\nEid\u{202e}  ".into(),
            pause_all: "x".repeat(500),
            resume_all: "استكمال الكل".into(),
            quit: "Quit".into(),
        }
        .cleaned()
        .unwrap();
        assert_eq!(t.show, "ShowEid");
        assert_eq!(t.pause_all.chars().count(), MAX_TEXT);
        assert_eq!(t.resume_all, "استكمال الكل");
        let empty = TrayTexts {
            quit: " \u{202e} ".into(),
            ..TrayTexts::default()
        };
        assert!(empty.cleaned().is_err());
        assert!(TrayTexts::default().cleaned().is_ok());
    }
}
