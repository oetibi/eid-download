//! Quoting of strings printed as copy-pasteable commands. File names and paths come from the
//! network or the user and must never be able to run code when pasted into a shell.
//! (The engine itself never uses a shell; this is only for the *printed* resume hint.)

/// POSIX sh: single quotes; `'` becomes `'\''`.
pub fn posix(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// PowerShell: single-quoted literal; `'` doubles. PowerShell also treats the typographic
/// quotes U+2018..U+201B as quote characters, so they are doubled too.
pub fn powershell(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        out.push(c);
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// cmd.exe: inside double quotes `& | < > ^` are literal, but `"` cannot be escaped and `%VAR%`
/// / `!VAR!` expand even inside quotes. Such strings are refused (`None`): use PowerShell.
pub fn cmd(s: &str) -> Option<String> {
    if s.chars()
        .any(|c| matches!(c, '"' | '%' | '!' | '\r' | '\n' | '\0') || c.is_control())
    {
        return None;
    }
    // a trailing backslash would escape the closing quote for the C runtime parser
    let mut t = s.to_string();
    if t.ends_with('\\') {
        t.push('\\');
    }
    Some(format!("\"{t}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_quotes() {
        assert_eq!(posix("a b"), "'a b'");
        assert_eq!(posix("it's"), "'it'\\''s'");
    }

    #[test]
    fn powershell_quotes() {
        assert_eq!(powershell("it's"), "'it''s'");
        assert_eq!(powershell("\u{2019}"), "'\u{2019}\u{2019}'");
    }

    #[test]
    fn cmd_refuses_unsafe() {
        assert!(cmd("a%PATH%b").is_none());
        assert!(cmd("a\"b").is_none());
        assert_eq!(cmd("C:\\x y\\").unwrap(), "\"C:\\x y\\\\\"");
        assert_eq!(cmd("a&b|c").unwrap(), "\"a&b|c\"");
    }
}
