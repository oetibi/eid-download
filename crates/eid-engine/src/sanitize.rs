//! File-name sanitation. Everything that names a file on disk and comes from outside
//! (Content-Disposition, URL, extension, state file, imported list) goes through here.

/// Longest name (bytes, UTF-8) we ever produce; engine suffixes (`.eid-part`,
/// `.eid-state.tmp`, ` (999)`) stay far below the 255 limit of common filesystems.
pub const MAX_NAME_BYTES: usize = 200;

const FALLBACK: &str = "download";

fn is_forbidden_char(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        || c.is_control()
        // bidi controls & invisible formatting that can disguise an extension (U+202E ...)
        || matches!(c as u32,
            0x061C | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
            | 0xFEFF | 0x180E)
}

fn is_reserved_device(stem: &str) -> bool {
    let up = stem.trim_end_matches([' ', '.']).to_ascii_uppercase();
    matches!(up.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((up.starts_with("COM") || up.starts_with("LPT"))
            && up.len() == 4
            && matches!(up.as_bytes()[3], b'1'..=b'9'))
        // superscript digits COM¹ COM² COM³ LPT¹ ...
        || ((up.starts_with("COM") || up.starts_with("LPT"))
            && up.chars().count() == 4
            && matches!(up.chars().nth(3), Some('\u{b9}' | '\u{b2}' | '\u{b3}')))
}

fn truncate_bytes(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    // keep a short extension if there is one
    let (stem, ext) = match name.rfind('.') {
        Some(i) if name.len() - i <= 16 && i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let budget = max.saturating_sub(ext.len());
    let mut end = budget.min(stem.len());
    while end > 0 && !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &stem[..end], ext)
}

/// Turn untrusted text into a single safe file-name component. Never returns an empty
/// string, never contains a path separator, `..`, a Windows-forbidden char, a bidi override,
/// a reserved device name, or a trailing dot/space.
pub fn sanitize_file_name(raw: &str) -> String {
    // 1. last component only (both separator kinds), so "../../x" -> "x", "C:\\a\\b.txt" -> "b.txt"
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    // 2. replace forbidden chars (this also kills NTFS ADS "a.txt:stream" and drive prefixes)
    let mut s: String = base
        .chars()
        .map(|c| if is_forbidden_char(c) { '_' } else { c })
        .collect();
    // 3. no trailing dots/spaces, no leading spaces
    s = s
        .trim_end_matches([' ', '.'])
        .trim_start_matches(' ')
        .to_string();
    // 4. dots-only names
    if s.is_empty() || s.chars().all(|c| c == '.') {
        return FALLBACK.to_string();
    }
    // 5. reserved device names (with or without extension, "CON.txt", "con .txt")
    let stem = s.split('.').next().unwrap_or("");
    if is_reserved_device(stem) {
        s = format!("_{s}");
    }
    // 6. length
    let s = truncate_bytes(&s, MAX_NAME_BYTES);
    let s = s.trim_end_matches([' ', '.']).to_string();
    if s.is_empty() {
        FALLBACK.to_string()
    } else {
        s
    }
}

/// Longest sidecar suffix (`.eid-state.broken`) plus the longest collision suffix (` (999)`):
/// what a file name needs on top of itself so that every file of its job fits a path limit.
pub const NAME_PATH_RESERVE: usize = 17 + 6;

/// Shorten `name` (keeping a short extension) so that `dir_units + 1 + name + NAME_PATH_RESERVE`
/// stays within `limit` UTF-16 units. `Err` when the directory alone leaves no room for a useful
/// name (then the caller must refuse with a clear message, not fail later with an obscure OS error).
pub fn fit_name_to_path_limit(
    name: &str,
    dir_units: usize,
    limit: usize,
) -> Result<String, String> {
    const MIN_NAME_UNITS: usize = 12;
    let budget = limit
        .checked_sub(dir_units.saturating_add(1 + NAME_PATH_RESERVE))
        .filter(|b| *b >= MIN_NAME_UNITS)
        .ok_or_else(|| {
            format!(
                "the download folder path is too long ({dir_units} characters): Windows accepts at most {limit} \
                 characters per file path without long-path support; choose a shorter folder"
            )
        })?;
    let units = |s: &str| s.encode_utf16().count();
    if units(name) <= budget {
        return Ok(name.to_string());
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && units(&name[i..]) <= 16 && units(&name[i..]) < budget => {
            (&name[..i], &name[i..])
        }
        _ => (name, ""),
    };
    let mut keep = String::new();
    for c in stem.chars() {
        if units(&keep) + c.len_utf16() + units(ext) > budget {
            break;
        }
        keep.push(c);
    }
    let out = sanitize_file_name(&format!("{keep}{ext}"));
    debug_assert!(units(&out) <= budget);
    Ok(out)
}

/// Collision candidate `stem (n).ext` for an already-sanitized `name`. The bytes the suffix
/// needs are reserved BEFORE the stem is shortened, so the number can never be truncated away
/// (P1 review m1: a maximal stem used to lose its own ` (n)` and every candidate equalled the
/// occupied base name). Result is `<= MAX_NAME_BYTES`, valid UTF-8, and unique per `n`.
pub fn numbered_name(name: &str, n: u32) -> String {
    let (stem, ext) = match name.rfind('.') {
        // a normal extension only; a huge "extension" is part of the stem
        Some(i) if i > 0 && name.len() - i <= 16 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let suffix = format!(" ({n})");
    let budget = MAX_NAME_BYTES.saturating_sub(suffix.len() + ext.len());
    let mut end = budget.min(stem.len());
    while end > 0 && !stem.is_char_boundary(end) {
        end -= 1;
    }
    sanitize_file_name(&format!("{}{suffix}{ext}", &stem[..end]))
}

/// `true` if `name` is exactly what [`sanitize_file_name`] would produce, i.e. safe to trust
/// when read back from a state file.
pub fn is_safe_stored_name(name: &str) -> bool {
    !name.is_empty() && sanitize_file_name(name) == name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_safe(out: &str) {
        assert!(!out.is_empty());
        assert!(
            !out.contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*']),
            "{out:?}"
        );
        assert!(!out.ends_with(['.', ' ']), "{out:?}");
        assert!(out != "." && out != "..");
        assert!(out.len() <= MAX_NAME_BYTES);
        assert!(!out.contains('\u{202e}'));
        let stem = out.split('.').next().unwrap();
        assert!(!is_reserved_device(stem), "reserved: {out:?}");
        assert!(is_safe_stored_name(out), "not idempotent: {out:?}");
    }

    #[test]
    fn security_sanitize_traversal_and_absolute() {
        for (i, want) in [
            ("../../x", "x"),
            ("..\\..\\x.txt", "x.txt"),
            ("/etc/passwd", "passwd"),
            ("C:\\Windows\\system32\\evil.dll", "evil.dll"),
            ("\\\\server\\share\\f.bin", "f.bin"),
            ("\\\\?\\C:\\x\\y.zip", "y.zip"),
            ("a/../../b.txt", "b.txt"),
            ("..", "download"),
            ("...", "download"),
            ("", "download"),
            ("   ", "download"),
            ("./", "download"),
        ] {
            let o = sanitize_file_name(i);
            assert_eq!(o, want, "input {i:?}");
            assert_safe(&o);
        }
    }

    #[test]
    fn security_windows_reserved_names_and_ads_and_trailing() {
        for i in [
            "CON",
            "con",
            "NUL.txt",
            "aux.tar.gz",
            "COM1",
            "lpt9.log",
            "PRN ",
            "con .txt",
            "CON.",
            "COM\u{b9}",
            "CONIN$",
            "a.txt:ads",
            "a.txt:$DATA",
            "name:stream:$DATA",
            "a.txt.",
            "a.txt ",
            "a.txt. . ",
            "file<name>.txt",
            "q?*.zip",
            "tab\tname",
            "nul\0byte",
        ] {
            let o = sanitize_file_name(i);
            assert_safe(&o);
        }
        assert_eq!(sanitize_file_name("a.txt:ads"), "a.txt_ads");
        assert_eq!(sanitize_file_name("NUL.txt"), "_NUL.txt");
        assert_eq!(sanitize_file_name("a.txt. . "), "a.txt");
        assert_eq!(sanitize_file_name("CON"), "_CON");
    }

    #[test]
    fn security_rtl_override_and_control_chars_stripped() {
        let o = sanitize_file_name("invoice\u{202e}fdp.exe");
        assert!(!o.contains('\u{202e}'));
        assert_safe(&o);
        let o = sanitize_file_name("a\u{200f}b\u{2066}c\u{feff}.txt");
        assert_eq!(o, "a_b_c_.txt");
    }

    #[test]
    fn security_name_suffixes_stay_within_max_bytes_arabic() {
        for n in 190..=205 {
            // 2-byte Arabic letters + extension
            let name = format!("{}.zip", "ع".repeat(n / 2));
            let o = sanitize_file_name(&name);
            assert!(o.len() <= MAX_NAME_BYTES, "len {}", o.len());
            assert!(o.ends_with(".zip"));
            assert!(std::str::from_utf8(o.as_bytes()).is_ok());
            // engine suffixes keep us under 255
            assert!(o.len() + ".eid-state.tmp".len() + " (999)".len() < 255);
            assert_safe(&o);
        }
        let name = "ع".repeat(300);
        assert_safe(&sanitize_file_name(&name));
    }

    #[test]
    fn names_are_fitted_to_a_path_limit_with_room_for_every_sidecar_and_suffix() {
        let units = |s: &str| s.encode_utf16().count();
        let limit = 259;
        for dir_units in [3usize, 50, 120, 200, 215] {
            for raw in [
                "a".repeat(200),
                format!("{}.zip", "b".repeat(196)),
                "ع".repeat(100),
                format!("{}.pdf", "ع".repeat(98)),
                format!("{}.tar.gz", "😀".repeat(40)),
                "short.txt".to_string(),
            ] {
                let base = sanitize_file_name(&raw);
                let fitted = fit_name_to_path_limit(&base, dir_units, limit).unwrap();
                assert!(is_safe_stored_name(&fitted), "{fitted:?}");
                // the longest file of the job: collision suffix + longest sidecar suffix
                let longest = dir_units + 1 + units(&numbered_name(&fitted, 999)) + 17;
                assert!(
                    longest <= limit,
                    "dir {dir_units}: {longest} > {limit} for {fitted:?}"
                );
                assert!(units(&fitted) <= units(&base));
                if raw.ends_with(".zip") || raw.ends_with(".pdf") {
                    assert!(
                        fitted.ends_with(&raw[raw.rfind('.').unwrap()..]),
                        "extension kept: {fitted:?}"
                    );
                }
                if units(&base) + dir_units + 1 + NAME_PATH_RESERVE <= limit {
                    assert_eq!(fitted, base, "a name that fits is untouched");
                }
            }
        }
        // a directory that leaves no room is a clear error, not a truncation to nothing
        let e = fit_name_to_path_limit("a.txt", 240, limit).unwrap_err();
        assert!(e.contains("too long"), "{e}");
        assert!(fit_name_to_path_limit("a.txt", usize::MAX, limit).is_err());
        assert!(fit_name_to_path_limit("a.txt", 0, 0).is_err());
    }

    #[test]
    fn numbered_names_keep_their_suffix_for_maximal_names() {
        let maximal = [
            "a".repeat(MAX_NAME_BYTES),                        // no extension
            format!("{}.zip", "a".repeat(MAX_NAME_BYTES - 4)), // short extension
            format!("{}.{}", "a".repeat(40), "e".repeat(MAX_NAME_BYTES - 41)), // huge "extension"
            "ع".repeat(MAX_NAME_BYTES / 2),                    // 2-byte letters, no extension
            format!("{}.pdf", "ع".repeat((MAX_NAME_BYTES - 4) / 2)),
            format!("{}.tar.gz", "😀".repeat(48)), // 4-byte letters
        ];
        for raw in maximal {
            let base = sanitize_file_name(&raw);
            assert!(base.len() <= MAX_NAME_BYTES);
            let mut seen = std::collections::HashSet::new();
            seen.insert(base.clone());
            for n in 1..1000u32 {
                let c = numbered_name(&base, n);
                assert!(c.len() <= MAX_NAME_BYTES, "n={n} len={}", c.len());
                assert_ne!(c, base, "candidate {n} equals the occupied base");
                assert!(c.contains(&format!(" ({n})")), "n={n}: {c:?}");
                assert!(is_safe_stored_name(&c), "{c:?}");
                assert!(seen.insert(c.clone()), "duplicate candidate {c:?}");
                // engine side files fit too
                assert!(c.len() + ".eid-state.tmp".len() < 255);
            }
        }
        // small names are untouched apart from the suffix
        assert_eq!(numbered_name("a.txt", 1), "a (1).txt");
        assert_eq!(numbered_name("archive.tar.gz", 2), "archive.tar (2).gz");
        assert_eq!(numbered_name(".bashrc", 3), ".bashrc (3)");
    }

    #[test]
    fn arabic_and_normal_names_are_preserved() {
        assert_eq!(sanitize_file_name("تقرير 2026.pdf"), "تقرير 2026.pdf");
        assert_eq!(sanitize_file_name("movie (1).mp4"), "movie (1).mp4");
        assert_eq!(sanitize_file_name(".bashrc"), ".bashrc");
    }

    #[test]
    fn stored_name_check_rejects_unsanitized() {
        assert!(!is_safe_stored_name("../x"));
        assert!(!is_safe_stored_name("CON"));
        assert!(!is_safe_stored_name(""));
        assert!(is_safe_stored_name("ok.bin"));
    }
}
