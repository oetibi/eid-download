//! Architecture/safety guard: OS-specific code and process spawning live in eid-platform only;
//! no TLS-verification bypass; no shell invocation; no raw listening sockets outside the test
//! server. Scans the `src/` of every crate (tests may use real shells to *verify* quoting).
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn crates() -> Vec<(String, Vec<PathBuf>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let mut v = vec![];
    for e in std::fs::read_dir(&root).unwrap() {
        let p = e.unwrap().path();
        if p.join("src").is_dir() {
            let mut files = vec![];
            rs_files(&p.join("src"), &mut files);
            v.push((p.file_name().unwrap().to_string_lossy().into_owned(), files));
        }
    }
    assert!(v.len() >= 4, "guard found too few crates: {}", v.len());
    v
}

fn scan(forbidden: &[&str], skip_crates: &[&str]) -> Vec<String> {
    let mut bad = vec![];
    for (name, files) in crates() {
        if skip_crates.contains(&name.as_str()) {
            continue;
        }
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap();
            for (i, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                for pat in forbidden {
                    if code.contains(pat) {
                        bad.push(format!("{}:{}: {pat}", f.display(), i + 1));
                    }
                }
            }
        }
    }
    bad
}

#[test]
fn platform_code_and_process_spawning_only_in_eid_platform() {
    let bad = scan(
        &[
            "cfg(windows",
            "cfg!(windows",
            "cfg(unix",
            "cfg!(unix",
            "target_os",
            "std::process::Command",
            "tokio::process",
            "Command::new",
            "os::windows",
            "windows_sys",
        ],
        &["eid-platform"],
    );
    assert!(
        bad.is_empty(),
        "OS-specific / process code outside eid-platform:\n{}",
        bad.join("\n")
    );
}

#[test]
fn no_tls_verification_bypass_anywhere() {
    let bad = scan(
        &[
            "danger_accept_invalid",
            "accept_invalid_certs",
            "accept_invalid_hostnames",
            "dangerous()",
            "NoCertificateVerification",
        ],
        &[],
    );
    assert!(
        bad.is_empty(),
        "TLS bypass strings found:\n{}",
        bad.join("\n")
    );
}

#[test]
fn no_shell_invocations_in_product_code() {
    let bad = scan(
        &[
            "\"sh\", \"-c\"",
            "\"-c\", \"sh\"",
            "\"cmd\", \"/c\"",
            "\"/c\"",
            "\"/C\"",
            "powershell -",
            "bash -c",
            "\"bash\"",
        ],
        &[],
    );
    assert!(
        bad.is_empty(),
        "shell invocation in src:\n{}",
        bad.join("\n")
    );
}

#[test]
fn no_listening_sockets_outside_the_test_server() {
    let bad = scan(
        &["TcpListener::bind", "UdpSocket::bind", "listen("],
        &["eid-testserver"],
    );
    assert!(
        bad.is_empty(),
        "listening socket outside eid-testserver (ADR-0002: no localhost TCP):\n{}",
        bad.join("\n")
    );
}
