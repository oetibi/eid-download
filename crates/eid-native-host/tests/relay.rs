//! Native Messaging relay in-process: real frames in a byte buffer, a real IPC server behind it.
use eid_ipc::frame::MAX_FRAME;
use eid_ipc::{start, Command, Endpoint, Handler, Response, ServeOptions};
use eid_native_host::{run, Exit, HostConfig, Launcher};
use eid_platform::ipc;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<Command>>,
}
impl Handler for Recorder {
    fn handle(&self, cmd: Command) -> Result<serde_json::Value, String> {
        let mut c = self.calls.lock().unwrap();
        c.push(cmd.clone());
        match cmd {
            Command::Add { url, .. } => Ok(json!({"id": c.len(), "url": url})),
            _ => Ok(json!({"pong": true})),
        }
    }
}

struct Fixture {
    ep: Endpoint,
    rec: Arc<Recorder>,
    running: Option<eid_ipc::Running>,
    _d: tempfile::TempDir,
}

fn fixture(tag: &str) -> Fixture {
    let d = tempfile::tempdir().unwrap();
    let ep = Endpoint {
        name: format!(
            "nm{}-{tag}-{}",
            std::process::id(),
            &ipc::new_token().unwrap()[..8]
        ),
        token_path: d.path().join("tok"),
    };
    let rec = Arc::new(Recorder::default());
    let running = Some(start(&ep, rec.clone(), ServeOptions::default()).unwrap());
    Fixture {
        ep,
        rec,
        running,
        _d: d,
    }
}

fn cfg(ep: &Endpoint) -> HostConfig {
    HostConfig {
        endpoint: ep.clone(),
        launcher: None,
        ready_deadline: Duration::from_millis(600),
        poll: Duration::from_millis(20),
    }
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut v = (body.len() as u32).to_le_bytes().to_vec();
    v.extend_from_slice(body);
    v
}

fn parse_frames(mut out: &[u8]) -> Vec<Response> {
    let mut v = vec![];
    while !out.is_empty() {
        assert!(out.len() >= 4, "stdout has a partial frame header: {out:?}");
        let n = u32::from_le_bytes(out[..4].try_into().unwrap()) as usize;
        assert!(out.len() >= 4 + n, "stdout has a partial frame body");
        v.push(serde_json::from_slice(&out[4..4 + n]).expect("stdout frame must be a Response"));
        out = &out[4 + n..];
    }
    v
}

/// Run a whole session over `input` and return the responses and the exit reason.
async fn session(cfg: &HostConfig, input: Vec<u8>) -> (Vec<Response>, Exit) {
    let mut r = &input[..];
    let mut out: Vec<u8> = Vec::new();
    let exit = run(&mut r, &mut out, cfg).await;
    (parse_frames(&out), exit)
}

fn add(id: &str, url: &str) -> Vec<u8> {
    frame(
        json!({"v":1,"id":id,"command":{"cmd":"add","url":url}})
            .to_string()
            .as_bytes(),
    )
}

fn ping(id: &str) -> Vec<u8> {
    frame(
        json!({"v":1,"id":id,"command":{"cmd":"ping"}})
            .to_string()
            .as_bytes(),
    )
}

#[tokio::test]
async fn nm_valid_messages_reach_the_app_and_answers_come_back_in_order() {
    let f = fixture("valid");
    let mut input = ping("p1");
    input.extend(add("a1", "http://h/a.zip"));
    input.extend(add("a2", "http://h/\u{0645}\u{0644}\u{0641}.zip")); // Arabic path
    let (resp, exit) = session(&cfg(&f.ep), input).await;
    assert_eq!(exit, Exit::Eof);
    assert_eq!(resp.len(), 3);
    assert!(resp.iter().all(|r| r.ok), "{resp:?}");
    assert_eq!(
        resp.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["p1", "a1", "a2"]
    );
    assert_eq!(f.rec.calls.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn nm_rejects_malformed_oversized_and_extra_field_messages() {
    let f = fixture("reject");
    let c = cfg(&f.ep);

    // malformed / extra fields / wrong version / not allowed from a browser: refused one by one,
    // the session continues, and nothing reaches the app
    let mut input = vec![];
    for bad in [
        &b"not json"[..],
        br#"{"v":1,"id":"x","command":{"cmd":"ping"},"extra":1}"#,
        br#"{"v":1,"id":"x","command":{"cmd":"add","url":"http://h/","dir":"C:\\"}}"#,
        br#"{"v":2,"id":"x","command":{"cmd":"ping"}}"#,
        br#"{"v":1,"id":"../x","command":{"cmd":"ping"}}"#,
        &[0xff, 0xfe, 0xfd],
        b"[]",
        b"null",
    ] {
        input.extend(frame(bad));
    }
    input.extend(frame(
        json!({"v":1,"id":"l","command":{"cmd":"list"}})
            .to_string()
            .as_bytes(),
    ));
    input.extend(frame(
        json!({"v":1,"id":"p","command":{"cmd":"pause","item":1}})
            .to_string()
            .as_bytes(),
    ));
    input.extend(ping("good"));
    let (resp, exit) = session(&c, input).await;
    assert_eq!(exit, Exit::Eof);
    assert_eq!(resp.len(), 11);
    assert!(resp[..10].iter().all(|r| !r.ok), "{resp:?}");
    assert_eq!(
        resp[8].error.as_deref(),
        Some("command not allowed from the browser")
    );
    assert_eq!(
        resp[9].error.as_deref(),
        Some("command not allowed from the browser")
    );
    assert!(resp[10].ok && resp[10].id == "good");
    assert_eq!(
        f.rec.calls.lock().unwrap().len(),
        1,
        "only the one valid ping may have reached the app"
    );

    // an oversized frame header: refused without reading a body, session over
    for len in [(MAX_FRAME as u32) + 1, u32::MAX] {
        let mut input = len.to_le_bytes().to_vec();
        input.extend(add("late", "http://h/never"));
        let (resp, exit) = session(&c, input).await;
        assert_eq!(exit, Exit::ProtocolError);
        assert_eq!(resp.len(), 1);
        assert!(!resp[0].ok);
    }
    // truncated header / truncated body / zero length
    for input in [
        vec![5u8, 0],
        [10u8, 0, 0, 0, b'{'].to_vec(),
        0u32.to_le_bytes().to_vec(),
    ] {
        let (resp, exit) = session(&c, input).await;
        assert_eq!(exit, Exit::ProtocolError);
        assert_eq!(resp.len(), 1);
        assert!(!resp[0].ok);
    }
    // a message of exactly the maximum size is read, found invalid, and answered
    let (resp, exit) = session(&c, frame(&vec![b'{'; MAX_FRAME])).await;
    assert_eq!(exit, Exit::Eof);
    assert!(!resp[0].ok);

    assert_eq!(
        f.rec.calls.lock().unwrap().len(),
        1,
        "nothing hostile reached the app"
    );
    // empty stdin: clean exit, no output
    let (resp, exit) = session(&c, vec![]).await;
    assert_eq!((resp.len(), exit), (0, Exit::Eof));
}

#[tokio::test]
async fn nm_duplicate_request_id_adds_one_task_even_across_sessions() {
    let f = fixture("dup");
    let c = cfg(&f.ep);
    // the same id twice in one session, then again from a brand-new host process
    let mut input = add("same", "http://h/once.zip");
    input.extend(add("same", "http://h/once.zip"));
    let (r1, _) = session(&c, input).await;
    let (r2, _) = session(&c, add("same", "http://h/once.zip")).await;
    let all: Vec<_> = r1.iter().chain(&r2).collect();
    assert_eq!(all.len(), 3);
    assert!(all.iter().all(|r| r.ok && r.result == all[0].result));
    assert_eq!(
        f.rec.calls.lock().unwrap().len(),
        1,
        "one request id = one task"
    );
    // the same id with another URL is refused, not run
    let (r3, _) = session(&c, add("same", "http://h/other.zip")).await;
    assert!(!r3[0].ok);
    assert_eq!(f.rec.calls.lock().unwrap().len(), 1);
}

struct StartsApp {
    ep: Endpoint,
    launches: Arc<AtomicUsize>,
    started: Arc<Mutex<Option<eid_ipc::Running>>>,
    after: Duration,
}
impl Launcher for StartsApp {
    fn launch(&self) -> std::io::Result<()> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        let (ep, slot, after) = (self.ep.clone(), self.started.clone(), self.after);
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            let r = start(&ep, Arc::new(Recorder::default()), ServeOptions::default()).unwrap();
            *slot.lock().unwrap() = Some(r);
        });
        Ok(())
    }
}

#[tokio::test]
async fn nm_starts_the_app_once_when_absent_and_then_delivers() {
    let d = tempfile::tempdir().unwrap();
    let ep = Endpoint {
        name: format!(
            "nmL{}-{}",
            std::process::id(),
            &ipc::new_token().unwrap()[..8]
        ),
        token_path: d.path().join("tok"),
    };
    let launches = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Mutex::new(None));
    let c = HostConfig {
        endpoint: ep.clone(),
        launcher: Some(Box::new(StartsApp {
            ep: ep.clone(),
            launches: launches.clone(),
            started: started.clone(),
            after: Duration::from_millis(300),
        })),
        ready_deadline: Duration::from_secs(10),
        poll: Duration::from_millis(20),
    };
    // two messages: the app is started for the first, reused for the second
    let mut input = add("first", "http://h/a");
    input.extend(add("second", "http://h/b"));
    let (resp, exit) = session(&c, input).await;
    assert_eq!(exit, Exit::Eof);
    assert!(resp.iter().all(|r| r.ok), "{resp:?}");
    assert_eq!(
        launches.load(Ordering::SeqCst),
        1,
        "the app must be launched exactly once"
    );
    let r = started.lock().unwrap().take().unwrap();
    r.stop().await;
}

struct Never(Arc<AtomicUsize>);
impl Launcher for Never {
    fn launch(&self) -> std::io::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::other("cannot start in this test"))
    }
}

#[tokio::test]
async fn nm_gives_up_with_an_error_response_when_the_app_never_comes_up() {
    let d = tempfile::tempdir().unwrap();
    let ep = Endpoint {
        name: format!(
            "nmN{}-{}",
            std::process::id(),
            &ipc::new_token().unwrap()[..8]
        ),
        token_path: d.path().join("tok"),
    };
    let n = Arc::new(AtomicUsize::new(0));
    let c = HostConfig {
        endpoint: ep,
        launcher: Some(Box::new(Never(n.clone()))),
        ready_deadline: Duration::from_millis(400),
        poll: Duration::from_millis(20),
    };
    let t = std::time::Instant::now();
    let (resp, exit) = session(&c, add("x", "http://h/a")).await;
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "must give up at the deadline"
    );
    assert_eq!(exit, Exit::Eof);
    assert_eq!(resp.len(), 1);
    assert!(!resp[0].ok);
    assert_eq!(resp[0].id, "x");
    assert!(resp[0].error.as_deref().unwrap().contains("not running"));
    assert_eq!(n.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn nm_survives_the_app_restarting_between_messages() {
    let mut f = fixture("restart");
    let c = cfg(&f.ep);
    let (r1, _) = session(&c, add("one", "http://h/a")).await;
    assert!(r1[0].ok);
    // restart the app on the same endpoint (new token); a new host session still works
    f.running.take().unwrap().stop().await;
    let rec2 = Arc::new(Recorder::default());
    let run2 = start(&f.ep, rec2.clone(), ServeOptions::default()).unwrap();
    let (r2, _) = session(&c, add("two", "http://h/b")).await;
    assert!(r2[0].ok, "{r2:?}");
    assert_eq!(rec2.calls.lock().unwrap().len(), 1);
    run2.stop().await;
}

#[test]
fn the_launcher_refuses_a_missing_program_whether_overridden_or_looked_up_next_to_the_host() {
    use eid_native_host::AppLauncher;
    let d = tempfile::tempdir().unwrap();
    let ep = Endpoint {
        name: "x".into(),
        token_path: d.path().join("tok"),
    };
    // override pointing nowhere
    let l = AppLauncher {
        exe: Some(d.path().join("missing")),
        endpoint: ep.clone(),
    };
    assert_eq!(
        l.resolve().unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert!(l.launch().is_err());
    // a directory is not a program either
    let l = AppLauncher {
        exe: Some(d.path().to_path_buf()),
        endpoint: ep.clone(),
    };
    assert_eq!(
        l.resolve().unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    // no override: looks for `eid` next to the running executable (this test binary's folder has
    // none; its files are named `relay-<hash>` / `eid_native_host-<hash>`)
    let l = AppLauncher {
        exe: None,
        endpoint: ep,
    };
    let sibling = std::env::current_exe()
        .unwrap()
        .with_file_name(format!("eid{}", std::env::consts::EXE_SUFFIX));
    assert!(
        !sibling.exists(),
        "unexpected {} next to the test binary",
        sibling.display()
    );
    assert_eq!(
        l.resolve().unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}
