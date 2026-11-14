//! The real `eid-native-host` binary over real pipes: stdout must be protocol frames only, and
//! a missing app is launched as a really detached process.
use eid_ipc::{start, Client, ClientError, Command, Endpoint, Handler, Response, ServeOptions};
use eid_platform::ipc;
use eid_platform::process::command;
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const HOST: &str = env!("CARGO_BIN_EXE_eid-native-host");

struct Count(AtomicUsize);
impl Handler for Count {
    fn handle(&self, cmd: Command) -> Result<serde_json::Value, String> {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        match cmd {
            Command::Add { url, .. } => Ok(json!({"id": n, "url": url})),
            _ => Ok(json!({"pong": true})),
        }
    }
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut v = (body.len() as u32).to_le_bytes().to_vec();
    v.extend_from_slice(body);
    v
}

fn req(id: &str, cmd: serde_json::Value) -> Vec<u8> {
    frame(
        json!({"v": 1, "id": id, "command": cmd})
            .to_string()
            .as_bytes(),
    )
}

/// Run the host (`exe`) with `input` on stdin and extra environment; return
/// (stdout bytes, stderr text, exit code). Fails the test (instead of hanging) after 60 s: a
/// host that leaked its pipes to a child would never let the readers reach EOF.
async fn host_at(
    exe: PathBuf,
    ep: &Endpoint,
    env: Vec<(&'static str, std::ffi::OsString)>,
    input: Vec<u8>,
) -> (Vec<u8>, String, Option<i32>) {
    let (name, tok) = (ep.name.clone(), ep.token_path.clone());
    tokio::task::spawn_blocking(move || {
        let mut c = command(exe, Vec::<String>::new());
        c.env("EID_IPC_ENDPOINT", name)
            .env("EID_IPC_TOKEN_FILE", tok)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx2 = tx.clone();
        std::thread::spawn(move || {
            let mut b = vec![];
            stdout.read_to_end(&mut b).unwrap();
            let _ = tx.send((0, b));
        });
        std::thread::spawn(move || {
            let mut b = vec![];
            stderr.read_to_end(&mut b).unwrap();
            let _ = tx2.send((1, b));
        });
        let _ = stdin.write_all(&input); // the host may stop reading after a bad frame
        drop(stdin);
        let (mut out, mut err) = (None, None);
        while out.is_none() || err.is_none() {
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok((0, b)) => out = Some(b),
                Ok((_, b)) => err = Some(String::from_utf8_lossy(&b).into_owned()),
                Err(_) => {
                    let _ = child.kill();
                    panic!("the host's stdout/stderr never reached EOF (leaked pipe handle?)");
                }
            }
        }
        let status = child.wait().unwrap();
        (out.unwrap(), err.unwrap(), status.code())
    })
    .await
    .unwrap()
}

async fn host(ep: &Endpoint, input: Vec<u8>) -> (Vec<u8>, String, Option<i32>) {
    host_at(HOST.into(), ep, vec![], input).await
}

fn frames(mut out: &[u8]) -> Vec<Response> {
    let mut v = vec![];
    while !out.is_empty() {
        assert!(out.len() >= 4, "partial header on stdout: {out:?}");
        let n = u32::from_le_bytes(out[..4].try_into().unwrap()) as usize;
        assert!(out.len() >= 4 + n, "partial body on stdout");
        v.push(serde_json::from_slice(&out[4..4 + n]).unwrap_or_else(|_| {
            panic!("stdout carried a non-protocol frame: {:?}", &out[4..4 + n])
        }));
        out = &out[4 + n..];
    }
    v
}

fn endpoint(tag: &str, d: &tempfile::TempDir) -> Endpoint {
    Endpoint {
        name: format!(
            "nb{}-{tag}-{}",
            std::process::id(),
            &ipc::new_token().unwrap()[..8]
        ),
        token_path: d.path().join("tok"),
    }
}

fn setup(tag: &str) -> (Endpoint, Arc<Count>, eid_ipc::Running, tempfile::TempDir) {
    let d = tempfile::tempdir().unwrap();
    let ep = endpoint(tag, &d);
    let h = Arc::new(Count(AtomicUsize::new(0)));
    let r = start(&ep, h.clone(), ServeOptions::default()).unwrap();
    (ep, h, r, d)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nm_binary_stdout_is_protocol_only_and_valid_messages_arrive() {
    let (ep, h, run, _d) = setup("ok");
    let mut input = req("a", json!({"cmd": "ping"}));
    input.extend(req(
        "b",
        json!({"cmd": "add", "url": "http://h/\u{0645}.zip"}),
    ));
    // garbage that is not UTF-8 and not JSON, in the middle of the session
    input.extend(frame(&[0xff, 0xfe, 0x00, 0x80]));
    input.extend(req("c", json!({"cmd": "ping"})));
    let (out, err, code) = host(&ep, input).await;
    assert_eq!(code, Some(0), "stderr: {err}");
    let r = frames(&out); // panics if stdout holds anything but whole frames
    assert_eq!(r.len(), 4, "{r:?}");
    assert!(r[0].ok && r[1].ok && !r[2].ok && r[3].ok);
    assert_eq!(h.0.load(Ordering::SeqCst), 3);
    run.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nm_binary_hostile_frames_end_the_session_with_frames_only_on_stdout() {
    let (ep, h, run, _d) = setup("bad");
    for input in [
        // oversized header, then bytes that must never be interpreted
        {
            let mut v = (3u32 << 20).to_le_bytes().to_vec();
            v.extend(req("z", json!({"cmd": "ping"})));
            v
        },
        u32::MAX.to_le_bytes().to_vec(),
        vec![9, 0, 0],
        {
            let mut v = 50u32.to_le_bytes().to_vec();
            v.extend_from_slice(b"short");
            v
        },
    ] {
        let (out, err, code) = host(&ep, input).await;
        assert_eq!(code, Some(1), "stderr: {err}");
        let r = frames(&out);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(!r[0].ok);
    }
    assert_eq!(
        h.0.load(Ordering::SeqCst),
        0,
        "no hostile frame may reach the app"
    );
    // diagnostics go to stderr only
    let (out, err, _) = host(&ep, u32::MAX.to_le_bytes().to_vec()).await;
    assert!(!String::from_utf8_lossy(&out).contains("bad frame"));
    assert!(err.contains("bad frame"), "{err}");
    run.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nm_binary_without_a_running_app_answers_with_an_error_frame() {
    let d = tempfile::tempdir().unwrap();
    let ep = endpoint("none", &d);
    // The program the host would launch does not exist, so launching fails and the host must give
    // up at its deadline with an error frame (no hang, nothing printed to stdout). This uses
    // `EID_NATIVE_HOST_APP` pointing at a missing file instead of copying the host to an empty
    // directory: a test that WRITES an executable and then runs it races with the other tests of
    // this process, whose fork() children briefly inherit the open write descriptor, and exec of a
    // file that is open for writing fails with ETXTBSY ("Text file busy") on Linux.
    let missing = d
        .path()
        .join(format!("no-such-app{}", std::env::consts::EXE_SUFFIX));
    assert!(!missing.exists());
    let t = std::time::Instant::now();
    let (out, err, code) = host_at(
        HOST.into(),
        &ep,
        vec![
            ("EID_NATIVE_HOST_READY_MS", "500".into()),
            ("EID_NATIVE_HOST_APP", missing.into_os_string()),
        ],
        req("q", json!({"cmd": "ping"})),
    )
    .await;
    assert_eq!(code, Some(0), "{err}");
    let r = frames(&out);
    assert_eq!(r.len(), 1);
    assert!(!r[0].ok && r[0].id == "q");
    assert!(r[0].error.as_deref().unwrap().contains("not running"));
    assert!(err.contains("cannot start the app"), "{err}");
    assert!(err.contains("not found"), "{err}");
    assert!(t.elapsed() < Duration::from_secs(30));
}

fn fake_app() -> PathBuf {
    // target/<profile>/eid-native-host[.exe]  ->  target/<profile>/examples/fake_app[.exe]
    let p = std::path::Path::new(HOST)
        .parent()
        .unwrap()
        .join("examples")
        .join(format!("fake_app{}", std::env::consts::EXE_SUFFIX));
    assert!(
        p.is_file(),
        "example fake_app was not built: {}",
        p.display()
    );
    p
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nm_binary_launches_the_app_as_a_detached_process_and_does_not_leak_its_pipes() {
    let d = tempfile::tempdir().unwrap();
    let ep = endpoint("launch", &d);
    let mut input = req("one", json!({"cmd": "ping"}));
    input.extend(req("two", json!({"cmd": "add", "url": "http://h/x.zip"})));
    let (out, err, code) = host_at(
        HOST.into(),
        &ep,
        vec![("EID_NATIVE_HOST_APP", fake_app().into_os_string())],
        input,
    )
    .await; // returns only if the launched app did not inherit the host's stdout/stderr pipes
    assert_eq!(code, Some(0), "{err}");
    let r = frames(&out);
    assert_eq!(r.len(), 2, "{r:?} / {err}");
    assert!(r.iter().all(|x| x.ok), "{r:?}");
    assert_eq!(r[0].result.as_ref().unwrap()["fake"], true);

    // the host is gone but the app it started is still serving: it is detached
    let mut c = Client::connect(&ep)
        .await
        .expect("the launched app must outlive the host");
    c.call(Command::Ping {}).await.unwrap();
    // stop it and wait until the endpoint is really free (no leaked process)
    c.call(Command::Shutdown {}).await.unwrap();
    let t = std::time::Instant::now();
    loop {
        match Client::connect(&ep).await {
            Err(ClientError::NotRunning) => break,
            _ => {
                assert!(t.elapsed() < Duration::from_secs(20), "app did not stop");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// No code path of the host may write to stdout except through the framing writer.
#[test]
fn stdout_is_never_printed_to_in_the_host_sources() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for e in std::fs::read_dir(src).unwrap() {
        let p = e.unwrap().path();
        let text = std::fs::read_to_string(&p).unwrap();
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for bad in [
                "println!",
                "print!",
                "dbg!",
                "io::stdout()",
                "std::io::stdout",
            ] {
                // a match counts only as a whole word (`eprintln!` is stderr), and the one sanctioned
                // stdout writer is tokio's, which `main.rs` wraps in the framing writer
                if code.contains("tokio::io::stdout()") {
                    continue;
                }
                let hit = code.match_indices(bad).any(|(at, _)| {
                    !code[..at]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_')
                });
                assert!(
                    !hit,
                    "{}:{}: `{bad}` would corrupt the Native Messaging stream",
                    p.display(),
                    i + 1
                );
            }
        }
    }
}
