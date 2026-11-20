//! v0.2 (owner's Windows test: the portable ZIP registered nothing, so the browser extension
//! could not reach the app): Settings > "Connect browsers" registers THIS copy for Chrome, Edge
//! and Firefox, sees another copy's registration, never removes it, and refuses a copy that
//! cannot serve the browser. Real registration: HKCU on Windows (a test-only host name, removed
//! at the end), the per-user browser folders under a temporary HOME elsewhere.
use eid_gui::browser::{connect, disconnect, host_exe, status};
use std::path::{Path, PathBuf};

fn copy_in(dir: &Path, with_host: bool) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let app = dir.join(format!("EidDownload{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&app, b"app").unwrap();
    if with_host {
        std::fs::write(host_exe(&app), b"host").unwrap();
    }
    app
}

fn states(v: &[eid_gui::browser::LinkState]) -> Vec<(&str, &str)> {
    v.iter().map(|s| (s.browser, s.state)).collect()
}

#[test]
fn connect_this_copy_see_another_and_never_remove_it() {
    let root = tempfile::tempdir().unwrap();
    if !eid_platform::IS_WINDOWS {
        // the only test in this binary: the browsers' per-user folders go to a temporary HOME
        std::env::set_var("HOME", root.path().join("home"));
    }
    let name = format!("com.eid.test{}", std::process::id());
    let data = root.path().join("data");
    let a = copy_in(&root.path().join("portable"), true);
    let b = copy_in(&root.path().join("installed"), true);
    let none = [
        ("chrome", "none"),
        ("edge", "none"),
        ("firefox", "none"),
        ("brave", "none"),
    ];
    assert_eq!(states(&status(&a, &name)), none);

    // connect copy A: all three point at A's eid-native-host
    let s = connect(&a, &name, &data).unwrap();
    assert_eq!(
        states(&s),
        [
            ("chrome", "this"),
            ("edge", "this"),
            ("firefox", "this"),
            ("brave", "this")
        ]
    );
    let m: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            data.join("native-messaging")
                .join(format!("{name}.chrome.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        Path::new(m["path"].as_str().unwrap()),
        std::path::absolute(host_exe(&a)).unwrap()
    );
    assert_eq!(
        m["allowed_origins"][0],
        "chrome-extension://ambokeecedpgicighhcpdfjphdnphgaa/"
    );
    let f: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            data.join("native-messaging")
                .join(format!("{name}.firefox.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(f["allowed_extensions"][0], "eid-download@eid-download");

    // copy B sees "another copy" (and which), and its Disconnect leaves A's registration alone
    let sb = status(&b, &name);
    assert!(sb.iter().all(|s| s.state == "other"), "{sb:?}");
    assert!(
        sb[0].host.as_deref().unwrap().contains("portable"),
        "{sb:?}"
    );
    assert!(disconnect(&b, &name)
        .unwrap()
        .iter()
        .all(|s| s.state == "other"));
    assert!(status(&a, &name).iter().all(|s| s.state == "this"));

    // a copy that cannot serve the browser is refused and changes nothing
    let zipped = copy_in(
        &root
            .path()
            .join("Temp1_EidDownload-windows-x64.zip")
            .join("x"),
        true,
    );
    assert_eq!(connect(&zipped, &name, &data).unwrap_err(), "from_zip");
    let lonely = copy_in(&root.path().join("lonely"), false);
    assert_eq!(
        connect(&lonely, &name, &data).unwrap_err(),
        "no_native_host"
    );
    assert!(status(&a, &name).iter().all(|s| s.state == "this"));

    // B takes over, then A's Disconnect leaves B alone; B's Disconnect removes everything
    connect(&b, &name, &data).unwrap();
    assert!(status(&a, &name).iter().all(|s| s.state == "other"));
    disconnect(&a, &name).unwrap();
    assert!(status(&b, &name).iter().all(|s| s.state == "this"));
    assert_eq!(states(&disconnect(&b, &name).unwrap()), none);
}
