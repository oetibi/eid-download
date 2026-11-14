//! Import / export of download lists: text (one URL per line), JSON and CSV.
//!
//! An imported file is UNTRUSTED. It can carry URLs and plain names only: never a directory (the
//! queue's directory is the user's), never credentials (stripped), never a path (names are
//! sanitized to one component). The whole file is size- and count-bounded and a violation rejects
//! it as a whole instead of half-importing a hostile file.

use crate::batch;
use crate::sanitize::sanitize_file_name;
use crate::urlsafe::parse_and_strip;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_IMPORT_ITEMS: usize = 50_000;
pub const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_REPORTED: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListItem {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
    Csv,
}

impl Format {
    pub fn from_extension(path: &str) -> Option<Format> {
        match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
            "txt" | "list" => Some(Format::Text),
            "json" => Some(Format::Json),
            "csv" => Some(Format::Csv),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PortError {
    TooLarge,
    NotUtf8,
    TooManyItems,
    Malformed(String),
}

impl std::fmt::Display for PortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortError::TooLarge => write!(f, "file is larger than {MAX_IMPORT_BYTES} bytes"),
            PortError::NotUtf8 => write!(f, "file is not valid UTF-8"),
            PortError::TooManyItems => write!(f, "file lists more than {MAX_IMPORT_ITEMS} items"),
            PortError::Malformed(m) => write!(f, "malformed file: {m}"),
        }
    }
}
impl std::error::Error for PortError {}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub items: Vec<ListItem>,
    /// (1-based line/record number, reason) for entries that were skipped; capped.
    pub rejected: Vec<(usize, String)>,
    pub rejected_total: usize,
    pub duplicates: usize,
    /// Entries whose `user:pass@` was removed (never stored).
    pub credentials_dropped: usize,
}

struct Collector {
    out: Imported,
    seen: HashSet<(String, Option<String>, Option<String>)>,
}

impl Collector {
    fn new() -> Self {
        Collector {
            out: Imported::default(),
            seen: HashSet::new(),
        }
    }
    fn reject(&mut self, line: usize, why: impl Into<String>) {
        self.out.rejected_total += 1;
        if self.out.rejected.len() < MAX_REPORTED {
            self.out.rejected.push((line, why.into()));
        }
    }
    fn add(
        &mut self,
        line: usize,
        url: &str,
        name: Option<&str>,
        subdir: Option<&str>,
    ) -> Result<(), PortError> {
        let (clean, creds) = match parse_and_strip(url.trim()) {
            Ok(v) => v,
            Err(e) => {
                self.reject(line, e);
                return Ok(());
            }
        };
        if creds.is_some() {
            self.out.credentials_dropped += 1;
        }
        let clean_name = name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(sanitize_file_name);
        let clean_sub = subdir
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(sanitize_file_name);
        let key = (clean.to_string(), clean_name.clone(), clean_sub.clone());
        if !self.seen.insert(key.clone()) {
            self.out.duplicates += 1;
            return Ok(());
        }
        if self.out.items.len() >= MAX_IMPORT_ITEMS {
            return Err(PortError::TooManyItems);
        }
        self.out.items.push(ListItem {
            url: key.0,
            name: clean_name,
            subdir: clean_sub,
        });
        Ok(())
    }
}

fn decode(bytes: &[u8]) -> Result<&str, PortError> {
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(PortError::TooLarge);
    }
    let s = std::str::from_utf8(bytes).map_err(|_| PortError::NotUtf8)?;
    Ok(s.strip_prefix('\u{feff}').unwrap_or(s))
}

/// Import `bytes` (already read, at most `MAX_IMPORT_BYTES + 1` of them are needed to decide).
/// For `Format::Text`, `(a-b)` patterns in a line are expanded (see `batch`).
pub fn import(bytes: &[u8], format: Format) -> Result<Imported, PortError> {
    let text = decode(bytes)?;
    let mut c = Collector::new();
    match format {
        Format::Text => {
            for (i, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if line.len() > MAX_LINE_BYTES {
                    c.reject(i + 1, "line too long");
                    continue;
                }
                match batch::expand(line) {
                    Ok(urls) => {
                        for u in urls {
                            c.add(i + 1, &u, None, None)?;
                        }
                    }
                    Err(e) => c.reject(i + 1, e.to_string()),
                }
            }
        }
        Format::Json => {
            #[derive(Deserialize)]
            struct Doc {
                version: u32,
                items: Vec<ListItem>,
            }
            let doc: Doc =
                serde_json::from_str(text).map_err(|e| PortError::Malformed(e.to_string()))?;
            if doc.version != 1 {
                return Err(PortError::Malformed(format!(
                    "unsupported version {}",
                    doc.version
                )));
            }
            if doc.items.len() > MAX_IMPORT_ITEMS {
                return Err(PortError::TooManyItems);
            }
            for (i, it) in doc.items.iter().enumerate() {
                c.add(i + 1, &it.url, it.name.as_deref(), it.subdir.as_deref())?;
            }
        }
        Format::Csv => {
            let rows = parse_csv(text)?;
            let mut rows = rows.into_iter();
            let header = rows
                .next()
                .ok_or_else(|| PortError::Malformed("empty CSV".into()))?;
            let col = |n: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(n));
            let url_col =
                col("url").ok_or_else(|| PortError::Malformed("CSV has no 'url' column".into()))?;
            let (name_col, sub_col) = (col("name"), col("subdir"));
            for (i, row) in rows.enumerate() {
                if row.iter().all(|f| f.is_empty()) {
                    continue;
                }
                let get = |ix: usize| row.get(ix).map(|s| unneutralize(s));
                let Some(url) = get(url_col) else {
                    c.reject(i + 2, "row has no url field");
                    continue;
                };
                let name = name_col.and_then(get);
                let sub = sub_col.and_then(get);
                c.add(i + 2, &url, name.as_deref(), sub.as_deref())?;
            }
        }
    }
    Ok(c.out)
}

// ---------------------------------------------------------------- export

/// Spreadsheet formula injection: a cell that starts with `= + - @` (or a tab/CR) would be
/// executed by Excel on open. Such cells get a leading apostrophe on export and lose it on import.
fn neutralize(s: &str) -> String {
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{s}")
    } else {
        s.to_string()
    }
}
fn unneutralize(s: &str) -> String {
    match s.strip_prefix('\'') {
        Some(rest) if rest.starts_with(['=', '+', '-', '@', '\t', '\r']) => rest.to_string(),
        _ => s.to_string(),
    }
}

fn csv_field(s: &str) -> String {
    let s = neutralize(s);
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

pub fn export(items: &[ListItem], format: Format) -> String {
    match format {
        Format::Text => items.iter().map(|i| format!("{}\n", i.url)).collect(),
        Format::Json => {
            #[derive(Serialize)]
            struct Doc<'a> {
                version: u32,
                items: &'a [ListItem],
            }
            let mut s =
                serde_json::to_string_pretty(&Doc { version: 1, items }).expect("serializes");
            s.push('\n');
            s
        }
        Format::Csv => {
            let mut s = String::from("url,name,subdir\r\n");
            for i in items {
                s.push_str(&format!(
                    "{},{},{}\r\n",
                    csv_field(&i.url),
                    csv_field(i.name.as_deref().unwrap_or("")),
                    csv_field(i.subdir.as_deref().unwrap_or(""))
                ));
            }
            s
        }
    }
}

/// RFC 4180 reader: quotes, doubled quotes, newlines inside quotes, CRLF or LF.
fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, PortError> {
    let mut rows: Vec<Vec<String>> = vec![];
    let mut row: Vec<String> = vec![];
    let mut field = String::new();
    let mut in_q = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(ch) = chars.next() {
        any = true;
        if field.len() > MAX_LINE_BYTES {
            return Err(PortError::Malformed("CSV field too long".into()));
        }
        if in_q {
            match ch {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_q = false,
                c => field.push(c),
            }
        } else {
            match ch {
                '"' if field.is_empty() => in_q = true,
                ',' => row.push(std::mem::take(&mut field)),
                '\r' => {}
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    if rows.len() > MAX_IMPORT_ITEMS + 1 {
                        return Err(PortError::TooManyItems);
                    }
                }
                c => field.push(c),
            }
        }
    }
    if in_q {
        return Err(PortError::Malformed("unterminated quote in CSV".into()));
    }
    if any && (!field.is_empty() || !row.is_empty()) {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn li(url: &str, name: Option<&str>, sub: Option<&str>) -> ListItem {
        ListItem {
            url: url.into(),
            name: name.map(Into::into),
            subdir: sub.map(Into::into),
        }
    }

    fn sample() -> Vec<ListItem> {
        vec![
            li("http://h/a.bin", None, None),
            li("https://h/b.zip?x=1&y=2", Some("my file.zip"), Some("Docs")),
            li("http://h/عربي/ملف.pdf", Some("تقرير, \"final\".pdf"), None),
            li("http://h/c", Some("=cmd|' /C calc'!A0"), Some("+sub")),
            li("http://h/d", Some("line\nbreak"), None),
            li("http://h/e", Some("-1+1"), Some("@x")),
        ]
    }

    #[test]
    fn export_then_import_returns_the_same_list_in_every_format() {
        // names are what sanitize_file_name would produce: the round trip is on sanitized data
        let items: Vec<ListItem> = sample()
            .into_iter()
            .map(|i| ListItem {
                url: parse_and_strip(&i.url).unwrap().0.to_string(),
                name: i.name.as_deref().map(sanitize_file_name),
                subdir: i.subdir.as_deref().map(sanitize_file_name),
            })
            .collect();
        for f in [Format::Json, Format::Csv] {
            let text = export(&items, f);
            let back = import(text.as_bytes(), f).unwrap();
            assert_eq!(back.items, items, "{f:?}\n{text}");
            assert_eq!(back.rejected_total, 0);
        }
        // text carries URLs only
        let urls: Vec<_> = items.iter().map(|i| li(&i.url, None, None)).collect();
        let back = import(export(&items, Format::Text).as_bytes(), Format::Text).unwrap();
        assert_eq!(back.items, urls);
    }

    #[test]
    fn csv_export_neutralizes_formulas_and_import_restores_them() {
        let items = vec![li("http://h/x", Some("=SUM(A1)"), Some("@evil"))];
        let csv = export(&items, Format::Csv);
        assert!(csv.contains("'=SUM(A1)") && csv.contains("'@evil"), "{csv}");
        for line in csv.lines().skip(1) {
            for cell in line.split(',') {
                assert!(
                    !cell.starts_with(['=', '+', '-', '@']),
                    "executable cell: {cell}"
                );
            }
        }
        // sanitize_file_name keeps '=' and '@', so the names survive the trip unchanged
        let back = import(csv.as_bytes(), Format::Csv).unwrap();
        assert_eq!(back.items[0].name.as_deref(), Some("=SUM(A1)"));
        // an apostrophe that did NOT guard a formula is data, not neutralization
        let back = import(b"url,name\nhttp://h/x,'plain\n", Format::Csv).unwrap();
        assert_eq!(back.items[0].name.as_deref(), Some("'plain"));
    }

    #[test]
    fn malicious_files_are_rejected_or_neutralized_never_crash() {
        // too large (rejected before parsing)
        let huge = vec![b'a'; MAX_IMPORT_BYTES + 1];
        for f in [Format::Text, Format::Json, Format::Csv] {
            assert_eq!(import(&huge, f), Err(PortError::TooLarge));
        }
        // not UTF-8
        assert_eq!(
            import(&[0xff, 0xfe, 0x00, b'h'], Format::Text),
            Err(PortError::NotUtf8)
        );
        // too many items: rejected as a whole
        let many: String = (0..6).map(|k| format!("http://h/{k}/(0-9999)\n")).collect();
        assert!(matches!(
            import(many.as_bytes(), Format::Text),
            Err(PortError::TooManyItems)
        ));
        // JSON: wrong version, wrong shape, deep nesting, truncated, huge item list
        for bad in [
            r#"{"version":2,"items":[]}"#,
            r#"{"items":[]}"#,
            r#"[1,2,3]"#,
            r#"{"version":1,"items":[{"url":5}]}"#,
            &format!("{}1{}", "[".repeat(100_000), "]".repeat(100_000)),
            r#"{"version":1,"items":[{"url":"http://h/""#,
            "",
        ] {
            assert!(
                matches!(
                    import(bad.as_bytes(), Format::Json),
                    Err(PortError::Malformed(_))
                ),
                "{}",
                &bad[..bad.len().min(40)]
            );
        }
        // CSV: no url column, unterminated quote, giant field
        assert!(import(b"name\nx\n", Format::Csv).is_err());
        assert!(import(b"url\n\"http://h/x\n", Format::Csv).is_err());
        assert!(import(
            format!("url\n{}\n", "a".repeat(MAX_LINE_BYTES + 10)).as_bytes(),
            Format::Csv
        )
        .is_err());
        assert!(import(b"", Format::Csv).is_err());
        // hostile entries inside an otherwise valid file are skipped/sanitized, not fatal
        let text = "\
# comment\n\
javascript:alert(1)\n\
file:///etc/passwd\n\
ftp://h/a\n\
data:text/html;base64,AAAA\n\
http://\n\
not a url\n\
http://bob:SECRET@h/with-creds\n\
http://h/ok\n\
http://h/ok\n";
        let r = import(text.as_bytes(), Format::Text).unwrap();
        assert_eq!(
            r.items,
            vec![
                li("http://h/with-creds", None, None),
                li("http://h/ok", None, None)
            ]
        );
        assert_eq!(r.rejected_total, 6, "{:?}", r.rejected);
        assert_eq!(r.credentials_dropped, 1);
        assert_eq!(r.duplicates, 1);
        assert!(!format!("{r:?}").contains("SECRET"));
        // names/subdirs cannot carry paths or control characters out of an import
        let json = r#"{"version":1,"items":[
            {"url":"http://h/a","name":"../../etc/passwd","subdir":"..\\..\\evil"},
            {"url":"http://h/b","name":"C:\\Windows\\x.dll","subdir":"/abs"},
            {"url":"http://h/c","name":"CON","subdir":"a\u0000b"},
            {"url":"http://h/d","name":"x\u202etxt.exe"}]}"#;
        let r = import(json.as_bytes(), Format::Json).unwrap();
        for it in &r.items {
            for n in [&it.name, &it.subdir].into_iter().flatten() {
                assert!(crate::sanitize::is_safe_stored_name(n), "{n:?}");
            }
        }
        assert_eq!(r.items[0].name.as_deref(), Some("passwd"));
        assert_eq!(r.items[1].name.as_deref(), Some("x.dll"));
        // the file can ask for no directory at all: there is simply no such field
        let r = import(
            br#"{"version":1,"items":[{"url":"http://h/a","dir":"/etc"}]}"#,
            Format::Json,
        );
        assert_eq!(r.unwrap().items, vec![li("http://h/a", None, None)]);
    }

    #[test]
    fn text_import_expands_patterns_within_bounds() {
        let r = import(
            b"http://h/img(08-11).jpg\n# c\n\nhttp://h/x\n",
            Format::Text,
        )
        .unwrap();
        let urls: Vec<_> = r.items.iter().map(|i| i.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "http://h/img08.jpg",
                "http://h/img09.jpg",
                "http://h/img10.jpg",
                "http://h/img11.jpg",
                "http://h/x"
            ]
        );
        let r = import(b"http://h/(0-99999)\n", Format::Text).unwrap();
        assert!(r.items.is_empty() && r.rejected_total == 1);
        // BOM and CRLF are tolerated
        let r = import(b"\xef\xbb\xbfhttp://h/a\r\nhttp://h/b\r\n", Format::Text).unwrap();
        assert_eq!(r.items.len(), 2);
    }

    #[test]
    fn format_detection() {
        assert_eq!(Format::from_extension("a.JSON"), Some(Format::Json));
        assert_eq!(Format::from_extension("x/y.csv"), Some(Format::Csv));
        assert_eq!(Format::from_extension("l.txt"), Some(Format::Text));
        assert_eq!(Format::from_extension("noext"), None);
    }
}
