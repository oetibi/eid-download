//! v0.2 clipboard monitoring for the desktop app (off by default, REQUIREMENTS F9).
//!
//! Lightweight by construction: the app only asks for the clipboard's *sequence number* (a
//! counter Windows increments on every change; no clipboard access, no data copied) about once
//! a second, and reads the text only when that number changed. The text is bounded, never
//! stored and never logged; the caller keeps at most one http(s) link from it.
//!
//! Windows only; elsewhere every function reports "not available".

/// The clipboard's change counter (`None` on other systems). On Windows 0 is a normal value
/// (nothing copied yet in this session), and it is also what a process without clipboard
/// access always sees: then it never changes and nothing is ever read.
pub fn sequence() -> Option<u32> {
    imp::sequence()
}

/// The clipboard's text, if any, cut to at most `max_chars` UTF-16 units (`None`: no text, the
/// clipboard is busy, or not available here).
pub fn read_text(max_chars: usize) -> Option<String> {
    imp::read_text(max_chars)
}

/// Put `text` on the clipboard (used by tests; the app never writes the clipboard itself
/// except through the page's own "copy link").
pub fn write_text(text: &str) -> std::io::Result<()> {
    imp::write_text(text)
}

#[cfg(windows)]
mod imp {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
        IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
    };

    /// `CF_UNICODETEXT` (winuser.h).
    const CF_UNICODETEXT: u32 = 13;

    pub fn sequence() -> Option<u32> {
        // SAFETY: no arguments (0 = nothing copied yet, or no access: see `sequence`).
        Some(unsafe { GetClipboardSequenceNumber() })
    }

    /// Open the clipboard, retrying briefly: another program may hold it for a moment.
    fn open() -> bool {
        for _ in 0..5 {
            // SAFETY: a null owner window is allowed.
            if unsafe { OpenClipboard(null_mut()) } != 0 {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    struct Opened;
    impl Drop for Opened {
        fn drop(&mut self) {
            // SAFETY: only constructed after a successful OpenClipboard on this thread.
            unsafe { CloseClipboard() };
        }
    }

    pub fn read_text(max_chars: usize) -> Option<String> {
        // SAFETY: plain query.
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } == 0 || !open() {
            return None;
        }
        let _guard = Opened;
        // SAFETY: the clipboard is open; the handle belongs to the clipboard (not freed by us).
        let h = unsafe { GetClipboardData(CF_UNICODETEXT) };
        if h.is_null() {
            return None;
        }
        // SAFETY: `h` is a global memory handle owned by the clipboard while it is open.
        let bytes = unsafe { GlobalSize(h) };
        let p = unsafe { GlobalLock(h) } as *const u16;
        if p.is_null() {
            return None;
        }
        let units = (bytes / 2).min(max_chars.saturating_add(1));
        // SAFETY: `p` points to at least `bytes` readable bytes while locked; we read at most
        // bytes/2 units and stop at the first NUL.
        let slice = unsafe { std::slice::from_raw_parts(p, units) };
        let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
        let text = String::from_utf16_lossy(&slice[..end.min(max_chars)]);
        // SAFETY: matches the GlobalLock above.
        unsafe { GlobalUnlock(h) };
        Some(text)
    }

    pub fn write_text(text: &str) -> std::io::Result<()> {
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        if !open() {
            return Err(std::io::Error::last_os_error());
        }
        let _guard = Opened;
        // SAFETY: the clipboard is open by us.
        unsafe { EmptyClipboard() };
        // SAFETY: allocation of a movable block the clipboard will own after SetClipboardData.
        let h = unsafe { GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) };
        if h.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `h` is a fresh block of `wide.len() * 2` bytes.
        unsafe {
            let p = GlobalLock(h) as *mut u16;
            if p.is_null() {
                GlobalFree(h);
                return Err(std::io::Error::last_os_error());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            GlobalUnlock(h);
            if SetClipboardData(CF_UNICODETEXT, h).is_null() {
                let e = std::io::Error::last_os_error();
                GlobalFree(h);
                return Err(e);
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn sequence() -> Option<u32> {
        None
    }
    pub fn read_text(_max_chars: usize) -> Option<String> {
        None
    }
    pub fn write_text(_text: &str) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "clipboard monitoring is Windows-only",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows: a real round trip through the system clipboard, and the change counter moves.
    /// It fails (never skips) if the clipboard cannot be used on the runner.
    #[cfg(windows)]
    #[test]
    fn windows_clipboard_round_trip_and_sequence_changes() {
        // ci-windows run 44557942324: the counter is 0 before anything was copied in the session
        write_text("first").expect("write the clipboard");
        let before = sequence().expect("clipboard sequence number");
        let text = "https://example.com/ملف تجريبي.zip";
        write_text(text).expect("write the clipboard");
        let after = sequence().expect("clipboard sequence number");
        assert_ne!(before, after, "sequence did not change after a write");
        assert_eq!(read_text(4096).as_deref(), Some(text));
        // bounded read
        assert_eq!(read_text(5).as_deref(), Some("https"));
    }

    #[cfg(not(windows))]
    #[test]
    fn other_systems_report_the_clipboard_as_not_available() {
        assert_eq!(sequence(), None);
        assert_eq!(read_text(10), None);
        assert!(write_text("x").is_err());
    }
}
