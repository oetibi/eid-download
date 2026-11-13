//! Review B4: every open of an existing job file must refuse a multi-link file BEFORE any
//! mutation. Runs on all platforms; on Windows it exercises `windows_link_count`
//! (GetFileInformationByHandle) — NOT yet executed on real Windows (STATUS: not tested).
use eid_platform::fs as pfs;

#[test]
fn open_regular_nofollow_refuses_a_hard_linked_file_and_leaves_it_untouched() {
    let d = tempfile::tempdir().unwrap();
    let victim = d.path().join("victim.bin");
    std::fs::write(&victim, vec![7u8; 1024]).unwrap();
    let part = d.path().join("job.eid-part");
    std::fs::hard_link(&victim, &part).expect("hard links must be creatable on this platform");
    assert!(
        pfs::open_regular_nofollow(&part).is_err(),
        "rw open must refuse"
    );
    assert!(
        pfs::open_regular_nofollow_read(&part).is_err(),
        "ro open must refuse"
    );
    assert_eq!(std::fs::read(&victim).unwrap(), vec![7u8; 1024]);
    // a plain single-link file is fine
    let ok = d.path().join("plain.bin");
    std::fs::write(&ok, b"x").unwrap();
    assert!(pfs::open_regular_nofollow(&ok).is_ok());
}
