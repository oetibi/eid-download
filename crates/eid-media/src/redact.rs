//! Redaction of tool text before it is shown, logged or stored (PLAN P4, G15).
//!
//! yt-dlp error messages and warnings quote URLs: page URLs with signed query strings, and
//! media URLs (`*.googlevideo.com/videoplayback?...&ip=203.0.113.7&...`) that carry the
//! user's IP address and short-lived tokens. Rules:
//!
//! * any `http://`/`https://` URL loses userinfo, query and fragment;
//! * a URL on a googlevideo host is replaced entirely (its host and path also identify the
//!   user's network);
//! * the value of a `Cookie`, `Set-Cookie`, `Authorization` or `Proxy-Authorization` header
//!   quoted in the text is replaced.

const URL_END: &[char] = &['"', '\'', '<', '>', '`', '(', ')', '[', ']', '{', '}', ','];
const SECRET_HEADERS: &[&str] = &[
    "set-cookie:",
    "cookie:",
    "proxy-authorization:",
    "authorization:",
];

/// Redact one piece of tool text.
pub fn redact_tool_text(s: &str) -> String {
    let s = redact_headers(s);
    redact_urls(&s)
}

fn redact_headers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let lower = line.to_ascii_lowercase();
        match SECRET_HEADERS
            .iter()
            .filter_map(|h| lower.find(h).map(|p| p + h.len()))
            .min()
        {
            // `lower` is an ASCII-lowercased copy of `line`, so byte offsets agree
            Some(end) => {
                out.push_str(&line[..end]);
                out.push_str(" <redacted>");
            }
            None => out.push_str(line),
        }
    }
    out
}

fn find_url_start(s: &str, from: usize) -> Option<usize> {
    let lower = s[from..].to_ascii_lowercase();
    let a = lower.find("http://");
    let b = lower.find("https://");
    match (a, b) {
        (Some(x), Some(y)) => Some(from + x.min(y)),
        (x, y) => x.or(y).map(|p| from + p),
    }
}

fn redact_urls(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    while let Some(start) = find_url_start(s, pos) {
        out.push_str(&s[pos..start]);
        let rest = &s[start..];
        let len = rest
            .find(|c: char| c.is_whitespace() || URL_END.contains(&c))
            .unwrap_or(rest.len());
        out.push_str(&redact_one(&rest[..len]));
        pos = start + len;
    }
    out.push_str(&s[pos..]);
    out
}

fn redact_one(u: &str) -> String {
    match url::Url::parse(u) {
        Ok(mut p) => {
            let host = p.host_str().unwrap_or("").to_ascii_lowercase();
            if host == "googlevideo.com" || host.ends_with(".googlevideo.com") {
                return "<media-url>".to_string();
            }
            let _ = p.set_username("");
            let _ = p.set_password(None);
            p.set_query(None);
            p.set_fragment(None);
            p.to_string()
        }
        Err(_) => "<url>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_untouched() {
        let t = "ERROR: [youtube] abc: Video unavailable. عربي";
        assert_eq!(redact_tool_text(t), t);
    }

    #[test]
    fn a_url_without_secrets_keeps_its_path() {
        assert_eq!(
            redact_tool_text("see https://example.org/a/b now"),
            "see https://example.org/a/b now"
        );
    }
}
