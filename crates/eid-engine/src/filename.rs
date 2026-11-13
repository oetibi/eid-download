//! Choosing a file name from `Content-Disposition` (RFC 6266 / 5987) or the URL.

use crate::sanitize::sanitize_file_name;
use percent_encoding::percent_decode;
use url::Url;

fn pct_decode_charset(v: &str, charset: &str) -> Option<String> {
    let bytes: Vec<u8> = percent_decode(v.as_bytes()).collect();
    match charset.to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" => String::from_utf8(bytes).ok(),
        "iso-8859-1" | "latin1" => Some(bytes.iter().map(|&b| b as char).collect()),
        _ => None,
    }
}

/// Extract the suggested file name (raw, NOT yet sanitized) from a Content-Disposition value.
pub fn content_disposition_name(header: &str) -> Option<String> {
    if header.len() > 4096 {
        return None;
    }
    let mut plain: Option<String> = None;
    let mut ext: Option<String> = None;
    // split on ';' outside quotes
    let mut parts = Vec::new();
    let (mut cur, mut inq, mut esc) = (String::new(), false, false);
    for c in header.chars() {
        if esc {
            cur.push(c);
            esc = false;
        } else if c == '\\' && inq {
            esc = true;
            cur.push(c);
        } else if c == '"' {
            inq = !inq;
            cur.push(c);
        } else if c == ';' && !inq {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    for p in parts.iter().skip(1) {
        let Some((k, v)) = p.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        if k == "filename*" {
            // charset'lang'pct-encoded
            let mut it = v.splitn(3, '\'');
            if let (Some(cs), Some(_lang), Some(val)) = (it.next(), it.next(), it.next()) {
                if let Some(s) = pct_decode_charset(val, cs) {
                    ext = Some(s);
                }
            }
        } else if k == "filename" {
            let val = if v.starts_with('"') && v.ends_with('"') && v.len() >= 2 {
                let inner = &v[1..v.len() - 1];
                let mut o = String::new();
                let mut e = false;
                for c in inner.chars() {
                    if e {
                        o.push(c);
                        e = false;
                    } else if c == '\\' {
                        e = true;
                    } else {
                        o.push(c);
                    }
                }
                o
            } else {
                v.to_string()
            };
            plain = Some(val);
        }
    }
    ext.or(plain).filter(|s| !s.trim().is_empty())
}

/// Last path segment of the URL, percent-decoded (lossy UTF-8).
pub fn url_name(u: &Url) -> Option<String> {
    let seg = u.path_segments()?.rev().find(|s| !s.is_empty())?;
    let d = percent_decode(seg.as_bytes())
        .decode_utf8_lossy()
        .into_owned();
    if d.is_empty() {
        None
    } else {
        Some(d)
    }
}

/// Final, sanitized name: Content-Disposition > URL path > "download".
pub fn choose_name(content_disposition: Option<&str>, final_url: &Url) -> String {
    let raw = content_disposition
        .and_then(content_disposition_name)
        .or_else(|| url_name(final_url))
        .unwrap_or_default();
    sanitize_file_name(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc5987_wins_over_plain() {
        let h = "attachment; filename=\"fallback.txt\"; filename*=UTF-8''%D8%AA%D9%82%D8%B1%D9%8A%D8%B1.pdf";
        assert_eq!(content_disposition_name(h).unwrap(), "تقرير.pdf");
    }

    #[test]
    fn quoted_with_escapes_and_semicolons() {
        let h = r#"attachment; filename="a;b\"c.txt""#;
        assert_eq!(content_disposition_name(h).unwrap(), "a;b\"c.txt");
    }

    #[test]
    fn hostile_names_are_sanitized_by_choose_name() {
        let u = Url::parse("http://h/x").unwrap();
        for h in [
            "attachment; filename=\"../../etc/passwd\"",
            "attachment; filename=\"CON\"",
            "attachment; filename=\"evil\u{202e}fdp.exe\"",
            "attachment; filename=\"a.txt:ads\"",
            "attachment; filename*=UTF-8''..%2F..%2Fx.sh",
            "attachment; filename*=UTF-8''%5C%5Cserver%5Cshare%5Cx",
        ] {
            let n = choose_name(Some(h), &u);
            assert!(!n.contains(['/', '\\', ':']), "{n}");
            assert!(crate::sanitize::is_safe_stored_name(&n), "{n}");
        }
    }

    #[test]
    fn url_fallback_and_default() {
        let u = Url::parse("http://h/a/b%20c.zip?x=1").unwrap();
        assert_eq!(choose_name(None, &u), "b c.zip");
        let u = Url::parse("http://h/").unwrap();
        assert_eq!(choose_name(None, &u), "download");
    }
}
