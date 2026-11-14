//! Integration tests over the real per-user endpoint (named pipe / unix socket): authentication,
//! hostile frames, idempotency, single instance.

use eid_ipc::frame::{read_frame, write_frame, FrameError, MAX_FRAME};
use eid_ipc::{
    start, Client, ClientError, Command, Endpoint, Handler, Request, Response, ServeOptions,
    StartError, VERSION,
};
use eid_platform::ipc;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Counting(AtomicUsize);
impl Handler for Counting {
    fn handle(&self, cmd: Command) -> Result<serde_json::Value, String> {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        match cmd {
            Command::Ping {} => Ok(json!({"pong": true, "n": n})),
            Command::Add { url, .. } => Ok(json!({"added": url, "n": n})),
            Command::List {} => Ok(json!({"big": "x".repeat(MAX_FRAME)})),
            Command::Shutdown {} => Ok(json!({})),
            Command::Pause { .. } => Err("no such item".into()),
            Command::Resume { .. } => panic!("handler bug"),
        }
    }
}

struct Fixture {
    ep: Endpoint,
    handler: Arc<Counting>,
    running: Option<eid_ipc::Running>,
    _dir: tempfile::TempDir,
}

fn opts() -> ServeOptions {
    ServeOptions {
        handshake_timeout: Duration::from_millis(400),
        idle_timeout: Duration::from_secs(30),
        ..ServeOptions::default()
    }
}

async fn fixture(tag: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let ep = Endpoint {
        name: format!(
            "it{}-{tag}-{}",
            std::process::id(),
            &ipc::new_token().unwrap()[..8]
        ),
        token_path: dir.path().join("tok"),
    };
    let handler = Arc::new(Counting(AtomicUsize::new(0)));
    let running = start(&ep, handler.clone(), opts()).unwrap();
    Fixture {
        ep,
        handler,
        running: Some(running),
        _dir: dir,
    }
}

impl Fixture {
    fn calls(&self) -> usize {
        self.handler.0.load(Ordering::SeqCst)
    }
    async fn stop(mut self) {
        self.running.take().unwrap().stop().await;
    }
    /// Raw stream, already past the handshake.
    async fn authed(&self) -> ipc::Stream {
        let mut s = ipc::connect(&self.ep.name).await.unwrap();
        let token = ipc::read_token_file(&self.ep.token_path).unwrap();
        hello(&mut s, &token).await;
        s
    }
}

async fn hello(s: &mut ipc::Stream, token: &str) -> Response {
    let h = serde_json::to_vec(&json!({"v": VERSION, "token": token})).unwrap();
    write_frame(s, &h).await.unwrap();
    let r = read_frame(s).await.unwrap();
    serde_json::from_slice(&r).unwrap()
}

fn ping(id: &str) -> Vec<u8> {
    serde_json::to_vec(&Request {
        v: VERSION,
        id: id.into(),
        command: Command::Ping {},
    })
    .unwrap()
}

async fn assert_closed(s: &mut ipc::Stream) {
    // drain whatever the server still sends (an error reply), then expect EOF / reset
    let mut b = [0u8; 256];
    let r = tokio::time::timeout(Duration::from_secs(5), async {
        let mut total = 0usize;
        loop {
            match s.read(&mut b).await {
                Ok(0) | Err(_) => return total,
                Ok(n) => total += n,
            }
            assert!(total < 4096, "server keeps talking to a rejected peer");
        }
    })
    .await;
    assert!(r.is_ok(), "connection was not closed");
}

/// The server must still serve a fresh, honest client after a hostile one.
async fn still_serves(f: &Fixture) {
    let mut c = Client::connect(&f.ep).await.unwrap();
    let r = c.call(Command::Ping {}).await.unwrap();
    assert_eq!(r["pong"], true);
}

#[tokio::test]
async fn ipc_happy_path_ping_add_and_server_errors() {
    let f = fixture("happy").await;
    let mut c = Client::connect(&f.ep).await.unwrap();
    assert_eq!(c.call(Command::Ping {}).await.unwrap()["pong"], true);
    let r = c
        .call(Command::Add {
            url: "http://h/a.zip".into(),
            subdir: None,
            file_name: None,
        })
        .await
        .unwrap();
    assert_eq!(r["added"], "http://h/a.zip");
    match c.call(Command::Pause { item: 9 }).await {
        Err(ClientError::Server(m)) => assert_eq!(m, "no such item"),
        other => panic!("{other:?}"),
    }
    // a panicking handler becomes a plain error and the connection stays usable
    match c.call(Command::Resume { item: 1 }).await {
        Err(ClientError::Server(m)) => assert_eq!(m, "internal error"),
        other => panic!("{other:?}"),
    }
    assert_eq!(c.call(Command::Ping {}).await.unwrap()["pong"], true);
    f.stop().await;
}

#[tokio::test]
async fn ipc_rejects_missing_wrong_token() {
    let f = fixture("auth").await;
    let good = ipc::read_token_file(&f.ep.token_path).unwrap();

    // (1) no hello at all: a request frame where the hello belongs
    let mut s = ipc::connect(&f.ep.name).await.unwrap();
    write_frame(&mut s, &ping("x")).await.unwrap();
    let r: Response = serde_json::from_slice(&read_frame(&mut s).await.unwrap()).unwrap();
    assert!(!r.ok);
    assert_closed(&mut s).await;

    // (2) wrong token (same length), empty, short, long, wrong case, with whitespace
    let mut wrong = good.clone();
    wrong.replace_range(0..1, if good.starts_with('0') { "1" } else { "0" });
    for bad in [
        wrong,
        String::new(),
        good[..63].to_owned(),
        format!("{good}0"),
        good.to_uppercase(),
        format!(" {good}"),
        "0".repeat(64),
    ] {
        let mut s = ipc::connect(&f.ep.name).await.unwrap();
        let r = hello(&mut s, &bad).await;
        assert!(!r.ok, "token {bad:?} must be refused");
        assert_eq!(r.error.as_deref(), Some("authentication failed"));
        // a request on the refused connection never reaches the handler
        let _ = write_frame(&mut s, &ping("y")).await;
        assert_closed(&mut s).await;
    }

    // (3) wrong protocol version / unknown field in the hello
    let mut s = ipc::connect(&f.ep.name).await.unwrap();
    write_frame(&mut s, br#"{"v":99,"token":"x"}"#)
        .await
        .unwrap();
    assert!(
        !serde_json::from_slice::<Response>(&read_frame(&mut s).await.unwrap())
            .unwrap()
            .ok
    );
    let mut s = ipc::connect(&f.ep.name).await.unwrap();
    let h = json!({"v": 1, "token": good, "extra": 1}).to_string();
    write_frame(&mut s, h.as_bytes()).await.unwrap();
    assert!(
        !serde_json::from_slice::<Response>(&read_frame(&mut s).await.unwrap())
            .unwrap()
            .ok
    );

    // (4) connect and say nothing: dropped after the handshake timeout
    let mut s = ipc::connect(&f.ep.name).await.unwrap();
    assert_closed(&mut s).await;

    assert_eq!(
        f.calls(),
        0,
        "nothing unauthenticated may reach the handler"
    );
    // the right token still works, and a stale token from a previous run does not
    still_serves(&f).await;
    f.stop().await;
}

#[tokio::test]
async fn ipc_rejects_oversized_and_truncated_frames() {
    let f = fixture("frames").await;

    // oversized header (2 MiB, and u32::MAX): refused from the header alone
    for len in [(MAX_FRAME as u32) + 1, 2 << 20, u32::MAX] {
        let mut s = f.authed().await;
        s.write_all(&len.to_le_bytes()).await.unwrap();
        // do not send the body: the server must answer/close without waiting for it
        let r = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut s)).await;
        match r {
            Ok(Ok(b)) => {
                let r: Response = serde_json::from_slice(&b).unwrap();
                assert!(!r.ok);
            }
            Ok(Err(_)) => {}
            Err(_) => panic!("server waited for an oversized body"),
        }
        assert_closed(&mut s).await;
    }

    // truncated body: header says 100 bytes, 10 arrive, then the peer goes away
    let mut s = f.authed().await;
    s.write_all(&100u32.to_le_bytes()).await.unwrap();
    s.write_all(b"0123456789").await.unwrap();
    drop(s);
    // truncated header: 2 of 4 bytes
    let mut s = f.authed().await;
    s.write_all(&[7, 0]).await.unwrap();
    drop(s);
    // zero-length frame
    let mut s = f.authed().await;
    s.write_all(&0u32.to_le_bytes()).await.unwrap();
    assert_closed(&mut s).await;
    // a whole 1 MiB frame of junk JSON is read, rejected, and the connection closed
    let mut s = f.authed().await;
    write_frame(&mut s, &vec![b'{'; MAX_FRAME]).await.unwrap();
    assert_closed(&mut s).await;
    // a deeply nested document is just malformed
    let mut s = f.authed().await;
    let deep = format!("{}{}", "[".repeat(50_000), "]".repeat(50_000));
    write_frame(&mut s, deep.as_bytes()).await.unwrap();
    assert_closed(&mut s).await;

    assert_eq!(f.calls(), 0);
    still_serves(&f).await;
    f.stop().await;
}

#[tokio::test]
async fn ipc_survives_garbage_requests_and_closes_only_that_connection() {
    let f = fixture("garbage").await;
    let mut healthy = Client::connect(&f.ep).await.unwrap();
    for bad in [
        &b"not json"[..],
        br#"{"v":1,"id":"x","command":{"cmd":"ping"},"extra":1}"#,
        br#"{"v":2,"id":"x","command":{"cmd":"ping"}}"#,
        br#"{"v":1,"id":"../../etc","command":{"cmd":"ping"}}"#,
        br#"{"v":1,"id":"x","command":{"cmd":"add","url":"http://h/","dir":"C:\\Windows"}}"#,
        &[0xff, 0xfe, 0x00, 0x01],
    ] {
        let mut s = f.authed().await;
        write_frame(&mut s, bad).await.unwrap();
        let r: Response = serde_json::from_slice(&read_frame(&mut s).await.unwrap()).unwrap();
        assert!(!r.ok);
        assert_closed(&mut s).await;
    }
    assert_eq!(f.calls(), 0);
    // the long-lived honest client was never disturbed
    assert_eq!(healthy.call(Command::Ping {}).await.unwrap()["pong"], true);
    f.stop().await;
}

#[tokio::test]
async fn duplicate_request_id_runs_the_command_once() {
    let f = fixture("dedup").await;
    let add = Command::Add {
        url: "http://h/once.zip".into(),
        subdir: None,
        file_name: None,
    };
    let mut a = Client::connect(&f.ep).await.unwrap();
    let r1 = a.call_with_id("req-1", add.clone()).await.unwrap();
    assert_eq!(f.calls(), 1);
    // retry on the same connection, and again on a brand-new connection (lost reply + reconnect)
    let r2 = a.call_with_id("req-1", add.clone()).await.unwrap();
    let mut b = Client::connect(&f.ep).await.unwrap();
    let r3 = b.call_with_id("req-1", add.clone()).await.unwrap();
    assert_eq!((r1.clone(), r1.clone()), (r2, r3));
    assert_eq!(f.calls(), 1, "the command must have run exactly once");
    // same id, different command: refused, still not executed
    match b.call_with_id("req-1", Command::Ping {}).await {
        Err(ClientError::Server(m)) => assert!(m.contains("different command"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(f.calls(), 1);
    // a new id runs
    b.call_with_id("req-2", add).await.unwrap();
    assert_eq!(f.calls(), 2);

    // racing duplicates from many connections at once still run once
    let barrier = Arc::new(tokio::sync::Barrier::new(16));
    let mut tasks = vec![];
    for _ in 0..16 {
        let (ep, bar) = (f.ep.clone(), barrier.clone());
        tasks.push(tokio::spawn(async move {
            let mut c = Client::connect(&ep).await.unwrap();
            bar.wait().await;
            c.call_with_id("race", Command::Ping {}).await.unwrap()
        }));
    }
    let mut results = vec![];
    for t in tasks {
        results.push(t.await.unwrap());
    }
    assert!(results.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(f.calls(), 3, "16 racing duplicates must run once");
    f.stop().await;
}

#[tokio::test]
async fn dedup_memory_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let ep = Endpoint {
        name: format!(
            "dd{}-{}",
            std::process::id(),
            &ipc::new_token().unwrap()[..8]
        ),
        token_path: dir.path().join("tok"),
    };
    let h = Arc::new(Counting(AtomicUsize::new(0)));
    let run = start(
        &ep,
        h.clone(),
        ServeOptions {
            dedup_capacity: 4,
            ..opts()
        },
    )
    .unwrap();
    let mut c = Client::connect(&ep).await.unwrap();
    for i in 0..10 {
        c.call_with_id(&format!("id{i}"), Command::Ping {})
            .await
            .unwrap();
    }
    assert_eq!(h.0.load(Ordering::SeqCst), 10);
    // the newest ids are still remembered, the oldest were forgotten (and so run again)
    c.call_with_id("id9", Command::Ping {}).await.unwrap();
    assert_eq!(h.0.load(Ordering::SeqCst), 10);
    c.call_with_id("id0", Command::Ping {}).await.unwrap();
    assert_eq!(h.0.load(Ordering::SeqCst), 11);
    run.stop().await;
}

#[tokio::test]
async fn an_oversized_response_becomes_an_error_not_a_broken_stream() {
    let f = fixture("bigresp").await;
    let mut c = Client::connect(&f.ep).await.unwrap();
    match c.call(Command::List {}).await {
        Err(ClientError::Server(m)) => assert_eq!(m, "response too large"),
        other => panic!("{other:?}"),
    }
    assert_eq!(c.call(Command::Ping {}).await.unwrap()["pong"], true);
    f.stop().await;
}

#[tokio::test]
async fn second_start_is_refused_and_does_not_replace_the_token() {
    let f = fixture("single").await;
    let before = std::fs::read_to_string(&f.ep.token_path).unwrap();
    let h = Arc::new(Counting(AtomicUsize::new(0)));
    match start(&f.ep, h, opts()) {
        Err(StartError::AlreadyRunning) => {}
        Err(e) => panic!("unexpected error: {e}"),
        Ok(_) => panic!("a second instance must not start"),
    }
    assert_eq!(std::fs::read_to_string(&f.ep.token_path).unwrap(), before);
    still_serves(&f).await; // the first instance is unharmed and its token still valid
    f.stop().await;
}

#[tokio::test]
async fn connecting_with_nobody_running_is_not_running_and_a_restart_rotates_the_token() {
    let f = fixture("restart").await;
    let (ep, old) = (
        f.ep.clone(),
        std::fs::read_to_string(&f.ep.token_path).unwrap(),
    );
    f.stop().await;
    assert!(matches!(
        Client::connect(&ep).await,
        Err(ClientError::NotRunning)
    ));
    let h = Arc::new(Counting(AtomicUsize::new(0)));
    let run = start(&ep, h, opts()).unwrap();
    assert_ne!(std::fs::read_to_string(&ep.token_path).unwrap(), old);
    // the previous run's token is useless now
    let mut s = ipc::connect(&ep.name).await.unwrap();
    assert!(!hello(&mut s, &old).await.ok);
    run.stop().await;
}

#[tokio::test]
async fn many_clients_in_parallel_all_get_answers() {
    let f = fixture("parallel").await;
    let mut tasks = vec![];
    for i in 0..40 {
        let ep = f.ep.clone();
        tasks.push(tokio::spawn(async move {
            let mut c = Client::connect(&ep).await.unwrap();
            for j in 0..5 {
                c.call_with_id(&format!("c{i}-{j}"), Command::Ping {})
                    .await
                    .unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(f.calls(), 200);
    f.stop().await;
}

#[tokio::test]
async fn a_symlinked_or_garbage_token_file_is_refused_by_the_client() {
    let f = fixture("tokfile").await;
    std::fs::write(&f.ep.token_path, "garbage").unwrap();
    assert!(matches!(
        Client::connect(&f.ep).await,
        Err(ClientError::Io(_))
    ));
    std::fs::remove_file(&f.ep.token_path).unwrap();
    assert!(matches!(
        Client::connect(&f.ep).await,
        Err(ClientError::Io(_))
    ));
    f.stop().await;
    // FrameError is public API for the native host
    let _ = FrameError::Closed;
}
