//! v0.1: the video job list (`eid_cli::video_jobs`) driving the real `eid` binary with the fake
//! yt-dlp: consent + tool install, probe, a playlist job with a selection, pause / resume /
//! remove of a running job, persistence. Plumbing only: not proof that real YouTube works.
mod video_env;
use eid_cli::video_jobs::{JobsConfig, Kind, VideoJobs};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use video_env::*;

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn job(v: &VideoJobs, id: u64) -> Value {
    v.list()
        .into_iter()
        .find(|j| j["job"] == id)
        .unwrap_or(Value::Null)
}

// one test: the status functions read this process's environment (the per-user folders)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn video_jobs_end_to_end_with_the_fake_tool() {
    let e = Env::new().await;
    for (k, v) in e.envs() {
        // the only test in this binary; set before anything reads the environment
        std::env::set_var(k, v);
    }
    let state = e.root.path().join("cfg").join("video-jobs.json");
    let cfg = JobsConfig {
        eid_exe: env!("CARGO_BIN_EXE_eid").into(),
        state_file: state.clone(),
        default_dir: e.out(),
        max_running: 1,
        env: e
            .envs()
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
    };
    let v = VideoJobs::start(cfg.clone());

    // nothing before consent; probe says TOOLS_MISSING and downloads nothing
    let st = v.tools_status();
    assert_eq!(st["installed"], false);
    assert_eq!(st["consent"], false);
    let err = v.probe(&e.url("video/abc", "")).await.unwrap_err();
    assert!(err.contains("TOOLS_MISSING"), "{err}");
    assert_eq!(e.tool_requests(), 0);
    assert!(v.tools_install(false).is_err());
    assert!(v.consent_text("ar").unwrap().len() > 100);

    v.tools_install(true).unwrap();
    wait_for("tools", 60, || {
        let s = v.tools_status();
        s["installed"] == true && s["installing"] == false
    })
    .await;
    assert_eq!(v.tools_status()["consent"], true);

    // probe a playlist, then download items 1 and 3 of it
    let list = e.root.path().join("list.json");
    std::fs::write(
        &list,
        serde_json::to_vec(&json!([
            {"id": "a", "title": "Alpha"}, {"id": "b", "title": "Beta"}, {"id": "c", "title": "Gamma"}
        ]))
        .unwrap(),
    )
    .unwrap();
    let purl = e.url(
        "playlist/pl1",
        &format!("&list={}&ptitle=Mix", pct(&list.display().to_string())),
    );
    let p = v.probe(&purl).await.unwrap();
    assert_eq!(p["kind"], "playlist");
    assert_eq!(p["items"].as_array().unwrap().len(), 3);
    assert!(
        v.add(Kind::Playlist, &purl, "4320p", None, None).is_err(),
        "no 8K"
    );
    assert!(v
        .add(Kind::Video, &purl, "720p", Some(vec![1]), None)
        .is_err());
    let id = v
        .add(Kind::Playlist, &purl, "360p", Some(vec![1, 3]), None)
        .unwrap();
    wait_for("playlist job", 60, || job(&v, id)["state"] == "done").await;
    let j = job(&v, id);
    assert_eq!(j["title"], "Mix");
    assert_eq!(j["done_items"], 2);
    assert_eq!(j["total_items"], 2);
    let states: Vec<&str> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["done", "skipped", "done"]);
    let folder = e.out().join("Mix");
    assert!(folder.join("01 - Alpha.mp4").is_file());
    assert!(folder.join("03 - Gamma.mp4").is_file());
    assert!(!folder.join("02 - Beta.mp4").exists());

    // a long-running job: pause stops the child, resume starts it again, remove stops it
    let slow = e.url("sleep/60", "");
    let id2 = v.add(Kind::Video, &slow, "best", None, None).unwrap();
    wait_for("running", 30, || job(&v, id2)["state"] == "running").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let t0 = Instant::now();
    v.pause(id2).unwrap();
    assert_eq!(job(&v, id2)["state"], "paused");
    // the child really ended: a resumed job can start again (max_running = 1)
    v.resume(id2).unwrap();
    wait_for("running again", 30, || job(&v, id2)["state"] == "running").await;
    assert!(
        t0.elapsed() < Duration::from_secs(20),
        "pause took {:?}",
        t0.elapsed()
    );
    v.remove(id2).unwrap();
    assert!(job(&v, id2).is_null());

    // the list survives a restart; nothing restarts by itself
    let id3 = v.add(Kind::Video, &slow, "720p", None, None).unwrap();
    wait_for("running 3", 30, || job(&v, id3)["state"] == "running").await;
    v.shutdown().await;
    drop(v);
    let saved: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert!(saved["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|j| j["job"] == id));
    let v2 = VideoJobs::start(cfg);
    assert_eq!(job(&v2, id)["state"], "done");
    assert_eq!(job(&v2, id3)["state"], "paused");
    v2.remove(id3).unwrap();
}
