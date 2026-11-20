//! Wire messages. Every type rejects unknown fields; every string is bounded; the protocol
//! version is checked. The same `Request`/`Response` are used on the pipe and (without the
//! handshake) in browser Native Messaging.

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const MAX_ID_LEN: usize = 64;
pub const MAX_URL_LEN: usize = 8192;
pub const MAX_NAME_LEN: usize = 255;
pub const MAX_TOKEN_LEN: usize = 128;
/// Most playlist items one `add_playlist` may name.
pub const MAX_ITEMS: usize = 5000;
/// The only video qualities accepted (owner 2026-11-20: 144p to 4320p (8K), nothing taller).
/// Same list as the media crate's `QUALITY_CHOICES` (checked by a test in eid-cli).
pub const VIDEO_QUALITIES: &[&str] = &[
    "144p", "240p", "360p", "480p", "720p", "1080p", "1440p", "2160p", "4320p", "best", "audio",
    "m4a",
];
/// v0.2: what to do when the chosen height is missing. Same list as the media crate's
/// `FALLBACK_CHOICES` (checked by a test in eid-cli). Absent = `lower` (the v0.1 behaviour).
pub const VIDEO_FALLBACKS: &[&str] = &["lower", "higher", "skip"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub v: u32,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// Liveness and protocol check.
    Ping {},
    /// Add a download to the default queue. The directory is never chosen by the caller:
    /// only an optional sub-folder name and file name (both sanitized by the engine).
    Add {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subdir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_name: Option<String>,
    },
    List {},
    /// Ask the running app to exit (never allowed from a browser).
    Shutdown {},
    Pause {
        item: u64,
    },
    Resume {
        item: u64,
    },
    /// Bring the desktop app's window to the front (a second double-click on the app sends it;
    /// the headless `eid serve` has no window and refuses it). Never allowed from a browser.
    Show {},
    /// v0.1: what a page address is (video or playlist: title, items, offered qualities).
    /// Read-only and slow (it runs yt-dlp): served outside the idempotency lock.
    Probe {
        url: String,
    },
    /// v0.1: download one video (or its audio) at a quality of [`VIDEO_QUALITIES`] into the
    /// default folder (the caller never chooses a folder).
    AddVideo {
        url: String,
        quality: String,
        /// v0.2: [`VIDEO_FALLBACKS`]; absent = `lower`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_missing: Option<String>,
        /// v0.2 (the download menu): the exact formats the menu showed, `video+audio` (ids
        /// from the app's own probe); `quality` applies if the site no longer offers them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
    },
    /// v0.1: download a playlist (all items, or only `items`, numbered from 1 in playlist
    /// order) into a numbered folder named after it, in the default folder.
    AddPlaylist {
        url: String,
        quality: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        items: Option<Vec<u32>>,
        /// v0.2: [`VIDEO_FALLBACKS`]; absent = `lower`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_missing: Option<String>,
    },
    /// v0.2: open the desktop app's Video window with this page address and bring it to the
    /// front; the user chooses there (this video or the whole playlist, episodes, quality,
    /// folder). From the browser's button (owner's request 2026-11-20). The headless
    /// `eid serve` has no window and refuses it.
    OpenVideo {
        url: String,
    },
}

impl Command {
    /// The subset browser extensions may use through Native Messaging (least privilege).
    pub fn allowed_from_browser(&self) -> bool {
        matches!(
            self,
            Command::Ping {}
                | Command::Add { .. }
                | Command::Probe { .. }
                | Command::AddVideo { .. }
                | Command::AddPlaylist { .. }
                | Command::OpenVideo { .. }
        )
    }

    /// Commands that may take long (they run an external tool): served without holding the
    /// idempotency lock, on a blocking thread, with a longer client timeout.
    pub fn is_slow(&self) -> bool {
        matches!(self, Command::Probe { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub v: u32,
    /// Client-chosen idempotency key: the same id never runs its command twice (while the app
    /// that saw it is alive: ids are kept in memory only). The cache is global, keyed by this id
    /// alone, and also replays failures, so a client (the P6 extension in particular) must use a
    /// fresh unique id per user action and reuse one only to retry that very action.
    pub id: String,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub v: u32,
    pub id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: &str, result: serde_json::Value) -> Response {
        Response {
            v: VERSION,
            id: id.to_owned(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }
    pub fn err(id: &str, error: impl Into<String>) -> Response {
        Response {
            v: VERSION,
            id: id.to_owned(),
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

fn plain(s: &str, max: usize) -> bool {
    s.len() <= max && !s.chars().any(char::is_control)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

impl Request {
    /// Structural validation after parsing (limits that serde cannot express).
    pub fn validate(&self) -> Result<(), String> {
        if self.v != VERSION {
            return Err(format!("unsupported protocol version {}", self.v));
        }
        if !valid_id(&self.id) {
            return Err("invalid request id".into());
        }
        if let Command::Add {
            url,
            subdir,
            file_name,
        } = &self.command
        {
            if url.is_empty() || !plain(url, MAX_URL_LEN) {
                return Err("invalid url".into());
            }
            for (n, v) in [("subdir", subdir), ("file_name", file_name)] {
                if let Some(v) = v {
                    if v.is_empty() || !plain(v, MAX_NAME_LEN) {
                        return Err(format!("invalid {n}"));
                    }
                }
            }
        }
        let video_url = match &self.command {
            Command::Probe { url } | Command::OpenVideo { url } => Some(url),
            Command::AddVideo {
                url,
                quality,
                if_missing,
                ..
            }
            | Command::AddPlaylist {
                url,
                quality,
                if_missing,
                ..
            } => {
                if !VIDEO_QUALITIES.contains(&quality.as_str()) {
                    return Err("invalid quality".into());
                }
                if let Some(f) = if_missing {
                    if !VIDEO_FALLBACKS.contains(&f.as_str()) {
                        return Err("invalid if_missing".into());
                    }
                }
                Some(url)
            }
            _ => None,
        };
        if let Some(url) = video_url {
            let web = url.starts_with("https://") || url.starts_with("http://");
            if !web || !plain(url, MAX_URL_LEN) {
                return Err("invalid url".into());
            }
        }
        if let Command::AddVideo {
            format: Some(f),
            quality,
            ..
        } = &self.command
        {
            if !format_spec_ok(f) || (quality == "m4a" && f.contains('+')) {
                return Err("invalid format".into());
            }
            if !format_quality_ok(quality) {
                return Err("invalid format: exact formats go with a fixed height or m4a".into());
            }
        }
        if let Command::AddPlaylist {
            items: Some(items), ..
        } = &self.command
        {
            if items.is_empty() || items.len() > MAX_ITEMS || items.contains(&0) {
                return Err("invalid items".into());
            }
        }
        Ok(())
    }
}

/// Parse and validate a request frame. The error never echoes the input.
pub fn parse_request(bytes: &[u8]) -> Result<Request, String> {
    let r: Request = serde_json::from_slice(bytes).map_err(|_| "malformed request".to_string())?;
    r.validate()?;
    Ok(r)
}

pub fn parse_hello(bytes: &[u8]) -> Result<Hello, String> {
    let h: Hello = serde_json::from_slice(bytes).map_err(|_| "malformed hello".to_string())?;
    if h.v != VERSION {
        return Err(format!("unsupported protocol version {}", h.v));
    }
    if h.token.len() > MAX_TOKEN_LEN {
        return Err("malformed hello".into());
    }
    Ok(h)
}

/// v0.2: the qualities exact formats may go with: a fixed height, or `m4a` with one stream.
pub fn format_quality_ok(quality: &str) -> bool {
    quality == "m4a" || (quality.ends_with('p') && VIDEO_QUALITIES.contains(&quality))
}

/// v0.2: `video+audio` or `video`: one or two ids of 1-64 characters of `A-Z a-z 0-9 _ - .`,
/// not starting with `-` (the same rule as the media crate's `check_format_spec`).
pub fn format_spec_ok(s: &str) -> bool {
    let parts: Vec<&str> = s.split('+').collect();
    parts.len() <= 2
        && parts.iter().all(|id| {
            !id.is_empty()
                && id.len() <= 64
                && !id.starts_with('-')
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(json: &str) -> Result<Request, String> {
        parse_request(json.as_bytes())
    }

    #[test]
    fn valid_requests_parse() {
        let r = req(r#"{"v":1,"id":"a-1.b_2","command":{"cmd":"ping"}}"#).unwrap();
        assert_eq!(r.command, Command::Ping {});
        let r = req(r#"{"v":1,"id":"x","command":{"cmd":"add","url":"http://h/a.zip"}}"#).unwrap();
        assert!(matches!(r.command, Command::Add { subdir: None, .. }));
        let r = req(r#"{"v":1,"id":"x","command":{"cmd":"pause","item":7}}"#).unwrap();
        assert_eq!(r.command, Command::Pause { item: 7 });
        let r = req(r#"{"v":1,"id":"x","command":{"cmd":"show"}}"#).unwrap();
        assert_eq!(r.command, Command::Show {});
    }

    #[test]
    fn unknown_fields_wrong_version_and_bad_shapes_are_rejected() {
        for bad in [
            r#"{"v":1,"id":"x","command":{"cmd":"ping"},"extra":1}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"ping","extra":1}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add","url":"http://h/","dir":"C:\\"}}"#,
            r#"{"v":2,"id":"x","command":{"cmd":"ping"}}"#,
            r#"{"v":1,"command":{"cmd":"ping"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"nope"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"pause","item":-1}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"pause","item":"1"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add","url":""}}"#,
            r#"{"v":1,"id":"","command":{"cmd":"ping"}}"#,
            r#"{"v":1,"id":"a b","command":{"cmd":"ping"}}"#,
            r#"{"v":1,"id":"../x","command":{"cmd":"ping"}}"#,
            r#"[1,2]"#,
            r#""ping""#,
            r#"null"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add","url":"http://h/\u0000"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add","url":"http://h/","file_name":"a\nb"}}"#,
            "not json",
            "",
        ] {
            assert!(req(bad).is_err(), "should be rejected: {bad}");
        }
        let long_id = format!(
            r#"{{"v":1,"id":"{}","command":{{"cmd":"ping"}}}}"#,
            "a".repeat(MAX_ID_LEN + 1)
        );
        assert!(req(&long_id).is_err());
        let long_url = format!(
            r#"{{"v":1,"id":"x","command":{{"cmd":"add","url":"http://h/{}"}}}}"#,
            "a".repeat(MAX_URL_LEN)
        );
        assert!(req(&long_url).is_err());
        // deeply nested input is refused by serde_json, never a stack overflow
        let deep = format!("{}1{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(req(&deep).is_err());
    }

    #[test]
    fn errors_do_not_echo_the_input() {
        let e =
            req(r#"{"v":1,"id":"x","command":{"cmd":"add","url":"http://u:SECRET@h/","zzz":1}}"#)
                .unwrap_err();
        assert!(!e.contains("SECRET"), "{e}");
    }

    #[test]
    fn video_commands_parse_and_reject_bad_input() {
        let r = req(r#"{"v":1,"id":"x","command":{"cmd":"probe","url":"https://www.youtube.com/watch?v=a"}}"#).unwrap();
        assert!(r.command.is_slow());
        let r = req(r#"{"v":1,"id":"x","command":{"cmd":"add_video","url":"https://y.test/w","quality":"1080p"}}"#).unwrap();
        assert!(!r.command.is_slow());
        let r = req(r#"{"v":1,"id":"x","command":{"cmd":"add_playlist","url":"https://y.test/p","quality":"audio","items":[1,3]}}"#).unwrap();
        assert_eq!(
            r.command,
            Command::AddPlaylist {
                url: "https://y.test/p".into(),
                quality: "audio".into(),
                items: Some(vec![1, 3]),
                if_missing: None,
            }
        );
        // v0.2: optional "if the quality is missing"
        let r = req(r#"{"v":1,"id":"x","command":{"cmd":"add_video","url":"https://y.test/w","quality":"720p","if_missing":"higher"}}"#).unwrap();
        assert!(
            matches!(r.command, Command::AddVideo { if_missing: Some(ref f), .. } if f == "higher")
        );
        for bad in [
            r#"{"v":1,"id":"x","command":{"cmd":"add_video","url":"https://y.test/w","quality":"8640p"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add_video","url":"https://y.test/w","quality":"bv*+ba"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add_video","url":"https://y.test/w","quality":"720p","if_missing":"any"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add_video","url":"https://y.test/w","quality":"720p","if_missing":"--exec x"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add_playlist","url":"https://y.test/p","quality":"best","if_missing":7}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add_video","url":"https://y.test/w"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add_video","url":"file:///etc/passwd","quality":"best"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"probe","url":"javascript:alert(1)"}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"probe","url":""}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add_playlist","url":"https://y.test/p","quality":"best","items":[]}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add_playlist","url":"https://y.test/p","quality":"best","items":[0]}}"#,
            r#"{"v":1,"id":"x","command":{"cmd":"add_playlist","url":"https://y.test/p","quality":"best","dir":"C:\"}}"#,
        ] {
            assert!(req(bad).is_err(), "should be rejected: {bad}");
        }
        let many: Vec<u32> = (1..=(MAX_ITEMS as u32 + 1)).collect();
        let j = serde_json::json!({"v":1,"id":"x","command":{"cmd":"add_playlist","url":"https://y.test/p","quality":"best","items":many}});
        assert!(req(&j.to_string()).is_err());
    }

    /// v0.2 the download menu: `format` is one or two plain format ids, only with a fixed
    /// height or m4a; absent stays absent on the wire (older apps and extensions agree).
    #[test]
    fn add_video_format_is_one_or_two_plain_ids_with_a_fixed_height() {
        let r = req(r#"{"v":1,"id":"x","command":{"cmd":"add_video","url":"https://y.test/w","quality":"1080p","format":"299+140"}}"#).unwrap();
        assert!(
            matches!(r.command, Command::AddVideo { format: Some(ref f), .. } if f == "299+140")
        );
        assert!(req(r#"{"v":1,"id":"x","command":{"cmd":"add_video","url":"https://y.test/w","quality":"m4a","format":"140"}}"#).is_ok());
        assert!(req(r#"{"v":1,"id":"x","command":{"cmd":"add_video","url":"https://y.test/w","quality":"4320p","format":"402+140"}}"#).is_ok());
        let long = "9".repeat(65);
        for (q, f) in [
            ("1080p", "bv*+ba"),
            ("1080p", "1+2+3"),
            ("1080p", ""),
            ("1080p", "+"),
            ("1080p", "-f"),
            ("1080p", "299/140"),
            ("1080p", "299 140"),
            ("1080p", "299;rm"),
            ("1080p", long.as_str()),
            ("best", "299+140"),
            ("audio", "140"),
        ] {
            let j = serde_json::json!({"v":1,"id":"x","command":{"cmd":"add_video","url":"https://y.test/w","quality":q,"format":f}});
            assert!(req(&j.to_string()).is_err(), "{q} {f:?} accepted");
        }
        let plain = serde_json::to_string(&Command::AddVideo {
            url: "https://y.test/w".into(),
            quality: "720p".into(),
            if_missing: None,
            format: None,
        })
        .unwrap();
        assert!(!plain.contains("format"), "{plain}");
    }

    #[test]
    fn browser_subset_is_only_ping_add_and_the_video_commands() {
        for c in [
            Command::Probe { url: "u".into() },
            Command::AddVideo {
                url: "u".into(),
                quality: "best".into(),
                if_missing: None,
                format: None,
            },
            Command::AddPlaylist {
                url: "u".into(),
                quality: "best".into(),
                items: None,
                if_missing: None,
            },
        ] {
            assert!(c.allowed_from_browser());
        }
        assert!(Command::Ping {}.allowed_from_browser());
        assert!(Command::Add {
            url: "u".into(),
            subdir: None,
            file_name: None
        }
        .allowed_from_browser());
        assert!(!Command::List {}.allowed_from_browser());
        assert!(!Command::Shutdown {}.allowed_from_browser());
        assert!(!Command::Pause { item: 1 }.allowed_from_browser());
        assert!(!Command::Resume { item: 1 }.allowed_from_browser());
        assert!(!Command::Show {}.allowed_from_browser());
        assert!(Command::OpenVideo { url: "u".into() }.allowed_from_browser());
    }

    /// v0.2: open_video carries one http(s) page address, nothing else.
    #[test]
    fn open_video_is_one_checked_web_address() {
        let ok = br#"{"v":1,"id":"ext-1","command":{"cmd":"open_video","url":"https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL1"}}"#;
        assert_eq!(
            parse_request(ok).unwrap().command,
            Command::OpenVideo {
                url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL1".into()
            }
        );
        for bad in [
            &br#"{"v":1,"id":"ext-1","command":{"cmd":"open_video","url":"file:///C:/x"}}"#[..],
            br#"{"v":1,"id":"ext-1","command":{"cmd":"open_video","url":"javascript:alert(1)"}}"#,
            br#"{"v":1,"id":"ext-1","command":{"cmd":"open_video","url":"https://h/x","dir":"C:/"}}"#,
            br#"{"v":1,"id":"ext-1","command":{"cmd":"open_video"}}"#,
        ] {
            assert!(parse_request(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }
}
