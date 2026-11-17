//! P2 against the real engine and the bad test server: the Grabber over HTTP, and a queue whose
//! jobs are real downloads (parallel limit, resume after a stop).
mod common;
use common::*;
use eid_engine::grabber::{grab_url, GrabOptions};
use eid_engine::queue::{Queue, QueueSet, Status};
use eid_engine::queue_runner::EngineRunner;
use eid_engine::scheduler::{
    load_queues, save_queues, NoPower, QueueEvent, Scheduler, SchedulerConfig, SystemClock,
};
use eid_engine::CancellationToken;
use eid_testserver::{Behavior, Content, Redirect, TestServer};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn html_behavior() -> Behavior {
    Behavior {
        content_type: Some("text/html; charset=utf-8".into()),
        ..Behavior::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grabber_lists_the_links_of_a_real_page_and_follows_a_redirect() {
    let srv = TestServer::start().await;
    let page = br#"<html><body><a href="/files/a.zip">A</a><a href="b.pdf">B</a>
        <a href="http://other.example/c.iso">C</a><script>location="http://evil/x"</script></body></html>"#;
    srv.add(
        "/dir/page.html",
        Content::Bytes(page.to_vec()),
        html_behavior(),
    );
    srv.add(
        "/old",
        Content::Bytes(vec![]),
        Behavior {
            redirect: Some(Redirect::To("/dir/page.html".into())),
            ..Behavior::default()
        },
    );
    for path in ["/dir/page.html", "/old"] {
        let url = format!("{}{}", srv.base_url(), path);
        let g = grab_url(
            &url,
            &fast_cfg(1),
            &GrabOptions::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let urls: Vec<&str> = g.iter().map(|x| x.url.as_str()).collect();
        let base = srv.base_url();
        assert_eq!(
            urls,
            [
                format!("{base}/files/a.zip"),
                format!("{base}/dir/b.pdf"),
                "http://other.example/c.iso".to_string()
            ],
            "{path}"
        );
    }
    let only_zip = GrabOptions {
        exts: vec!["zip".into()],
        ..GrabOptions::default()
    };
    let url = format!("{}/dir/page.html", srv.base_url());
    let g = grab_url(&url, &fast_cfg(1), &only_zip, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(g.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grabber_refuses_a_huge_page_before_reading_it_and_other_bad_pages() {
    let srv = TestServer::start().await;
    // 300 MiB claimed HTML: refused from Content-Length, the body is never pulled
    srv.add(
        "/huge.html",
        Content::Generated {
            seed: 1,
            len: 300 << 20,
        },
        html_behavior(),
    );
    let url = format!("{}/huge.html", srv.base_url());
    let r = grab_url(
        &url,
        &fast_cfg(1),
        &GrabOptions::default(),
        &CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(r, Err(eid_engine::Error::InvalidInput(ref m)) if m.contains("larger than")),
        "{r:?}"
    );
    // what the server managed to push is bounded by kernel socket buffers (a few MiB), nowhere
    // near the 300 MiB it offered: the client stopped reading at the header check
    let sent = srv.stats("/huge.html").bytes_sent_total;
    assert!(
        sent < 32 << 20,
        "the server sent {sent} bytes of a refused page"
    );
    // same refusal through the streaming cap when the limit is below the page size
    srv.add(
        "/mid.html",
        Content::Bytes(vec![b' '; 5000]),
        html_behavior(),
    );
    let url = format!("{}/mid.html", srv.base_url());
    let tiny = GrabOptions {
        max_bytes: 1000,
        ..GrabOptions::default()
    };
    assert!(
        grab_url(&url, &fast_cfg(1), &tiny, &CancellationToken::new())
            .await
            .is_err()
    );
    // not HTML
    srv.add(
        "/file.zip",
        Content::Bytes(b"PK".to_vec()),
        Behavior {
            content_type: Some("application/zip".into()),
            ..Behavior::default()
        },
    );
    let url = format!("{}/file.zip", srv.base_url());
    let r = grab_url(
        &url,
        &fast_cfg(1),
        &GrabOptions::default(),
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(r, Err(eid_engine::Error::BadResponse(_))), "{r:?}");
    // 404 and a refused scheme
    let url = format!("{}/missing", srv.base_url());
    assert!(matches!(
        grab_url(
            &url,
            &fast_cfg(1),
            &GrabOptions::default(),
            &CancellationToken::new()
        )
        .await,
        Err(eid_engine::Error::Status { status: 404, .. })
    ));
    assert!(grab_url(
        "file:///etc/passwd",
        &fast_cfg(1),
        &GrabOptions::default(),
        &CancellationToken::new()
    )
    .await
    .is_err());
    // a page that never answers is bounded by the header deadline
    srv.add(
        "/hang.html",
        Content::Bytes(b"<a href=x>".to_vec()),
        Behavior {
            header_delay: Some(eid_testserver::HeaderDelay {
                skip: 0,
                count: 100,
                delay: Duration::from_secs(60),
                partial: false,
            }),
            ..html_behavior()
        },
    );
    let mut cfg = fast_cfg(1);
    cfg.header_timeout = Duration::from_millis(300);
    let t = std::time::Instant::now();
    let url = format!("{}/hang.html", srv.base_url());
    assert!(grab_url(
        &url,
        &cfg,
        &GrabOptions::default(),
        &CancellationToken::new()
    )
    .await
    .is_err());
    assert!(t.elapsed() < Duration::from_secs(10));
}

/// Maximum number of simultaneously open requests over several paths, from the server's own log.
fn peak_overlap(srv: &TestServer, paths: &[String]) -> usize {
    use eid_testserver::ServerEventKind::*;
    let mut ev: Vec<(u64, i32)> = vec![];
    for p in paths {
        for e in srv.stats(p).events {
            ev.push((e.t_us, if e.kind == RequestStart { 1 } else { -1 }));
        }
    }
    ev.sort_by_key(|e| (e.0, -e.1)); // at the same instant, count the start first (pessimistic)
    let (mut cur, mut peak) = (0i32, 0i32);
    for (_, d) in ev {
        cur += d;
        peak = peak.max(cur);
    }
    peak as usize
}

fn real_cfg() -> eid_engine::Config {
    let mut c = fast_cfg(1);
    c.checkpoint_interval = Duration::from_millis(30);
    c
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queue_of_real_downloads_respects_its_parallel_limit_and_finishes_everything() {
    let srv = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<String> = (0..6).map(|i| format!("/q{i}.bin")).collect();
    for (i, p) in paths.iter().enumerate() {
        srv.add(
            p,
            Content::Generated {
                seed: 40 + i as u64,
                len: 1 << 20,
            },
            Behavior::slow(1_500_000),
        );
    }
    let qpath = dir.path().join("queues.json");
    let mut qs = QueueSet::default();
    let mut q = Queue::new("main", dir.path().join("dl").to_str().unwrap());
    q.max_parallel = 2;
    qs.queues.push(q);
    for p in &paths {
        qs.add_item(
            "main",
            &format!("{}{}", srv.base_url(), p),
            None,
            None,
            None,
        )
        .unwrap();
    }
    save_queues(&qpath, &qs).unwrap();
    let events: Arc<Mutex<Vec<QueueEvent>>> = Arc::default();
    let ev = events.clone();
    let s = Scheduler::open(
        SchedulerConfig::new(&qpath),
        Arc::new(EngineRunner::new(real_cfg())),
        Arc::new(SystemClock),
        Arc::new(NoPower),
        Arc::new(move |e| ev.lock().unwrap().push(e)),
    )
    .unwrap();
    s.run(CancellationToken::new(), true).await.unwrap();
    let done = load_queues(&qpath).unwrap();
    assert!(
        done.queues[0]
            .items
            .iter()
            .all(|i| i.status == Status::Done),
        "{:?}",
        done.queues[0].items
    );
    // each file is bit-exact
    for (i, it) in done.queues[0].items.iter().enumerate() {
        let name = it
            .file_name
            .as_ref()
            .expect("name recorded when the job started");
        let sha = sha_of_file(&dir.path().join("dl").join(name));
        assert_eq!(
            sha,
            Content::Generated {
                seed: 40 + i as u64,
                len: 1 << 20
            }
            .sha256_hex(),
            "{name}"
        );
    }
    let peak = peak_overlap(&srv, &paths);
    assert_eq!(peak, 2, "server-observed concurrent downloads");
    // no leftovers beside the 6 finished files
    assert_eq!(only_files(&dir.path().join("dl")).len(), 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_the_queue_mid_download_and_reopening_resumes_from_the_frontier() {
    let srv = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let len: u64 = 12 << 20;
    srv.add(
        "/big.bin",
        Content::Generated { seed: 5, len },
        Behavior::slow(4_000_000),
    );
    srv.add(
        "/small.bin",
        Content::Generated {
            seed: 6,
            len: 200_000,
        },
        Behavior::default(),
    );
    let qpath = dir.path().join("queues.json");
    let dl = dir.path().join("dl");
    let mut qs = QueueSet::default();
    let mut q = Queue::new("main", dl.to_str().unwrap());
    q.max_parallel = 1;
    qs.queues.push(q);
    for p in ["/small.bin", "/big.bin"] {
        qs.add_item(
            "main",
            &format!("{}{}", srv.base_url(), p),
            None,
            None,
            None,
        )
        .unwrap();
    }
    save_queues(&qpath, &qs).unwrap();
    let open = |q: &std::path::Path| {
        Scheduler::open(
            SchedulerConfig::new(q),
            Arc::new(EngineRunner::new(real_cfg())),
            Arc::new(SystemClock),
            Arc::new(NoPower),
            Arc::new(|_| {}),
        )
        .unwrap()
    };
    let s = open(&qpath);
    let cancel = CancellationToken::new();
    let (s2, c2) = (s.clone(), cancel.clone());
    let h = tokio::spawn(async move { s2.run(c2, true).await });
    // wait until >= 30% of big.bin is durable in its sidecar
    let t = std::time::Instant::now();
    let frontier = loop {
        if let Ok(st) = eid_engine::state::peek(&dl.join("big.bin.eid-state")) {
            let next: u64 = st.segments.iter().map(|s| s.next - s.start).sum();
            if next * 10 >= len * 3 {
                break next;
            }
        }
        assert!(t.elapsed() < Duration::from_secs(30), "no durable progress");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    cancel.cancel();
    h.await.unwrap().unwrap();
    let mid = load_queues(&qpath).unwrap();
    let st = |n: &str| {
        mid.queues[0]
            .items
            .iter()
            .find(|i| i.url.ends_with(n))
            .unwrap()
            .status
    };
    assert_eq!(st("small.bin"), Status::Done);
    assert_eq!(
        st("big.bin"),
        Status::Pending,
        "a stopped job stays resumable, not failed"
    );
    assert_eq!(
        mid.queues[0]
            .items
            .iter()
            .find(|i| i.url.ends_with("big.bin"))
            .unwrap()
            .file_name
            .as_deref(),
        Some("big.bin")
    );
    let state_now = eid_engine::state::peek(&dl.join("big.bin.eid-state")).unwrap();
    let saved: u64 = state_now.segments.iter().map(|s| s.next - s.start).sum();
    assert!(saved >= frontier);
    // reopen: the finished item is not requested again, the big one resumes at its frontier
    srv.reset_stats("/small.bin");
    srv.reset_stats("/big.bin");
    srv.set_behavior("/big.bin", Behavior::default());
    let s = open(&qpath);
    s.run(CancellationToken::new(), true).await.unwrap();
    assert_eq!(
        srv.stats("/small.bin").requests.len(),
        0,
        "a finished item was downloaded again"
    );
    let starts: Vec<u64> = srv
        .stats("/big.bin")
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
        let seg = state_now
            .segments
            .iter()
            .find(|g| g.start <= s0 && s0 < g.end)
            .expect("inside the file");
        assert!(
            s0 >= seg.next,
            "re-requested durable bytes: {s0} < {} (segment {seg:?})",
            seg.next
        );
    }
    assert!(
        starts.iter().any(|&x| x > 0),
        "the resumed download restarted from byte 0: {starts:?}"
    );
    let done = load_queues(&qpath).unwrap();
    assert!(done.queues[0]
        .items
        .iter()
        .all(|i| i.status == Status::Done));
    assert_eq!(
        sha_of_file(&dl.join("big.bin")),
        Content::Generated { seed: 5, len }.sha256_hex()
    );
    assert_eq!(
        only_files(&dl),
        vec!["big.bin".to_string(), "small.bin".to_string()]
    );
}
