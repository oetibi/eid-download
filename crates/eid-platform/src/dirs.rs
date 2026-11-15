//! Per-user default locations (environment based; no registry or shell-folder lookups yet, so a
//! redirected Downloads folder on Windows is not honoured — MANUAL-TESTS).

use std::path::PathBuf;

pub(crate) fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// `%USERPROFILE%\Downloads` on Windows, `$HOME/Downloads` elsewhere. `None` if the base is
/// unknown or not an absolute path.
pub fn default_download_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = env_dir("USERPROFILE");
    #[cfg(not(windows))]
    let base = env_dir("HOME");
    base.map(|b| b.join("Downloads"))
}

/// Per-user local data directory: `%LOCALAPPDATA%\EidDownload` on Windows (video tools live in
/// its `tools` sub-folder, ARCHITECTURE section 6), `$XDG_DATA_HOME/eid-download` (or
/// `~/.local/share/eid-download`) elsewhere. `None` if the base is unknown or not absolute.
pub fn local_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        env_dir("LOCALAPPDATA").map(|p| p.join("EidDownload"))
    }
    #[cfg(not(windows))]
    {
        let base = env_dir("XDG_DATA_HOME")
            .or_else(|| env_dir("HOME").map(|h| h.join(".local").join("share")))?;
        Some(base.join("eid-download"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_or_empty_environment_directory_is_ignored() {
        // a variable name nothing else reads, so no other test can race with it
        let var = format!("EID_TEST_DIR_{}", std::process::id());
        // SAFETY: only this test touches this uniquely named variable.
        unsafe {
            std::env::set_var(&var, "relative/dir");
            assert_eq!(env_dir(&var), None);
            std::env::set_var(&var, "");
            assert_eq!(env_dir(&var), None);
            let abs = std::env::temp_dir();
            std::env::set_var(&var, &abs);
            assert_eq!(env_dir(&var), Some(abs));
            std::env::remove_var(&var);
        }
        assert_eq!(env_dir(&var), None);
    }

    #[test]
    fn the_local_data_dir_is_absolute_and_ours() {
        if let Some(d) = local_data_dir() {
            assert!(d.is_absolute());
            let n = d.file_name().unwrap().to_string_lossy().to_ascii_lowercase();
            assert!(n == "eiddownload" || n == "eid-download", "{n}");
        }
    }

    #[test]
    fn the_default_download_dir_is_absolute_and_named_downloads() {
        if let Some(d) = default_download_dir() {
            assert!(d.is_absolute());
            assert_eq!(d.file_name().unwrap(), "Downloads");
        }
    }
}
