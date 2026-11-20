//! v0.2 diagnostics (owner's report 2026-11-20: "Download the tools" closed the app on a real
//! PC, with no trace of why). A small log file and a crash record, both in the per-user data
//! folder (`%LOCALAPPDATA%\EidDownload\logs`):
//! - `eid-download.log`: start (version, build commit, program path), tool setup steps and
//!   their outcome, panics. Links are redacted; no cookies, passwords or file contents. Capped
//!   at 1 MiB (the previous part is kept as `eid-download.old.log`).
//! - `crash.txt`: written by the panic hook. On the next start it is renamed to
//!   `crash-<unix time>.txt` and the window says the app closed unexpectedly, with that path.
//!
//! Logging never fails the caller: errors writing the log are ignored.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The commit this program was built from (7 hex digits, or "local").
pub const BUILD_COMMIT: &str = env!("EID_BUILD_COMMIT");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_LOG: u64 = 1 << 20;
static LOCK: Mutex<()> = Mutex::new(());

/// `%LOCALAPPDATA%\EidDownload\logs` (or the platform's data folder).
pub fn log_dir() -> Option<PathBuf> {
    eid_platform::dirs::local_data_dir().map(|d| d.join("logs"))
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time (UTC, no dependencies).
pub fn utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Append one line to the log in `dir` (rotating at 1 MiB). Never fails the caller.
pub fn log_in(dir: &Path, line: &str) {
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _ = std::fs::create_dir_all(dir);
    let file = dir.join("eid-download.log");
    if std::fs::metadata(&file).is_ok_and(|m| m.len() > MAX_LOG) {
        let _ = std::fs::rename(&file, dir.join("eid-download.old.log"));
    }
    // one line per entry: the text is redacted and kept on one line
    let text: String = eid_engine::urlsafe::redact_text(line)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(4000)
        .collect();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
    {
        let _ = writeln!(f, "{} [{}] {text}", utc(now()), std::process::id());
    }
}

/// Append one line to the app's log.
pub fn log(line: &str) {
    if let Some(d) = log_dir() {
        log_in(&d, line);
    }
}

/// The panic text written to the crash record: what, where, which thread, build, backtrace.
pub fn panic_report(info: &std::panic::PanicHookInfo<'_>, backtrace: &str) -> String {
    let what = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".into());
    let at = info
        .location()
        .map(|l| format!("{}:{}", l.file(), l.line()))
        .unwrap_or_default();
    let thread = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    format!(
        "Eid Download {VERSION} ({BUILD_COMMIT}) closed by a panic at {}\nthread: {thread}\nat: {at}\nmessage: {}\nprogram: {}\n\nbacktrace:\n{backtrace}\n",
        utc(now()),
        eid_engine::urlsafe::redact_text(&what),
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    )
}

/// Record panics (`crash.txt` + the log) before the default handling. `role` says which process
/// (the window or the video worker).
pub fn install_panic_hook(role: &'static str) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture().to_string();
        let report = panic_report(info, &bt);
        if let Some(d) = log_dir() {
            let _ = std::fs::create_dir_all(&d);
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(d.join("crash.txt"))
            {
                let _ = writeln!(f, "[{role}] {report}");
            }
            log_in(
                &d,
                &format!(
                    "PANIC in {role}: {}",
                    report.lines().take(4).collect::<Vec<_>>().join(" | ")
                ),
            );
        }
        previous(info);
    }));
}

/// A crash record left by the previous run in `dir`, renamed so it is reported once: the new
/// path, or `None`.
pub fn take_last_crash_in(dir: &Path) -> Option<PathBuf> {
    let c = dir.join("crash.txt");
    if !c.is_file() {
        return None;
    }
    let kept = dir.join(format!("crash-{}.txt", now()));
    std::fs::rename(&c, &kept).ok()?;
    Some(kept)
}

pub fn take_last_crash() -> Option<PathBuf> {
    log_dir().and_then(|d| take_last_crash_in(&d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates_are_exact() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_795_219_199), "2026-11-20T23:59:59Z");
        assert_eq!(utc(4_107_542_400), "2100-03-01T00:00:00Z");
    }

    /// The log is one redacted line per entry, rotates at 1 MiB, and a crash record is reported
    /// once (renamed), never lost.
    #[test]
    fn the_log_redacts_rotates_and_the_crash_record_is_reported_once() {
        let d = tempfile::tempdir().unwrap();
        log_in(
            d.path(),
            "tools install failed for https://user:secret@h.example/x?token=abc\nsecond line",
        );
        let t = std::fs::read_to_string(d.path().join("eid-download.log")).unwrap();
        assert_eq!(t.lines().count(), 1, "{t}");
        assert!(!t.contains("secret") && !t.contains("token=abc"), "{t}");
        assert!(
            t.contains("tools install failed") && t.contains("second line"),
            "{t}"
        );
        std::fs::write(
            d.path().join("eid-download.log"),
            vec![b'x'; (MAX_LOG + 1) as usize],
        )
        .unwrap();
        log_in(d.path(), "after rotation");
        assert!(d.path().join("eid-download.old.log").is_file());
        assert!(std::fs::read_to_string(d.path().join("eid-download.log"))
            .unwrap()
            .contains("after rotation"));
        assert_eq!(take_last_crash_in(d.path()), None);
        std::fs::write(d.path().join("crash.txt"), "boom").unwrap();
        let kept = take_last_crash_in(d.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "boom");
        assert_eq!(take_last_crash_in(d.path()), None, "reported once");
    }

    /// A real panic goes through the hook into `crash.txt` with the message and the place.
    #[test]
    fn a_panic_is_written_with_its_message_and_place() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().to_path_buf();
        let prev = std::panic::take_hook();
        let d2 = dir.clone();
        std::panic::set_hook(Box::new(move |info| {
            let r = panic_report(info, "bt");
            // only this test's panic (other tests run in parallel in this process)
            if r.contains("no reactor running") {
                let _ = std::fs::write(d2.join("crash.txt"), r);
            }
        }));
        let _ = std::panic::catch_unwind(|| panic!("no reactor running"));
        std::panic::set_hook(prev);
        let r = std::fs::read_to_string(dir.join("crash.txt")).unwrap();
        assert!(
            r.contains("no reactor running") && r.contains("diag.rs") && r.contains(BUILD_COMMIT),
            "{r}"
        );
    }
}
