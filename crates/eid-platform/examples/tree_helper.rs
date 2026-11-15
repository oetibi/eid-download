//! Test helper for the supervised-process tests (never shipped). Modes:
//!
//! * `argv ARGS...`: print the arguments after the mode as a JSON array of strings.
//! * `env`: print the whole environment as a JSON object.
//! * `sleep SECS`: sleep.
//! * `grandchild SECS [GSECS]`: start `tree_helper sleep GSECS` (default SECS) the way an
//!   ordinary tool starts its helpers (plain CreateProcess/fork-exec, no flags), print
//!   `{"child":PID,"grandchild":PID}` and sleep SECS itself.
//! * `flood BYTES`: write BYTES to stdout AND BYTES to stderr, interleaved, then exit 0.
//! * `console`: print `{"console_window":bool}` (Windows `GetConsoleWindow() != 0`).
//! * `console-tree`: start a grandchild in `console` mode (plain spawn, inherits our
//!   console) and print `{"child":bool,"grandchild":bool}`.
//! * `console-supervise`: (started with a console of its own) run `console-tree` through
//!   `eid_platform::process::spawn` and print its result plus `"parent":bool`.
//! * `supervise`: start `grandchild 600` through `eid_platform::process::spawn`, forward its
//!   JSON line (plus our own pid as `"eid"`), then sleep: stands in for `eid` being killed.
use std::io::Write;
use std::time::Duration;

fn me() -> std::path::PathBuf {
    std::env::current_exe().expect("current_exe")
}

fn console_window() -> bool {
    #[cfg(windows)]
    {
        // SAFETY: plain FFI query.
        !unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() }.is_null()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn out(s: &str) {
    let mut o = std::io::stdout();
    let _ = o.write_all(s.as_bytes());
    let _ = o.write_all(b"\n");
    let _ = o.flush();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("");
    let num = |i: usize| -> u64 { args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0) };
    match mode {
        "argv" => {
            let rest: Vec<String> = std::env::args_os()
                .skip(2)
                .map(|a| a.into_string().expect("argument is not valid Unicode"))
                .collect();
            out(&serde_json::to_string(&rest).unwrap());
        }
        "env" => {
            let m: serde_json::Map<String, serde_json::Value> = std::env::vars_os()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        serde_json::Value::String(v.to_string_lossy().into_owned()),
                    )
                })
                .collect();
            out(&serde_json::Value::Object(m).to_string());
        }
        "sleep" => std::thread::sleep(Duration::from_secs(num(1))),
        "grandchild" => {
            let secs = num(1);
            let gsecs = args.get(2).map(|_| num(2)).unwrap_or(secs);
            // stdout/stderr go to null: a grandchild that holds the supervisor's pipes would
            // only test pipe handling, not the job/group kill this mode exists for
            // never waited on: the grandchild must outlive us when our tree is not killed
            #[allow(clippy::zombie_processes)]
            let g = std::process::Command::new(me())
                .args(["sleep", &gsecs.to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn grandchild");
            out(&format!(
                "{{\"child\":{},\"grandchild\":{}}}",
                std::process::id(),
                g.id()
            ));
            std::thread::sleep(Duration::from_secs(secs));
        }
        "flood" => {
            let total = num(1) as usize;
            let chunk = vec![b'x'; 64 * 1024];
            let mut o = std::io::stdout().lock();
            let mut e = std::io::stderr().lock();
            let mut done = 0;
            while done < total {
                let n = chunk.len().min(total - done);
                o.write_all(&chunk[..n]).unwrap();
                e.write_all(&chunk[..n]).unwrap();
                done += n;
            }
            o.flush().unwrap();
            e.flush().unwrap();
        }
        "console" => out(&format!("{{\"console_window\":{}}}", console_window())),
        "console-tree" => {
            let o = std::process::Command::new(me())
                .arg("console")
                .stdin(std::process::Stdio::null())
                .output()
                .expect("spawn grandchild");
            let g: serde_json::Value = serde_json::from_slice(&o.stdout).expect("grandchild json");
            out(&format!(
                "{{\"child\":{},\"grandchild\":{}}}",
                console_window(),
                g["console_window"]
            ));
        }
        "console-supervise" => {
            // Started by the test with CREATE_NEW_CONSOLE, so THIS process has a real console
            // window. It starts `console-tree` through process::spawn: without CREATE_NO_WINDOW
            // the child would inherit this visible console and the check would see it.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let f = rt.block_on(async {
                let env: Vec<_> = eid_platform::process::ENV_PASSTHROUGH
                    .iter()
                    .filter_map(|k| std::env::var_os(k).map(|v| ((*k).into(), v)))
                    .collect();
                eid_platform::process::spawn(eid_platform::process::ChildSpec {
                    program: me(),
                    args: vec!["console-tree".into()],
                    cwd: std::env::temp_dir(),
                    env,
                    stdout: eid_platform::process::StreamSpec::capture(
                        4096,
                        eid_platform::process::Keep::Head,
                    ),
                    stderr: eid_platform::process::StreamSpec::capture(
                        4096,
                        eid_platform::process::Keep::Tail,
                    ),
                })
                .expect("supervised spawn")
                .wait()
                .await
                .expect("wait")
            });
            let mut v: serde_json::Value =
                serde_json::from_slice(&f.stdout.data).expect("console-tree json");
            v["parent"] = console_window().into();
            out(&v.to_string());
        }
        "supervise" => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let (tx, mut rx) = tokio::sync::mpsc::channel(4);
                let mut stdout = eid_platform::process::StreamSpec::capture(
                    4096,
                    eid_platform::process::Keep::Head,
                );
                stdout.lines = Some(tx);
                let env: Vec<_> = eid_platform::process::ENV_PASSTHROUGH
                    .iter()
                    .filter_map(|k| std::env::var_os(k).map(|v| ((*k).into(), v)))
                    .collect();
                let sup = eid_platform::process::spawn(eid_platform::process::ChildSpec {
                    program: me(),
                    args: vec!["grandchild".into(), "600".into()],
                    cwd: std::env::temp_dir(),
                    env,
                    stdout,
                    stderr: eid_platform::process::StreamSpec::capture(
                        4096,
                        eid_platform::process::Keep::Tail,
                    ),
                })
                .expect("supervised spawn");
                let line = rx.recv().await.expect("line from the child");
                let mut v: serde_json::Value = serde_json::from_slice(&line).unwrap();
                v["eid"] = std::process::id().into();
                out(&v.to_string());
                tokio::time::sleep(Duration::from_secs(600)).await;
                drop(sup);
            });
        }
        other => {
            eprintln!("tree_helper: unknown mode {other:?}");
            std::process::exit(2);
        }
    }
}
