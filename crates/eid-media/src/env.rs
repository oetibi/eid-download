//! The child environment of yt-dlp: an allowlist (ADR-0003 2026-11-15 amendment).
//!
//! Python honours many variables (`PYTHONPATH`, `PYTHONHOME`, `PYTHONSTARTUP`, ...) that would
//! let any program which can set our environment run code inside the tool; yt-dlp itself reads
//! more. Nothing is inherited: the child gets the OS essentials, a `PATH` made of our tools
//! directory and the system directory only, `YTDLP_NO_PLUGINS=1`, and proxy settings.

use std::ffi::OsString;
use std::path::Path;

/// Proxy variables passed through when set (both spellings; on Windows names are
/// case-insensitive anyway).
pub const PROXY_VARS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

/// Build the complete child environment. `lookup` reads a variable of our own environment
/// (`std::env::var_os` in production; a fake map in tests).
pub fn child_env(
    tools_dir: &Path,
    lookup: impl Fn(&str) -> Option<OsString>,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = vec![];
    let mut push = |k: &str, v: OsString| {
        let dup = env
            .iter()
            .any(|(e, _)| e.to_string_lossy().eq_ignore_ascii_case(k) && cfg_case_insensitive());
        if !dup {
            env.push((k.into(), v));
        }
    };
    for k in eid_platform::process::ENV_PASSTHROUGH {
        if let Some(v) = lookup(k) {
            push(k, v);
        }
    }
    let mut dirs = vec![tools_dir.to_path_buf()];
    dirs.extend(eid_platform::process::system_program_dirs());
    let path = std::env::join_paths(dirs).unwrap_or_else(|_| tools_dir.as_os_str().to_owned());
    push("PATH", path);
    push("YTDLP_NO_PLUGINS", "1".into());
    for k in PROXY_VARS {
        if let Some(v) = lookup(k) {
            push(k, v);
        }
    }
    env
}

/// Environment variable names are case-insensitive on Windows, so `HTTP_PROXY` and
/// `http_proxy` are one variable there and only the first is kept.
fn cfg_case_insensitive() -> bool {
    eid_platform::IS_WINDOWS
}

/// The names [`child_env`] may ever produce (for the allowlist test).
pub fn allowed_names() -> Vec<&'static str> {
    let mut v: Vec<&str> = eid_platform::process::ENV_PASSTHROUGH.to_vec();
    v.extend(["PATH", "YTDLP_NO_PLUGINS"]);
    v.extend(PROXY_VARS);
    v
}
