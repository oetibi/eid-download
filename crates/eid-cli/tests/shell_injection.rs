//! The printed "resume" command is pasted by users into real shells. Every hostile name must
//! round-trip as one literal argument and never run anything. Executes real shells: `sh` here,
//! PowerShell (pwsh) wherever present, cmd.exe on Windows (CI runs the Windows ones).
use eid_cli::quote;
use eid_platform::process::command;
use std::path::PathBuf;

const HOSTILE: &[&str] = &[
    "plain.eid-state",
    "with space/and (parens).eid-state",
    "$(touch PWNED)",
    "`touch PWNED`",
    "'; touch PWNED; '",
    "\"; touch PWNED; \"",
    "&& touch PWNED",
    "| touch PWNED",
    "a\ntouch PWNED\nb",
    "\u{2018}; New-Item PWNED; \u{2019}",
    "\u{201C}; New-Item PWNED; \u{201D}",
    "'\\'' ; touch PWNED ; '",
    "$env:COMSPEC",
    "${HOME}",
    "%COMSPEC%",
    "!HOME!",
    "C:\\dir with space\\",
    "back\\slash\\",
    "ملف عربي (1).eid-state",
    "semi;colon & amp ^ caret < > pipe |",
    "-leading-dash",
    "",
];

fn helper() -> PathBuf {
    // CARGO_BIN_EXE_eid = <target>/<profile>/eid ; examples live next to it
    let dir = PathBuf::from(env!("CARGO_BIN_EXE_eid"))
        .parent()
        .unwrap()
        .join("examples");
    let p = dir.join(if eid_platform::IS_WINDOWS {
        "argv_echo.exe"
    } else {
        "argv_echo"
    });
    assert!(p.exists(), "helper example not built: {p:?}");
    p
}

fn available(prog: &str, probe: &[&str]) -> bool {
    command(prog, probe.iter().copied())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn shell_injection_posix_sh() {
    if !available("sh", &["-c", "true"]) {
        if !eid_platform::IS_WINDOWS {
            panic!("sh must exist on Unix");
        }
        eprintln!("SKIPPED: no sh on this machine");
        return;
    }
    let h = helper();
    for s in HOSTILE {
        let work = tempfile::tempdir().unwrap();
        let line = format!("{} {}", quote::posix(&h.to_string_lossy()), quote::posix(s));
        let out = command("sh", ["-c", line.as_str()])
            .current_dir(work.path())
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            *s,
            "round trip of {s:?}"
        );
        assert!(!work.path().join("PWNED").exists(), "{s:?} executed code");
        assert_eq!(
            std::fs::read_dir(work.path()).unwrap().count(),
            0,
            "{s:?} created files"
        );
    }
}

#[test]
fn shell_injection_powershell() {
    let Some(ps) = ["pwsh", "powershell"]
        .into_iter()
        .find(|p| available(p, &["-NoProfile", "-Command", "1"]))
    else {
        assert!(
            std::env::var_os("EID_REQUIRE_SHELLS").is_none(),
            "PowerShell required (EID_REQUIRE_SHELLS) but not found"
        );
        eprintln!("SKIPPED: no PowerShell on this machine (CI sets EID_REQUIRE_SHELLS)");
        return;
    };
    for s in HOSTILE {
        let work = tempfile::tempdir().unwrap();
        let script = format!(
            "[Console]::OutputEncoding=[Text.UTF8Encoding]::new(); [Console]::Out.Write({})",
            quote::powershell(s)
        );
        let out = command(
            ps,
            ["-NoProfile", "-NonInteractive", "-Command", script.as_str()],
        )
        .current_dir(work.path())
        .output()
        .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            *s,
            "round trip of {s:?} via {ps}; stderr {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!work.path().join("PWNED").exists(), "{s:?} executed code");
        assert_eq!(
            std::fs::read_dir(work.path()).unwrap().count(),
            0,
            "{s:?} created files"
        );
    }
}

#[test]
fn shell_injection_cmd_exe() {
    // The refusal logic is portable and always checked.
    for s in HOSTILE {
        let q = quote::cmd(s);
        if s.contains(['"', '%', '!', '\n']) {
            assert!(q.is_none(), "cmd must refuse {s:?}");
        } else {
            assert!(q.is_some(), "{s:?}");
        }
    }
    if !available("cmd", &["/d", "/c", "echo", "x"]) {
        if eid_platform::IS_WINDOWS {
            panic!("cmd.exe must exist on Windows");
        }
        eprintln!("SKIPPED actual cmd.exe execution: not Windows");
        return;
    }
    let h = helper();
    for s in HOSTILE {
        let Some(q) = quote::cmd(s) else { continue };
        let work = tempfile::tempdir().unwrap();
        let hp = quote::cmd(&h.to_string_lossy()).expect("helper path quotable");
        // everything after /c goes through cmd's own parser, exactly like a pasted command line
        let line = format!("{hp} {q}");
        let out = windows_compat::run_cmd(&line, work.path());
        assert_eq!(out, *s, "round trip of {s:?}");
        assert!(!work.path().join("PWNED").exists(), "{s:?} executed code");
    }
}

/// `cmd /d /s /c "<line>"`, with the raw command line passed verbatim on Windows.
#[allow(non_snake_case)]
mod windows_compat {
    #[cfg(windows)]
    pub fn run_cmd(line: &str, cwd: &std::path::Path) -> String {
        use std::os::windows::process::CommandExt;
        let mut c = eid_platform::process::command("cmd", ["/d", "/s", "/c"]);
        c.raw_arg(format!("\"{line}\"")).current_dir(cwd);
        String::from_utf8_lossy(&c.output().unwrap().stdout).into_owned()
    }
    #[cfg(not(windows))]
    pub fn run_cmd(_line: &str, _cwd: &std::path::Path) -> String {
        unreachable!("cmd.exe only exists on Windows")
    }
}
