//! Per-user default locations (environment based; no registry or shell-folder lookups yet, so a
//! redirected Downloads folder on Windows is not honoured — MANUAL-TESTS).

use std::path::PathBuf;

fn env_dir(var: &str) -> Option<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_download_dir_is_absolute_and_named_downloads() {
        if let Some(d) = default_download_dir() {
            assert!(d.is_absolute());
            assert_eq!(d.file_name().unwrap(), "Downloads");
        }
    }
}
