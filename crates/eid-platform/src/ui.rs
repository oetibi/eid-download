//! The one native dialog the desktop app needs before (or without) its own window: a fatal
//! start-up message. A window app has no console, so without this a failure to create the window
//! (for example Microsoft Edge WebView2 missing) would end the process with nothing on screen.

/// The text of the fatal start-up message (both languages; pure, so it is unit tested).
pub fn fatal_text(detail: &str) -> String {
    // keep the dialog readable whatever the error text is
    let detail: String = detail
        .chars()
        .filter(|c| {
            (!c.is_control() || *c == '\n')
                && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(600)
        .collect();
    format!(
        "تعذّر تشغيل Eid Download.\nEid Download could not start.\n\n{detail}\n\n\
         إن كانت المشكلة WebView2 فثبّت «Microsoft Edge WebView2 Runtime» من موقع Microsoft.\n\
         If WebView2 is missing, install the Microsoft Edge WebView2 Runtime from Microsoft."
    )
}

/// Show a blocking error box (Windows); elsewhere print to stderr.
pub fn fatal_message(title: &str, text: &str) {
    imp::show(title, text);
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().filter(|&c| c != 0).chain([0]).collect()
    }

    pub fn show(title: &str, text: &str) {
        let (t, m) = (wide(title), wide(text));
        // SAFETY: both buffers are NUL-terminated UTF-16 and outlive the call; no owner window.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                m.as_ptr(),
                t.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn show(title: &str, text: &str) {
        eprintln!("{title}: {text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_text_keeps_the_reason_and_drops_control_characters() {
        let t = fatal_text("WebView2 not found\u{7}\u{202e}x\r\nline2");
        assert!(t.contains("WebView2 not found"));
        assert!(t.contains("could not start"));
        assert!(t.contains("تعذّر تشغيل"));
        assert!(!t.contains('\u{7}') && !t.contains('\r') && !t.contains('\u{202e}'));
        let long = fatal_text(&"a".repeat(5000));
        assert!(long.len() < 1500);
    }
}
