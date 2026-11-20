//! v0.2: connect the browser extension to THIS copy of the app from Settings (owner's Windows
//! test: the portable ZIP registered nothing, so the extension could never reach the app; only
//! the installer wrote the Native Messaging keys).
//!
//! "Connect" writes the three manifests (absolute path of the `eid-native-host` next to this
//! app) into the per-user data folder and points Chrome, Edge and Firefox at them (HKCU on
//! Windows, never admin). "Disconnect" removes only registrations that point at THIS copy: an
//! installed copy's registration is never removed from a portable one. The extension IDs are
//! the ones the installer uses (`extension/native-host-ids.json`, built in). Tauri-free.

use eid_platform::native_host::{self, Browser, Manifest};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const HOST_NAME: &str = "com.eid.download";
const IDS: &str = include_str!("../../../extension/native-host-ids.json");
const ALL: [Browser; 4] = [
    Browser::Chrome,
    Browser::Edge,
    Browser::Firefox,
    Browser::Brave,
];

fn tag(b: Browser) -> &'static str {
    match b {
        Browser::Chrome => "chrome",
        Browser::Edge => "edge",
        Browser::Brave => "brave",
        Browser::Firefox => "firefox",
    }
}

/// One browser's state for the window: `this` (connected to this copy), `other` (to another
/// copy, e.g. the installed one; `host` says which), `none`, or `error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkState {
    pub browser: &'static str,
    pub state: &'static str,
    pub host: Option<String>,
}

/// The native host next to `app`.
pub fn host_exe(app: &Path) -> PathBuf {
    app.with_file_name(format!("eid-native-host{}", std::env::consts::EXE_SUFFIX))
}

/// The manifest for this copy (IDs from the built-in list; Edge uses the Chrome-style IDs).
pub fn manifest_for(app: &Path, name: &str) -> Result<Manifest, String> {
    let v: serde_json::Value = serde_json::from_str(IDS).map_err(|e| e.to_string())?;
    if v["placeholder"] != false {
        return Err("this build has no extension IDs".into());
    }
    let list = |k: &str| -> Vec<String> {
        v[k].as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut chrome = list("chrome");
    for id in list("edge") {
        if !chrome.contains(&id) {
            chrome.push(id);
        }
    }
    let exe = std::path::absolute(host_exe(app)).map_err(|e| e.to_string())?;
    let m = Manifest {
        name: name.to_owned(),
        description: "Eid Download native messaging host".into(),
        exe,
        chrome_extension_ids: chrome,
        firefox_extension_ids: list("firefox"),
    };
    m.validate().map_err(|e| e.to_string())?;
    Ok(m)
}

fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        // Windows paths are case-insensitive; elsewhere this is only used for display decisions
        if eid_platform::IS_WINDOWS {
            p.to_string_lossy().to_lowercase()
        } else {
            p.to_string_lossy().into_owned()
        }
    };
    norm(a) == norm(b)
}

/// Where each browser's registration points, compared with this copy.
pub fn status(app: &Path, name: &str) -> Vec<LinkState> {
    let mine = host_exe(app);
    ALL.iter()
        .map(|b| match native_host::registered_host(*b, name) {
            Ok(None) => LinkState {
                browser: tag(*b),
                state: "none",
                host: None,
            },
            Ok(Some(h)) => LinkState {
                browser: tag(*b),
                state: if same_file(&h, &mine) {
                    "this"
                } else {
                    "other"
                },
                host: Some(h.to_string_lossy().into_owned()),
            },
            Err(e) => LinkState {
                browser: tag(*b),
                state: "error",
                host: Some(e.to_string()),
            },
        })
        .collect()
}

/// Connect all three browsers to this copy. Refused (nothing written) when this copy cannot
/// serve the browser: started from inside a ZIP, or no native host next to it.
pub fn connect(app: &Path, name: &str, data_dir: &Path) -> Result<Vec<LinkState>, String> {
    let problems = crate::core::install_problems(app, &|p| p.is_file());
    if let Some(p) = problems.first() {
        return Err((*p).to_owned());
    }
    let m = manifest_for(app, name)?;
    native_host::register(&m, &ALL, &data_dir.join("native-messaging"))
        .map_err(|e| e.to_string())?;
    Ok(status(app, name))
}

/// Remove the registrations that point at THIS copy; others are left alone.
pub fn disconnect(app: &Path, name: &str) -> Result<Vec<LinkState>, String> {
    let mine: Vec<Browser> = ALL
        .iter()
        .zip(status(app, name))
        .filter(|(_, s)| s.state == "this")
        .map(|(b, _)| *b)
        .collect();
    if !mine.is_empty() {
        let m = Manifest {
            name: name.to_owned(),
            description: String::new(),
            exe: std::path::absolute(host_exe(app)).map_err(|e| e.to_string())?,
            chrome_extension_ids: vec![],
            firefox_extension_ids: vec![],
        };
        native_host::unregister(&m, &mine).map_err(|e| e.to_string())?;
    }
    Ok(status(app, name))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_built_in_ids_make_a_valid_manifest_for_every_browser() {
        let d = tempfile::tempdir().unwrap();
        let m = super::manifest_for(&d.path().join("EidDownload.exe"), super::HOST_NAME).unwrap();
        assert!(!m.chrome_extension_ids.is_empty() && !m.firefox_extension_ids.is_empty());
        assert!(m.exe.is_absolute() && m.exe.starts_with(d.path()));
        for b in super::ALL {
            m.json(b).unwrap();
        }
    }
}
