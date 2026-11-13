//! URL hygiene: credentials are never stored/printed; display forms drop the query.

use url::Url;

/// Parse an http(s) URL. Returns the credential-free URL plus any `user:pass@` it carried
/// (percent-decoded). The credentials are for in-memory use during this run only.
pub fn parse_and_strip(raw: &str) -> Result<(Url, Option<(String, String)>), String> {
    if raw.len() > 8192 {
        return Err("URL too long".into());
    }
    let mut u = Url::parse(raw.trim()).map_err(|_| "not a valid URL".to_string())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(format!("unsupported scheme '{}'", u.scheme()));
    }
    if u.host_str().is_none() {
        return Err("URL has no host".into());
    }
    let creds = if !u.username().is_empty() || u.password().is_some() {
        let dec = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        };
        let c = (dec(u.username()), dec(u.password().unwrap_or("")));
        let _ = u.set_username("");
        let _ = u.set_password(None);
        Some(c)
    } else {
        None
    };
    u.set_fragment(None);
    Ok((u, creds))
}

/// Safe form for logs, events and error messages: scheme://host[:port]/path (no userinfo,
/// no query, no fragment). Query strings often carry signed tokens.
pub fn redact(u: &Url) -> String {
    let mut c = u.clone();
    let _ = c.set_username("");
    let _ = c.set_password(None);
    c.set_query(None);
    c.set_fragment(None);
    c.to_string()
}

/// Redact an arbitrary string that may contain a URL (e.g. an upstream error message).
pub fn redact_text(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let trimmed = w.trim_matches(|c: char| "()[]<>\"',;".contains(c));
            if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                match Url::parse(trimmed) {
                    Ok(u) => redact(&u),
                    Err(_) => "<url>".to_string(),
                }
            } else {
                w.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_credentials_stripped_and_returned() {
        let (u, c) =
            parse_and_strip("https://al%40ice:p%40ss@host.example:8443/a/b.zip?sig=SECRET#frag")
                .unwrap();
        assert_eq!(c, Some(("al@ice".into(), "p@ss".into())));
        let s = u.to_string();
        assert!(!s.contains("p%40ss") && !s.contains("al%40ice") && !s.contains('@'));
        assert!(
            s.contains("sig=SECRET"),
            "query is kept for the request itself"
        );
        assert!(!s.contains("frag"));
    }

    #[test]
    fn security_redact_drops_query_and_userinfo() {
        let u = Url::parse("https://u:p@h.example/x/y?token=ABC123&a=b#f").unwrap();
        let r = redact(&u);
        assert_eq!(r, "https://h.example/x/y");
        assert!(!redact_text("failed GET https://h.example/x?token=ABC123 now").contains("ABC123"));
        assert!(!redact_text("see (https://u:pw@h.example/p?sig=Z)").contains("pw"));
    }

    #[test]
    fn rejects_non_http_and_huge() {
        assert!(parse_and_strip("file:///etc/passwd").is_err());
        assert!(parse_and_strip("ftp://x/y").is_err());
        assert!(parse_and_strip("javascript:alert(1)").is_err());
        assert!(parse_and_strip(&format!("http://h/{}", "a".repeat(9000))).is_err());
    }
}
