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
    // nothing above 8K, no raw format strings, no folder from the caller
    // (an invalid request is answered, then that connection is closed: a fresh one each time)
    for quality in ["8640p", "bv*+ba"] {
        let mut cl = Client::connect(&ep).await.unwrap();
        match cl
            .call(Command::AddVideo {
                url: video.clone(),
                quality: quality.into(),
                if_missing: None,
                format: None,
            })
            .await
        {
            Err(ClientError::Server(m)) => assert!(m.contains("quality"), "{m}"),
            other => panic!("{quality}: {other:?}"),
        }
    }
    // v0.2: exact formats are ids only, and only with a fixed height or m4a
    for (quality, format) in [
        ("720p", "bv*+ba"),
        ("720p", "1+2+3"),
        ("best", "v1080+140"),
        ("720p", "-x"),
    ] {
        let mut cl = Client::connect(&ep).await.unwrap();
        match cl
            .call(Command::AddVideo {
                url: video.clone(),
                quality: quality.into(),
                if_missing: None,
                format: Some(format.into()),
            })
            .await
        {
            Err(ClientError::Server(m)) => assert!(m.contains("format"), "{m}"),
            other => panic!("{quality} {format}: {other:?}"),
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
        serde_json::json!(["360p", "1080p", "best", "audio", "m4a"])
    );
    // v0.2 the download menu's options: exact format pairs, tallest first, the tool's details
    assert_eq!(
        p["options"],
        serde_json::json!([
            {"quality": "1080p", "height": 1080, "format": "v1080+140", "container": "mp4",
             "fps": 60, "codec": "H.264", "hdr": null, "audio": true, "size": 10_805_000},
            {"quality": "360p", "height": 360, "format": "v360+140", "container": "mp4",
             "fps": 30, "codec": "H.264", "hdr": null, "audio": true, "size": 3_605_000},
        ])
    );
    assert_eq!(
        p["audio_options"],
        serde_json::json!([
            {"quality": "m4a", "original": true, "format": "140", "codec": "AAC", "kbps": 128, "size": 5000},
            {"quality": "audio", "original": false, "format": null, "codec": "MP3", "kbps": 128, "size": null},
        ])
    );
    // the 1080p line, exactly as shown
    let opt = &p["options"][0];
    let r = cl
        .call(Command::AddVideo {
            url: video.clone(),
            quality: opt["quality"].as_str().unwrap().into(),
            if_missing: None,
            format: Some(opt["format"].as_str().unwrap().into()),
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
    let saved = std::fs::read_to_string(&jobs_file).unwrap();
    assert!(
        saved.contains(r#""format": "v1080+140""#) || saved.contains(r#""format":"v1080+140""#),
        "the exact formats are saved with the job (resume after a restart): {saved}"
    );

    // v0.2 (owner's Windows test): a YouTube page handed over as a normal "add" becomes a
    // VIDEO job; the file queue never sees it (it used to save the page as a file `watch`)
    for (page, kind) in [
        ("https://www.youtube.com/watch?v=abc123", "video"),
        ("https://youtu.be/abc123", "video"),
        ("https://www.youtube.com/playlist?list=PL1", "playlist"),
    ] {
        let mut cl = Client::connect(&ep).await.unwrap();
        let r = cl
            .call(Command::Add {
                url: page.into(),
                subdir: None,
                file_name: None,
            })
            .await
            .unwrap();
        assert_eq!(r["routed"], "video", "{page}: {r}");
        assert!(
            r["job"].as_u64().is_some() && r.get("id").is_none(),
            "{page}: {r}"
        );
        let jobs: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&jobs_file).unwrap()).unwrap();
        let j = jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["job"] == r["job"])
            .unwrap_or_else(|| panic!("{page}: job not saved: {jobs}"));
        assert_eq!(
            (j["url"].as_str(), j["kind"].as_str()),
            (Some(page), Some(kind)),
            "{j}"
        );
    }
    let l = cl.call(Command::List {}).await.unwrap();
    assert!(
        !l.to_string().contains("youtu"),
        "a video page reached the file queue: {l}"
    );
    // a plain file link still goes to the file queue
    let f = format!("{}/yt-dlp.exe", e.srv.base_url());
    let r = cl
        .call(Command::Add {
            url: f,
            subdir: None,
            file_name: None,
        })
        .await
        .unwrap();
    assert!(r["id"].as_u64().is_some(), "{r}");
}
