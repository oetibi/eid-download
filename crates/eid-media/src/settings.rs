//! `settings.json`: a private file in the per-user config folder (P4 plan section 3). For now
//! it holds only the video consent record; the UI settings join it in P5.

use crate::MediaError;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Largest settings file accepted.
pub const MAX_SETTINGS_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub media_consent: Option<ConsentRecord>,
}

/// What the user agreed to: the consent text version, the SHA-256 of the exact text shown,
/// and when (Unix seconds).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentRecord {
    pub version: u32,
    pub text_sha256: String,
    pub ts: u64,
}

#[derive(Clone, Debug)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> SettingsStore {
        SettingsStore { path: path.into() }
    }

    /// `<config dir>/settings.json` (`%APPDATA%\EidDownload\settings.json` on Windows).
    pub fn default_location() -> Option<SettingsStore> {
        eid_platform::ipc::config_dir().map(|d| SettingsStore::new(d.join("settings.json")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Missing file = defaults. Anything else that is not a small, valid settings document
    /// (symlink, oversized, malformed, unknown fields) is an error: video stays off rather
    /// than trusting a damaged or planted file.
    pub fn load(&self) -> Result<Settings, MediaError> {
        let f = match eid_platform::fs::open_regular_nofollow_read(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
            Err(e) => return Err(MediaError::Settings(format!("cannot open: {e}"))),
        };
        let mut buf = Vec::new();
        f.take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(|e| MediaError::Settings(format!("cannot read: {e}")))?;
        if buf.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(MediaError::Settings("larger than 64 KiB".into()));
        }
        serde_json::from_slice(&buf).map_err(|e| MediaError::Settings(format!("invalid: {e}")))
    }

    /// Atomic, private write (Unix 0600; Windows protected user-only DACL).
    pub fn save(&self, s: &Settings) -> Result<(), MediaError> {
        let data = serde_json::to_vec_pretty(s).map_err(|e| MediaError::Settings(e.to_string()))?;
        eid_platform::ipc::write_private_atomic(&self.path, &data)
            .map_err(|e| MediaError::Settings(format!("cannot write: {e}")))
    }
}
