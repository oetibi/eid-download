//! Small atomic JSON store for settings-like files (queues, ...): owner-only, checksummed,
//! `.bak` rotation, strict size bound, quarantine of corrupt files. Unlike `state::load`, a
//! READ ERROR (sharing violation, permissions) never renames anything: only content that was read
//! successfully and found invalid is quarantined as `.broken` (PLAN P1 open item (a), applied to
//! new code from the start).

use crate::error::{Error, Result};
use eid_platform::fs as pfs;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    version: u32,
    /// SHA-256 (hex) of the JSON serialization of `payload`.
    checksum: String,
    payload: T,
}

fn aux(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn digest<T: Serialize>(payload: &T) -> Result<String> {
    let bytes = serde_json::to_vec(payload).map_err(|e| Error::BadState(e.to_string()))?;
    Ok(crate::state::hex(&Sha256::digest(bytes)))
}

/// Atomically write `payload` at `path` (previous generation kept as `<path>.bak`).
pub fn save<T: Serialize>(path: &Path, version: u32, payload: &T) -> Result<()> {
    let env = Envelope {
        version,
        checksum: digest(payload)?,
        payload,
    };
    let json = serde_json::to_vec(&env).map_err(|e| Error::BadState(e.to_string()))?;
    if json.len() as u64 > MAX_STORE_BYTES {
        return Err(Error::InvalidInput(
            "store would exceed its size limit".into(),
        ));
    }
    let tmp = aux(path, ".tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = pfs::create_new_private(&tmp).map_err(Error::io("create store tmp"))?;
        f.write_all(&json).map_err(Error::io("write store"))?;
        f.sync_all().map_err(Error::io("sync store"))?;
    }
    crate::fault::hit("store_tmp_written");
    if path.exists() {
        pfs::replace_atomic(path, &aux(path, ".bak")).map_err(Error::io("rotate store to .bak"))?;
        crate::fault::hit("store_moved_to_bak");
    }
    pfs::replace_atomic(&tmp, path).map_err(Error::io("replace store"))?;
    if let Some(dir) = path.parent() {
        let _ = pfs::sync_dir(dir);
    }
    Ok(())
}

enum Read_<T> {
    Missing,
    Io(std::io::Error),
    Invalid(String),
    Ok(T),
}

fn read_one<T: DeserializeOwned + Serialize>(
    path: &Path,
    version: u32,
    validate: &dyn Fn(&T) -> std::result::Result<(), String>,
) -> Read_<T> {
    let f = match pfs::open_regular_nofollow_read(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Read_::Missing,
        Err(e) => return Read_::Io(e),
    };
    let len = match f.metadata() {
        Ok(m) => m.len(),
        Err(e) => return Read_::Io(e),
    };
    if len > MAX_STORE_BYTES {
        return Read_::Invalid("file too large".into());
    }
    let mut buf = Vec::with_capacity(len as usize);
    if let Err(e) = f.take(MAX_STORE_BYTES + 1).read_to_end(&mut buf) {
        return Read_::Io(e);
    }
    if buf.len() as u64 > MAX_STORE_BYTES {
        return Read_::Invalid("file too large".into());
    }
    let env: Envelope<T> = match serde_json::from_slice(&buf) {
        Ok(e) => e,
        Err(e) => return Read_::Invalid(format!("not valid JSON for this store: {e}")),
    };
    if env.version != version {
        return Read_::Invalid(format!("unsupported version {}", env.version));
    }
    match digest(&env.payload) {
        Ok(d) if d == env.checksum => {}
        _ => return Read_::Invalid("checksum mismatch (file corrupted or edited)".into()),
    }
    if let Err(e) = validate(&env.payload) {
        return Read_::Invalid(e);
    }
    Read_::Ok(env.payload)
}

/// Load `path` (falling back to `.bak`). `Ok(None)` when neither exists (a fresh install).
/// `validate` is the caller's strict semantic check of the untrusted payload.
pub fn load<T: DeserializeOwned + Serialize>(
    path: &Path,
    version: u32,
    validate: &dyn Fn(&T) -> std::result::Result<(), String>,
) -> Result<Option<T>> {
    let bak = aux(path, ".bak");
    match read_one::<T>(path, version, validate) {
        Read_::Ok(v) => Ok(Some(v)),
        Read_::Io(e) => Err(Error::Io {
            context: "read store",
            source: e,
        }),
        Read_::Missing => match read_one::<T>(&bak, version, validate) {
            Read_::Ok(v) => Ok(Some(v)),
            Read_::Missing => Ok(None),
            Read_::Io(e) => Err(Error::Io {
                context: "read store backup",
                source: e,
            }),
            Read_::Invalid(why) => Err(Error::BadState(format!("backup: {why}"))),
        },
        Read_::Invalid(primary) => {
            // content was read and is bad: keep it for inspection and use the backup
            let _ = std::fs::rename(path, aux(path, ".broken"));
            match read_one::<T>(&bak, version, validate) {
                Read_::Ok(v) => Ok(Some(v)),
                Read_::Missing => Err(Error::BadState(primary)),
                Read_::Io(e) => Err(Error::Io {
                    context: "read store backup",
                    source: e,
                }),
                Read_::Invalid(b) => Err(Error::BadState(format!("{primary}; backup: {b}"))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct P {
        n: u32,
        names: Vec<String>,
    }
    fn ok(_: &P) -> std::result::Result<(), String> {
        Ok(())
    }
    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eid-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn store_roundtrip_private_and_missing_is_none() {
        let d = dir("rt");
        let p = d.join("q.json");
        assert_eq!(load::<P>(&p, 1, &ok).unwrap(), None);
        let v = P {
            n: 7,
            names: vec!["عربي".into(), "a b".into()],
        };
        save(&p, 1, &v).unwrap();
        save(&p, 1, &v).unwrap(); // rotates to .bak
        assert_eq!(load::<P>(&p, 1, &ok).unwrap(), Some(v));
        let want = (!eid_platform::IS_WINDOWS).then_some(true);
        assert_eq!(eid_platform::fs::owner_only(&p).unwrap(), want);
        assert_eq!(
            eid_platform::fs::owner_only(&aux(&p, ".bak")).unwrap(),
            want
        );
    }

    #[test]
    fn store_rejects_corrupt_tampered_huge_and_wrong_version_then_uses_the_backup() {
        let d = dir("bad");
        let p = d.join("q.json");
        let good = P {
            n: 1,
            names: vec!["x".into()],
        };
        save(&p, 1, &good).unwrap();
        let newer = P {
            n: 2,
            names: vec!["y".into()],
        };
        save(&p, 1, &newer).unwrap(); // primary = newer, .bak = good
                                      // truncated primary => backup (older generation) is used and the bad file is kept
        let full = std::fs::read(&p).unwrap();
        std::fs::write(&p, &full[..full.len() / 2]).unwrap();
        assert_eq!(load::<P>(&p, 1, &ok).unwrap(), Some(good.clone()));
        assert!(aux(&p, ".broken").exists());
        // tampered payload (checksum no longer matches), no backup left => error, never data
        let d2 = dir("tamper");
        let p2 = d2.join("q.json");
        save(&p2, 1, &good).unwrap();
        let text = String::from_utf8(std::fs::read(&p2).unwrap()).unwrap();
        std::fs::write(&p2, text.replace("\"n\":1", "\"n\":999")).unwrap();
        assert!(matches!(load::<P>(&p2, 1, &ok), Err(Error::BadState(_))));
        // wrong version
        let d3 = dir("ver");
        let p3 = d3.join("q.json");
        save(&p3, 1, &good).unwrap();
        assert!(matches!(load::<P>(&p3, 2, &ok), Err(Error::BadState(_))));
        // semantic validation failure
        let d4 = dir("val");
        let p4 = d4.join("q.json");
        save(&p4, 1, &good).unwrap();
        let strict = |p: &P| {
            (p.n != 1)
                .then_some(())
                .map_or(Err("n must not be 1".into()), |_| Ok(()))
        };
        assert!(matches!(
            load::<P>(&p4, 1, &strict),
            Err(Error::BadState(_))
        ));
        // oversized file is refused without being parsed
        let d5 = dir("huge");
        let p5 = d5.join("q.json");
        let f = std::fs::File::create(&p5).unwrap();
        f.set_len(MAX_STORE_BYTES + 1).unwrap();
        assert!(matches!(load::<P>(&p5, 1, &ok), Err(Error::BadState(_))));
        // a payload that would exceed the limit is refused at save time
        let big = P {
            n: 0,
            names: vec!["x".repeat(1 << 20); 17],
        };
        assert!(matches!(
            save(&d5.join("big.json"), 1, &big),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn store_never_follows_a_symlink() {
        let d = dir("link");
        let victim = d.join("victim.json");
        std::fs::write(&victim, b"{}").unwrap();
        let p = d.join("q.json");
        // AGENTS 6.1: this test FAILS when the link cannot be created
        eid_platform::fs::symlink_file(&victim, &p).expect("symlinks must be creatable");
        assert!(load::<P>(&p, 1, &ok).is_err());
        assert!(
            save(
                &p,
                1,
                &P {
                    n: 1,
                    names: vec![]
                }
            )
            .is_err()
                || {
                    // a save replaces the link itself (rename over it) and never writes through it
                    std::fs::read(&victim).unwrap() == b"{}"
                }
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"{}");
    }
}
