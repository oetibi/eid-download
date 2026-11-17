//! The child environment of yt-dlp: an allowlist (ADR-0003 2026-11-15 amendment).
//!
//! Python honours many variables (`PYTHONPATH`, `PYTHONHOME`, `PYTHONSTARTUP`, ...) that would
//! let any program which can set our environment run code inside the tool; yt-dlp itself reads
//! more. Nothing is inherited: the child gets the OS essentials, a `PATH` made of our tools
//! directory and the system directory only, `YTDLP_NO_PLUGINS=1`, `DENO_DIR` (our own folder,
//! see [`DENO_CACHE_DIR`]), and proxy settings.

use std::ffi::OsString;
use std::path::Path;

/// Deno's cache folder (`DENO_DIR`), next to the tool sets: `<tools root>/deno-cache`.
///
/// Deno (run by yt-dlp for YouTube's JavaScript challenge) writes its caches to `DENO_DIR`,
/// by default `%LOCALAPPDATA%\deno`, and gives itself a second name there:
/// `node_compat_bin/node.exe`, a hard link to `deno.exe`. Outside our folders, that link made
/// every later tool lock refuse `deno.exe` (D11), and withdrawing consent did not remove the
/// cache (found by the real-tool tests, STATUS 2026-11-17). With `DENO_DIR` here, both stay
/// in our folder: [`crate::tools::ToolStore::lock`] removes the link before every lock, and
/// [`crate::tools::ToolStore::remove_all`] deletes the cache with the tools.
pub const DENO_CACHE_DIR: &str = "deno-cache";

/// `DENO_DIR` for the tools in `tools_dir` (a set folder directly under the tools root).
pub fn deno_dir(tools_dir: &Path) -> std::path::PathBuf {
    tools_dir.parent().unwrap_or(tools_dir).join(DENO_CACHE_DIR)
}

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
    // always ours, never inherited (the parent's DENO_DIR is not looked up)
    push("DENO_DIR", deno_dir(tools_dir).into_os_string());
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
    v.extend(["PATH", "YTDLP_NO_PLUGINS", "DENO_DIR"]);
    v.extend(PROXY_VARS);
    v
}
