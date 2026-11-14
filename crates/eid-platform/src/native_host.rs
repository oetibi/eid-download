//! Registration of the Native Messaging host with browsers (per user, never admin).
//!
//! * Chrome/Edge manifest: `allowed_origins` = `chrome-extension://<id>/` (the browser refuses to
//!   start the host for any other extension);
//! * Firefox manifest: `allowed_extensions`;
//! * Windows: a key under `HKCU\Software\...\NativeMessagingHosts\<name>` whose default value is
//!   the manifest path; Linux: the manifest file in the browser's per-user directory.
//!
//! Every identifier is validated before it reaches a file or the registry.

use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Chrome,
    Edge,
    Firefox,
}

impl Browser {
    fn tag(self) -> &'static str {
        match self {
            Browser::Chrome => "chrome",
            Browser::Edge => "edge",
            Browser::Firefox => "firefox",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Host name, e.g. `com.eid.download`: lowercase letters, digits, `_` and `.`.
    pub name: String,
    pub description: String,
    /// Absolute path of `eid-native-host`.
    pub exe: PathBuf,
    /// Chrome/Edge extension ids (32 chars a-p).
    pub chrome_extension_ids: Vec<String>,
    /// Firefox extension ids (`name@domain` or `{uuid}`).
    pub firefox_extension_ids: Vec<String>,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

fn valid_host_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && !n.starts_with('.')
        && !n.ends_with('.')
        && !n.contains("..")
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

fn valid_chrome_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

fn valid_firefox_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b'-' | b'{' | b'}')
        })
}

impl Manifest {
    pub fn validate(&self) -> io::Result<()> {
        if !valid_host_name(&self.name) {
            return Err(bad("invalid native host name"));
        }
        if !self.exe.is_absolute() {
            return Err(bad("the native host path must be absolute"));
        }
        let p = self.exe.to_string_lossy();
        if p.chars().any(char::is_control) {
            return Err(bad("control character in the native host path"));
        }
        if self.description.chars().any(char::is_control) || self.description.len() > 200 {
            return Err(bad("invalid description"));
        }
        if self
            .chrome_extension_ids
            .iter()
            .any(|i| !valid_chrome_id(i))
        {
            return Err(bad("invalid Chrome/Edge extension id"));
        }
        if self
            .firefox_extension_ids
            .iter()
            .any(|i| !valid_firefox_id(i))
        {
            return Err(bad("invalid Firefox extension id"));
        }
        Ok(())
    }

    /// The manifest JSON for `browser`. Fails if that browser would be left with no allowed
    /// extension (an empty list would let nobody — or, worse in other tools, everybody — in).
    pub fn json(&self, browser: Browser) -> io::Result<String> {
        self.validate()?;
        let mut m = serde_json::Map::new();
        m.insert("name".into(), self.name.clone().into());
        m.insert("description".into(), self.description.clone().into());
        m.insert(
            "path".into(),
            self.exe.to_string_lossy().into_owned().into(),
        );
        m.insert("type".into(), "stdio".into());
        match browser {
            Browser::Chrome | Browser::Edge => {
                if self.chrome_extension_ids.is_empty() {
                    return Err(bad("no Chrome/Edge extension id given"));
                }
                let o: Vec<_> = self
                    .chrome_extension_ids
                    .iter()
                    .map(|i| format!("chrome-extension://{i}/"))
                    .collect();
                m.insert("allowed_origins".into(), o.into());
            }
            Browser::Firefox => {
                if self.firefox_extension_ids.is_empty() {
                    return Err(bad("no Firefox extension id given"));
                }
                m.insert(
                    "allowed_extensions".into(),
                    self.firefox_extension_ids.clone().into(),
                );
            }
        }
        serde_json::to_string_pretty(&m).map_err(io::Error::other)
    }
}

fn write_manifest(path: &Path, json: &str) -> io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut f = crate::fs::create_new_file(&tmp)?;
    f.write_all(json.as_bytes())?;
    f.sync_all()?;
    drop(f);
    crate::fs::replace_atomic(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Where a browser looks, for the report: a file path (Linux) or a registry key (Windows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    pub browser: Browser,
    pub manifest: PathBuf,
    pub location: String,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    fn w(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    pub fn key_path(browser: Browser, name: &str) -> String {
        let base = match browser {
            Browser::Chrome => "Software\\Google\\Chrome\\NativeMessagingHosts",
            Browser::Edge => "Software\\Microsoft\\Edge\\NativeMessagingHosts",
            Browser::Firefox => "Software\\Mozilla\\NativeMessagingHosts",
        };
        format!("{base}\\{name}")
    }

    pub fn register(browser: Browser, name: &str, manifest: &Path) -> io::Result<String> {
        let key = key_path(browser, name);
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated wide strings; `h` is closed below.
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                w(key.as_ref()).as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut h,
                std::ptr::null_mut(),
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        let value = w(manifest.as_os_str());
        // SAFETY: `value` is a NUL-terminated UTF-16 buffer of the stated byte length.
        let rc = unsafe {
            RegSetValueExW(
                h,
                std::ptr::null(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        };
        // SAFETY: `h` was opened above.
        unsafe { RegCloseKey(h) };
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        Ok(format!("HKCU\\{key}"))
    }

    pub fn unregister(browser: Browser, name: &str) -> io::Result<()> {
        let key = key_path(browser, name);
        // SAFETY: valid NUL-terminated wide string.
        let rc = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, w(key.as_ref()).as_ptr()) };
        match rc {
            ERROR_SUCCESS => Ok(()),
            2 => Ok(()), // ERROR_FILE_NOT_FOUND: already gone
            e => Err(io::Error::from_raw_os_error(e as i32)),
        }
    }

    /// Read the default value of the registered key back (tests and diagnostics).
    pub fn read_back(browser: Browser, name: &str) -> io::Result<Option<String>> {
        use windows_sys::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_SZ};
        let key = key_path(browser, name);
        let mut len = 0u32;
        // SAFETY: size query then read into a buffer of the reported size.
        unsafe {
            let rc = RegGetValueW(
                HKEY_CURRENT_USER,
                w(key.as_ref()).as_ptr(),
                std::ptr::null(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut len,
            );
            if rc == 2 {
                return Ok(None);
            }
            if rc != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
            let mut buf = vec![0u16; (len as usize).div_ceil(2)];
            let rc = RegGetValueW(
                HKEY_CURRENT_USER,
                w(key.as_ref()).as_ptr(),
                std::ptr::null(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            );
            if rc != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
            while buf.last() == Some(&0) {
                buf.pop();
            }
            Ok(Some(String::from_utf16_lossy(&buf)))
        }
    }
}

/// Write the manifests into `manifest_dir` and register them for `browsers`.
/// Windows: registry keys under HKCU. Elsewhere: the per-user browser directory under `$HOME`.
pub fn register(
    manifest: &Manifest,
    browsers: &[Browser],
    manifest_dir: &Path,
) -> io::Result<Vec<Registered>> {
    #[cfg(windows)]
    {
        register_with(manifest, browsers, manifest_dir, |b, name, path| {
            imp::register(b, name, path)
        })
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
            .ok_or_else(|| bad("no HOME directory"))?;
        register_in_home(manifest, browsers, manifest_dir, &home)
    }
}

/// Remove what [`register`] created for `browsers` (manifest files are left in place).
pub fn unregister(manifest: &Manifest, browsers: &[Browser]) -> io::Result<()> {
    if !valid_host_name(&manifest.name) {
        return Err(bad("invalid native host name"));
    }
    #[cfg(windows)]
    {
        for b in browsers {
            imp::unregister(*b, &manifest.name)?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| bad("no HOME directory"))?;
        for b in browsers {
            let p = unix_location(*b, &home, &manifest.name);
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

fn register_with(
    manifest: &Manifest,
    browsers: &[Browser],
    manifest_dir: &Path,
    mut locate: impl FnMut(Browser, &str, &Path) -> io::Result<String>,
) -> io::Result<Vec<Registered>> {
    manifest.validate()?;
    if !manifest_dir.is_absolute() {
        return Err(bad("the manifest directory must be absolute"));
    }
    // build every manifest first: a missing id must not leave a half-registered state
    let mut jobs = vec![];
    for b in browsers {
        jobs.push((*b, manifest.json(*b)?));
    }
    let mut done = vec![];
    for (b, json) in jobs {
        let path = manifest_dir.join(format!("{}.{}.json", manifest.name, b.tag()));
        write_manifest(&path, &json)?;
        let location = locate(b, &manifest.name, &path)?;
        done.push(Registered {
            browser: b,
            manifest: path,
            location,
        });
    }
    Ok(done)
}

#[cfg(not(windows))]
fn unix_location(b: Browser, home: &Path, name: &str) -> PathBuf {
    let dir = match b {
        Browser::Chrome => ".config/google-chrome/NativeMessagingHosts",
        Browser::Edge => ".config/microsoft-edge/NativeMessagingHosts",
        Browser::Firefox => ".mozilla/native-messaging-hosts",
    };
    home.join(dir).join(format!("{name}.json"))
}

#[cfg(not(windows))]
fn register_in_home(
    manifest: &Manifest,
    browsers: &[Browser],
    manifest_dir: &Path,
    home: &Path,
) -> io::Result<Vec<Registered>> {
    // On Linux the browser reads the manifest from its own directory, so the file *is* the
    // registration (the copy in `manifest_dir` is the canonical one).
    register_with(manifest, browsers, manifest_dir, |b, name, canonical| {
        let target = unix_location(b, home, name);
        let json = std::fs::read_to_string(canonical)?;
        write_manifest(&target, &json)?;
        Ok(target.to_string_lossy().into_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME_ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    fn manifest(dir: &Path) -> Manifest {
        Manifest {
            name: "com.eid.download".into(),
            description: "Eid Download native host".into(),
            exe: dir.join("eid-native-host.exe"),
            chrome_extension_ids: vec![CHROME_ID.into()],
            firefox_extension_ids: vec!["eid@example.org".into()],
        }
    }

    #[test]
    fn manifests_have_the_exact_browser_specific_shape() {
        let d = tempfile::tempdir().unwrap();
        let m = manifest(d.path());
        let c: serde_json::Value = serde_json::from_str(&m.json(Browser::Chrome).unwrap()).unwrap();
        assert_eq!(c["name"], "com.eid.download");
        assert_eq!(c["type"], "stdio");
        assert_eq!(
            c["allowed_origins"],
            serde_json::json!([format!("chrome-extension://{CHROME_ID}/")])
        );
        assert!(c.get("allowed_extensions").is_none());
        assert_eq!(c["path"], m.exe.to_string_lossy().as_ref());
        let e: serde_json::Value = serde_json::from_str(&m.json(Browser::Edge).unwrap()).unwrap();
        assert_eq!(e["allowed_origins"], c["allowed_origins"]);
        let f: serde_json::Value =
            serde_json::from_str(&m.json(Browser::Firefox).unwrap()).unwrap();
        assert_eq!(
            f["allowed_extensions"],
            serde_json::json!(["eid@example.org"])
        );
        assert!(f.get("allowed_origins").is_none());
    }

    #[test]
    fn hostile_or_incomplete_identifiers_are_refused() {
        let d = tempfile::tempdir().unwrap();
        let ok = manifest(d.path());
        for name in [
            "",
            "Com.Eid",
            "a b",
            "../x",
            ".a",
            "a.",
            "a..b",
            "a\\b",
            "a/b",
            &"x".repeat(65),
        ] {
            let mut m = ok.clone();
            m.name = name.into();
            assert!(m.validate().is_err(), "name {name:?}");
        }
        for id in [
            "",
            "short",
            "ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP",
            &"q".repeat(32),
            &"a".repeat(33),
            "*",
        ] {
            let mut m = ok.clone();
            m.chrome_extension_ids = vec![id.into()];
            assert!(m.validate().is_err(), "chrome id {id:?}");
        }
        for id in ["", "a b", "a\"b", "*", "a/b", "x\ny"] {
            let mut m = ok.clone();
            m.firefox_extension_ids = vec![id.into()];
            assert!(m.validate().is_err(), "firefox id {id:?}");
        }
        let mut m = ok.clone();
        m.exe = PathBuf::from("relative/eid-native-host.exe");
        assert!(m.validate().is_err());
        let mut m = ok.clone();
        m.exe = d.path().join("a\nb");
        assert!(m.validate().is_err());
        // an empty allow-list is an error, never a wildcard
        let mut m = ok.clone();
        m.chrome_extension_ids.clear();
        assert!(m.json(Browser::Chrome).is_err());
        let mut m = ok;
        m.firefox_extension_ids.clear();
        assert!(m.json(Browser::Firefox).is_err());
    }

    #[test]
    fn a_path_with_quotes_and_backslashes_survives_json_escaping() {
        let d = tempfile::tempdir().unwrap();
        let mut m = manifest(d.path());
        m.exe = d.path().join("we\"ird \\ dir").join("eid-native-host.exe");
        let v: serde_json::Value = serde_json::from_str(&m.json(Browser::Chrome).unwrap()).unwrap();
        assert_eq!(v["path"], m.exe.to_string_lossy().as_ref());
    }

    #[test]
    fn a_failure_for_one_browser_registers_nothing() {
        let d = tempfile::tempdir().unwrap();
        let mut m = manifest(d.path());
        m.firefox_extension_ids.clear();
        let dir = d.path().join("manifests");
        let r = register_with(&m, &[Browser::Chrome, Browser::Firefox], &dir, |_, _, _| {
            panic!("must not register anything")
        });
        assert!(r.is_err());
        assert!(!dir.exists(), "no manifest may be written");
    }

    #[cfg(windows)]
    #[test]
    fn windows_registers_under_hkcu_reads_back_and_unregisters() {
        let d = tempfile::tempdir().unwrap();
        let mut m = manifest(d.path());
        // a unique, test-only host name: never touches the real `com.eid.download` keys
        m.name = format!("com.eid.test{}", std::process::id());
        let dir = d.path().join("manifests");
        let all = [Browser::Chrome, Browser::Edge, Browser::Firefox];
        let r = register(&m, &all, &dir).unwrap();
        assert_eq!(r.len(), 3);
        for x in &r {
            assert!(x.location.starts_with("HKCU\\Software\\"), "{x:?}");
            let back = imp::read_back(x.browser, &m.name).unwrap().unwrap();
            assert_eq!(back, x.manifest.to_string_lossy());
            let text = std::fs::read_to_string(&x.manifest).unwrap();
            serde_json::from_str::<serde_json::Value>(&text).unwrap();
        }
        // idempotent
        register(&m, &all, &dir).unwrap();
        unregister(&m, &all).unwrap();
        for b in all {
            assert_eq!(imp::read_back(b, &m.name).unwrap(), None);
        }
        unregister(&m, &all).unwrap(); // already gone is fine
    }

    #[cfg(not(windows))]
    #[test]
    fn linux_writes_the_manifest_where_each_browser_looks() {
        let d = tempfile::tempdir().unwrap();
        let m = manifest(d.path());
        let (home, dir) = (d.path().join("home"), d.path().join("manifests"));
        let all = [Browser::Chrome, Browser::Edge, Browser::Firefox];
        let r = register_in_home(&m, &all, &dir, &home).unwrap();
        assert_eq!(r.len(), 3);
        for x in &r {
            let t = std::fs::read_to_string(&x.location).unwrap();
            assert_eq!(t, std::fs::read_to_string(&x.manifest).unwrap());
        }
        assert!(home
            .join(".config/google-chrome/NativeMessagingHosts/com.eid.download.json")
            .is_file());
        assert!(home
            .join(".mozilla/native-messaging-hosts/com.eid.download.json")
            .is_file());
    }
}
