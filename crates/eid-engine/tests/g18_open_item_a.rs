//! PLAN P1 open item (a), DEFERRED by the owner (2026-11-14): `state::load` is not read-only. A reader
//! that calls it while the job's checkpoint task rotates the sidecar (primary -> .bak -> new primary)
//! can see the primary missing, then find it present again and rename a VALID file to `.broken`
//! (this made `etag_change_between_attempts_restarts_with_new_content` fail once on CI).
//! This test reproduces the engine behaviour and is `#[ignore]`d until (a) is scheduled; run it with
//! `cargo test -p eid-engine --test g18_open_item_a -- --ignored`. Test-side observers of a LIVE
//! sidecar must use the read-only `state::peek`.
use eid_engine::state::{self, SegState, State};
#[test]
#[ignore = "documents deferred open item (a): state::load quarantines a valid file during rotation"]
fn open_item_a_state_load_quarantines_a_valid_file_during_rotation() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("e.bin.eid-state");
    let mut s = State {
        version: 1,
        url: "http://h/e.bin".into(),
        file_name: "e.bin".into(),
        total: Some(100),
        etag: None,
        last_modified: None,
        ranged: true,
        segments: vec![SegState {
            start: 0,
            next: 0,
            end: 100,
        }],
        restarts: 0,
        finalize: None,
        checksum: String::new(),
    };
    s.seal();
    state::save(&p, &s).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (st2, p2, s2) = (stop.clone(), p.clone(), s.clone());
    let w = std::thread::spawn(move || {
        let mut n = 0;
        while !st2.load(std::sync::atomic::Ordering::SeqCst) {
            state::save(&p2, &s2).unwrap();
            n += 1;
        }
        n
    });
    let mut seen = false;
    for _ in 0..200000 {
        let _ = state::load(&p);
        if d.path().join("e.bin.eid-state.broken").exists() {
            seen = true;
            break;
        }
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let n = w.join().unwrap();
    println!("saves={n} broken_created={seen}");
    assert!(
        !seen,
        "a valid, concurrently rotated state file was quarantined as .broken by a reader"
    );
}
