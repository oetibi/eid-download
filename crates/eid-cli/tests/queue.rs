//! End-to-end tests of `eid queue ...` and `eid grab` with the real binary.
use eid_engine::scheduler::load_queues;
use eid_platform::process::command;
use eid_testserver::{Behavior, Content, TestServer};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const EID: &str = env!("CARGO_BIN_EXE_eid");

fn sha(p: &Path) -> String {
    eid_engine::state::hex(&Sha256::digest(std::fs::read(p).unwrap()))
}

async fn run(args: Vec<String>) -> std::process::Output {
    tokio::task::spawn_blocking(move || command(EID, args).output().unwrap())
        .await
        .unwrap()
}
fn s(a: &[&str]) -> Vec<String> {
    a.iter().map(|x| x.to_string()).collect()
}
fn fast() -> Vec<String> {
    s(&[
        "--ramp-ms",
        "5",
        "--checkpoint-ms",
        "50",
        "--min-split",
        "65536",
        "--attempts",
        "6",
        "-c",
        "2",
    ])
}
fn text(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_queue_add_list_export_import_roundtrip_and_no_secrets() {
    let d = tempfile::tempdir().unwrap();
    let q = d.path().join("q.json");
    let dl = d.path().join("dl");
    let qs = q.to_str().unwrap();
    let o = run(s(&[
        "queue",
        "add",
        qs,
        "-d",
        dl.to_str().unwrap(),
        "--parallel",
        "3",
        "http://h.example/img(08-11).jpg",
    ]))
    .await;
    assert!(o.status.success(), "{}", text(&o));
    let o = run(s(&[
        "queue",
        "add",
        qs,
        "http://bob:HUNTER2pw@h.example/secret.bin?token=SIGNED123",
    ]))
    .await;
    assert!(o.status.success(), "{}", text(&o));
    let o = run(s(&["queue", "list", qs])).await;
    let out = text(&o);
    assert!(
        out.contains("img08.jpg") && out.contains("img11.jpg") && out.contains("secret.bin"),
        "{out}"
    );
    assert!(out.contains("parallel=3"));
    for secret in ["HUNTER2pw", "bob", "SIGNED123", "token="] {
        assert!(!out.contains(secret), "list printed {secret}: {out}");
    }
    let on_disk = std::fs::read_to_string(&q).unwrap();
    for secret in ["HUNTER2pw", "bob:"] {
        assert!(!on_disk.contains(secret), "queue file holds {secret}");
    }
    assert!(
        on_disk.contains("token=SIGNED123"),
        "the query is needed to download and stays in the (private) file"
    );
    // export -> import into a fresh queue file gives the same list
    for ext in ["json", "csv"] {
        let out_file = d.path().join(format!("export.{ext}"));
        let o = run(s(&["queue", "export", qs, out_file.to_str().unwrap()])).await;
        assert!(o.status.success(), "{}", text(&o));
        let q2 = d.path().join(format!("q2-{ext}.json"));
        let o = run(s(&[
            "queue",
            "import",
            q2.to_str().unwrap(),
            out_file.to_str().unwrap(),
            "-d",
            dl.to_str().unwrap(),
        ]))
        .await;
        assert!(o.status.success(), "{}", text(&o));
        let (a, b) = (load_queues(&q).unwrap(), load_queues(&q2).unwrap());
        let urls = |x: &eid_engine::queue::QueueSet| {
            x.queues[0]
                .items
                .iter()
                .map(|i| i.url.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(urls(&a), urls(&b), "{ext}");
        // exporting twice to the same file refuses to overwrite
        let again = run(s(&["queue", "export", qs, out_file.to_str().unwrap()])).await;
        assert!(!again.status.success());
    }
    // a hostile list file is handled, never a crash
    let evil = d.path().join("evil.txt");
    std::fs::write(
        &evil,
        "javascript:alert(1)\nfile:///etc/passwd\n../../x\nhttp://ok.example/f\n\u{0}\u{0}\n",
    )
    .unwrap();
    let q3 = d.path().join("q3.json");
    let o = run(s(&[
        "queue",
        "import",
        q3.to_str().unwrap(),
        evil.to_str().unwrap(),
        "-d",
        dl.to_str().unwrap(),
    ]))
    .await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(load_queues(&q3).unwrap().queues[0].items.len(), 1);
    let big = d.path().join("big.txt");
    std::fs::write(&big, vec![b'a'; 9 << 20]).unwrap();
    let o = run(s(&[
        "queue",
        "import",
        q3.to_str().unwrap(),
        big.to_str().unwrap(),
    ]))
    .await;
    assert!(
        !o.status.success() && text(&o).contains("larger than"),
        "{}",
        text(&o)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_queue_run_downloads_everything_and_reports_failures() {
    let srv = TestServer::start().await;
    for i in 0..3 {
        srv.add(
            &format!("/f{i}.bin"),
            Content::Generated {
                seed: 70 + i,
                len: 600_000,
            },
            Behavior::default(),
        );
    }
    let d = tempfile::tempdir().unwrap();
    let (q, dl) = (d.path().join("q.json"), d.path().join("dl"));
    let mut add = s(&[
        "queue",
        "add",
        q.to_str().unwrap(),
        "-d",
        dl.to_str().unwrap(),
        "--parallel",
        "2",
        "--retries",
        "0",
    ]);
    add.push(format!("{}/f(0-2).bin", srv.base_url()));
    assert!(run(add).await.status.success());
    let mut args = s(&["queue", "run", q.to_str().unwrap()]);
    args.extend(fast());
    let o = run(args.clone()).await;
    assert!(o.status.success(), "{}", text(&o));
    for i in 0..3u64 {
        assert_eq!(
            sha(&dl.join(format!("f{i}.bin"))),
            Content::Generated {
                seed: 70 + i,
                len: 600_000
            }
            .sha256_hex()
        );
    }
    // running again does nothing (everything is Done) and succeeds
    let count = |srv: &TestServer| -> Vec<usize> {
        (0..3)
            .map(|i| srv.stats(&format!("/f{i}.bin")).requests.len())
            .collect()
    };
    let before = count(&srv);
    assert!(before.iter().all(|&n| n >= 1));
    let o = run(args).await;
    assert!(o.status.success());
    assert_eq!(before, count(&srv), "a finished item was requested again");
    // a 404 is a permanent failure: exit code 1, other items still done
    let (q, dl) = (d.path().join("q2.json"), d.path().join("dl2"));
    let mut add = s(&[
        "queue",
        "add",
        q.to_str().unwrap(),
        "-d",
        dl.to_str().unwrap(),
        "--retries",
        "0",
    ]);
    add.push(format!("{}/missing.bin", srv.base_url()));
    add.push(format!("{}/f1.bin", srv.base_url()));
    assert!(run(add).await.status.success());
    let mut args = s(&["queue", "run", q.to_str().unwrap()]);
    args.extend(fast());
    let o = run(args).await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let qs = load_queues(&q).unwrap();
    let st = |n: &str| {
        qs.queues[0]
            .items
            .iter()
            .find(|i| i.url.ends_with(n))
            .unwrap()
            .status
    };
    assert_eq!(st("missing.bin"), eid_engine::queue::Status::Failed);
    assert_eq!(st("f1.bin"), eid_engine::queue::Status::Done);
    assert!(!text(&o).contains("127.0.0.1:") || !text(&o).contains('?'));
}

/// SIGKILL a running queue (like a crash/power cut), run it again: the finished item is not
/// requested again, the interrupted one resumes at its durable frontier, the rest completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_queue_sigkill_then_run_resumes_and_never_repeats_finished_items() {
    let srv = TestServer::start().await;
    let len: u64 = 12 << 20;
    srv.add(
        "/a-small.bin",
        Content::Generated {
            seed: 1,
            len: 300_000,
        },
        Behavior::default(),
    );
    srv.add(
        "/b-big.bin",
        Content::Generated { seed: 2, len },
        Behavior {
            throttle_bps: Some(2_500_000),
            ..Behavior::default()
        },
    );
    srv.add(
        "/c-tail.bin",
        Content::Generated {
            seed: 3,
            len: 300_000,
        },
        Behavior::default(),
    );
    let d = tempfile::tempdir().unwrap();
    let (q, dl) = (d.path().join("q.json"), d.path().join("dl"));
    let mut add = s(&[
        "queue",
        "add",
        q.to_str().unwrap(),
        "-d",
        dl.to_str().unwrap(),
        "--parallel",
        "1",
    ]);
    for n in ["a-small", "b-big", "c-tail"] {
        add.push(format!("{}/{n}.bin", srv.base_url()));
    }
    assert!(run(add).await.status.success());
    let mut args = s(&["queue", "run", q.to_str().unwrap(), "-q"]);
    args.extend(fast());
    let mut child = command(EID, args.clone()).spawn().unwrap();
    // wait until >= 30% of the big file is durable
    let t = Instant::now();
    loop {
        if let Ok(st) = eid_engine::state::peek(&dl.join("b-big.bin.eid-state")) {
            let next: u64 = st.segments.iter().map(|s| s.next - s.start).sum();
            if next * 10 >= len * 3 {
                break;
            }
        }
        assert!(t.elapsed() < Duration::from_secs(40), "no durable progress");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    // what the next process will see (a kill can leave the primary missing: peek falls back to nothing,
    // so read through the engine's loader that knows the .bak)
    let sp: PathBuf = dl.join("b-big.bin.eid-state");
    let st = eid_engine::state::load(&sp).unwrap();
    let durable: u64 = st.segments.iter().map(|s| s.next - s.start).sum();
    assert!(durable * 10 >= len * 3);
    let mid = load_queues(&q).unwrap();
    let status = |n: &str| {
        mid.queues[0]
            .items
            .iter()
            .find(|i| i.url.ends_with(n))
            .unwrap()
            .status
    };
    assert_eq!(status("a-small.bin"), eid_engine::queue::Status::Done);
    // fresh stats, a fast server
    for p in ["/a-small.bin", "/b-big.bin", "/c-tail.bin"] {
        srv.reset_stats(p);
    }
    srv.set_behavior("/b-big.bin", Behavior::default());
    let o = run(args).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        srv.stats("/a-small.bin").requests.len(),
        0,
        "a finished item was downloaded again"
    );
    assert!(
        !srv.stats("/c-tail.bin").requests.is_empty(),
        "the pending item was never downloaded"
    );
    let starts: Vec<u64> = srv
        .stats("/b-big.bin")
        .requests
        .iter()
        .filter_map(|r| r.range_header.as_deref())
        .map(|h| {
            h.trim_start_matches("bytes=")
                .split('-')
                .next()
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect();
    assert!(!starts.is_empty());
    for &s0 in &starts {
        let seg = st
            .segments
            .iter()
            .find(|g| g.start <= s0 && s0 < g.end)
            .unwrap();
        assert!(
            s0 >= seg.next,
            "re-requested durable bytes: {s0} < {}",
            seg.next
        );
    }
    assert!(
        starts.iter().any(|&x| x > 0),
        "restarted from byte 0: {starts:?}"
    );
    assert_eq!(
        sha(&dl.join("a-small.bin")),
        Content::Generated {
            seed: 1,
            len: 300_000
        }
        .sha256_hex()
    );
    assert_eq!(
        sha(&dl.join("b-big.bin")),
        Content::Generated { seed: 2, len }.sha256_hex()
    );
    assert_eq!(
        sha(&dl.join("c-tail.bin")),
        Content::Generated {
            seed: 3,
            len: 300_000
        }
        .sha256_hex()
    );
    let mut left: Vec<String> = std::fs::read_dir(&dl)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        ["a-small.bin", "b-big.bin", "c-tail.bin"],
        "junk left behind"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_grab_lists_links_and_adds_them_to_a_queue() {
    let srv = TestServer::start().await;
    srv.add(
        "/page.html",
        Content::Bytes(
            br#"<a href="/x/a.zip">a</a><a href="b.iso">b</a><a href="/c.pdf">c</a>"#.to_vec(),
        ),
        Behavior {
            content_type: Some("text/html".into()),
            ..Behavior::default()
        },
    );
    srv.add(
        "/huge.html",
        Content::Generated {
            seed: 1,
            len: 100 << 20,
        },
        Behavior {
            content_type: Some("text/html".into()),
            ..Behavior::default()
        },
    );
    let url = format!("{}/page.html", srv.base_url());
    let o = run(s(&["grab", &url, "--ext", "zip,iso"])).await;
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(out.lines().count(), 2, "{out}");
    assert!(out.contains("/x/a.zip") && out.contains("/b.iso") && !out.contains("c.pdf"));
    let d = tempfile::tempdir().unwrap();
    let q = d.path().join("q.json");
    let o = run(s(&[
        "grab",
        &url,
        "--add-to",
        q.to_str().unwrap(),
        "-d",
        d.path().join("dl").to_str().unwrap(),
    ]))
    .await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(load_queues(&q).unwrap().queues[0].items.len(), 3);
    let o = run(s(&["grab", &format!("{}/huge.html", srv.base_url())])).await;
    assert!(!o.status.success());
    assert!(text(&o).contains("larger than"), "{}", text(&o));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_queue_categorize_sorts_downloads_into_sub_folders_inside_the_download_dir() {
    let srv = TestServer::start().await;
    for (p, seed) in [
        ("/a.zip", 1u64),
        ("/b.mp3", 2),
        ("/c.pdf", 3),
        ("/d.unknown", 4),
    ] {
        srv.add(
            p,
            Content::Generated { seed, len: 200_000 },
            Behavior::default(),
        );
    }
    let d = tempfile::tempdir().unwrap();
    let (q, dl) = (d.path().join("q.json"), d.path().join("dl"));
    let mut add = s(&[
        "queue",
        "add",
        q.to_str().unwrap(),
        "-d",
        dl.to_str().unwrap(),
        "--categorize",
        "--retries",
        "0",
    ]);
    for p in ["a.zip", "b.mp3", "c.pdf", "d.unknown"] {
        add.push(format!("{}/{p}", srv.base_url()));
    }
    assert!(run(add).await.status.success());
    let mut args = s(&["queue", "run", q.to_str().unwrap(), "-q"]);
    args.extend(fast());
    let o = run(args).await;
    assert!(o.status.success(), "{}", text(&o));
    for (sub, name, seed) in [
        ("Compressed", "a.zip", 1u64),
        ("Music", "b.mp3", 2),
        ("Documents", "c.pdf", 3),
        ("Other", "d.unknown", 4),
    ] {
        let p = dl.join(sub).join(name);
        assert_eq!(
            sha(&p),
            Content::Generated { seed, len: 200_000 }.sha256_hex(),
            "{p:?}"
        );
    }
    // nothing outside the download directory, nothing loose in it
    let mut top: Vec<String> = std::fs::read_dir(&dl)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    top.sort();
    assert_eq!(top, ["Compressed", "Documents", "Music", "Other"]);
    let mut outer: Vec<String> = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    outer.sort();
    assert_eq!(
        outer,
        ["dl", "q.json", "q.json.bak"],
        "nothing was written outside the download directory"
    );
}
