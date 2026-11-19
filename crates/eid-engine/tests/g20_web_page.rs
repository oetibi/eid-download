//! v0.2, owner's Windows test (2026-11-19): a YouTube link added as a normal download saved a
//! 1.4 MB HTML page named `watch`. With `Config::refuse_web_pages` (on in the desktop app and
//! `eid serve`) a link that answers with a web page is refused BEFORE anything is written, with
//! a message that says what to do; real page files (`.html`), a page the user names `.html`,
//! and every other type still download, and `eid get` (default config) is unchanged.
mod common;
use common::*;
use eid_engine::{CancellationToken, Error, JobSpec};
use eid_testserver::{Behavior, Content, RangeMode};
use std::path::Path;

fn page() -> Content {
    Content::Bytes(b"<!doctype html><title>YouTube</title><p>not a video".repeat(2000))
}

fn typed(ct: &str, range: RangeMode) -> Behavior {
    Behavior {
        content_type: Some(ct.into()),
        range,
        ..Behavior::default()
    }
}

async fn get(
    url: &str,
    dir: &Path,
    name: Option<&str>,
    refuse: bool,
) -> eid_engine::Result<eid_engine::Outcome> {
    let mut cfg = fast_cfg(4);
    cfg.refuse_web_pages = refuse;
    eid_engine::download(
        JobSpec {
            url: url.into(),
            dir: dir.into(),
            file_name: name.map(str::to_owned),
        },
        cfg,
        Log::default().sink(),
        CancellationToken::new(),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_web_page_is_refused_before_anything_is_written_and_files_still_download() {
    // a page under a file-less name, ranged (206) and plain (200), any letter case
    for (ct, range) in [
        ("text/html; charset=utf-8", RangeMode::Honor),
        ("TEXT/HTML", RangeMode::Ignore200),
        ("application/xhtml+xml", RangeMode::Honor),
    ] {
        let (_s, url) = serve("/watch", page(), typed(ct, range)).await;
        let d = tempfile::tempdir().unwrap();
        let e = get(&format!("{url}?v=abc"), d.path(), None, true)
            .await
            .expect_err(ct);
        assert!(matches!(e, Error::WebPage(_)), "{ct}: {e}");
        let text = e.to_string();
        assert!(
            text.contains("web page") && text.contains(".html"),
            "{text}"
        );
        assert!(
            only_files(d.path()).is_empty(),
            "{ct}: nothing may be written: {:?}",
            only_files(d.path())
        );
        // a name chosen by the user does not hide a page either...
        let e = get(&url, d.path(), Some("clip.mp4"), true)
            .await
            .expect_err(ct);
        assert!(matches!(e, Error::WebPage(_)), "{ct}: {e}");
        assert!(only_files(d.path()).is_empty());
        // ...unless it is a page name: then the page is saved, exactly
        let o = get(&url, d.path(), Some("saved page.html"), true)
            .await
            .unwrap();
        assert_eq!(sha_of_file(&o.path), page().sha256_hex(), "{ct}");
    }

    // a real .html link, and every non-page type, download as before
    let (_s, url) = serve(
        "/docs/index.html",
        page(),
        typed("text/html", RangeMode::Honor),
    )
    .await;
    let d = tempfile::tempdir().unwrap();
    let o = get(&url, d.path(), None, true).await.unwrap();
    assert_eq!(sha_of_file(&o.path), page().sha256_hex());
    let bin = Content::Generated {
        seed: 7,
        len: 3 << 20,
    };
    for ct in [
        None,
        Some("application/octet-stream"),
        Some("video/mp4"),
        Some("text/plain"),
    ] {
        let b = Behavior {
            content_type: ct.map(str::to_owned),
            ..Behavior::default()
        };
        let (_s, url) = serve("/watch", bin.clone(), b).await;
        let d = tempfile::tempdir().unwrap();
        let o = get(&url, d.path(), None, true)
            .await
            .unwrap_or_else(|e| panic!("{ct:?}: {e}"));
        assert_eq!(sha_of_file(&o.path), bin.sha256_hex(), "{ct:?}");
    }

    // off (the default, `eid get`): the page is saved as asked, as before
    let (_s, url) = serve("/watch", page(), typed("text/html", RangeMode::Honor)).await;
    let d = tempfile::tempdir().unwrap();
    let o = get(&url, d.path(), None, false).await.unwrap();
    assert_eq!(sha_of_file(&o.path), page().sha256_hex());
    assert!(!eid_engine::Config::default().refuse_web_pages);
}

/// Through the queue (the desktop app's path): the item fails at once with the message, is not
/// retried, and no file is left.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queued_web_page_fails_once_with_the_message_and_leaves_nothing() {
    use eid_engine::queue::{Queue, QueueSet, Status};
    use eid_engine::queue_runner::EngineRunner;
    use eid_engine::scheduler::{
        load_queues, save_queues, NoPower, Scheduler, SchedulerConfig, SystemClock,
    };
    use std::sync::Arc;
    let (s, url) = serve("/watch", page(), typed("text/html", RangeMode::Honor)).await;
    let d = tempfile::tempdir().unwrap();
    let qpath = d.path().join("queues.json");
    let dl = d.path().join("dl");
    std::fs::create_dir(&dl).unwrap();
    let mut qs = QueueSet::default();
    qs.queues.push(Queue::new("main", dl.to_str().unwrap()));
    qs.add_item("main", &url, None, None, None).unwrap();
    save_queues(&qpath, &qs).unwrap();
    let mut cfg = fast_cfg(4);
    cfg.refuse_web_pages = true;
    let sched = Scheduler::open(
        SchedulerConfig::new(&qpath),
        Arc::new(EngineRunner::new(cfg)),
        Arc::new(SystemClock),
        Arc::new(NoPower),
        Arc::new(|_| {}),
    )
    .unwrap();
    sched.run(CancellationToken::new(), true).await.unwrap();
    let it = &load_queues(&qpath).unwrap().queues[0].items[0];
    assert_eq!(it.status, Status::Failed);
    assert!(
        it.last_error.as_deref().unwrap_or("").contains("web page"),
        "{:?}",
        it.last_error
    );
    assert_eq!(it.attempts, 1, "a page is not retried");
    assert!(only_files(&dl).is_empty(), "{:?}", only_files(&dl));
    assert_eq!(
        s.stats("/watch").requests.len(),
        1,
        "one request, no retries"
    );
}
