//! v0.1: the desktop app's download settings (IDM-style "Connection" / "Scheduler" options),
//! kept in `<config dir>/gui-settings.json` (the engine's checksummed store) and applied live:
//! simultaneous downloads and the scheduler window go to the main queue, the speed limit to
//! the global limiter, connections per download to the runner (new downloads). Pure logic here
//! (tested without a window); the Tauri commands are thin wrappers in `eid-gui`.
use eid_engine::queue::{QueueSet, Window};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MAX_PARALLEL: u32 = 8;
pub const MAX_CONNECTIONS: u32 = 32;
/// 0 = unlimited; the cap only rejects nonsense (≈ 100 Gbit/s).
pub const MAX_LIMIT_KIB: u64 = 12_500_000;
const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuiSettings {
    /// Downloads running at the same time (main queue).
    pub max_parallel: u32,
    /// Connections per download (the engine ramps up to this, adaptively).
    pub connections: u32,
    /// Global speed limit in KiB/s for all downloads together (0 = unlimited).
    pub speed_limit_kib: u64,
    /// Scheduler: only start/run downloads inside this daily window (`None` = any time).
    #[serde(default)]
    pub schedule: Option<Window>,
    /// v0.2: an icon in the Windows notification area (show, pause/resume all, quit).
    #[serde(default = "on")]
    pub tray: bool,
    /// v0.2: closing the window hides it to the notification area instead of quitting
    /// (only while the icon is shown).
    #[serde(default)]
    pub close_to_tray: bool,
    /// v0.2: offer to download a link copied to the clipboard (off by default, REQUIREMENTS
    /// F9). The clipboard text is never stored or logged.
    #[serde(default)]
    pub clipboard: bool,
    /// v0.2 (owner decision 2026-11-19): sleep / hibernate / shut down after the downloads are
    /// OFF unless the user turns them on here; while off they are refused everywhere (quitting
    /// the app is always allowed).
    #[serde(default)]
    pub power_actions: bool,
}

fn on() -> bool {
    true
}

impl Default for GuiSettings {
    fn default() -> Self {
        GuiSettings {
            max_parallel: 3,
            connections: 8,
            speed_limit_kib: 0,
            schedule: None,
            tray: true,
            close_to_tray: false,
            clipboard: false,
            power_actions: false,
        }
    }
}

impl GuiSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_PARALLEL).contains(&self.max_parallel) {
            return Err(format!("simultaneous downloads must be 1..={MAX_PARALLEL}"));
        }
        if !(1..=MAX_CONNECTIONS).contains(&self.connections) {
            return Err(format!("connections must be 1..={MAX_CONNECTIONS}"));
        }
        if self.speed_limit_kib > MAX_LIMIT_KIB {
            return Err("speed limit too large".into());
        }
        if let Some(w) = &self.schedule {
            w.validate()?;
            if w.weekdays == 0 {
                return Err("the schedule needs at least one day".into());
            }
        }
        Ok(())
    }

    /// Bytes per second for [`eid_engine::Limiter::set_rate`] (0 = unlimited).
    pub fn limit_bps(&self) -> u64 {
        self.speed_limit_kib.saturating_mul(1024)
    }

    /// Put the queue-level settings on `queue` (the desktop app's main queue).
    pub fn apply_to_queues(&self, qs: &mut QueueSet, queue: &str) -> Result<(), String> {
        let q = qs
            .queues
            .iter_mut()
            .find(|q| q.name == queue)
            .ok_or_else(|| format!("no queue {queue:?}"))?;
        q.max_parallel = self.max_parallel;
        q.window = self.schedule;
        Ok(())
    }
}

pub fn default_file() -> Option<PathBuf> {
    eid_platform::ipc::config_dir().map(|d| d.join("gui-settings.json"))
}

/// The saved settings; a missing file gives the defaults. A corrupt or invalid file is kept
/// aside by the store (`*.broken`, never silently overwritten); its backup or the defaults
/// are used.
pub fn load(path: &Path) -> GuiSettings {
    match eid_engine::store::load::<GuiSettings>(path, VERSION, &|s| s.validate()) {
        Ok(Some(s)) => s,
        _ => GuiSettings::default(),
    }
}

pub fn save(path: &Path, s: &GuiSettings) -> Result<(), String> {
    s.validate()?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    eid_engine::store::save(path, VERSION, s).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eid_engine::queue::Queue;

    #[test]
    fn a_v01_settings_file_loads_with_tray_on_and_clipboard_off() {
        let v01 = r#"{"max_parallel":2,"connections":16,"speed_limit_kib":0,"schedule":null}"#;
        let s: GuiSettings = serde_json::from_str(v01).unwrap();
        assert_eq!((s.max_parallel, s.connections), (2, 16));
        assert!(s.tray && !s.close_to_tray && !s.clipboard && !s.power_actions);
        let d = GuiSettings::default();
        assert!(d.tray && !d.close_to_tray && !d.clipboard && !d.power_actions);
        // still strict about unknown fields
        assert!(serde_json::from_str::<GuiSettings>(
            r#"{"max_parallel":2,"connections":16,"speed_limit_kib":0,"evil":1}"#
        )
        .is_err());
    }

    #[test]
    fn defaults_are_valid_and_bounds_are_enforced() {
        let d = GuiSettings::default();
        assert!(d.validate().is_ok());
        for bad in [
            GuiSettings {
                max_parallel: 0,
                ..d.clone()
            },
            GuiSettings {
                max_parallel: 9,
                ..d.clone()
            },
            GuiSettings {
                connections: 0,
                ..d.clone()
            },
            GuiSettings {
                connections: 33,
                ..d.clone()
            },
            GuiSettings {
                speed_limit_kib: MAX_LIMIT_KIB + 1,
                ..d.clone()
            },
            GuiSettings {
                schedule: Some(Window {
                    start_min: 1440,
                    end_min: 10,
                    weekdays: 1,
                }),
                ..d.clone()
            },
            GuiSettings {
                schedule: Some(Window {
                    start_min: 60,
                    end_min: 120,
                    weekdays: 0,
                }),
                ..d.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert_eq!(
            GuiSettings {
                speed_limit_kib: 500,
                ..d
            }
            .limit_bps(),
            512_000
        );
    }

    #[test]
    fn save_load_round_trip_and_a_corrupt_file_is_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gui-settings.json");
        assert_eq!(load(&p), GuiSettings::default());
        let s = GuiSettings {
            max_parallel: 5,
            connections: 16,
            speed_limit_kib: 2048,
            schedule: Some(Window {
                start_min: 23 * 60,
                end_min: 7 * 60,
                weekdays: Window::ALL_DAYS,
            }),
            tray: false,
            close_to_tray: true,
            clipboard: true,
            power_actions: true,
        };
        save(&p, &s).unwrap();
        assert_eq!(load(&p), s);
        assert!(save(
            &p,
            &GuiSettings {
                connections: 0,
                ..s.clone()
            }
        )
        .is_err());
        assert_eq!(load(&p), s, "a refused save leaves the file alone");
        std::fs::write(&p, b"{not json").unwrap();
        let got = load(&p);
        assert!(got == s || got == GuiSettings::default(), "{got:?}");
        assert!(dir.path().join("gui-settings.json.broken").exists());
    }

    #[test]
    fn queue_settings_reach_the_main_queue() {
        let mut qs = QueueSet {
            queues: vec![Queue::new("main", "/d")],
            next_id: 1,
        };
        let w = Window {
            start_min: 120,
            end_min: 360,
            weekdays: 0b0011111,
        };
        let s = GuiSettings {
            max_parallel: 6,
            schedule: Some(w),
            ..GuiSettings::default()
        };
        s.apply_to_queues(&mut qs, "main").unwrap();
        assert_eq!(qs.queues[0].max_parallel, 6);
        assert_eq!(qs.queues[0].window, Some(w));
        assert!(qs.validate().is_ok());
        assert!(s.apply_to_queues(&mut qs, "nope").is_err());
        GuiSettings::default()
            .apply_to_queues(&mut qs, "main")
            .unwrap();
        assert_eq!(qs.queues[0].window, None);
    }
}
