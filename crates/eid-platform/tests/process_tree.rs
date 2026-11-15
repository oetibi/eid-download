//! P4 G1: supervised child processes (`eid_platform::process::spawn`).
//! The helper is `examples/tree_helper.rs` (built by `cargo test`); it starts a grandchild the
//! way an ordinary tool (yt-dlp starting ffmpeg) does, without any special flag.
use eid_platform::process::{
    process_is_alive, spawn, ChildSpec, Keep, StreamSpec, Supervised, ENV_PASSTHROUGH,
};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn helper() -> PathBuf {
    // The test binary lives somewhere under target/<profile>/ (the layout differs between
    // cargo versions); examples are in target/<profile>/examples/.
    let name = format!("tree_helper{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().unwrap();
    exe.ancestors()
        .map(|a| a.join("examples").join(&name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| {
            panic!(
                "example tree_helper was not built (run the whole crate's tests): {}",
                exe.display()
            )
        })
}

fn base_env() -> Vec<(OsString, OsString)> {
    ENV_PASSTHROUGH
        .iter()
        .filter_map(|k| std::env::var_os(k).map(|v| (OsString::from(*k), v)))
        .collect()
}

fn spec(args: &[&str]) -> ChildSpec {
    ChildSpec {
        program: helper(),
        args: args.iter().map(OsString::from).collect(),
        cwd: std::env::temp_dir(),
        env: base_env(),
        stdout: StreamSpec::capture(1 << 20, Keep::Head),
        stderr: StreamSpec::capture(1 << 20, Keep::Tail),
    }
}

/// Start `spec` with a line channel on stdout and return the first JSON line.
async fn start_with_line(mut s: ChildSpec) -> (Supervised, serde_json::Value) {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    s.stdout.lines = Some(tx);
    let sup = spawn(s).unwrap();
    let line = tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .expect("no line from the helper within 30 s")
        .expect("helper closed stdout without a line");
    (sup, serde_json::from_slice(&line).unwrap())
}

fn pid(v: &serde_json::Value, k: &str) -> u32 {
    v[k].as_u64().unwrap_or_else(|| panic!("no {k} in {v}")) as u32
}

async fn wait_dead(pid: u32, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while process_is_alive(pid) {
        assert!(
            Instant::now() < deadline,
            "{what} (pid {pid}) still alive 15 s after the tree was killed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawned_arguments_reach_the_child_byte_exact() {
    let hostile = [
        "",
        " ",
        "a b",
        "\"",
        "\\",
        "a\\\\\"b",
        "trailing\\",
        "-leading-dash",
        "--",
        "$(touch PWNED)`id`;|&",
        "%PATH%",
        "^&^|",
        "عربي ملف",
        "\u{202E}gpj.exe",
        "tab\there",
    ];
    let mut args = vec!["argv"];
    args.extend(hostile);
    let f = spawn(spec(&args)).unwrap().wait().await.unwrap();
    assert!(f.status.success(), "{f:?}");
    let got: Vec<String> = serde_json::from_slice(&f.stdout.data).unwrap();
    assert_eq!(got, hostile);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_environment_is_exactly_what_the_caller_passed() {
    let mut s = spec(&["env"]);
    s.env.push(("EID_ALLOWED".into(), "yes".into()));
    let f = spawn(s).unwrap().wait().await.unwrap();
    let m: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&f.stdout.data).unwrap();
    let mut got: Vec<String> = m.keys().map(|k| k.to_ascii_uppercase()).collect();
    got.sort();
    let mut want: Vec<String> = base_env()
        .iter()
        .map(|(k, _)| k.to_string_lossy().to_ascii_uppercase())
        .chain(["EID_ALLOWED".to_string()])
        .collect();
    want.sort();
    assert_eq!(got, want, "the child saw variables nobody passed: {m:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_are_never_looked_up_in_path() {
    let mut s = spec(&["sleep", "0"]);
    s.program = PathBuf::from(format!("tree_helper{}", std::env::consts::EXE_SUFFIX));
    let e = spawn(s).err().expect("a bare program name must be refused");
    assert!(e.to_string().contains("absolute"), "{e}");
    let mut s = spec(&["sleep", "0"]);
    s.cwd = PathBuf::from("relative-dir");
    assert!(spawn(s).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_output_is_bounded_and_both_pipes_drain() {
    // 8 MiB on each pipe: a supervisor that read only one pipe would deadlock on the other
    // (pipe buffers are a few KiB); we keep 64 KiB of each and count the rest.
    let total = 8 * 1024 * 1024;
    let mut s = spec(&["flood", &total.to_string()]);
    s.stdout = StreamSpec::capture(64 * 1024, Keep::Head);
    s.stderr = StreamSpec::capture(64 * 1024, Keep::Tail);
    let f = tokio::time::timeout(Duration::from_secs(60), spawn(s).unwrap().wait())
        .await
        .expect("flooding child did not finish: a pipe is not drained")
        .unwrap();
    assert!(f.status.success());
    for c in [&f.stdout, &f.stderr] {
        assert_eq!(c.total, total as u64);
        assert_eq!(c.data.len(), 64 * 1024);
        assert!(c.truncated);
        assert!(c.data.iter().all(|&b| b == b'x'));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_kills_the_whole_process_tree() {
    let (sup, v) = start_with_line(spec(&["grandchild", "600"])).await;
    let (c, g) = (pid(&v, "child"), pid(&v, "grandchild"));
    assert!(process_is_alive(c) && process_is_alive(g));
    drop(sup); // what a cancelled `wait()` future does
    wait_dead(c, "child").await;
    wait_dead(g, "grandchild").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_finished_child_takes_its_leftover_grandchildren_with_it() {
    // the direct child exits after 1 s while its grandchild would sleep 600 s
    let (sup, v) = start_with_line(spec(&["grandchild", "1", "600"])).await;
    let g = pid(&v, "grandchild");
    assert!(process_is_alive(g));
    // a grandchild that outlives `wait` would also hold our stdout pipe open forever
    let f = tokio::time::timeout(Duration::from_secs(30), sup.wait())
        .await
        .expect("wait() hung on a leftover grandchild")
        .unwrap();
    assert!(f.status.success());
    wait_dead(g, "grandchild").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grandchild_spawned_at_startup_is_in_the_job() {
    // The grandchild is started by the helper's very first action. On Windows the child is
    // created suspended and only resumed after it was assigned to the job, so the grandchild
    // inherits the job; on Unix it inherits the process group.
    let (sup, v) = start_with_line(spec(&["grandchild", "600"])).await;
    let (c, g) = (pid(&v, "child"), pid(&v, "grandchild"));
    assert!(sup.tree_contains(c).unwrap(), "child not in the tree");
    assert!(sup.tree_contains(g).unwrap(), "grandchild escaped the tree");
    assert!(
        !sup.tree_contains(std::process::id()).unwrap(),
        "the test process itself must not be in the child's tree"
    );
    drop(sup);
    wait_dead(g, "grandchild").await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killpg_kills_the_group() {
    let (sup, v) = start_with_line(spec(&["grandchild", "600"])).await;
    let g = pid(&v, "grandchild");
    sup.kill_tree();
    wait_dead(g, "grandchild").await;
    drop(sup);
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killing_eid_kills_the_tree() {
    // `supervise` plays eid: it starts the tree through process::spawn and is then killed
    // abruptly (TerminateProcess, no destructor runs). The job handle dies with it, and
    // KILL_ON_JOB_CLOSE must take the child and the grandchild down.
    use tokio::io::AsyncBufReadExt;
    let mut eid = tokio::process::Command::new(helper())
        .arg("supervise")
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = tokio::io::BufReader::new(eid.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
        .await
        .expect("no line from the supervisor")
        .unwrap()
        .expect("supervisor closed stdout");
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    let (c, g) = (pid(&v, "child"), pid(&v, "grandchild"));
    assert!(process_is_alive(c) && process_is_alive(g));
    eid.start_kill().unwrap();
    let _ = eid.wait().await;
    wait_dead(c, "child").await;
    wait_dead(g, "grandchild").await;
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_and_grandchild_have_no_console_window() {
    // The supervising process must itself own a visible console window, otherwise a child
    // without CREATE_NO_WINDOW would inherit "no window" and the test would prove nothing
    // (red-checked: run directly from a windowless test process it passed without the flag).
    // So the helper is started with CREATE_NEW_CONSOLE and calls process::spawn from there.
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    let o = std::process::Command::new(helper())
        .arg("console-supervise")
        .creation_flags(CREATE_NEW_CONSOLE)
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        v["parent"], true,
        "the supervising process has no console window, so this test cannot discriminate: {v}"
    );
    assert_eq!(v["child"], false, "the child has a console window: {v}");
    assert_eq!(
        v["grandchild"], false,
        "the grandchild has a console window: {v}"
    );
}

#[cfg(windows)]
#[test]
fn the_console_check_can_see_a_window() {
    // Calibration for the test above: the same check must report a window for a process that
    // really gets one (CREATE_NEW_CONSOLE). If it cannot tell the difference on this machine
    // the test fails, and "no black window" is NOT claimed (MANUAL-TESTS #11 stays open).
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    let o = std::process::Command::new(helper())
        .arg("console")
        .creation_flags(CREATE_NEW_CONSOLE)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["console_window"], true, "calibration failed: {v}");
}
