// A window app: no console window behind it (the attribute is ignored on other systems).
#![windows_subsystem = "windows"]

use eid_gui::app::{self, AppState, Boot};
use eid_gui::video;
use eid_platform::ui::{fatal_message, fatal_text};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};

const TITLE: &str = "Eid Download";

/// `--endpoint NAME`: talk on another per-user endpoint than "main" (like `eid serve`).
fn endpoint_arg() -> Option<String> {
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "--endpoint" {
            return it.next();
        }
    }
    None
}

fn main() {
    let state = match tauri::async_runtime::block_on(app::boot(endpoint_arg())) {
        Ok(Boot::Owner(state)) => state,
        // the open window came to the front: nothing else to show
        Ok(Boot::AlreadyOpen { shown: true }) => return,
        Ok(Boot::AlreadyOpen { shown: false }) => {
            fatal_message(
                TITLE,
                "Eid Download مفتوح بالفعل.\nEid Download is already open.",
            );
            return;
        }
        Err(e) => {
            fatal_message(TITLE, &fatal_text(&e));
            std::process::exit(1);
        }
    };
    let mut state = Some(state);
    let built = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            app::list,
            app::add,
            app::pause,
            app::resume,
            app::remove,
            app::clear_finished,
            app::pick_folder,
            app::reveal,
            app::open_file,
            app::segments,
            app::tray_texts,
            app::queues,
            app::queue_set,
            app::item_limit,
            app::cancel_countdown,
            app::categories,
            app::import_list,
            app::export_list,
            app::settings_get,
            app::settings_set,
            video::video_probe,
            video::tools_status,
            video::tools_consent_text,
            video::tools_install,
            video::video_add,
            video::playlist_add,
            video::video_jobs,
            video::video_pause,
            video::video_resume,
            video::video_retry,
            video::video_remove,
        ])
        .setup(move |a| {
            a.manage(state.take().expect("setup runs once"));
            let h = a.handle().clone();
            app::exit_on_cancel(&h);
            let window = WebviewWindowBuilder::new(a, "main", WebviewUrl::App("index.html".into()))
                .title("Eid Download — عيد للتحميل")
                .inner_size(1040.0, 680.0)
                .min_inner_size(720.0, 460.0)
                // links dragged onto the window reach the page as HTML drag-and-drop events
                // (the page reads only http/https text; dropped files are refused there)
                .disable_drag_drop_handler()
                .build();
            match window {
                Ok(_) => {
                    app::focus_on_show(&h);
                    // v0.2: "quit when finished" needs the app handle
                    let _ = h.state::<AppState>().power.app.set(h.clone());
                    // v0.2: tray icon (if on) and the clipboard watcher (idle unless on)
                    let tray_on = h
                        .state::<AppState>()
                        .settings
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .tray;
                    // an icon that cannot be created is not fatal: the window works without it
                    let _ = eid_gui::desktop::set_tray(&h, tray_on);
                    eid_gui::desktop::watch_clipboard(&h);
                }
                // e.g. WebView2 missing: say so, then exit through RunEvent::Exit, which stops
                // the downloads cleanly and writes the queue
                Err(e) => {
                    fatal_message(TITLE, &fatal_text(&e.to_string()));
                    h.exit(1);
                }
            }
            Ok(())
        })
        .on_window_event(|w, ev| {
            // v0.2 close to tray: hide instead of quitting (only while the icon is there)
            if let WindowEvent::CloseRequested { api, .. } = ev {
                if w.label() == "main" && eid_gui::desktop::hide_on_close(w.app_handle()) {
                    api.prevent_close();
                    let _ = w.hide();
                }
            }
        })
        .build(tauri::generate_context!());
    let built = match built {
        Ok(b) => b,
        Err(e) => {
            fatal_message(TITLE, &fatal_text(&e.to_string()));
            std::process::exit(1);
        }
    };
    built.run(|h, ev| {
        if let RunEvent::Exit = ev {
            if let Some(st) = h.try_state::<AppState>() {
                st.shutdown_blocking();
            }
        }
    });
}
