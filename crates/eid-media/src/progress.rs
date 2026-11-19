//! Parsing our own progress lines (template [`crate::args::PROGRESS_TEMPLATE`]).

use crate::args::PROGRESS_PREFIX;

/// One progress sample. Every field may be unknown (`NA` from yt-dlp).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    pub downloaded: Option<u64>,
    /// Exact total, or yt-dlp's estimate when the exact one is unknown.
    pub total: Option<u64>,
    pub speed: Option<u64>,
    pub eta_secs: Option<u64>,
    /// Height of the format being downloaded (`None` for audio or when unknown).
    pub height: Option<u32>,
}

/// A number from the tool: non-negative, finite, saturated into `u64`, never panics.
fn num(s: &str) -> Option<u64> {
    if s.len() > 32 {
        return None;
    }
    let f: f64 = s.parse().ok()?;
    if !f.is_finite() || f < 0.0 {
        return None;
    }
    // `as` saturates for floats; anything above u64::MAX becomes u64::MAX
    Some(f as u64)
}

/// Parse one stdout line; `None` if it is not one of our progress lines.
pub fn parse_line(line: &[u8]) -> Option<Progress> {
    let s = std::str::from_utf8(line).ok()?.trim();
    let rest = s.strip_prefix(PROGRESS_PREFIX)?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    // 6 fields with the height (current template); 5 from an older template or tool
    if f.len() != 5 && f.len() != 6 {
        return None;
    }
    Some(Progress {
        downloaded: num(f[0]),
        total: num(f[1]).or_else(|| num(f[2])),
        speed: num(f[3]),
        eta_secs: num(f[4]),
        height: f
            .get(5)
            .and_then(|h| num(h))
            .and_then(|h| u32::try_from(h).ok())
            .filter(|h| *h > 0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_parse_and_hostile_numbers_never_panic() {
        let p = parse_line(b"[eid-progress] 1024 2048 NA 512.5 2").unwrap();
        assert_eq!(p.downloaded, Some(1024));
        assert_eq!(p.total, Some(2048));
        assert_eq!(p.speed, Some(512));
        assert_eq!(p.eta_secs, Some(2));
        let p = parse_line(b"[eid-progress] NA NA 4096.0 NA NA").unwrap();
        assert_eq!(p.total, Some(4096), "falls back to the estimate");
        let p = parse_line(b"[eid-progress] 1e400 -5 nan inf 99999999999999999999999").unwrap();
        assert_eq!(p.downloaded, None, "1e400 is inf");
        assert_eq!(p.total, None);
        assert_eq!(p.eta_secs, Some(u64::MAX), "huge values saturate");
        assert!(parse_line(b"[download] 50%").is_none());
        assert!(parse_line(b"[eid-progress] 1 2 3").is_none());
        assert!(parse_line(b"[eid-progress] 1 2 3 4 5 6 7").is_none());
        assert_eq!(p.height, None, "five fields: no height");
        let p = parse_line(b"[eid-progress] 1 2 NA 3 4 1080").unwrap();
        assert_eq!(p.height, Some(1080));
        for h in ["NA", "-1", "0", "99999999999", "1e400"] {
            let line = format!("[eid-progress] 1 2 NA 3 4 {h}");
            assert_eq!(parse_line(line.as_bytes()).unwrap().height, None, "{h}");
        }
        assert!(parse_line(&[0xff, 0xfe]).is_none());
    }
}
