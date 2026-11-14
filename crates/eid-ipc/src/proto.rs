//! Wire messages. Every type rejects unknown fields; every string is bounded; the protocol
//! version is checked. The same `Request`/`Response` are used on the pipe and (without the
//! handshake) in browser Native Messaging.

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const MAX_ID_LEN: usize = 64;
pub const MAX_URL_LEN: usize = 8192;
pub const MAX_NAME_LEN: usize = 255;
pub const MAX_TOKEN_LEN: usize = 128;

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
    Pause {
        item: u64,
    },
    Resume {
        item: u64,
    },
}

impl Command {
    /// The subset browser extensions may use through Native Messaging (least privilege).
    pub fn allowed_from_browser(&self) -> bool {
        matches!(self, Command::Ping {} | Command::Add { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub v: u32,
    /// Client-chosen idempotency key: the same id never runs its command twice.
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
    fn browser_subset_is_only_ping_and_add() {
        assert!(Command::Ping {}.allowed_from_browser());
        assert!(Command::Add {
            url: "u".into(),
            subdir: None,
            file_name: None
        }
        .allowed_from_browser());
        assert!(!Command::List {}.allowed_from_browser());
        assert!(!Command::Pause { item: 1 }.allowed_from_browser());
        assert!(!Command::Resume { item: 1 }.allowed_from_browser());
    }
}
