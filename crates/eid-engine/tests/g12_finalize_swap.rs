//! M2: what lands at the final name must be the file the engine VERIFIED (size + SHA-256 through
//! the retained handle), even if the part NAME is swapped/relinked during finalization; a
//! destination replaced before success is refused; a crash between link and unlink (Unix) is
//! recovered by content. Own binary: the fault hooks are process-global, so the tests here are
//! serialised with a lock.
mod common;
use common::*;
use eid_engine::{fault, JobSpec};
use eid_testserver::{Behavior, Content};
use std::path::{Path, PathBuf};
use std::time::Duration;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const EVIL: &[u8] = b"EVIL-SUBSTITUTE";

type Task = tokio::task::JoinHandle<eid_engine::Result<eid_engine::Outcome>>;

fn start(url: String, dir: PathBuf) -> Task {
    tokio::spawn(async move {
        eid_engine::download(
            JobSpec {
                url,
                dir,
                file_name: None,
            },
            fast_cfg(2),
            Log::default().sink(),
            eid_engine::CancellationToken::new(),
        )
        .await
    })
}

async fn wait_reached(reached: std::sync::mpsc::Receiver<()>, point: &str) {
    let ok =
        tokio::task::spawn_blocking(move || reached.recv_timeout(Duration::from_secs(20)).is_ok())
            .await
            .unwrap();
    assert!(ok, "{point}: pause point never reached");
}

fn content() -> Content {
    Content::Generated {
        seed: 1201,
        len: 1 << 20,
    }
}

fn plain() -> Behavior {
    Behavior::default()
}

/// Move the part aside (the verified inode survives under `aside`) and plant `EVIL` at its name.
fn swap_part_for_evil(dir: &Path) -> PathBuf {
    let part = dir.join("f.bin.eid-part");
    let aside = dir.join("aside.orig");
    std::fs::rename(&part, &aside).expect("rename the part aside (handle stays open)");
    std::fs::write(&part, EVIL).unwrap();
    aside
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finalization_with_a_swapped_part_publishes_the_original_bytes_on_linux_and_refuses_elsewhere(
) {
    let _g = SERIAL.lock().await;
    let c = content();
    let (_srv, url) = serve("/f.bin", c.clone(), plain()).await;
    let dir = tempfile::tempdir().unwrap();
    let (reached, release) = fault::set_pause_point("final_before_rename");
    let task = start(url, dir.path().to_path_buf());
    wait_reached(reached, "final_before_rename").await;
    let aside = swap_part_for_evil(dir.path());
    release.send(()).unwrap();
    let r = task.await.unwrap();
    let dest = dir.path().join("f.bin");
    let part = dir.path().join("f.bin.eid-part");
    #[cfg(target_os = "linux")]
    {
        let o = r.unwrap_or_else(|e| panic!("linux must publish the verified inode: {e}"));
        assert_eq!(o.path, dest);
        assert_eq!(
            sha_of_file(&dest),
            c.sha256_hex(),
            "published bytes are not the verified bytes"
        );
        assert_eq!(sha_of_file(&aside), c.sha256_hex());
        assert_eq!(std::fs::read(&part).unwrap(), EVIL, "substitute modified");
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert!(r.is_err(), "a swapped part must be refused, got {r:?}");
        assert!(!dest.exists(), "nothing may be published after a refusal");
        assert!(
            dir.path().join("f.bin.eid-state").exists(),
            "state must be kept after a refusal"
        );
        assert_eq!(std::fs::read(&part).unwrap(), EVIL, "substitute modified");
        let _ = aside;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finalization_with_a_symlink_swapped_for_the_part_never_publishes_the_link() {
    let _g = SERIAL.lock().await;
    let c = content();
    let (_srv, url) = serve("/f.bin", c.clone(), plain()).await;
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside.secret");
    std::fs::write(&outside, b"SECRET").unwrap();
    let (reached, release) = fault::set_pause_point("final_before_rename");
    let task = start(url, dir.path().to_path_buf());
    wait_reached(reached, "final_before_rename").await;
    let part = dir.path().join("f.bin.eid-part");
    std::fs::rename(&part, dir.path().join("aside.orig")).unwrap();
    std::os::unix::fs::symlink(&outside, &part).unwrap();
    release.send(()).unwrap();
    let r = task.await.unwrap();
    let dest = dir.path().join("f.bin");
    if dest.exists() || std::fs::symlink_metadata(&dest).is_ok() {
        let md = std::fs::symlink_metadata(&dest).unwrap();
        assert!(
            !md.file_type().is_symlink(),
            "a symlink was published as the download (result {r:?})"
        );
        assert_eq!(
            sha_of_file(&dest),
            c.sha256_hex(),
            "published bytes are not the verified bytes"
        );
    } else {
        assert!(r.is_err(), "no file published but the job reported success");
    }
    assert_eq!(std::fs::read(&outside).unwrap(), b"SECRET");
    assert!(
        std::fs::symlink_metadata(&part)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the substituted symlink must be left alone"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finalization_detects_a_destination_replaced_before_success() {
    let _g = SERIAL.lock().await;
    let c = content();
    let (_srv, url) = serve("/f.bin", c.clone(), plain()).await;
    let dir = tempfile::tempdir().unwrap();
    let (reached, release) = fault::set_pause_point("part_renamed_final");
    let task = start(url, dir.path().to_path_buf());
    wait_reached(reached, "part_renamed_final").await;
    let dest = dir.path().join("f.bin");
    std::fs::rename(&dest, dir.path().join("moved.orig")).unwrap();
    std::fs::write(&dest, EVIL).unwrap();
    release.send(()).unwrap();
    let r = task.await.unwrap();
    assert!(
        r.is_err(),
        "success was reported although the destination is not the verified file: {:?}",
        r.map(|o| o.path)
    );
    assert_eq!(std::fs::read(&dest).unwrap(), EVIL, "foreign file touched");
    assert!(
        dir.path().join("f.bin.eid-state").exists(),
        "state must be kept"
    );
}

/// Crash between link and unlink: part and dest are two names of one inode.
#[cfg(unix)]
async fn crash_between_link_and_unlink(
    c: &Content,
    b: Behavior,
) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (srv, url) = serve("/f.bin", c.clone(), b).await;
    let dir = tempfile::tempdir().unwrap();
    fault::set_panic_point("final_linked", 1);
    let crashed = start(url, dir.path().to_path_buf()).await;
    assert!(
        crashed.is_err_and(|e| e.is_panic()),
        "crash point not reached"
    );
    let (part, dest) = (dir.path().join("f.bin.eid-part"), dir.path().join("f.bin"));
    use std::os::unix::fs::MetadataExt;
    let (mp, md) = (
        std::fs::metadata(&part).expect("part must still exist"),
        std::fs::metadata(&dest).expect("dest must exist"),
    );
    assert_eq!((mp.ino(), mp.nlink()), (md.ino(), 2), "expected two names");
    drop(srv); // the origin is gone: recovery must not need the network
    (dir, part, dest)
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_between_link_and_unlink_is_recovered_without_the_network() {
    let _g = SERIAL.lock().await;
    let c = content();
    let (dir, part, dest) = crash_between_link_and_unlink(&c, plain()).await;
    let sp = dir.path().join("f.bin.eid-state");
    let o = eid_engine::resume(
        &sp,
        fast_cfg(2),
        Log::default().sink(),
        eid_engine::CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("resume after a crash between link and unlink failed: {e}"));
    assert_eq!(o.path, dest);
    assert_eq!(sha_of_file(&dest), c.sha256_hex());
    assert!(!part.exists(), "the leftover part name was not removed");
    assert!(!sp.exists(), "state not cleaned up");
    assert!(
        !dir.path().join("f.bin.eid-lock").exists(),
        "lock file left behind"
    );
}

/// Server without validators and without ranges: the state is NOT resumable, so a rejected
/// candidate must not be mistaken for an ordinary restart (which deletes the part).
#[cfg(unix)]
fn non_resumable() -> Behavior {
    Behavior {
        etag: eid_testserver::EtagMode::None,
        advertise_ranges: false,
        ..Behavior::default()
    }
}

#[cfg(unix)]
async fn resume_once(sp: &Path, log: &Log) -> eid_engine::Result<eid_engine::Outcome> {
    eid_engine::resume(
        sp,
        fast_cfg(2),
        log.sink(),
        eid_engine::CancellationToken::new(),
    )
    .await
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_between_link_and_unlink_with_a_tampered_inode_is_not_adopted() {
    let _g = SERIAL.lock().await;
    let c = content();
    for (label, b) in [("resumable", plain()), ("non-resumable", non_resumable())] {
        let (dir, part, dest) = crash_between_link_and_unlink(&c, b).await;
        // tamper IN PLACE (same inode under both names): same size, different content
        {
            use std::os::unix::fs::FileExt;
            let f = std::fs::OpenOptions::new().write(true).open(&dest).unwrap();
            f.write_all_at(b"TAMPERED", 0).unwrap();
        }
        let sp = dir.path().join("f.bin.eid-state");
        let log = Log::default();
        let r = resume_once(&sp, &log).await;
        assert!(
            matches!(&r, Err(e) if !matches!(e, eid_engine::Error::Busy)),
            "{label}: a tampered file must be refused with an error, got {:?}",
            r.map(|o| o.path)
        );
        assert_eq!(std::fs::read(&dest).unwrap()[..8], *b"TAMPERED", "{label}");
        assert!(
            part.exists() && sp.exists(),
            "{label}: evidence (part + state) must be kept"
        );
        assert_eq!(
            log.count(|e| matches!(e, eid_engine::Event::Finished { .. })),
            0,
            "{label}"
        );
    }
}

/// Pause recovery after it hashed the pair, run `mutate`, release, and expect a refusal that
/// keeps everything on disk.
#[cfg(unix)]
async fn recovery_refused_after(
    label: &str,
    b: Behavior,
    mutate: impl FnOnce(&Path, &Path),
) -> tempfile::TempDir {
    let c = content();
    let (dir, part, dest) = crash_between_link_and_unlink(&c, b).await;
    let sp = dir.path().join("f.bin.eid-state");
    let (reached, release) = fault::set_pause_point("recovery_hashed");
    let sp2 = sp.clone();
    let task = tokio::spawn(async move { resume_once(&sp2, &Log::default()).await });
    wait_reached(reached, "recovery_hashed").await;
    mutate(&part, &dest);
    release.send(()).unwrap();
    let r = task.await.unwrap();
    assert!(
        r.is_err(),
        "{label}: recovery reported success although the pair changed while hashing: {:?}",
        r.map(|o| o.path)
    );
    assert!(
        part.exists() && sp.exists(),
        "{label}: part + state must be kept"
    );
    dir
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_refuses_a_destination_substituted_while_hashing() {
    let _g = SERIAL.lock().await;
    for (label, b) in [("resumable", plain()), ("non-resumable", non_resumable())] {
        let dir = recovery_refused_after(label, b, |_part, dest| {
            std::fs::rename(dest, dest.with_file_name("moved.orig")).unwrap();
            std::fs::write(dest, EVIL).unwrap();
        })
        .await;
        let dest = dir.path().join("f.bin");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            EVIL,
            "{label}: foreign file touched"
        );
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_refuses_a_third_link_created_while_hashing() {
    let _g = SERIAL.lock().await;
    for (label, b) in [("resumable", plain()), ("non-resumable", non_resumable())] {
        let dir = recovery_refused_after(label, b, |_part, dest| {
            std::fs::hard_link(dest, dest.with_file_name("third")).unwrap();
        })
        .await;
        assert!(dir.path().join("f.bin").exists(), "{label}");
        assert!(dir.path().join("third").exists(), "{label}");
    }
}
