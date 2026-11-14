//! Permanent security tests (docs/PLAN.md "permanent security tests").
mod common;
use common::*;
use eid_testserver::{Behavior, Content};

fn c() -> Content {
    Content::Bytes(b"payload-bytes".to_vec())
}

/// Server-suggested names must never escape the download directory or produce Windows-special
/// names, whatever the encoding.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn security_hostile_content_disposition_stays_inside_dir() {
    let hostile = [
        "attachment; filename=\"../../evil.txt\"",
        "attachment; filename=\"..\\\\..\\\\evil.txt\"",
        "attachment; filename=\"/etc/passwd\"",
        "attachment; filename=\"C:\\\\Windows\\\\win.ini\"",
        "attachment; filename=\"\\\\\\\\server\\\\share\\\\x.txt\"",
        "attachment; filename*=UTF-8''%2e%2e%2f%2e%2e%2fevil.txt",
        "attachment; filename*=UTF-8''..%5c..%5cevil.txt",
        "attachment; filename=\"CON.txt\"",
        "attachment; filename=\"nul\"",
        "attachment; filename=\"aux.tar.gz\"",
        "attachment; filename=\"LPT1\"",
        "attachment; filename=\"a.txt:hidden\"",
        "attachment; filename=\"a.txt::$DATA\"",
        "attachment; filename=\"trailing. \"",
        "attachment; filename=\"spaces   \"",
        "attachment; filename=\"fdp.\u{202e}txt.exe\"",
        "attachment; filename=\"\"",
        "attachment; filename=\".\"",
        "attachment; filename=\"..\"",
        "attachment; filename=\"semi;colon.bin\"",
    ];
    for h in hostile {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("downloads");
        std::fs::create_dir(&dir).unwrap();
        let (_srv, url) = serve(
            "/dl",
            c(),
            Behavior {
                content_disposition: Some(h.to_string()),
                ..Behavior::default()
            },
        )
        .await;
        let (o, _) = get(&url, &dir, fast_cfg(1))
            .await
            .unwrap_or_else(|e| panic!("{h}: {e}"));
        assert_eq!(o.path.parent().unwrap(), dir, "{h}");
        let files = only_files(&dir);
        assert_eq!(files.len(), 1, "{h}: {files:?}");
        let name = &files[0];
        let lower = name.to_ascii_lowercase();
        let stem = lower.split('.').next().unwrap_or("");
        assert!(
            !name.contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*', '\u{202e}']),
            "{h} -> {name:?}"
        );
        assert!(
            !["con", "prn", "aux", "nul", "lpt1", "com1"].contains(&stem),
            "{h} -> {name:?}"
        );
        assert!(
            !name.ends_with('.') && !name.ends_with(' ') && name != "." && name != "..",
            "{h} -> {name:?}"
        );
        assert_eq!(
            only_files(parent.path()),
            vec!["downloads".to_string()],
            "{h}: nothing escaped to the parent"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn security_hostile_url_path_names() {
    for p in [
        "/..%2f..%2fevil.txt",
        "/CON",
        "/a%3Ab",
        "/x.txt%20",
        "/%5c%5cserver%5cshare",
    ] {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        let (_srv, url) = serve(p, c(), Behavior::default()).await;
        let _ = get(&url, &dir, fast_cfg(1)).await;
        assert_eq!(only_files(parent.path()), vec!["d".to_string()], "{p}");
        for f in only_files(&dir) {
            assert!(
                !f.contains(['/', '\\', ':']) && !f.ends_with(' ') && !f.ends_with('.'),
                "{p} -> {f:?}"
            );
        }
    }
}

#[cfg(unix)]
mod links {
    use super::*;
    use std::os::unix::fs::symlink;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn security_symlink_at_target_names_is_never_followed() {
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, b"keep me").unwrap();
        let dir = tempfile::tempdir().unwrap();
        // planted links where the engine would create/rename
        symlink(&victim, dir.path().join("f.bin")).unwrap();
        symlink(&victim, dir.path().join("f (1).bin.eid-part")).unwrap();
        symlink(&victim, dir.path().join("f (2).bin.eid-state")).unwrap();
        let (_srv, url) = serve("/f.bin", c(), Behavior::default()).await;
        let (o, _) = get(&url, dir.path(), fast_cfg(1)).await.unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
        assert_eq!(std::fs::read(&o.path).unwrap(), b"payload-bytes");
        assert!(!std::fs::symlink_metadata(&o.path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn security_symlinked_part_file_is_refused_on_resume() {
        use eid_engine::{CancellationToken, Error};
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.bin");
        std::fs::write(&victim, vec![9u8; 4096]).unwrap();
        let (_srv, url) = serve(
            "/s.bin",
            Content::Generated {
                seed: 1,
                len: 8 << 20,
            },
            Behavior {
                throttle_bps: Some(2_000_000),
                ..Behavior::default()
            },
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            c2.cancel();
        });
        let r = eid_engine::download(
            eid_engine::JobSpec {
                url,
                dir: dir.path().into(),
                file_name: None,
            },
            fast_cfg(2),
            Log::default().sink(),
            cancel,
        )
        .await;
        assert!(matches!(r, Err(Error::Cancelled)));
        let part = dir.path().join("s.bin.eid-part");
        std::fs::remove_file(&part).unwrap();
        symlink(&victim, &part).unwrap();
        let sp = dir.path().join("s.bin.eid-state");
        let r = eid_engine::resume(
            &sp,
            fast_cfg(2),
            Log::default().sink(),
            CancellationToken::new(),
        )
        .await;
        assert!(r.is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), vec![9u8; 4096]);
    }
}

/// Review B4: runs on EVERY platform (it used to be Unix-only, so Windows, the primary OS, had
/// no proof that a hard-linked part file is refused before any write reaches the victim).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn security_hardlinked_part_file_is_refused_on_resume() {
    use eid_engine::{CancellationToken, Error};
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim.bin");
    std::fs::write(&victim, vec![7u8; 4096]).unwrap();
    let (srv, url) = serve(
        "/h.bin",
        Content::Generated {
            seed: 1,
            len: 8 << 20,
        },
        Behavior {
            throttle_bps: Some(2_000_000),
            ..Behavior::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        c2.cancel();
    });
    let r = eid_engine::download(
        eid_engine::JobSpec {
            url,
            dir: dir.path().into(),
            file_name: None,
        },
        fast_cfg(2),
        Log::default().sink(),
        cancel,
    )
    .await;
    assert!(matches!(r, Err(Error::Cancelled)));
    // attacker swaps the part file for a hard link to a victim file
    let part = dir.path().join("h.bin.eid-part");
    std::fs::remove_file(&part).unwrap();
    // no cfg: NTFS hard links need no privilege; failure to create one must FAIL the test
    std::fs::hard_link(&victim, &part).expect("hard links must be creatable on this platform");
    let sp = dir.path().join("h.bin.eid-state");
    let r = eid_engine::resume(
        &sp,
        fast_cfg(2),
        Log::default().sink(),
        CancellationToken::new(),
    )
    .await;
    assert!(r.is_err(), "hardlinked part must not be written through");
    assert_eq!(std::fs::read(&victim).unwrap(), vec![7u8; 4096]);
    drop(srv);
}

/// Windows CI precondition for the symlink tests above: tells us whether the runner may create
/// symlinks at all (so a skipped test is visible, not silently green).
#[test]
fn symlink_capability_precheck() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("t");
    std::fs::write(&t, b"x").unwrap();
    let r = eid_platform::fs::symlink_file(&t, &d.path().join("l"));
    // AGENTS §6: a symlink test must FAIL when the link cannot be created, on every OS (a
    // Windows runner needs the symlink privilege; grant it in CI instead of skipping).
    assert!(
        r.is_ok(),
        "this machine cannot create symlinks, so symlink safety cannot be tested: {r:?}"
    );
}

/// M5: the part, its sidecars (state, backup, lock) and anything else a running job leaves are
/// owner-only EVEN IN A WORLD-WRITABLE, UMASK-022 DIRECTORY; the finished file is NOT private
/// (ordinary `0666 & !umask`). Unix only: Windows ACL inspection is not implemented (P3), so
/// this test does not exist there and the required list marks it `unix:` (NOT TESTED), instead
/// of passing on an unknown ACL.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn security_generated_part_and_state_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    // permissive parent: the privacy must come from the files, not from the directory
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let cancel = eid_engine::CancellationToken::new();
    let (_srv, url) = serve(
        "/p.bin",
        Content::Generated {
            seed: 1,
            len: 8 << 20,
        },
        Behavior {
            throttle_bps: Some(1_000_000),
            ..Behavior::default()
        },
    )
    .await;
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        c2.cancel();
    });
    let r = eid_engine::download(
        eid_engine::JobSpec {
            url: url.clone(),
            dir: dir.path().into(),
            file_name: None,
        },
        fast_cfg(2),
        Log::default().sink(),
        cancel,
    )
    .await;
    assert!(matches!(r, Err(eid_engine::Error::Cancelled)), "{r:?}");
    let names = only_files(dir.path());
    for needed in ["p.bin.eid-part", "p.bin.eid-state", "p.bin.eid-lock"] {
        assert!(
            names.iter().any(|n| n == needed),
            "{needed} missing in {names:?}"
        );
    }
    let mut checked = 0;
    for n in &names {
        let p = dir.path().join(n);
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "{n} must be owner-only, is {mode:o}");
        assert_eq!(
            eid_platform::fs::owner_only(&p).unwrap(),
            Some(true),
            "{n}: unknown is not proof"
        );
        checked += 1;
    }
    assert!(checked >= 3);
    // resume to completion: the PUBLISHED file is ordinary user data, not 0600
    let st = dir.path().join("p.bin.eid-state");
    let done = eid_engine::resume(
        &st,
        fast_cfg(2),
        Log::default().sink(),
        eid_engine::CancellationToken::new(),
    )
    .await
    .unwrap();
    let mode = std::fs::metadata(&done.path).unwrap().permissions().mode() & 0o777;
    let umask = eid_platform::fs::current_umask().expect("umask known on unix");
    assert_eq!(
        mode,
        0o666 & !umask,
        "final file mode {mode:o}, umask {umask:o}"
    );
    assert_eq!(
        sha_of_file(&done.path),
        Content::Generated {
            seed: 1,
            len: 8 << 20
        }
        .sha256_hex()
    );
}
