//! v0.1: the video commands over IPC (what the browser's native host sends) against a real
//! `eid serve`, with the fake yt-dlp. Plumbing only: not proof that real YouTube works.
mod video_env;
use eid_ipc::{Client, ClientError, Command, Endpoint};
use std::process::Stdio;
use std::time::{Duration, Instant};
use video_env::*;

struct Serve(std::process::Child);
impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipc_probe_and_add_video_through_eid_serve() {
    let e = Env::new().await;
    let ep = Endpoint {
        name: format!(
            "sv{}-{}",
            std::process::id(),
            &eid_platform::ipc::new_token().unwrap()[..8]
        ),
        token_path: e.root.path().join("tok"),
    };
    let out = e.out().display().to_string();
    let q = e.root.path().join("q.json").display().to_string();
    let tok = ep.token_path.display().to_string();
    let mut c = e.cmd(&[
        "serve",
        "-q",
        "-d",
        &out,
        "--queues",
        &q,
        "--endpoint",
        &ep.name,
        "--token-file",
        &tok,
    ]);
    c.stdout(Stdio::null()).stderr(Stdio::null());
    let _srv = Serve(c.spawn().unwrap());
    let t0 = Instant::now();
    let mut cl = loop {
        match Client::connect(&ep).await {
            Ok(c) => break c,
            Err(_) if t0.elapsed() < Duration::from_secs(30) => {
                tokio::time::sleep(Duration::from_millis(100)).await
            }
            Err(e) => panic!("server never became ready: {e}"),
        }
    };
    let video = e.url("video/abc", "&title=Clip&heights=360,1080");
    // before consent: refused, nothing installed or started
    match cl.call(Command::Probe { url: video.clone() }).await {
        Err(ClientError::Server(m)) => assert!(m.contains("TOOLS_MISSING"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(e.tool_requests(), 0);
    // no 8K, no raw format strings, no folder from the caller
    // (an invalid request is answered, then that connection is closed: a fresh one each time)
    for quality in ["4320p", "bv*+ba"] {
        let mut cl = Client::connect(&ep).await.unwrap();
        match cl
            .call(Command::AddVideo {
                url: video.clone(),
                quality: quality.into(),
                if_missing: None,
            })
            .await
        {
            Err(ClientError::Server(m)) => assert!(m.contains("quality"), "{m}"),
            other => panic!("{quality}: {other:?}"),
        }
    }
    let mut cl = Client::connect(&ep).await.unwrap();
    // consent + tools (the desktop app's tools_install does the same), then probe and add
    let o = e.run(&["video", "setup", "--yes-i-agree", "-q"]).await;
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let p = cl
        .call(Command::Probe { url: video.clone() })
        .await
        .unwrap();
    assert_eq!(p["kind"], "video");
    assert_eq!(
        p["qualities"],
        serde_json::json!(["360p", "1080p", "best", "audio"])
    );
    let r = cl
        .call(Command::AddVideo {
            url: video.clone(),
            quality: "360p".into(),
            if_missing: None,
        })
        .await
        .unwrap();
    assert!(r["job"].as_u64().is_some(), "{r}");
    let t0 = Instant::now();
    while !e.out().join("Clip.mp4").is_file() {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "the video was not downloaded: {:?}",
            entries(&e.out())
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // the job list lives in the per-user config folder of this test, not the real one
    let cfg = e.root.path().join("cfg");
    let jobs_file = if eid_platform::IS_WINDOWS {
        cfg.join("EidDownload").join("video-jobs.json")
    } else {
        cfg.join("eid-download").join("video-jobs.json")
    };
    assert!(jobs_file.is_file(), "{}", jobs_file.display());
}
