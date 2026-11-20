fn main() {
    // v0.2: which commit this build is (shown in Settings and written to the log), so a report
    // from a real PC can be matched to the exact package. CI sets GITHUB_SHA; local builds say
    // "local".
    let commit = std::env::var("GITHUB_SHA")
        .ok()
        .filter(|s| s.len() >= 7 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|s| s[..7].to_owned())
        .unwrap_or_else(|| "local".into());
    println!("cargo:rustc-env=EID_BUILD_COMMIT={commit}");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    tauri_build::build()
}
