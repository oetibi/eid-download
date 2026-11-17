//! The installed video tools (`%LOCALAPPDATA%\EidDownload\tools`), P4 plan section 3.
//!
//! Layout: `<root>/set-<manifest id>-<random>/` holds one complete, verified set of tool files;
//! `<root>/current.json` names the set in use. Installing builds a new set in a fresh
//! `.staging-<random>` folder, verifies every file, renames the folder into place, and only
//! then switches `current.json` (atomic replace) and deletes the previous set; any failure
//! leaves the previous set in use. Tools are never looked up in `PATH`.
//!
//! Before every run [`ToolStore::lock`] opens each tool with a share-read-only handle,
//! hashes it **through that handle** and compares it with the manifest; the caller keeps the
//! [`LockedTools`] until the whole process tree has exited (D11).

use crate::args::ToolPaths;
use crate::fetch::{FetchRequest, Fetcher};
use crate::manifest::{Manifest, ToolFile};
use crate::MediaError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

const CURRENT: &str = "current.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Current {
    set: String,
    manifest_id: String,
}

#[derive(Clone, Debug)]
pub struct ToolStore {
    root: PathBuf,
}

/// Tool files opened share-read-only and verified; keep this alive until the process tree that
/// uses the tools has exited.
#[derive(Debug)]
pub struct LockedTools {
    pub paths: ToolPaths,
    handles: Vec<File>,
}

impl LockedTools {
    /// Number of locked files (for tests).
    pub fn locked_files(&self) -> usize {
        self.handles.len()
    }
}

/// 64 random bits (hex) for a fresh folder name; uniqueness is enforced anyway by
/// `create_dir`/`rename` failing on an existing name.
fn random_suffix() -> Result<String, MediaError> {
    Ok(eid_platform::ipc::new_token()?[..16].to_string())
}

fn is_set_name(s: &str) -> bool {
    s.starts_with("set-")
        && s.len() <= 80
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// SHA-256 of an open file, read from its start through this handle.
pub fn sha256_of_handle(f: &File) -> std::io::Result<String> {
    use std::io::{Seek, SeekFrom};
    let mut f = f;
    f.seek(SeekFrom::Start(0))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

impl ToolStore {
    pub fn new(root: impl Into<PathBuf>) -> ToolStore {
        ToolStore { root: root.into() }
    }

    /// `%LOCALAPPDATA%\EidDownload\tools` (Unix: `~/.local/share/eid-download/tools`).
    pub fn default_location() -> Option<ToolStore> {
        eid_platform::dirs::local_data_dir().map(|d| ToolStore::new(d.join("tools")))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn read_current(&self) -> Result<Option<Current>, MediaError> {
        let p = self.root.join(CURRENT);
        let f = match eid_platform::fs::open_regular_nofollow_read(&p) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut buf = vec![];
        f.take(4097).read_to_end(&mut buf)?;
        if buf.len() > 4096 {
            return Err(MediaError::BadToolPath("current.json too large".into()));
        }
        let c: Current = serde_json::from_slice(&buf)
            .map_err(|e| MediaError::BadToolPath(format!("current.json: {e}")))?;
        if !is_set_name(&c.set) {
            return Err(MediaError::BadToolPath(
                "current.json names an invalid folder".into(),
            ));
        }
        Ok(Some(c))
    }

    /// Paths of the installed set for `m`. Errors with [`MediaError::NotInstalled`] when no set
    /// is installed, the set was installed for a different manifest, or a file is missing.
    pub fn current(&self, m: &Manifest) -> Result<ToolPaths, MediaError> {
        let Some(c) = self.read_current()? else {
            return Err(MediaError::NotInstalled);
        };
        if c.manifest_id != m.id {
            return Err(MediaError::NotInstalled);
        }
        let dir = self.root.join(&c.set);
        // the set folder must be a real directory directly under the root, not a link
        match std::fs::symlink_metadata(&dir) {
            Ok(md) if md.is_dir() => {}
            _ => return Err(MediaError::NotInstalled),
        }
        let p = |n: &str| dir.join(n);
        for f in m.files() {
            if !p(&f.name).is_file() {
                return Err(MediaError::NotInstalled);
            }
        }
        let paths = ToolPaths {
            ytdlp: p("yt-dlp.exe"),
            ffmpeg: p("ffmpeg.exe"),
            deno: p("deno.exe"),
        };
        paths.validate()?;
        Ok(paths)
    }

    /// Open every tool of the current set share-read-only, hash it through the open handle
    /// and compare with the manifest. Blocking (hashes the files): call it from a blocking
    /// thread, see [`ToolStore::lock_blocking`].
    pub fn lock(&self, m: &Manifest) -> Result<LockedTools, MediaError> {
        let paths = self.current(m)?;
        self.drop_deno_links();
        let dir = paths.ytdlp.parent().expect("set folder").to_path_buf();
        let mut handles = vec![];
        for f in m.files() {
            let path = dir.join(&f.name);
            let h = eid_platform::fs::open_tool_locked(&path).map_err(|e| {
                if eid_platform::fs::is_sharing_violation(&e) {
                    MediaError::ToolBusy {
                        file: f.name.clone(),
                    }
                } else {
                    MediaError::Io(e)
                }
            })?;
            let md = h.metadata()?;
            if md.len() != f.size || sha256_of_handle(&h)? != f.sha256 {
                return Err(MediaError::HashMismatch {
                    file: f.name.clone(),
                });
            }
            handles.push(h);
        }
        Ok(LockedTools { paths, handles })
    }

    /// Remove the second name Deno gives `deno.exe` inside our `DENO_DIR`
    /// (`<root>/deno-cache/node_compat_bin`, see [`crate::env::DENO_CACHE_DIR`]) so that the
    /// lock below does not refuse the tool for it. Only that folder is removed; the rest of
    /// Deno's cache is kept. A link or file planted where the cache folder goes is removed as a
    /// name and never followed. Best effort: if the removal fails (for example because another
    /// run's Deno is using it), the lock refuses `deno.exe` as before, which is fail closed. A
    /// hard link anywhere else is never touched and is still refused.
    fn drop_deno_links(&self) {
        let cache = self.root.join(crate::env::DENO_CACHE_DIR);
        match std::fs::symlink_metadata(&cache) {
            Ok(md) if md.is_dir() && !md.file_type().is_symlink() => {
                // remove_dir_all does not follow links inside the tree
                let _ = std::fs::remove_dir_all(cache.join("node_compat_bin"));
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&cache).or_else(|_| std::fs::remove_dir(&cache));
            }
            Err(_) => {}
        }
    }

    /// [`ToolStore::lock`] on tokio's blocking pool.
    pub async fn lock_blocking(&self, m: &Manifest) -> Result<LockedTools, MediaError> {
        let (s, m) = (self.clone(), m.clone());
        tokio::task::spawn_blocking(move || s.lock(&m))
            .await
            .map_err(|e| MediaError::Io(std::io::Error::other(e)))?
    }

    /// Download, verify and install the set pinned by `m`. The previous set stays in use until
    /// the new one is complete.
    pub async fn install(
        &self,
        m: &Manifest,
        fetcher: &dyn Fetcher,
    ) -> Result<ToolPaths, MediaError> {
        self.install_cancellable(m, fetcher, &eid_engine::CancellationToken::new())
            .await
    }

    /// [`ToolStore::install`] that stops with [`MediaError::Cancelled`] when `cancel` fires:
    /// the running download is cancelled and awaited until it has released its files, then the
    /// staging folder is removed like after any other failure. The previous set stays current.
    /// Cancel through the token, never by dropping this future: a dropped future cannot clean up.
    pub async fn install_cancellable(
        &self,
        m: &Manifest,
        fetcher: &dyn Fetcher,
        cancel: &eid_engine::CancellationToken,
    ) -> Result<ToolPaths, MediaError> {
        m.validate()?;
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        std::fs::create_dir_all(&self.root)?;
        let staging = self.root.join(format!(".staging-{}", random_suffix()?));
        std::fs::create_dir(&staging)?;
        let r = self.install_into(m, fetcher, &staging, cancel).await;
        if r.is_err() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        r
    }

    async fn install_into(
        &self,
        m: &Manifest,
        fetcher: &dyn Fetcher,
        staging: &Path,
        cancel: &eid_engine::CancellationToken,
    ) -> Result<ToolPaths, MediaError> {
        let stop = || -> Result<(), MediaError> {
            if cancel.is_cancelled() {
                Err(MediaError::Cancelled)
            } else {
                Ok(())
            }
        };
        let dl = staging.join("dl");
        let out = staging.join("out");
        std::fs::create_dir(&dl)?;
        std::fs::create_dir(&out)?;
        for (i, t) in m.tools.iter().enumerate() {
            stop()?;
            let asset = format!("asset-{i}");
            let path = fetcher
                .fetch(FetchRequest {
                    url: &t.url,
                    dir: &dl,
                    file_name: &asset,
                    size: t.size,
                    sha256: &t.sha256,
                    allowed_hosts: &m.allowed_hosts,
                    cancel: cancel.clone(),
                })
                .await?;
            stop()?;
            let direct: Vec<&ToolFile> = t.files.iter().filter(|f| f.entry.is_none()).collect();
            let archived: Vec<&ToolFile> = t.files.iter().filter(|f| f.entry.is_some()).collect();
            if let [f] = direct.as_slice() {
                std::fs::rename(&path, out.join(&f.name))?;
            } else {
                let (p, files, o, c) = (
                    path.clone(),
                    archived.iter().map(|f| (*f).clone()).collect::<Vec<_>>(),
                    out.clone(),
                    cancel.clone(),
                );
                // awaited, never abandoned: it checks the token before every block, so a cancel
                // ends it within one 256 KiB read and it removes what it created
                tokio::task::spawn_blocking(move || {
                    let refs: Vec<&ToolFile> = files.iter().collect();
                    crate::zipx::extract_named_cancellable(&p, &refs, &o, &|| c.is_cancelled())
                })
                .await
                .map_err(|e| MediaError::Io(std::io::Error::other(e)))??;
                let _ = std::fs::remove_file(&path);
            }
        }
        stop()?;
        // verify the complete set once more, through handles, and make it executable
        let (o2, m2, c2) = (out.clone(), m.clone(), cancel.clone());
        tokio::task::spawn_blocking(move || -> Result<(), MediaError> {
            for f in m2.files() {
                if c2.is_cancelled() {
                    return Err(MediaError::Cancelled);
                }
                let h = eid_platform::fs::open_regular_nofollow(&o2.join(&f.name))?;
                if h.metadata()?.len() != f.size || sha256_of_handle(&h)? != f.sha256 {
                    return Err(MediaError::HashMismatch {
                        file: f.name.clone(),
                    });
                }
                eid_platform::fs::set_executable(&h)?;
                h.sync_all()?;
            }
            Ok(())
        })
        .await
        .map_err(|e| MediaError::Io(std::io::Error::other(e)))??;
        // last point to stop: from here the new set is switched in as one step
        stop()?;
        let previous = self.read_current().ok().flatten();
        let set = format!("set-{}-{}", m.id, random_suffix()?);
        std::fs::rename(&out, self.root.join(&set))?;
        let cur = serde_json::to_vec(&Current {
            set: set.clone(),
            manifest_id: m.id.clone(),
        })
        .map_err(|e| MediaError::Io(std::io::Error::other(e)))?;
        let tmp = staging.join("current.json.tmp");
        {
            use std::io::Write;
            let mut f = eid_platform::fs::create_new_file(&tmp)?;
            f.write_all(&cur)?;
            f.sync_all()?;
        }
        if let Err(e) = eid_platform::fs::replace_atomic(&tmp, &self.root.join(CURRENT)) {
            let _ = std::fs::remove_dir_all(self.root.join(&set));
            return Err(e.into());
        }
        let _ = std::fs::remove_dir_all(staging);
        if let Some(p) = previous {
            if p.set != set {
                // best effort: a running job may still hold the old files (Windows refuses)
                let _ = std::fs::remove_dir_all(self.root.join(p.set));
            }
        }
        self.current(m)
    }

    /// Delete every installed tool (consent withdrawn). A root that is a link is unlinked, never
    /// followed.
    pub fn remove_all(&self) -> Result<(), MediaError> {
        match std::fs::symlink_metadata(&self.root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
            Ok(md) if md.file_type().is_symlink() => {
                // a file link is removed with remove_file, a directory link or junction
                // (Windows) with remove_dir; neither follows the link
                std::fs::remove_file(&self.root)
                    .or_else(|_| std::fs::remove_dir(&self.root))
                    .map_err(Into::into)
            }
            Ok(md) if md.is_dir() => Ok(std::fs::remove_dir_all(&self.root)?),
            Ok(_) => Ok(std::fs::remove_file(&self.root)?),
        }
    }
}
