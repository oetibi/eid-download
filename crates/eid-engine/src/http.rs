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
        .pool_max_idle_per_host(64)
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

/// `Retry-After` as delta-seconds (HTTP-date form is not honoured: treated as unknown).
pub fn parse_retry_after(v: &str, cap: Duration) -> Option<Duration> {
    let secs: u64 = v.trim().parse().ok()?;
    Some(Duration::from_secs(secs).min(cap))
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
        assert_eq!(parse_retry_after("5", cap), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("18446744073709551615", cap), Some(cap));
        assert_eq!(parse_retry_after("-1", cap), None);
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT", cap),
            None
        );
        let cfg = Config::default();
        for a in [0u32, 1, 5, 40, u32::MAX] {
            assert!(backoff(&cfg, a) <= cfg.backoff_cap);
        }
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
