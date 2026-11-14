//! End-to-end tests of `eid serve` and the remote commands with the real binary over the real
//! per-user endpoint: single instance, forwarding, remote add/list/pause/resume, no secrets.
use eid_platform::process::command;
use eid_testserver::{Behavior, Content, TestServer};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

const EID: &str = env!("CARGO_BIN_EXE_eid");

fn s(a: &[&str]) -> Vec<String> {
    a.iter().map(|x| x.to_string()).collect()
}

async fn run(args: Vec<String>) -> std::process::Output {
    tokio::task::spawn_blocking(move || command(EID, args).output().unwrap())
        .await
        .unwrap()
}

fn text(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn sha(p: &Path) -> String {
    eid_engine::state::hex(&Sha256::digest(std::fs::read(p).unwrap()))
}

/// A running `eid serve` on a private endpoint; killed on drop.
struct App {
    child: std::process::Child,
    conn: Vec<String>,
    queues: PathBuf,
    dl: PathBuf,
    log: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl App {
    async fn start(tag: &str, extra: &[&str]) -> App {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("cli{}-{tag}", std::process::id());
        let conn = s(&[
            "--endpoint",
            &name,
            "--token-file",
            dir.path().join("tok").to_str().unwrap(),
        ]);
        let (queues, dl, log) = (
            dir.path().join("q.json"),
            dir.path().join("dl"),
            dir.path().join("serve.log"),
        );
        let mut args = s(&[
            "serve",
            "-d",
            dl.to_str().unwrap(),
            "--queues",
            queues.to_str().unwrap(),
            "--ramp-ms",
            "5",
            "--checkpoint-ms",
            "50",
            "--min-split",
            "65536",
            "--attempts",
            "4",
            "-c",
            "2",
        ]);
        args.extend(extra.iter().map(|x| x.to_string()));
        args.extend(conn.clone());
        let child = command(EID, args)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let app = App {
            child,
            conn,
            queues,
            dl,
            log,
            _dir: dir,
        };
        app.wait_ready().await;
        app
    }

    async fn wait_ready(&self) {
        let t = Instant::now();
        loop {
            let o = self.remote(&["list"]).await;
            if o.status.success() {
                return;
            }
            assert!(
                t.elapsed() < Duration::from_secs(30),
                "server never became ready: {}\nserver log: {}",
                text(&o),
                std::fs::read_to_string(&self.log).unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn remote(&self, args: &[&str]) -> std::process::Output {
        let mut a = s(args);
        a.extend(self.conn.clone());
        run(a).await
    }

    async fn list(&self) -> String {
        let o = self.remote(&["list"]).await;
        assert!(o.status.success(), "{}", text(&o));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    async fn wait_list(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let t = Instant::now();
        loop {
            let l = self.list().await;
            if pred(&l) {
                return l;
            }
            assert!(
                t.elapsed() < Duration::from_secs(60),
                "timed out waiting for {what}; list:\n{l}\nserver log: {}",
                std::fs::read_to_string(&self.log).unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_instance_hands_over_its_command_and_exits() {
    let srv = TestServer::start().await;
    srv.add(
        "/second.bin",
        Content::Generated {
            seed: 11,
            len: 300_000,
        },
        Behavior::default(),
    );
    let app = App::start("second", &[]).await;

    // A second `serve` with --add must not start another instance: it forwards and exits.
    let t = Instant::now();
    let mut args = s(&["serve", "--add", &format!("{}/second.bin", srv.base_url())]);
    args.extend(app.conn.clone());
    let o = run(args).await;
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        t.elapsed() < Duration::from_secs(15),
        "the second instance must exit promptly, took {:?}",
        t.elapsed()
    );
    assert!(
        text(&o).contains("handed to the running instance"),
        "{}",
        text(&o)
    );

    // the FIRST instance downloaded it, and the file is byte-exact
    app.wait_list("the forwarded item to finish", |l| l.contains(" done "))
        .await;
    let got = app.dl.join("second.bin");
    assert_eq!(
        sha(&got),
        Content::Generated {
            seed: 11,
            len: 300_000
        }
        .sha256_hex()
    );
    // a second `serve` without --add (and without -d) just reports and leaves
    let mut args = s(&["serve"]);
    args.extend(app.conn.clone());
    let o = run(args).await;
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("already running"), "{}", text(&o));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_add_list_pause_resume_drive_the_running_app() {
    let srv = TestServer::start().await;
    // slow enough to be paused while running
    srv.add(
        "/slow.bin",
        Content::Generated {
            seed: 21,
            len: 2_000_000,
        },
        Behavior::slow(200_000),
    );
    srv.add(
        "/fast.bin",
        Content::Generated {
            seed: 22,
            len: 400_000,
        },
        Behavior::default(),
    );
    let mut app = App::start("remote", &["--parallel", "2"]).await;
    let o = app
        .remote(&["add", &format!("{}/slow.bin", srv.base_url())])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("added #0"), "{}", text(&o));
    app.wait_list("slow item to start", |l| l.contains("#0 running"))
        .await;

    let o = app.remote(&["pause", "0"]).await;
    assert!(o.status.success(), "{}", text(&o));
    app.wait_list("slow item paused", |l| l.contains("#0 paused"))
        .await;

    let o = app
        .remote(&["add", &format!("{}/fast.bin", srv.base_url())])
        .await;
    assert!(text(&o).contains("added #1"), "{}", text(&o));
    app.wait_list("fast item done", |l| l.contains("#1 done"))
        .await;
    assert_eq!(
        sha(&app.dl.join("fast.bin")),
        Content::Generated {
            seed: 22,
            len: 400_000
        }
        .sha256_hex()
    );
    // the paused one stayed paused while the other ran
    assert!(app.list().await.contains("#0 paused"));

    let o = app.remote(&["resume", "0"]).await;
    assert!(o.status.success(), "{}", text(&o));
    app.wait_list("slow item done after resume", |l| l.contains("#0 done"))
        .await;
    assert_eq!(
        sha(&app.dl.join("slow.bin")),
        Content::Generated {
            seed: 21,
            len: 2_000_000
        }
        .sha256_hex()
    );

    // unknown ids and garbage are errors with exit code 1/2, never a crash of the app
    let o = app.remote(&["pause", "999"]).await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let o = app.remote(&["pause", "x"]).await;
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let o = app.remote(&["add"]).await;
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let o = app.remote(&["add", "a", "b"]).await;
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let o = app.remote(&["add", "file:///etc/passwd"]).await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let o = app.remote(&["add", "ftp://h/x"]).await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let l = app.list().await;
    assert!(!l.contains("passwd") && !l.contains("ftp:"), "{l}");
    assert!(app.child_alive());
}

impl App {
    fn child_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_commands_without_a_running_app_say_so_and_fail() {
    let d = tempfile::tempdir().unwrap();
    let conn = s(&[
        "--endpoint",
        &format!("nobody{}", std::process::id()),
        "--token-file",
        d.path().join("tok").to_str().unwrap(),
    ]);
    for cmd in [
        s(&["list"]),
        s(&["add", "http://h/x"]),
        s(&["pause", "1"]),
        s(&["resume", "1"]),
    ] {
        let mut a = cmd.clone();
        a.extend(conn.clone());
        let t = Instant::now();
        let o = run(a).await;
        assert_eq!(o.status.code(), Some(1), "{cmd:?}: {}", text(&o));
        assert!(text(&o).contains("not running"), "{cmd:?}: {}", text(&o));
        assert!(t.elapsed() < Duration::from_secs(10));
    }
    // an invalid endpoint name never turns into a path
    let o = run(s(&[
        "list",
        "--endpoint",
        "../../evil",
        "--token-file",
        "x",
    ]))
    .await;
    assert!(!o.status.success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_add_never_lets_the_caller_choose_a_directory_and_keeps_secrets_out_of_files() {
    let app = App::start("secrets", &[]).await;
    // credentials and a signed query in the URL; a traversal in sub-folder and file name
    let o = app
        .remote(&[
            "add",
            "http://user:HUNTER2SECRET@127.0.0.1:1/a.bin?token=QUERYSECRET",
            "--subdir",
            "../../outside",
            "--name",
            "..\\..\\evil.bin",
        ])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let l = app.list().await;
    assert!(l.contains("#0"), "{l}");
    assert!(
        !l.contains("HUNTER2SECRET") && !l.contains("QUERYSECRET"),
        "{l}"
    );
    // wait until the scheduler persisted it, then inspect the file itself
    let t = Instant::now();
    let file = loop {
        if let Ok(f) = std::fs::read_to_string(&app.queues) {
            if f.contains("127.0.0.1") {
                break f;
            }
        }
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "queue file never written"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        !file.contains("HUNTER2SECRET"),
        "password leaked into the queue file"
    );
    assert!(
        !file.contains("QUERYSECRET") || file.contains("token="),
        "{file}"
    );
    assert!(!file.contains("user:"), "{file}");
    let item: serde_json::Value = serde_json::from_str(&file).unwrap();
    let sub = item.to_string();
    assert!(
        !sub.contains("../") && !sub.contains("..\\"),
        "unsanitized path in {sub}"
    );
    // nothing escaped the download directory
    let parent = app.dl.parent().unwrap();
    assert!(!parent.join("outside").exists());
    assert!(!parent.join("evil.bin").exists());
    // the server's own log never prints the secrets either
    let log = std::fs::read_to_string(&app.log).unwrap_or_default();
    assert!(
        !log.contains("HUNTER2SECRET") && !log.contains("QUERYSECRET"),
        "{log}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eid_stop_makes_the_running_app_exit_cleanly_and_free_its_endpoint() {
    let mut app = App::start("stop", &[]).await;
    let o = app.remote(&["stop"]).await;
    assert!(o.status.success(), "{}", text(&o));
    // the process exits by itself (exit code 0 = graceful), and the endpoint is free again
    let t = Instant::now();
    let status = loop {
        if let Some(s) = app.child.try_wait().unwrap() {
            break s;
        }
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "the app did not exit after `eid stop`"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(status.success(), "unclean exit: {status:?}");
    let o = app.remote(&["list"]).await;
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("not running"), "{}", text(&o));
    // a new instance can start on the same endpoint afterwards
    let mut args = s(&["serve", "-q", "-d", app.dl.to_str().unwrap(), "--queues"]);
    args.push(app.queues.to_str().unwrap().to_owned());
    args.extend(app.conn.clone());
    let mut again = command(EID, args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    app.wait_ready().await;
    let o = app.remote(&["stop"]).await;
    assert!(o.status.success());
    assert!(again.wait().unwrap().success());
}
