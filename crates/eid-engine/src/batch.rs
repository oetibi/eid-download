//! Batch URL patterns: `https://h/img(001-050).jpg`, `(a-e)`, several groups, and the `*` form.
//! Everything is bounded: a hostile pattern cannot ask for a billion URLs.

/// Most URLs one pattern may expand to.
pub const MAX_EXPANSION: usize = 10_000;
/// Most `(..-..)` groups in one pattern.
pub const MAX_GROUPS: usize = 3;
/// A pattern longer than this is refused.
pub const MAX_PATTERN_BYTES: usize = 8192;

#[derive(Debug, PartialEq, Eq)]
pub enum BatchError {
    TooLong,
    TooMany { limit: usize },
    TooManyGroups,
    Reversed(String),
    NumberTooBig(String),
}

impl std::fmt::Display for BatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BatchError::TooLong => write!(f, "pattern too long"),
            BatchError::TooMany { limit } => write!(f, "pattern expands to more than {limit} URLs"),
            BatchError::TooManyGroups => write!(f, "more than {MAX_GROUPS} ranges in one pattern"),
            BatchError::Reversed(r) => write!(f, "range ({r}) runs backwards"),
            BatchError::NumberTooBig(r) => write!(f, "range ({r}) has a number that is too large"),
        }
    }
}
impl std::error::Error for BatchError {}

#[derive(Debug)]
enum Group {
    /// from, to (inclusive), zero-padding width
    Num(u32, u32, usize),
    /// single ASCII letters, same case, inclusive
    Alpha(u8, u8),
}

impl Group {
    fn len(&self) -> u64 {
        match self {
            Group::Num(a, b, _) => u64::from(b - a) + 1,
            Group::Alpha(a, b) => u64::from(b - a) + 1,
        }
    }
    fn nth(&self, i: u64) -> String {
        match self {
            Group::Num(a, _, w) => format!("{:0w$}", u64::from(*a) + i, w = *w),
            Group::Alpha(a, _) => ((*a + i as u8) as char).to_string(),
        }
    }
}

/// Recognise `(<digits>-<digits>)` or `(<letter>-<letter>)` at the start of `s`.
/// Returns the group and the number of bytes consumed. Anything else (e.g. `(1)`, `(x-yz)`) is
/// ordinary text, so real file names with parentheses are left alone.
fn parse_group(s: &str) -> Option<Result<(Group, usize), BatchError>> {
    let end = s.find(')')?;
    let inner = &s[1..end];
    let (a, b) = inner.split_once('-')?;
    let consumed = end + 1;
    if !a.is_empty()
        && !b.is_empty()
        && a.bytes().all(|c| c.is_ascii_digit())
        && b.bytes().all(|c| c.is_ascii_digit())
    {
        if a.len() > 9 || b.len() > 9 {
            return Some(Err(BatchError::NumberTooBig(inner.to_string())));
        }
        let (x, y): (u32, u32) = (a.parse().ok()?, b.parse().ok()?);
        if y < x {
            return Some(Err(BatchError::Reversed(inner.to_string())));
        }
        // 0-padding only when the start has a leading zero: (001-050) -> 001, (1-10) -> 1
        let width = if a.len() > 1 && a.starts_with('0') {
            a.len()
        } else {
            0
        };
        return Some(Ok((Group::Num(x, y, width), consumed)));
    }
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    if ab.len() == 1
        && bb.len() == 1
        && ab[0].is_ascii_alphabetic()
        && bb[0].is_ascii_alphabetic()
        && ab[0].is_ascii_lowercase() == bb[0].is_ascii_lowercase()
    {
        if bb[0] < ab[0] {
            return Some(Err(BatchError::Reversed(inner.to_string())));
        }
        return Some(Ok((Group::Alpha(ab[0], bb[0]), consumed)));
    }
    None
}

/// Expand a pattern into concrete strings (URL validation is the caller's job). A pattern with no
/// range group expands to itself.
pub fn expand(pattern: &str) -> Result<Vec<String>, BatchError> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(BatchError::TooLong);
    }
    // split into literal pieces and groups
    let mut pieces: Vec<String> = vec![String::new()];
    let mut groups: Vec<Group> = Vec::new();
    let mut i = 0;
    while i < pattern.len() {
        let rest = &pattern[i..];
        if rest.starts_with('(') {
            match parse_group(rest) {
                Some(Ok((g, used))) => {
                    if groups.len() == MAX_GROUPS {
                        return Err(BatchError::TooManyGroups);
                    }
                    groups.push(g);
                    pieces.push(String::new());
                    i += used;
                    continue;
                }
                Some(Err(e)) => return Err(e),
                None => {}
            }
        }
        let ch = rest.chars().next().expect("non-empty rest");
        pieces.last_mut().expect("one piece").push(ch);
        i += ch.len_utf8();
    }
    let mut total: u64 = 1;
    for g in &groups {
        total = total.checked_mul(g.len()).unwrap_or(u64::MAX);
        if total > MAX_EXPANSION as u64 {
            return Err(BatchError::TooMany {
                limit: MAX_EXPANSION,
            });
        }
    }
    let mut out = Vec::with_capacity(total as usize);
    for n in 0..total {
        // the LAST group varies fastest (img(1-2)x(a-b) -> 1a 1b 2a 2b)
        let mut idx = vec![0u64; groups.len()];
        let mut rem = n;
        for (k, g) in groups.iter().enumerate().rev() {
            idx[k] = rem % g.len();
            rem /= g.len();
        }
        let mut s = String::new();
        for (k, piece) in pieces.iter().enumerate() {
            s.push_str(piece);
            if let Some(g) = groups.get(k) {
                s.push_str(&g.nth(idx[k]));
            }
        }
        out.push(s);
    }
    Ok(out)
}

/// The `*` form: every `*` in `pattern` is replaced by the numbers `from..=to`, zero-padded to
/// `width` digits (`0` = no padding).
pub fn expand_star(
    pattern: &str,
    from: u32,
    to: u32,
    width: usize,
) -> Result<Vec<String>, BatchError> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(BatchError::TooLong);
    }
    if to < from {
        return Err(BatchError::Reversed(format!("{from}-{to}")));
    }
    let count = u64::from(to - from) + 1;
    if count > MAX_EXPANSION as u64 {
        return Err(BatchError::TooMany {
            limit: MAX_EXPANSION,
        });
    }
    let width = width.min(9);
    Ok((from..=to)
        .map(|n| pattern.replace('*', &format!("{n:0width$}")))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_numeric_alpha_and_multi_group_patterns() {
        let v = expand("http://h/img(001-005).jpg").unwrap();
        assert_eq!(
            v,
            ["001", "002", "003", "004", "005"].map(|n| format!("http://h/img{n}.jpg"))
        );
        assert_eq!(expand("a(8-11)b").unwrap(), ["a8b", "a9b", "a10b", "a11b"]);
        assert_eq!(
            expand("a(08-11)b").unwrap(),
            ["a08b", "a09b", "a10b", "a11b"]
        );
        assert_eq!(expand("(a-c)").unwrap(), ["a", "b", "c"]);
        assert_eq!(expand("(X-Z)").unwrap(), ["X", "Y", "Z"]);
        // last group varies fastest
        assert_eq!(
            expand("i(1-2)x(a-b)").unwrap(),
            ["i1xa", "i1xb", "i2xa", "i2xb"]
        );
        assert_eq!(expand("(7-7)").unwrap(), ["7"]);
        // 001-050 really is 50 padded entries
        let v = expand("(001-050)").unwrap();
        assert_eq!((v.len(), v[0].as_str(), v[49].as_str()), (50, "001", "050"));
        // Arabic text around a group is preserved
        assert_eq!(expand("ملف(1-2).pdf").unwrap(), ["ملف1.pdf", "ملف2.pdf"]);
    }

    #[test]
    fn batch_literal_parentheses_are_left_alone() {
        for lit in [
            "movie (1).mp4",
            "a(b).txt",
            "x(1-)y",
            "x(-1)y",
            "x(ab-cd)y",
            "(1-2",
            "()",
            "x(a-1)y",
            "x(A-b)y",
        ] {
            assert_eq!(expand(lit).unwrap(), [lit], "{lit}");
        }
    }

    #[test]
    fn batch_hostile_patterns_are_bounded() {
        assert_eq!(
            expand("(0-99999)"),
            Err(BatchError::TooMany {
                limit: MAX_EXPANSION
            })
        );
        assert_eq!(
            expand("(1-100)(1-100)(1-2)"),
            Err(BatchError::TooMany {
                limit: MAX_EXPANSION
            })
        );
        assert!(expand("(0-9999)").is_ok());
        assert_eq!(
            expand("(0-4294967295)"),
            Err(BatchError::NumberTooBig("0-4294967295".into()))
        );
        assert_eq!(
            expand("(0-99999999999999999999)"),
            Err(BatchError::NumberTooBig("0-99999999999999999999".into()))
        );
        assert_eq!(expand("(5-1)"), Err(BatchError::Reversed("5-1".into())));
        assert_eq!(expand("(c-a)"), Err(BatchError::Reversed("c-a".into())));
        assert_eq!(
            expand("(1-2)(1-2)(1-2)(1-2)"),
            Err(BatchError::TooManyGroups)
        );
        assert_eq!(
            expand(&"x".repeat(MAX_PATTERN_BYTES + 1)),
            Err(BatchError::TooLong)
        );
        // many groups of size 1 are cheap but still capped by count
        assert!(expand(&"(1-1)".repeat(3)).is_ok());
    }

    #[test]
    fn batch_star_form_pads_and_is_bounded() {
        assert_eq!(
            expand_star("http://h/*/p*.png", 9, 11, 2).unwrap(),
            [
                "http://h/09/p09.png",
                "http://h/10/p10.png",
                "http://h/11/p11.png"
            ]
        );
        assert_eq!(expand_star("x*", 1, 3, 0).unwrap(), ["x1", "x2", "x3"]);
        assert_eq!(
            expand_star("no star", 1, 2, 0).unwrap(),
            ["no star", "no star"]
        );
        assert!(matches!(
            expand_star("x*", 3, 1, 0),
            Err(BatchError::Reversed(_))
        ));
        assert!(matches!(
            expand_star("x*", 0, u32::MAX, 0),
            Err(BatchError::TooMany { .. })
        ));
    }
}
