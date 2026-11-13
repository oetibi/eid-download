//! Review M1: the per-job lock file is deleted while locked, which has a known race: process B
//! opens the path, A finishes (deletes the file, releases the lock), C creates a NEW file at the
//! path and locks it, then B locks its ORPHAN inode. Without a handle-vs-path identity check B
//! and C both "own" the job. Own test binary: uses the process-global pause point.
mod common;
use eid_engine::{fault, Error, JobLock};

#[test]
fn lock_taken_on_an_orphaned_inode_is_never_an_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let a = JobLock::acquire(dir.path(), "job").unwrap();
    // B opens the path, then parks BEFORE locking
    let (reached, release) = fault::set_pause_point("lock_after_open");
    let d = dir.path().to_path_buf();
    let b = std::thread::spawn(move || JobLock::acquire(&d, "job"));
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("B never reached the barrier after open");
    // A finishes: removes the file while locked, then releases (drop)
    a.remove_file_keep_lock();
    drop(a);
    // C creates and locks a fresh file at the same path
    let c = JobLock::acquire(dir.path(), "job").expect("C must get the new file");
    // B now locks its orphan inode (free) — and must NOT be granted ownership
    release.send(()).unwrap();
    let b = b.join().unwrap();
    assert!(
        matches!(b, Err(Error::Busy)),
        "B was granted the job while C owns it: {:?}",
        b.map(|_| "two owners")
    );
    drop(c);
    // control: with nobody holding it, the lock is available again
    JobLock::acquire(dir.path(), "job").expect("lock free after C released");
}
