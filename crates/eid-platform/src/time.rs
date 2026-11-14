//! Local wall-clock time (for queue time windows). The only place that asks the OS for the local
//! time zone; the engine receives plain numbers through its `Clock` trait.

/// A moment, as the scheduler needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalNow {
    /// Seconds since the Unix epoch (UTC).
    pub unix: i64,
    /// 0 = Monday ... 6 = Sunday, in LOCAL time.
    pub weekday: u8,
    /// Minutes since local midnight (0..1440).
    pub minute: u16,
    /// Seconds inside the current minute (0..60).
    pub second: u8,
}

/// Break a Unix time + a fixed offset into a [`LocalNow`] (pure; used by the fake clocks in the
/// tests and by the real implementation's fallback). `offset_secs` is local minus UTC.
pub fn from_unix(unix: i64, offset_secs: i64) -> LocalNow {
    let local = unix.saturating_add(offset_secs);
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    // 1970-01-01 was a Thursday (= 3 when Monday = 0)
    let weekday = (days + 3).rem_euclid(7) as u8;
    LocalNow {
        unix,
        weekday,
        minute: (secs / 60) as u16,
        second: (secs % 60) as u8,
    }
}

/// The current local time.
pub fn local_now() -> LocalNow {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    #[cfg(unix)]
    {
        let t: libc::time_t = unix as libc::time_t;
        // SAFETY: `tm` is plain-old-data written by localtime_r; `t` is a valid time_t.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return from_unix(unix, 0);
        }
        LocalNow {
            unix,
            weekday: ((tm.tm_wday + 6) % 7) as u8,
            minute: (tm.tm_hour * 60 + tm.tm_min) as u16,
            second: tm.tm_sec.clamp(0, 59) as u8,
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        // SAFETY: SYSTEMTIME is plain-old-data filled in by GetLocalTime.
        let mut st: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe { GetLocalTime(&mut st) };
        LocalNow {
            unix,
            weekday: ((st.wDayOfWeek + 6) % 7) as u8,
            minute: st.wHour * 60 + st.wMinute,
            second: st.wSecond.min(59) as u8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_unix_weekday_minute_and_offsets() {
        // 1970-01-01 00:00:00 UTC is a Thursday
        let n = from_unix(0, 0);
        assert_eq!((n.weekday, n.minute, n.second), (3, 0, 0));
        // 2026-11-14 is a Saturday (the day this was written); 12:34:56 UTC
        let unix = 1_794_659_696; // 2026-11-14T12:34:56Z
        let n = from_unix(unix, 0);
        assert_eq!((n.weekday, n.minute, n.second), (5, 12 * 60 + 34, 56));
        // +3 h (Kuwait) moves the minute, same day
        let n = from_unix(unix, 3 * 3600);
        assert_eq!((n.weekday, n.minute), (5, 15 * 60 + 34));
        // an offset that crosses midnight moves the weekday: 22:00 UTC Sat + 3h = 01:00 Sun
        let sat_22 = unix - (12 * 3600 + 34 * 60 + 56) + 22 * 3600;
        let n = from_unix(sat_22, 3 * 3600);
        assert_eq!((n.weekday, n.minute), (6, 60));
        // negative times and extremes do not panic
        let _ = from_unix(-1, 0);
        let _ = from_unix(i64::MIN, i64::MAX);
        let _ = from_unix(i64::MAX, i64::MAX);
    }

    #[test]
    fn local_now_is_sane() {
        let n = local_now();
        assert!(n.unix > 1_700_000_000);
        assert!(n.weekday < 7 && n.minute < 1440 && n.second < 60);
    }
}
