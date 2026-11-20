//! v0.1: the machine interface the desktop app's video jobs use (`--events`, `--items`,
//! `--stop-on-stdin-eof`, `eid video probe`, `eid video setup`), end to end with the fake
//! yt-dlp. These prove the plumbing only; they are not proof that real YouTube works.
mod video_env;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};
use video_env::*;

fn text(o: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        o.status,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The JSON event lines of stdout (other lines, like the final folder, are not events).
fn events(o: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("ev").is_some())
        .collect()
}

fn write_list(e: &Env, items: &[Value]) -> PathBuf {
    let p = e.root.path().join("list.json");
    std::fs::write(&p, serde_json::to_vec(items).unwrap()).unwrap();
    p
}

fn purl(e: &Env, items: &[Value], ptitle: &str) -> String {
    let list = write_list(e, items);
    e.url(
        "playlist/pl1",
        &format!(
            "&list={}&ptitle={}",
            pct(&list.display().to_string()),
            pct(ptitle)
        ),
    )
}

async fn setup(e: &Env) {
    let o = e
        .run(&["video", "setup", "--yes-i-agree", "--events"])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let ev = events(&o);
    assert!(ev.iter().any(|v| v["ev"] == "ready"), "{}", text(&o));
    assert_eq!(ev.last().unwrap()["ev"], "end");
    assert_eq!(ev.last().unwrap()["code"], 0);
}

#[tokio::test]
async fn probe_refuses_before_consent_and_tools_and_installs_nothing() {
    let e = Env::new().await;
    let o = e.run(&["video", "probe", &e.url("video/abc", "")]).await;
    assert_eq!(o.status.code(), Some(3), "{}", text(&o));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("TOOLS_MISSING"),
        "{}",
        text(&o)
    );
    assert_eq!(e.tool_requests(), 0, "probe must never download the tools");
    assert!(
        e.calls().is_empty(),
        "probe must never start the tool without consent"
    );
}

#[tokio::test]
async fn probe_of_a_video_offers_only_available_heights_never_above_4320() {
    let e = Env::new().await;
    setup(&e).await;
    let url = e.url("video/abc", "&title=Clip&heights=144,360,1080,4320,8640");
    let o = e.run(&["video", "probe", &url]).await;
    assert!(o.status.success(), "{}", text(&o));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["kind"], "video");
    assert_eq!(v["title"], "Clip");
    assert_eq!(
        v["qualities"],
        json!(["144p", "360p", "1080p", "4320p", "best", "audio", "m4a"])
    );
}

#[tokio::test]
async fn probe_of_a_playlist_lists_its_items_in_order() {
    let e = Env::new().await;
    setup(&e).await;
    let url = purl(
        &e,
        &[
            json!({"id": "a", "title": "Alpha"}),
            json!({"id": "b", "title": "Beta"}),
            json!({"id": "c", "title": "Gamma"}),
        ],
        "Mix",
    );
    let o = e.run(&["video", "probe", &url]).await;
    assert!(o.status.success(), "{}", text(&o));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["kind"], "playlist");
    assert_eq!(v["title"], "Mix");
    let items = v["items"].as_array().unwrap();
    let got: Vec<(u64, &str)> = items
        .iter()
        .map(|i| (i["index"].as_u64().unwrap(), i["title"].as_str().unwrap()))
        .collect();
    assert_eq!(got, [(1, "Alpha"), (2, "Beta"), (3, "Gamma")]);
    assert!(!v["qualities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|q| q == "8640p"));
}

#[tokio::test]
async fn playlist_events_and_items_selection() {
    let e = Env::new().await;
    setup(&e).await;
    let url = purl(
        &e,
        &[
            json!({"id": "a", "title": "Alpha"}),
            json!({"id": "b", "title": "Beta"}),
            json!({"id": "c", "title": "Gamma"}),
        ],
        "Mix",
    );
    let out = e.out().display().to_string();
    let o = e
        .run(&[
            "video",
            "playlist",
            &url,
            "-d",
            &out,
            "--quality",
            "360p",
            "--items",
            "1,3",
            "--events",
            "-q",
        ])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let ev = events(&o);
    let plan = ev.iter().find(|v| v["ev"] == "plan").expect("plan event");
    assert_eq!(plan["kind"], "playlist");
    let states: Vec<&str> = plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["todo", "skipped", "todo"]);
    let done: Vec<u64> = ev
        .iter()
        .filter(|v| v["ev"] == "item" && v["state"] == "done")
        .map(|v| v["n"].as_u64().unwrap())
        .collect();
    assert_eq!(done, [1, 3]);
    assert!(ev.iter().any(|v| v["ev"] == "progress" && v["n"] == 1));
    assert_eq!(ev.last().unwrap()["ev"], "end");
    assert_eq!(ev.last().unwrap()["code"], 0);
    let folder = e.out().join("Mix");
    let mut files: Vec<String> = entries(&folder)
        .into_iter()
        .filter(|n| !n.starts_with('.'))
        .collect();
    files.sort();
    assert_eq!(files, ["001 - Alpha.mp4", "003 - Gamma.mp4"]);
    // the same command again: finished items are not downloaded again, the plan says done
    let before = e.calls().len();
    let o = e
        .run(&[
            "video",
            "playlist",
            &url,
            "-d",
            &out,
            "--quality",
            "360p",
            "--items",
            "1,3",
            "--events",
            "-q",
        ])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let plan = events(&o).into_iter().find(|v| v["ev"] == "plan").unwrap();
    let states: Vec<String> = plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["state"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(states, ["done", "skipped", "done"]);
    let downloads = e.calls()[before..]
        .iter()
        .filter(|c| {
            !c["argv"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "--flat-playlist")
        })
        .count();
    assert_eq!(downloads, 0, "finished items were downloaded again");
}

#[tokio::test]
async fn single_video_events_report_the_height_obtained() {
    let e = Env::new().await;
    setup(&e).await;
    let url = e.url("video/abc", "&title=Clip&heights=360,720");
    let out = e.out().display().to_string();
    let o = e
        .run(&[
            "video",
            "get",
            &url,
            "-d",
            &out,
            "--quality",
            "1080p",
            "--events",
        ])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let ev = events(&o);
    assert_eq!(ev[0]["ev"], "plan");
    assert_eq!(ev[0]["kind"], "video");
    let heights: Vec<u64> = ev
        .iter()
        .filter(|v| v["ev"] == "progress")
        .filter_map(|v| v["height"].as_u64())
        .collect();
    assert!(!heights.is_empty(), "{}", text(&o));
    assert!(
        heights.iter().all(|h| *h == 720),
        "fallback to the tallest not taller: {heights:?}"
    );
    assert!(ev.iter().any(|v| v["ev"] == "item" && v["state"] == "done"));
    assert_eq!(ev.last().unwrap()["code"], 0);
}

/// v0.2 the download menu: the exact formats shown are what is downloaded (the 360p line of a
/// video that also has 720p gets 360p); if they are gone, the height's own choice applies;
/// "m4a" is the original audio stream, saved as .m4a, never converted.
#[tokio::test]
async fn exact_formats_from_the_menu_and_the_original_m4a_audio() {
    let e = Env::new().await;
    setup(&e).await;
    let out = e.out().display().to_string();
    let get = |url: String, extra: &'static [&'static str]| {
        let out = out.clone();
        let e = &e;
        async move {
            let mut a = vec!["video", "get", url.as_str(), "-d", out.as_str(), "--events"];
            a.extend_from_slice(extra);
            e.run(&a).await
        }
    };
    let heights = |o: &Output| -> Vec<u64> {
        events(o)
            .iter()
            .filter(|v| v["ev"] == "progress")
            .filter_map(|v| v["height"].as_u64())
            .collect()
    };
    // exact: v360+140 although "720p" alone would give 720
    let o = get(
        e.url("video/p1", "&title=Pinned&heights=360,720"),
        &["--quality", "720p", "--format", "v360+140"],
    )
    .await;
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        heights(&o).iter().all(|h| *h == 360) && !heights(&o).is_empty(),
        "{}",
        text(&o)
    );
    assert!(e.out().join("Pinned.mp4").is_file());
    // the exact formats are gone: the height's choice (720p, lower if missing)
    let o = get(
        e.url("video/p2", "&title=Gone&heights=360,720"),
        &["--quality", "720p", "--format", "v1080+140"],
    )
    .await;
    assert!(o.status.success(), "{}", text(&o));
    assert!(heights(&o).iter().all(|h| *h == 720), "{}", text(&o));
    // original audio: .m4a, no conversion
    let o = get(
        e.url("video/p3", "&title=Song&heights=360"),
        &["--quality", "m4a", "--format", "140"],
    )
    .await;
    assert!(o.status.success(), "{}", text(&o));
    assert!(e.out().join("Song.m4a").is_file(), "{}", text(&o));
    assert!(!e.out().join("Song.mp3").exists() && !e.out().join("Song.mp4").exists());
    // refused before anything runs: only `get`, only ids, only a fixed height or m4a
    for bad in [
        vec![
            "video",
            "playlist",
            "https://www.youtube.com/playlist?list=PL1",
            "--quality",
            "720p",
            "--format",
            "v360+140",
        ],
        vec![
            "video",
            "get",
            "https://x.test/v",
            "--quality",
            "best",
            "--format",
            "v360+140",
        ],
        vec![
            "video",
            "get",
            "https://x.test/v",
            "--quality",
            "720p",
            "--format",
            "bv*+ba",
        ],
        vec![
            "video",
            "get",
            "https://x.test/v",
            "--quality",
            "m4a",
            "--format",
            "137+140",
        ],
        vec![
            "video",
            "get",
            "https://x.test/v",
            "--quality",
            "720p",
            "--format",
        ],
    ] {
        let o = e.run(&bad).await;
        assert!(!o.status.success(), "{bad:?} accepted: {}", text(&o));
    }
}

#[tokio::test]
async fn closing_stdin_stops_the_run_like_ctrl_c() {
    let e = Env::new().await;
    setup(&e).await;
    let out = e.out().display().to_string();
    let url = e.url("sleep/30", "");
    let mut c = e.cmd(&[
        "video",
        "get",
        &url,
        "-d",
        &out,
        "--events",
        "--stop-on-stdin-eof",
    ]);
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // writing does not stop it; only the end of input does
    stdin.write_all(b"x\n").unwrap();
    // the fake tool (`/sleep/30`) logs nothing: give it time to start, then check the run is
    // still going (only the end of input may stop it)
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the run ended before stdin was closed"
    );
    drop(stdin);
    let t1 = Instant::now();
    let o = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(o.status.code(), Some(130), "{}", text(&o));
    assert!(
        t1.elapsed() < Duration::from_secs(15),
        "took {:?}",
        t1.elapsed()
    );
    let ev = events(&o);
    assert_eq!(ev.last().unwrap()["ev"], "end");
    assert_eq!(ev.last().unwrap()["code"], 130);
}

#[test]
fn new_flags_are_only_for_their_commands() {
    for bad in [
        vec!["get", "https://x.test/v", "--items", "1"],
        vec!["probe"],
        vec!["setup", "https://x.test/v"],
        vec!["playlist", "https://x.test/p", "--items", "0"],
    ] {
        let argv: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
        assert!(eid_cli::video::parse(&argv).is_err(), "{bad:?}");
    }
    let ok: Vec<String> = [
        "playlist",
        "https://x.test/p",
        "--items",
        "1,4",
        "--events",
        "--stop-on-stdin-eof",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let a = eid_cli::video::parse(&ok).unwrap();
    assert_eq!(a.items, [1, 4]);
    assert!(a.events && a.stdin_stop);
}
