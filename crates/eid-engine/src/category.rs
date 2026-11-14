//! Automatic categories: file name extension (then MIME type) -> a sub-folder of the download
//! directory. Folder names are single sanitized components, so no rule — built in, edited by the
//! user or read from an imported file — can place a file outside the download directory.

use crate::sanitize::sanitize_file_name;
use std::path::{Path, PathBuf};

pub const MAX_CATEGORIES: usize = 64;
pub const MAX_EXTS_PER_CATEGORY: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Category {
    pub name: String,
    /// Lower-case extensions without the dot (`zip`, `tar.gz` is matched by its last part: `gz`).
    pub exts: Vec<String>,
    /// MIME prefixes such as `video/` or exact types such as `application/pdf`.
    pub mime: Vec<String>,
    /// One plain folder name (validated by [`Category::validate`]).
    pub folder: String,
}

impl Category {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() || self.name.len() > 64 || self.name.chars().any(char::is_control) {
            return Err("category name is empty, too long or has control characters".into());
        }
        if self.folder.is_empty() || sanitize_file_name(&self.folder) != self.folder {
            return Err(format!(
                "category folder {:?} is not a single plain folder name",
                self.folder
            ));
        }
        if self.exts.len() > MAX_EXTS_PER_CATEGORY || self.mime.len() > MAX_EXTS_PER_CATEGORY {
            return Err("too many extensions or MIME types in one category".into());
        }
        for e in &self.exts {
            if e.is_empty() || e.len() > 16 || !e.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return Err(format!("bad extension {e:?}"));
            }
            if e.bytes().any(|b| b.is_ascii_uppercase()) {
                return Err(format!("extension {e:?} must be lower-case"));
            }
        }
        for m in &self.mime {
            if m.is_empty() || m.len() > 100 || m.bytes().any(|b| b.is_ascii_control() || b == b' ')
            {
                return Err(format!("bad MIME type {m:?}"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Categorizer {
    pub rules: Vec<Category>,
    /// Used when nothing matches.
    pub fallback: Category,
}

fn cat(name: &str, folder: &str, exts: &[&str], mime: &[&str]) -> Category {
    Category {
        name: name.into(),
        exts: exts.iter().map(|s| s.to_string()).collect(),
        mime: mime.iter().map(|s| s.to_string()).collect(),
        folder: folder.into(),
    }
}

impl Default for Categorizer {
    fn default() -> Self {
        Categorizer {
            rules: vec![
                cat(
                    "Compressed",
                    "Compressed",
                    &[
                        "zip", "rar", "7z", "gz", "bz2", "xz", "tar", "tgz", "zst", "iso",
                    ],
                    &[
                        "application/zip",
                        "application/x-7z-compressed",
                        "application/gzip",
                    ],
                ),
                cat(
                    "Documents",
                    "Documents",
                    &[
                        "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "epub", "odt",
                        "rtf", "csv",
                    ],
                    &["application/pdf", "text/plain"],
                ),
                cat(
                    "Music",
                    "Music",
                    &["mp3", "flac", "wav", "aac", "ogg", "opus", "m4a", "wma"],
                    &["audio/"],
                ),
                cat(
                    "Video",
                    "Video",
                    &[
                        "mp4", "mkv", "avi", "mov", "webm", "wmv", "flv", "m4v", "ts",
                    ],
                    &["video/"],
                ),
                cat(
                    "Pictures",
                    "Pictures",
                    &[
                        "jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "tif", "tiff", "avif",
                    ],
                    &["image/"],
                ),
                cat(
                    "Programs",
                    "Programs",
                    &["exe", "msi", "msix", "apk", "deb", "rpm", "dmg", "appimage"],
                    &["application/x-msdownload"],
                ),
            ],
            fallback: cat("Other", "Other", &[], &[]),
        }
    }
}

fn last_extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() || ext.len() > 16 {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

impl Categorizer {
    pub fn validate(&self) -> Result<(), String> {
        if self.rules.len() > MAX_CATEGORIES {
            return Err("too many categories".into());
        }
        for c in self.rules.iter().chain(std::iter::once(&self.fallback)) {
            c.validate()?;
        }
        Ok(())
    }

    /// Extension first (the file name is what the user sees), then the MIME type
    /// (`Content-Type`, parameters ignored), then the fallback. First matching rule wins.
    pub fn classify(&self, file_name: &str, mime: Option<&str>) -> &Category {
        if let Some(ext) = last_extension(file_name) {
            if let Some(c) = self.rules.iter().find(|c| c.exts.iter().any(|e| *e == ext)) {
                return c;
            }
        }
        if let Some(m) = mime {
            let m = m
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if !m.is_empty() {
                if let Some(c) = self.rules.iter().find(|c| {
                    c.mime.iter().any(|p| {
                        if p.ends_with('/') {
                            m.starts_with(p.as_str())
                        } else {
                            m == *p
                        }
                    })
                }) {
                    return c;
                }
            }
        }
        &self.fallback
    }

    /// `<base>/<folder>`: the folder is re-sanitized as a belt-and-braces measure, so even a rule
    /// that skipped `validate` stays inside `base`.
    pub fn target_dir(&self, base: &Path, file_name: &str, mime: Option<&str>) -> PathBuf {
        let c = self.classify(file_name, mime);
        base.join(sanitize_file_name(&c.folder))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_by_extension_then_mime_then_fallback() {
        let c = Categorizer::default();
        c.validate().unwrap();
        for (name, mime, want) in [
            ("a.ZIP", None, "Compressed"),
            ("a.tar.gz", None, "Compressed"),
            ("تقرير.pdf", None, "Documents"),
            ("a.Mp3", None, "Music"),
            ("clip.mkv", Some("application/octet-stream"), "Video"),
            ("download", Some("video/mp4; charset=x"), "Video"),
            ("download", Some("IMAGE/PNG"), "Pictures"),
            ("download", Some("application/pdf"), "Documents"),
            ("download", None, "Other"),
            (".zip", None, "Other"), // a dotfile, not an extension
            ("noext.", None, "Other"),
            ("x.unknownext", Some("text/html"), "Other"),
            // the extension wins over a misleading MIME type
            ("setup.exe", Some("image/png"), "Programs"),
        ] {
            assert_eq!(c.classify(name, mime).name, want, "{name} {mime:?}");
        }
    }

    #[test]
    fn category_rules_that_try_to_escape_are_rejected_and_never_escape() {
        for bad in [
            "..",
            "../x",
            "a/b",
            "a\\b",
            "C:\\x",
            "/abs",
            "CON",
            "x:ads",
            "",
            "a\u{202e}b",
            "trail.",
        ] {
            let mut cat = Categorizer::default();
            cat.rules[0].folder = bad.into();
            assert!(cat.validate().is_err(), "{bad:?} must be rejected");
            // even unvalidated, target_dir stays one component inside base
            let base = std::path::Path::new("/dl");
            let t = cat.target_dir(base, "a.zip", None);
            assert_eq!(t.parent(), Some(base), "{bad:?} -> {t:?}");
            assert!(t.starts_with(base));
        }
        let mut c = Categorizer::default();
        c.rules[0].exts.push("Zip".into());
        assert!(c.validate().is_err(), "upper-case extension");
        c.rules[0].exts = vec!["a.b".into()];
        assert!(c.validate().is_err(), "dot inside an extension");
        c.rules[0].exts = (0..300).map(|i| format!("e{i}")).collect();
        assert!(c.validate().is_err(), "too many extensions");
        let mut c = Categorizer::default();
        c.rules
            .extend((0..100).map(|i| cat(&format!("c{i}"), &format!("f{i}"), &[], &[])));
        assert!(c.validate().is_err(), "too many categories");
    }
}
