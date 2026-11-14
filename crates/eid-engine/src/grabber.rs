//! Grabber: list the downloadable links of ONE page. No JavaScript, no crawling, no recursion.
//! Bounded on every axis: the page may not exceed `max_bytes` (checked against Content-Length
//! before any body is read, and again while streaming), nor contain more than `max_links`
//! distinct links; a page that does is REFUSED, not silently truncated.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::http;
use crate::urlsafe::parse_and_strip;
use futures_util::StreamExt;
use std::collections::HashSet;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

pub const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_MAX_LINKS: usize = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrabOptions {
    pub max_bytes: usize,
    pub max_links: usize,
    /// Lower-case extensions without dot; empty = every link.
    pub exts: Vec<String>,
    /// Keep only URLs containing this text (case-insensitive).
    pub contains: Option<String>,
    /// Keep only URLs matching this glob (`*` any run, `?` one char; case-insensitive).
    pub glob: Option<String>,
    pub same_host_only: bool,
}

impl Default for GrabOptions {
    fn default() -> Self {
        GrabOptions {
            max_bytes: DEFAULT_MAX_BYTES,
            max_links: DEFAULT_MAX_LINKS,
            exts: vec![],
            contains: None,
            glob: None,
            same_host_only: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Link,
    Image,
    Media,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrabItem {
    /// Absolute, credential-free http(s) URL.
    pub url: String,
    /// Visible link text (links) or `alt` (images), at most 200 chars; for display only.
    pub text: String,
    pub kind: Kind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum GrabError {
    TooLarge { limit: usize },
    TooManyLinks { limit: usize },
}

impl std::fmt::Display for GrabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrabError::TooLarge { limit } => write!(f, "page is larger than {limit} bytes"),
            GrabError::TooManyLinks { limit } => write!(f, "page has more than {limit} links"),
        }
    }
}
impl std::error::Error for GrabError {}

// ---------------------------------------------------------------- HTML scanning

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let rep = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => ent
                .strip_prefix('#')
                .and_then(|n| {
                    n.strip_prefix(['x', 'X']).map_or_else(
                        || n.parse::<u32>().ok(),
                        |h| u32::from_str_radix(h, 16).ok(),
                    )
                })
                .and_then(char::from_u32),
        };
        match rep {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A tag longer than this is not a tag: the window is skipped. Bounds the work per `<` so hostile
/// input (`<a <a <a ...` or endless attributes) costs O(n) overall instead of O(n^2).
const MAX_TAG_BYTES: usize = 4096;

enum Parsed<'a> {
    Tag(Tag<'a>),
    /// Not a tag start (`<` followed by a non-letter): move on by one byte.
    NotATag,
    /// A tag-looking start that does not close inside the window: skip the whole window.
    Junk,
}

struct Tag<'a> {
    name: String,
    attrs: Vec<(String, String)>,
    closing: bool,
    /// bytes consumed from the `<`
    len: usize,
    _src: std::marker::PhantomData<&'a str>,
}

fn parse_tag(full: &str) -> Parsed<'_> {
    let mut end = full.len().min(MAX_TAG_BYTES);
    while !full.is_char_boundary(end) {
        end -= 1;
    }
    let s = &full[..end];
    let truncated = end < full.len();
    let b = s.as_bytes();
    debug_assert_eq!(b[0], b'<');
    let mut i = 1;
    let closing = b.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    let start = i;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b':') {
        i += 1;
    }
    if i == start {
        return Parsed::NotATag;
    }
    let junk = Parsed::Junk;
    let name = s[start..i].to_ascii_lowercase();
    let mut attrs = vec![];
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        match b.get(i) {
            None => return junk, // does not close inside the window
            Some(b'>') => {
                i += 1;
                break;
            }
            _ => {}
        }
        let ks = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        if i == ks {
            i += 1; // stray byte
            continue;
        }
        let key = s[ks..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut val = String::new();
        if b.get(i) == Some(&b'=') {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            match b.get(i) {
                Some(q @ (b'"' | b'\'')) => {
                    let q = *q;
                    i += 1;
                    let vs = i;
                    while i < b.len() && b[i] != q {
                        i += 1;
                    }
                    if i >= b.len() {
                        return junk;
                    }
                    val = s[vs..i].to_string();
                    i += 1;
                }
                _ => {
                    let vs = i;
                    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                        i += 1;
                    }
                    val = s[vs..i].to_string();
                }
            }
        }
        if attrs.len() < 64 {
            attrs.push((key, decode_entities(&val)));
        }
    }
    let _ = truncated;
    Parsed::Tag(Tag {
        name,
        attrs,
        closing,
        len: i,
        _src: std::marker::PhantomData,
    })
}

fn attr<'a>(t: &'a Tag<'_>, k: &str) -> Option<&'a str> {
    t.attrs
        .iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.as_str())
}

/// Position just after the end of a raw-text element (`<script>`, `<style>`): found by scanning
/// forward for `</name` (ASCII case-insensitive) without copying or lower-casing the rest.
fn skip_raw_text(s: &str, name: &str) -> usize {
    let b = s.as_bytes();
    let n = name.as_bytes();
    let mut from = 0;
    while let Some(off) = s[from..].find("</") {
        let at = from + off;
        let cand = &b[at + 2..];
        if cand.len() >= n.len() && cand[..n.len()].eq_ignore_ascii_case(n) {
            return s[at..].find('>').map_or(s.len(), |j| at + j + 1);
        }
        from = at + 2;
    }
    s.len()
}

fn clean_text(s: &str) -> String {
    let t = decode_entities(s);
    let mut out = String::new();
    for w in t.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
        if out.chars().count() >= 200 {
            break;
        }
    }
    out.chars().filter(|c| !c.is_control()).take(200).collect()
}

fn resolve(base: &Url, raw: &str) -> Option<Url> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 8192 || raw.starts_with('#') {
        return None; // a pure fragment points back at the page itself
    }
    let u = base.join(raw).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    // credentials are stripped, never carried into a download list
    let (clean, _creds) = parse_and_strip(u.as_str()).ok()?;
    Some(clean)
}

/// Extract the links of `html`. `base` is the URL the page was fetched from.
pub fn extract(
    html: &str,
    base: &Url,
    opts: &GrabOptions,
) -> std::result::Result<Vec<GrabItem>, GrabError> {
    if html.len() > opts.max_bytes {
        return Err(GrabError::TooLarge {
            limit: opts.max_bytes,
        });
    }
    let mut base = base.clone();
    let mut found: Vec<GrabItem> = vec![];
    let mut seen: HashSet<String> = HashSet::new();
    let mut i = 0usize;
    // (index in `found` of the open <a>, text start offset)
    let mut open_a: Option<(usize, usize)> = None;
    let mut base_set = false;
    while let Some(off) = html[i..].find('<') {
        let at = i + off;
        let rest = &html[at..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(html.len(), |j| at + j + 3);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            i = rest.find('>').map_or(html.len(), |j| at + j + 1);
            continue;
        }
        let tag = match parse_tag(rest) {
            Parsed::Tag(t) => t,
            Parsed::NotATag => {
                i = at + 1;
                continue;
            }
            Parsed::Junk => {
                let mut skip = rest.len().min(MAX_TAG_BYTES);
                while !rest.is_char_boundary(skip) {
                    skip -= 1;
                }
                i = at + skip.max(1);
                continue;
            }
        };
        let after = at + tag.len;
        i = after;
        if tag.closing {
            if tag.name == "a" {
                if let Some((idx, text_from)) = open_a.take() {
                    found[idx].text = clean_text(&strip_tags(&html[text_from..at]));
                }
            }
            continue;
        }
        match tag.name.as_str() {
            "script" | "style" | "template" | "noscript" => {
                i = after + skip_raw_text(&html[after..], &tag.name);
                continue;
            }
            "base" if !base_set => {
                if let Some(h) = attr(&tag, "href").and_then(|h| resolve(&base, h)) {
                    base = h;
                    base_set = true;
                }
                continue;
            }
            _ => {}
        }
        let (src, kind) = match tag.name.as_str() {
            "a" | "area" => (attr(&tag, "href"), Kind::Link),
            "img" => (attr(&tag, "src"), Kind::Image),
            "source" | "video" | "audio" | "track" | "embed" => (attr(&tag, "src"), Kind::Media),
            _ => (None, Kind::Link),
        };
        if tag.name == "a" {
            if let Some((idx, text_from)) = open_a.take() {
                found[idx].text = clean_text(&strip_tags(&html[text_from..at]));
                // unclosed <a>
            }
        }
        let Some(url) = src.and_then(|s| resolve(&base, s)) else {
            continue;
        };
        if !seen.insert(url.to_string()) {
            continue;
        }
        if seen.len() > opts.max_links {
            return Err(GrabError::TooManyLinks {
                limit: opts.max_links,
            });
        }
        let text = if kind == Kind::Image {
            clean_text(attr(&tag, "alt").unwrap_or(""))
        } else {
            String::new()
        };
        found.push(GrabItem {
            url: url.to_string(),
            text,
            kind,
        });
        if tag.name == "a" {
            open_a = Some((found.len() - 1, after));
        }
    }
    if let Some((idx, text_from)) = open_a {
        found[idx].text = clean_text(&strip_tags(&html[text_from..]));
    }
    Ok(found.into_iter().filter(|g| keep(g, &base, opts)).collect())
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars().take(4000) {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn glob_match(pat: &[u8], s: &[u8]) -> bool {
    // iterative wildcard matching, O(n*m) worst case, no recursion
    let (mut p, mut t, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while t < s.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p].eq_ignore_ascii_case(&s[t])) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

fn url_extension(u: &str) -> Option<String> {
    let path = Url::parse(u).ok()?.path().to_string();
    let last = path.rsplit('/').next()?;
    let (stem, ext) = last.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

fn keep(g: &GrabItem, base: &Url, o: &GrabOptions) -> bool {
    if !o.exts.is_empty() && !url_extension(&g.url).is_some_and(|e| o.exts.contains(&e)) {
        return false;
    }
    if let Some(c) = &o.contains {
        if !g.url.to_ascii_lowercase().contains(&c.to_ascii_lowercase()) {
            return false;
        }
    }
    if let Some(p) = &o.glob {
        if !glob_match(p.as_bytes(), g.url.as_bytes()) {
            return false;
        }
    }
    if o.same_host_only {
        let same = Url::parse(&g.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            == base.host_str().map(str::to_string);
        if !same {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------- fetching

/// Read at most `max` bytes from a body stream; `Err(TooLarge)` the moment `max` is exceeded (the
/// rest is never read). `stall` bounds the wait for each chunk.
pub async fn read_capped<S, B, E>(mut stream: S, max: usize, stall: Duration) -> Result<Vec<u8>>
where
    S: futures_util::Stream<Item = std::result::Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match tokio::time::timeout(stall, stream.next()).await {
            Err(_) => return Err(Error::Network("page transfer stalled".into())),
            Ok(None) => return Ok(buf),
            Ok(Some(Err(_))) => return Err(Error::Network("page transfer failed".into())),
            Ok(Some(Ok(chunk))) => {
                let c = chunk.as_ref();
                if buf.len().saturating_add(c.len()) > max {
                    return Err(Error::InvalidInput(
                        GrabError::TooLarge { limit: max }.to_string(),
                    ));
                }
                buf.extend_from_slice(c);
            }
        }
    }
}

/// Fetch one page (GET, redirects bounded, TLS verified, identity encoding) and list its links.
pub async fn grab_url(
    url: &str,
    cfg: &Config,
    opts: &GrabOptions,
    cancel: &CancellationToken,
) -> Result<Vec<GrabItem>> {
    let (clean, creds) = parse_and_strip(url).map_err(Error::InvalidInput)?;
    let client = http::build_client(cfg)?;
    let mut rb = client
        .get(clean.clone())
        .header("accept", "text/html,application/xhtml+xml");
    if let Some((u, p)) = creds {
        rb = rb.basic_auth(u, Some(p));
    }
    let resp = tokio::select! {
        r = http::send_bounded(rb, cfg.header_timeout) => r,
        _ = cancel.cancelled() => return Err(Error::Cancelled),
    }
    .map_err(|e| match e {
        http::SendError::Net(e) => {
            Error::Network(crate::urlsafe::redact_text(&e.without_url().to_string()))
        }
        http::SendError::HeaderTimeout => {
            Error::Network("timed out waiting for the page headers".into())
        }
    })?;
    if !resp.status().is_success() {
        return Err(Error::Status {
            status: resp.status().as_u16(),
            url: crate::urlsafe::redact(&clean),
        });
    }
    let ctype = http::header_str(resp.headers(), "content-type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !(ctype.starts_with("text/html") || ctype.starts_with("application/xhtml+xml")) {
        return Err(Error::BadResponse("the URL is not an HTML page".into()));
    }
    if resp
        .content_length()
        .is_some_and(|n| n > opts.max_bytes as u64)
    {
        // refused before a single body byte is read
        return Err(Error::InvalidInput(
            GrabError::TooLarge {
                limit: opts.max_bytes,
            }
            .to_string(),
        ));
    }
    let final_url = resp.url().clone();
    let body = tokio::select! {
        r = read_capped(resp.bytes_stream(), opts.max_bytes, cfg.stall_timeout) => r?,
        _ = cancel.cancelled() => return Err(Error::Cancelled),
    };
    let html = String::from_utf8_lossy(&body);
    extract(&html, &final_url, opts).map_err(|e| Error::InvalidInput(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("http://example.com/dir/page.html").unwrap()
    }
    fn urls(v: &[GrabItem]) -> Vec<&str> {
        v.iter().map(|g| g.url.as_str()).collect()
    }

    #[test]
    fn grabber_extracts_resolves_dedups_and_ignores_scripts_and_comments() {
        let html = r##"<!doctype html><html><head>
<base href="/files/">
<script>var x = "<a href='http://evil/script.exe'>";</script>
<style>a { content: "<a href='http://evil/css'>" }</style>
</head><body>
<!-- <a href="http://evil/comment.zip">hidden</a> -->
<a href="one.zip">First &amp; best</a>
<A HREF='/abs/two.pdf' class=x>Second <b>file</b></A>
<a href=three.iso>three</a>
<a href="one.zip">duplicate</a>
<a href="javascript:alert(1)">js</a>
<a href="mailto:a@b.c">mail</a>
<a href="data:text/html;base64,AAAA">data</a>
<a href="blob:http://x/1">blob</a>
<a href="ftp://h/f">ftp</a>
<a href="#frag">frag only</a>
<a href="//cdn.example.org/lib.7z#x">proto relative</a>
<a href="http://bob:SECRET@h.example/creds.bin">creds</a>
<a>no href</a>
<img src="pic.png" alt="A picture">
<video src="v.mp4"></video><audio><source src="s.mp3"></audio>
<a href="unterminated
</body></html>"##;
        let g = extract(html, &base(), &GrabOptions::default()).unwrap();
        assert_eq!(
            urls(&g),
            [
                "http://example.com/files/one.zip",
                "http://example.com/abs/two.pdf",
                "http://example.com/files/three.iso",
                "http://cdn.example.org/lib.7z",
                "http://h.example/creds.bin",
                "http://example.com/files/pic.png",
                "http://example.com/files/v.mp4",
                "http://example.com/files/s.mp3",
            ]
        );
        assert_eq!(g[0].text, "First & best");
        assert_eq!(g[1].text, "Second file");
        assert_eq!(g[5].kind, Kind::Image);
        assert_eq!(g[5].text, "A picture");
        assert_eq!(g[6].kind, Kind::Media);
        let all = format!("{g:?}");
        assert!(!all.contains("SECRET") && !all.contains("evil"), "{all}");
    }

    #[test]
    fn grabber_filters() {
        let html = r#"<a href="a.ZIP">1</a><a href="b.pdf">2</a><a href="c.zip?x=1">3</a>
<a href="http://other.org/d.zip">4</a><a href="noext">5</a><a href="/v/SetUp.exe">6</a>"#;
        let run = |o: GrabOptions| {
            let g = extract(html, &base(), &o).unwrap();
            g.iter()
                .map(|x| x.url.rsplit('/').next().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            run(GrabOptions {
                exts: vec!["zip".into()],
                ..Default::default()
            }),
            ["a.ZIP", "c.zip?x=1", "d.zip"]
        );
        assert_eq!(
            run(GrabOptions {
                exts: vec!["zip".into(), "pdf".into()],
                same_host_only: true,
                ..Default::default()
            }),
            ["a.ZIP", "b.pdf", "c.zip?x=1"]
        );
        assert_eq!(
            run(GrabOptions {
                contains: Some("SETUP".into()),
                ..Default::default()
            }),
            ["SetUp.exe"]
        );
        assert_eq!(
            run(GrabOptions {
                glob: Some("*/dir/?.pdf".into()),
                ..Default::default()
            }),
            ["b.pdf"]
        );
        assert_eq!(
            run(GrabOptions {
                glob: Some("*.zip*".into()),
                ..Default::default()
            }),
            ["a.ZIP", "c.zip?x=1", "d.zip"]
        );
        assert_eq!(run(GrabOptions::default()).len(), 6);
        // glob matcher edge cases (no catastrophic backtracking)
        assert!(glob_match(b"*", b""));
        assert!(glob_match(b"a*b*c", b"aXXbYYc"));
        assert!(!glob_match(b"a*b*c", b"aXXbYY"));
        let evil = "a".repeat(5000);
        assert!(!glob_match(
            format!("{}b", "a*".repeat(500)).as_bytes(),
            evil.as_bytes()
        ));
    }

    #[test]
    fn grabber_refuses_huge_pages_and_too_many_links() {
        let opts = GrabOptions {
            max_bytes: 1000,
            max_links: 5,
            ..Default::default()
        };
        let big = format!("<a href='x'>{}</a>", "y".repeat(2000));
        assert_eq!(
            extract(&big, &base(), &opts),
            Err(GrabError::TooLarge { limit: 1000 })
        );
        let many: String = (0..6)
            .map(|i| format!("<a href='f{i}.zip'>x</a>"))
            .collect();
        assert_eq!(
            extract(&many, &base(), &opts),
            Err(GrabError::TooManyLinks { limit: 5 })
        );
        let ok: String = (0..5)
            .map(|i| format!("<a href='f{i}.zip'>x</a>"))
            .collect();
        assert_eq!(extract(&ok, &base(), &opts).unwrap().len(), 5);
        // duplicates do not count towards the cap
        let dup: String = (0..50)
            .map(|_| "<a href='same.zip'>x</a>".to_string())
            .collect();
        assert_eq!(
            extract(
                &dup,
                &base(),
                &GrabOptions {
                    max_links: 5,
                    max_bytes: 100_000,
                    ..Default::default()
                }
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn grabber_survives_pathological_markup() {
        let evil = [
            "<".repeat(100_000),
            "<a ".repeat(50_000),
            "<a href=\"".repeat(10_000),
            "<script>".repeat(10_000),
            "<!--".repeat(10_000),
            "<a href='x'".repeat(20_000),
            format!("<a {}>", "x=1 ".repeat(100_000)),
            "&#xFFFFFFFFFF;&#99999999999;&;&&&&".repeat(1000),
            "\u{0}<a href='\u{0}'>\u{0}</a>".to_string(),
        ];
        for h in evil {
            let o = GrabOptions {
                max_bytes: 10_000_000,
                ..Default::default()
            };
            let t = std::time::Instant::now();
            let _ = extract(&h, &base(), &o);
            assert!(
                t.elapsed() < Duration::from_secs(5),
                "slow on {}",
                &h[..h.len().min(20)]
            );
        }
    }

    #[tokio::test]
    async fn read_capped_stops_at_the_limit_without_reading_the_rest() {
        let pulled = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p2 = pulled.clone();
        let s = futures_util::stream::iter((0..1000).map(move |_| {
            p2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, ()>(vec![0u8; 1000])
        }));
        let r = read_capped(Box::pin(s), 5500, Duration::from_secs(1)).await;
        assert!(r.is_err());
        assert!(
            pulled.load(std::sync::atomic::Ordering::SeqCst) <= 7,
            "kept reading after the limit"
        );
        let s = futures_util::stream::iter((0..5).map(|_| Ok::<_, ()>(vec![1u8; 100])));
        assert_eq!(
            read_capped(Box::pin(s), 500, Duration::from_secs(1))
                .await
                .unwrap()
                .len(),
            500
        );
        let s = futures_util::stream::pending::<std::result::Result<Vec<u8>, ()>>();
        assert!(
            read_capped(Box::pin(s), 10, Duration::from_millis(20))
                .await
                .is_err(),
            "stall"
        );
    }
}
