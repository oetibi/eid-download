//! HTTP plumbing: client construction, header parsing with checked arithmetic.

use crate::config::{Config, MAX_TOTAL_BYTES};
use crate::error::{Error, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use std::time::Duration;

/// Build the shared client: HTTP/1.1 only (one TCP connection per segment; HTTP/2 would
/// multiplex them over a single congestion window), identity encoding, bounded safe redirects.
pub fn build_client(cfg: &Config) -> Result<reqwest::Client> {
    let max = cfg.max_redirects;
    let policy = Policy::custom(move |attempt| {
        let prev = attempt.previous().last().map(|u| u.scheme().to_string());
        match redirect_verdict(
            prev.as_deref(),
            attempt.url().scheme(),
            attempt.previous().len(),
            max,
        ) {
            Ok(()) => attempt.follow(),
            Err(why) => attempt.error(why),
        }
    });
    let mut h = HeaderMap::new();
    h.insert(
        HeaderName::from_static("accept-encoding"),
        HeaderValue::from_static("identity"),
    );
    let mut b = reqwest::Client::builder()
        .http1_only()
        .redirect(policy)
        .user_agent(cfg.user_agent.clone())
        .default_headers(h)
        .connect_timeout(cfg.connect_timeout)
        .pool_max_idle_per_host(0)
        .pool_idle_timeout(Duration::from_secs(60));
    for pem in &cfg.extra_root_certs_pem {
        let cert = reqwest::Certificate::from_pem(pem)
            .map_err(|e| Error::InvalidInput(format!("bad extra root certificate: {e}")))?;
        b = b.add_root_certificate(cert);
    }
    if !cfg.use_system_proxy {
        b = b.no_proxy();
    }
    b.build()
        .map_err(|e| Error::Network(crate::urlsafe::redact_text(&e.to_string())))
}

/// Why a bounded `send` failed.
pub enum SendError {
    Net(reqwest::Error),
    /// No complete response header block (including every redirect hop) within the deadline.
    HeaderTimeout,
}

/// `RequestBuilder::send` with a deadline on the response HEADERS. reqwest's `connect_timeout`
/// ends once the socket is up, and the engine's stall timeout only watches the body; without
/// this a server that accepts the connection and never finishes its headers would block a job
/// forever (P1 review M3). The deadline covers the whole redirect chain. Dropping the future
/// (deadline or caller cancel) drops the connection.
pub async fn send_bounded(
    rb: reqwest::RequestBuilder,
    limit: Duration,
) -> std::result::Result<reqwest::Response, SendError> {
    match tokio::time::timeout(limit, rb.send()).await {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => Err(SendError::Net(e)),
        Err(_) => Err(SendError::HeaderTimeout),
    }
}

/// Pure redirect policy (unit-tested): bounded hops, http(s) only, never https -> http.
pub fn redirect_verdict(
    prev_scheme: Option<&str>,
    next_scheme: &str,
    hops_so_far: usize,
    max: usize,
) -> std::result::Result<(), &'static str> {
    if hops_so_far >= max {
        return Err("too many redirects");
    }
    if prev_scheme == Some("https") && next_scheme != "https" {
        return Err("refusing https -> http downgrade redirect");
    }
    if !matches!(next_scheme, "http" | "https") {
        return Err("redirect to unsupported scheme");
    }
    Ok(())
}

/// Parsed `Content-Range: bytes first-last/total`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct ContentRange {
    pub first: u64,
    pub last: u64,
    pub total: Option<u64>,
}

/// Strict parser. Rejects malformed, inverted, overflowing or absurd values.
pub fn parse_content_range(v: &str) -> Option<ContentRange> {
    let v = v.trim();
    let rest = v.strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (a, b) = range.trim().split_once('-')?;
    let first: u64 = a.trim().parse().ok()?;
    let last: u64 = b.trim().parse().ok()?;
    if last < first {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        t => {
            let t: u64 = t.parse().ok()?;
            if t > MAX_TOTAL_BYTES || last >= t {
                return None;
            }
            Some(t)
        }
    };
    if last >= MAX_TOTAL_BYTES {
        return None;
    }
    Some(ContentRange { first, last, total })
}

/// `Retry-After` (RFC 9110 10.2.3): delta-seconds, or an HTTP-date (IMF-fixdate) measured
/// against `reference_unix`. `reference_unix` should be the server's own `Date` (so a skewed
/// local clock does not matter) and falls back to the local clock. The wait is capped. A date
/// that is not in the future or does not parse yields `None` (the caller backs off normally
/// instead of hot-looping on "retry now"). All arithmetic is saturating.
pub fn parse_retry_after(v: &str, cap: Duration, reference_unix: i64) -> Option<Duration> {
    let v = v.trim();
    if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        let secs: u64 = v.parse().unwrap_or(u64::MAX); // all digits but too big: saturate
        return Some(Duration::from_secs(secs).min(cap));
    }
    let at = parse_imf_fixdate(v)?;
    let secs = at.saturating_sub(reference_unix);
    if secs <= 0 {
        return None;
    }
    Some(Duration::from_secs(secs as u64).min(cap))
}

/// `Retry-After` from response headers: reference = the response `Date`, else the local clock.
pub fn retry_after_of(h: &HeaderMap, cap: Duration) -> Option<Duration> {
    let v = header_str(h, "retry-after")?;
    let reference = header_str(h, "date")
        .and_then(|d| parse_imf_fixdate(&d))
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        });
    parse_retry_after(&v, cap, reference)
}

/// Only strong ETags are usable validators (RFC 9110 8.8.1).
pub fn strong_etag(v: &str) -> Option<String> {
    let v = v.trim();
    if v.starts_with("W/") || v.is_empty() || v.len() > 512 {
        None
    } else {
        Some(v.to_string())
    }
}

/// Seconds since the Unix epoch of an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`, RFC 9110
/// 5.6.7). The obsolete formats are not accepted: an unparsable date makes the validator weak.
pub fn parse_imf_fixdate(v: &str) -> Option<i64> {
    let p: Vec<&str> = v.split_whitespace().collect();
    if p.len() != 6 || p[5] != "GMT" || !p[0].ends_with(',') {
        return None;
    }
    let day: i64 = p[1].parse().ok().filter(|d| (1..=31).contains(d))?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| *m == p[2])? as i64
        + 1;
    let year: i64 = p[3].parse().ok().filter(|y| (1970..=9999).contains(y))?;
    let t: Vec<i64> = p[4].split(':').filter_map(|x| x.parse().ok()).collect();
    if t.len() != 3 || t[0] > 23 || t[1] > 59 || t[2] > 60 {
        return None;
    }
    // days from civil (Howard Hinnant)
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + t[0] * 3600 + t[1] * 60 + t[2])
}

/// `Last-Modified` is a STRONG validator only if the server's own `Date` is at least one second
/// later (RFC 9110 8.8.2.2): otherwise the resource may change again within the same second.
/// No `Date`, an unparsable date, or `Date < LM + 1s` => `None` (treated as no validator).
pub fn strong_last_modified(lm: &str, date: Option<&str>) -> Option<String> {
    let lm = lm.trim();
    let l = parse_imf_fixdate(lm)?;
    let d = parse_imf_fixdate(date?.trim())?;
    (d > l).then(|| lm.to_string()) // whole seconds: d > l  <=>  d >= l + 1 s
}

pub fn header_str(h: &HeaderMap, name: &str) -> Option<String> {
    h.get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
}

/// Exponential backoff with full jitter, `attempt` starting at 1.
pub fn backoff(cfg: &Config, attempt: u32) -> Duration {
    let exp = cfg.backoff_base.saturating_mul(
        1u32.checked_shl(attempt.saturating_sub(1).min(16))
            .unwrap_or(u32::MAX),
    );
    let cap = exp.min(cfg.backoff_cap);
    let ms = cap.as_millis() as u64;
    Duration::from_millis(ms / 2 + fastrand::u64(0..=ms / 2))
}

#[cfg(test)]
mod tests {
    #[test]
    fn imf_fixdate_and_strong_last_modified() {
        use super::{parse_imf_fixdate as p, strong_last_modified as s};
        assert_eq!(p("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(p("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784_111_777));
        assert_eq!(p("Sat, 29 Feb 2020 12:00:00 GMT"), Some(1_582_977_600));
        assert_eq!(p("06 Nov 1994 08:49:37"), None);
        assert_eq!(p("Sunday, 06-Nov-94 08:49:37 GMT"), None);
        let lm = "Sun, 06 Nov 1994 08:49:37 GMT";
        assert!(
            s(lm, Some("Sun, 06 Nov 1994 08:49:37 GMT")).is_none(),
            "Date == LM"
        );
        assert!(
            s(lm, Some("Sun, 06 Nov 1994 08:49:38 GMT")).is_some(),
            "Date == LM+1s"
        );
        assert!(
            s(lm, Some("Sun, 06 Nov 1994 08:49:36 GMT")).is_none(),
            "Date < LM"
        );
        assert!(s(lm, None).is_none(), "no Date");
        assert!(s(lm, Some("garbage")).is_none());
        assert!(s("garbage", Some("Sun, 06 Nov 1994 08:49:38 GMT")).is_none());
    }

    use super::*;

    #[test]
    fn numbers_from_network_use_checked_ops_content_range() {
        assert_eq!(
            parse_content_range("bytes 0-99/100"),
            Some(ContentRange {
                first: 0,
                last: 99,
                total: Some(100)
            })
        );
        assert_eq!(parse_content_range("bytes 5-9/*").unwrap().total, None);
        for bad in [
            "bytes 10-5/100",
            "bytes 0-100/100",
            "bytes a-b/c",
            "bytes 0-99",
            "items 0-1/2",
            "bytes 0-18446744073709551615/18446744073709551615",
            "bytes 0-99/18446744073709551615",
            "bytes 18446744073709551615-18446744073709551615/*",
            "bytes -5-9/100",
            "",
        ] {
            assert_eq!(parse_content_range(bad), None, "{bad}");
        }
    }

    #[test]
    fn numbers_from_network_use_checked_ops_retry_after_and_backoff() {
        let cap = Duration::from_secs(120);
        assert_eq!(parse_retry_after("5", cap, 0), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("18446744073709551615", cap, 0), Some(cap));
        assert_eq!(
            parse_retry_after("99999999999999999999999999", cap, 0),
            Some(cap)
        );
        assert_eq!(parse_retry_after("-1", cap, 0), None);
        assert_eq!(parse_retry_after("", cap, 0), None);
        let cfg = Config::default();
        for a in [0u32, 1, 5, 40, u32::MAX] {
            assert!(backoff(&cfg, a) <= cfg.backoff_cap);
        }
    }

    #[test]
    fn retry_after_http_date_is_honoured_with_bounded_arithmetic() {
        let cap = Duration::from_secs(120);
        let now = parse_imf_fixdate("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        // future: 90 s after the reference
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:51:07 GMT", cap, now),
            Some(Duration::from_secs(90))
        );
        // beyond the cap is capped
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 12:00:00 GMT", cap, now),
            Some(cap)
        );
        // extreme: year 9999 does not overflow
        assert_eq!(
            parse_retry_after("Fri, 31 Dec 9999 23:59:59 GMT", cap, now),
            Some(cap)
        );
        assert_eq!(
            parse_retry_after("Fri, 31 Dec 9999 23:59:59 GMT", cap, i64::MIN),
            Some(cap)
        );
        // past, equal, malformed and obsolete forms: unknown (None), never a hot loop
        for bad in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sun, 06 Nov 1994 08:00:00 GMT",
            "Thu, 01 Jan 1970 00:00:00 GMT",
            "Sunday, 06-Nov-94 08:51:07 GMT",
            "tomorrow",
            "5 seconds",
        ] {
            assert_eq!(parse_retry_after(bad, cap, now), None, "{bad}");
        }
        // reference taken from the response Date header, not the local clock
        let mut h = HeaderMap::new();
        h.insert("date", "Sun, 06 Nov 1994 08:49:37 GMT".parse().unwrap());
        h.insert(
            "retry-after",
            "Sun, 06 Nov 1994 08:49:42 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after_of(&h, cap), Some(Duration::from_secs(5)));
        // without a Date the local clock is used: a 1994 date is in the past => None
        h.remove("date");
        assert_eq!(retry_after_of(&h, cap), None);
    }

    #[test]
    fn redirect_https_downgrade_refused() {
        assert!(redirect_verdict(Some("https"), "http", 1, 10).is_err());
        assert!(redirect_verdict(Some("https"), "https", 1, 10).is_ok());
        assert!(redirect_verdict(Some("http"), "https", 1, 10).is_ok());
        assert!(redirect_verdict(Some("http"), "http", 1, 10).is_ok());
        assert!(redirect_verdict(Some("http"), "ftp", 1, 10).is_err());
        assert!(redirect_verdict(Some("http"), "file", 1, 10).is_err());
        assert!(redirect_verdict(Some("http"), "http", 10, 10).is_err());
    }

    #[test]
    fn weak_etag_is_not_a_validator() {
        assert_eq!(strong_etag("W/\"x\""), None);
        assert_eq!(strong_etag("\"x\""), Some("\"x\"".into()));
    }
}
