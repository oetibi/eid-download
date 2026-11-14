//! PLAN P1 open item (a): `state::load` used to be a mutating read. A reader that called it while the
//! job's checkpoint task rotated the sidecar (primary -> .bak -> new primary) could see the primary
//! missing, then find it present again and rename a VALID file to `.broken` (this made
//! `etag_change_between_attempts_restarts_with_new_content` fail once on CI). `load` is read-only
//! now; this is the former reproducer, no longer ignored, with the same writer/reader race.
use eid_engine::state::{self, SegState, State};

#[test]
fn state_load_never_quarantines_a_valid_file_during_rotation() {
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
        let mut n = 0u64;
        while !st2.load(std::sync::atomic::Ordering::SeqCst) {
            state::save(&p2, &s2).unwrap();
            n += 1;
        }
        n
    });
    // Run until the writer has rotated plenty of times (an event, not a fixed sleep), bounded.
    let mut loads = 0u64;
    let mut ok = 0u64;
    let mut seen_broken = false;
    let start = std::time::Instant::now();
    while loads < 100_000 && start.elapsed() < std::time::Duration::from_secs(5) {
        if state::load(&p).is_ok() {
            ok += 1;
        }
        if d.path().join("e.bin.eid-state.broken").exists() {
            seen_broken = true;
            break;
        }
        loads += 1;
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let saves = w.join().unwrap();
    println!("saves={saves} loads={loads} ok={ok} broken_created={seen_broken}");
    assert!(
        !seen_broken,
        "a valid, concurrently rotated state file was quarantined as .broken by a reader"
    );
    assert!(
        saves >= 20,
        "the writer barely ran ({saves} saves): not a race test"
    );
    // the writer never lost the state either
    assert_eq!(state::load(&p).unwrap().file_name, "e.bin");
}
