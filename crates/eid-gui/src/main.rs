// A window app: no console window behind it (the attribute is ignored on other systems).
#![windows_subsystem = "windows"]

use eid_gui::app::{self, AppState, Boot};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

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
    let boot = tauri::async_runtime::block_on(app::boot(endpoint_arg()));
    let mut boot = Some(boot);
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
        ])
        .setup(move |a| {
            let h = a.handle().clone();
            let notice = |text: String, kind: MessageDialogKind| {
                let h2 = h.clone();
                h.dialog()
                    .message(text)
                    .title("Eid Download")
                    .kind(kind)
                    .show(move |_| h2.exit(0));
            };
            match boot.take().expect("setup runs once") {
                Ok(Boot::Owner(state)) => {
                    a.manage(state);
                    app::exit_on_cancel(&h);
                    WebviewWindowBuilder::new(a, "main", WebviewUrl::App("index.html".into()))
                        .title("Eid Download — عيد للتحميل")
                        .inner_size(1040.0, 680.0)
                        .min_inner_size(720.0, 460.0)
                        .build()?;
                }
                Ok(Boot::AlreadyOpen) => notice(
                    "Eid Download مفتوح بالفعل.\nEid Download is already open.".into(),
                    MessageDialogKind::Info,
                ),
                Err(e) => notice(
                    format!("تعذّر تشغيل Eid Download.\nEid Download could not start:\n{e}"),
                    MessageDialogKind::Error,
                ),
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("cannot start the window system (is Microsoft Edge WebView2 installed?)");
    built.run(|h, ev| {
        if let RunEvent::Exit = ev {
            if let Some(st) = h.try_state::<AppState>() {
                st.shutdown_blocking();
            }
        }
    });
}
