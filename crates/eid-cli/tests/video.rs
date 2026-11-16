//! P4 G3: `eid video info|get` end to end with the fake yt-dlp. Tool assets come from the local
//! eid-testserver through the `test-hooks` environment (test builds only); the fake writes a
//! call log (URL parameter `log=`) so the tests can see every tool invocation.
mod video_env;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use video_env::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn video_without_consent_runs_nothing_and_opens_no_connection() {
    let e = Env::new().await;
    let page = format!("{}/page", e.srv.base_url());
    e.srv
        .add("/page", Content::Bytes(b"x".to_vec()), Behavior::default());
    for args in [
        vec!["video", "get", page.as_str(), "-d"],
        vec!["video", "info", page.as_str()],
    ] {
        let mut a = args.clone();
        let out = e.out().display().to_string();
        if a.last() == Some(&"-d") {
            a.push(&out);
        }
        let o = e.run(&a).await;
        assert_eq!(o.status.code(), Some(3), "{}", text(&o));
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("consent"),
            "{}",
            text(&o)
        );
    }
    // zero requests of any kind reached the local server: no tool download, no page fetch
    assert_eq!(e.tool_requests(), 0);
    assert!(e.srv.stats("/page").requests.is_empty());
    assert!(
        !e.tools_dir().exists(),
        "tools folder created without consent"
    );
    assert!(
        !e.settings_file().exists(),
        "consent recorded without consent"
    );
    assert!(entries(&e.out()).is_empty());
    assert!(e.calls().is_empty(), "the tool ran");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_tty_and_no_flag_refuses_consent_without_waiting() {
    let e = Env::new().await;
    // stdin is a pipe that stays open and never sends anything: a program that waits for an
    // answer would hang here
    let mut c = e.cmd(&[
        "video",
        "get",
        &e.url("video/abc", ""),
        "-d",
        &e.out().display().to_string(),
    ]);
    c.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    let _stdin = child.stdin.take();
    let t = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if t.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            panic!("eid waited for consent input without a terminal");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(status.code(), Some(3));
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    assert_eq!(e.tool_requests(), 0);
    assert!(e.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn video_is_published_by_us_from_a_fresh_staging_dir() {
    let e = Env::new().await;
    let title = "../../evil \u{202E}4pm.exe: <CON> عنوان";
    let url = e.url("video/abc", &format!("&title={}", pct(title)));
    let out = e.out().display().to_string();
    // a file already carrying the published name is never overwritten
    let o = e
        .run(&["video", "get", &url, "-d", &out, "--yes-i-agree", "-q"])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let first = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    assert_eq!(
        first.parent().unwrap(),
        e.out(),
        "published outside the folder"
    );
    let name = first.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.ends_with(".mp4") && !name.contains('\u{202E}'),
        "{name}"
    );
    assert!(name.contains("عنوان"), "{name}");
    assert_eq!(
        hex(&std::fs::read(&first).unwrap()),
        hex(&fixture("clip.mp4"))
    );
    // the tool ran with our staging folder as its working directory, which is gone now
    let calls = e.calls();
    assert_eq!(calls.len(), 2, "-J then download: {calls:?}");
    let cwd = PathBuf::from(calls[1]["cwd"].as_str().unwrap());
    assert_eq!(cwd.parent().unwrap(), e.out());
    assert!(cwd
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(".eid-video-"));
    assert_eq!(calls[1]["resumed_from"], 0, "a fresh staging folder");
    assert_eq!(
        entries(&e.out()),
        std::slice::from_ref(&name),
        "staging left behind"
    );
    // same command again: a second file with a numbered name, the first untouched
    std::fs::write(&first, b"user's own edit").unwrap();
    let o = e
        .run(&["video", "get", &url, "-d", &out, "--yes-i-agree", "-q"])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let second = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    assert_ne!(second, first);
    assert_eq!(std::fs::read(&first).unwrap(), b"user's own edit");
    assert_eq!(e.tool_requests(), 3, "tools are downloaded once");
    // audio: published as mp3
    let o = e
        .run(&[
            "video",
            "get",
            &url,
            "-d",
            &out,
            "--yes-i-agree",
            "-q",
            "--quality",
            "audio",
        ])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let mp3 = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    assert!(mp3.extension().is_some_and(|x| x == "mp3"));
    assert_eq!(
        hex(&std::fs::read(&mp3).unwrap()),
        hex(&fixture("clip.mp3"))
    );
}

/// A link or junction (made by the test; the test fails if it cannot make one) planted where
/// the staging folder goes, or inside a staging folder that is reused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planted_symlink_or_junction_is_never_followed() {
    let e = Env::new().await;
    let url = e.url("video/abc", "");
    let out = e.out().display().to_string();
    let elsewhere = e.root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let staging = e.out().join(eid_cli::video::staging_name(
        &eid_media::args::MediaUrl::parse(&url).unwrap(),
        &eid_media::args::Quality::Best,
    ));
    let mut kinds: Vec<&str> = vec!["dir-symlink"];
    if eid_platform::IS_WINDOWS {
        kinds.push("junction");
    }
    kinds.push("file-symlink-inside");
    for kind in kinds {
        match kind {
            "dir-symlink" => make_dir_link(&elsewhere, &staging),
            "junction" => make_junction(&elsewhere, &staging),
            _ => {
                std::fs::create_dir(&staging).unwrap();
                eid_platform::fs::symlink_file(
                    &elsewhere.join("victim.mp4"),
                    &staging.join("abc.mp4"),
                )
                .expect("cannot create a symlink: this test must not pass without one");
            }
        }
        let o = e
            .run(&["video", "get", &url, "-d", &out, "--yes-i-agree", "-q"])
            .await;
        assert_eq!(o.status.code(), Some(1), "{kind}: {}", text(&o));
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("refusing"),
            "{kind}: {}",
            text(&o)
        );
        assert!(
            entries(&elsewhere).is_empty(),
            "{kind}: wrote through the link"
        );
        assert!(e.calls().is_empty(), "{kind}: the tool ran");
        remove_link(&staging);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_leaves_nothing_outside_staging_and_resume_reuses_it() {
    let e = Env::new().await;
    let stall = e.root.path().join("stall.flag");
    std::fs::write(&stall, b"1").unwrap();
    let url = e.url(
        "video/abc",
        &format!("&stall={}", pct(&stall.display().to_string())),
    );
    let out = e.out().display().to_string();
    let mut c = e.cmd(&["video", "get", &url, "-d", &out, "--yes-i-agree", "-q"]);
    c.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut eid = c.spawn().unwrap();
    // wait for the stalled download: the fake has logged its download call and written half
    let t = Instant::now();
    let (pid, part) = loop {
        let calls = e.calls();
        if calls.len() >= 2 {
            let cwd = PathBuf::from(calls[1]["cwd"].as_str().unwrap());
            let part = cwd.join("abc.mp4.part");
            if part
                .metadata()
                .is_ok_and(|m| m.len() as usize >= fixture("clip.mp4").len() / 2)
            {
                break (calls[1]["pid"].as_u64().unwrap() as u32, part);
            }
        }
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "the download never stalled"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(eid_platform::process::process_is_alive(pid));
    // stop eid: SIGINT (= Ctrl-C) on Unix; on Windows a hard kill (a console Ctrl-C needs a
    // real console, MANUAL-TESTS #14), where the job object must take the tool down
    #[cfg(unix)]
    assert!(command("kill", ["-INT", &eid.id().to_string()])
        .status()
        .unwrap()
        .success());
    #[cfg(windows)]
    eid.kill().unwrap();
    let st = tokio::task::spawn_blocking(move || eid.wait().unwrap())
        .await
        .unwrap();
    if cfg!(unix) {
        assert_eq!(st.code(), Some(130), "Ctrl-C exit code");
    }
    let t = Instant::now();
    while eid_platform::process::process_is_alive(pid) {
        assert!(
            t.elapsed() < Duration::from_secs(15),
            "the tool outlived eid"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // nothing outside the staging folder; the partial data is inside it
    let staging = part.parent().unwrap().to_path_buf();
    assert_eq!(
        entries(&e.out()),
        [staging.file_name().unwrap().to_string_lossy().into_owned()]
    );
    let half = part.metadata().unwrap().len();
    assert!(half > 0);
    // run the same command again: the same staging folder, continued from the partial data
    std::fs::remove_file(&stall).unwrap();
    let o = e
        .run(&["video", "get", &url, "-d", &out, "--yes-i-agree", "-q"])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let calls = e.calls();
    let last = calls.last().unwrap();
    assert_eq!(PathBuf::from(last["cwd"].as_str().unwrap()), staging);
    assert_eq!(
        last["resumed_from"], half,
        "the second run did not continue"
    );
    let done = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    assert_eq!(
        hex(&std::fs::read(&done).unwrap()),
        hex(&fixture("clip.mp4"))
    );
    assert!(!staging.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn video_names_fit_the_path_limit() {
    let e = Env::new().await;
    // a deep output folder and a 300-character Arabic title
    let mut deep = e.out();
    for i in 0..4 {
        deep = deep.join(format!("folder-{i}-xxxxxxxxxx"));
    }
    std::fs::create_dir_all(&deep).unwrap();
    let title = "عنوان طويل جداً ".repeat(20);
    let url = e.url("video/abc", &format!("&title={}", pct(&title)));
    let o = e
        .run(&[
            "video",
            "get",
            &url,
            "-d",
            &deep.display().to_string(),
            "--yes-i-agree",
            "-q",
        ])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let p = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    let name = p.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.len() <= 255, "component of {} bytes", name.len());
    assert!(
        name.ends_with(".mp4") && name.starts_with("عنوان"),
        "{name}"
    );
    if let Some(limit) = eid_platform::MAX_PATH_UNITS {
        let units = p.as_os_str().to_string_lossy().encode_utf16().count();
        assert!(units <= limit, "{units} > {limit}");
        // a folder that leaves no room for the tool's own files is refused before it runs
        let mut too_deep = e.out();
        while too_deep.as_os_str().len() < limit - 60 {
            too_deep = too_deep.join("yyyyyyyyyyyyyyyyyyyy");
        }
        std::fs::create_dir_all(&too_deep).unwrap();
        let before = e.calls().len();
        let o = e
            .run(&[
                "video",
                "get",
                &url,
                "-d",
                &too_deep.display().to_string(),
                "--yes-i-agree",
                "-q",
            ])
            .await;
        assert_eq!(o.status.code(), Some(1), "{}", text(&o));
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("too long"),
            "{}",
            text(&o)
        );
        assert_eq!(e.calls().len(), before, "the tool ran");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limit_rate_is_passed_from_the_limit_option() {
    let e = Env::new().await;
    let url = e.url("video/abc", "");
    let out = e.out().display().to_string();
    let o = e
        .run(&[
            "video",
            "get",
            &url,
            "-d",
            &out,
            "--yes-i-agree",
            "-q",
            "--limit",
            "1.5M",
        ])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let calls = e.calls();
    let argv: Vec<String> = serde_json::from_value(calls[1]["argv"].clone()).unwrap();
    assert!(
        argv.contains(&format!("--limit-rate={}", 3 * 512 * 1024)),
        "{argv:?}"
    );
    // without --limit no rate is passed
    let o = e
        .run(&["video", "get", &url, "-d", &out, "--yes-i-agree", "-q"])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let calls = e.calls();
    let argv: Vec<String> = serde_json::from_value(calls[3]["argv"].clone()).unwrap();
    assert!(
        !argv.iter().any(|a| a.starts_with("--limit-rate")),
        "{argv:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drm_error_is_refused_with_a_clear_message() {
    let e = Env::new().await;
    let out = e.out().display().to_string();
    let o = e
        .run(&[
            "video",
            "get",
            "https://fake.test/error/drm",
            "-d",
            &out,
            "--yes-i-agree",
            "-q",
        ])
        .await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("DRM-protected: refused"), "{err}");
    assert!(err.contains("does not get around any protection"), "{err}");
    assert!(
        entries(&e.out()).is_empty(),
        "something was left: {:?}",
        entries(&e.out())
    );
    // info reports it the same way
    let o = e
        .run(&[
            "video",
            "info",
            "https://fake.test/error/drm",
            "--yes-i-agree",
        ])
        .await;
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("DRM-protected: refused"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn video_info_prints_the_choices_and_consent_can_be_shown_and_revoked() {
    let e = Env::new().await;
    let url = e.url("video/abc", &format!("&title={}", pct("عنوان\u{1b}[2J")));
    let o = e.run(&["video", "info", &url, "--yes-i-agree"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let s = String::from_utf8_lossy(&o.stdout);
    assert!(s.contains("id: abc") && s.contains("عنوان"), "{s}");
    assert!(
        !s.contains('\u{1b}'),
        "terminal control reached stdout: {s:?}"
    );
    assert!(s.contains("18") && s.contains("90p"), "{s}");
    let o = e.run(&["video", "consent"]).await;
    assert!(String::from_utf8_lossy(&o.stdout).contains("consent: given"));
    let o = e.run(&["video", "revoke"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert!(!e.tools_dir().exists());
    let o = e.run(&["video", "consent", "--lang", "ar"]).await;
    let s = String::from_utf8_lossy(&o.stdout);
    assert!(
        s.contains("عيد للتحميل") && s.contains("consent: not given"),
        "{s}"
    );
}

// ------------------------------------------------------------------ Ctrl-C in every stage (P4 review M1)

/// Ctrl-C for a running `eid`: a real SIGINT on Unix; on Windows the `test-hooks` cancel file,
/// which fires the same cancellation token as the console handler (a real console Ctrl-C
/// cannot be sent to a child on CI, MANUAL-TESTS #14).
fn ctrl_c(eid: &std::process::Child, cancel_file: &std::path::Path) {
    #[cfg(unix)]
    {
        let _ = cancel_file;
        assert!(command("kill", ["-INT", &eid.id().to_string()])
            .status()
            .unwrap()
            .success());
    }
    #[cfg(windows)]
    {
        let _ = eid;
        std::fs::write(cancel_file, b"1").unwrap();
    }
}

/// Wait for `eid` to exit within `limit`; on timeout kill it (no orphan) and fail.
async fn exit_within(mut eid: std::process::Child, limit: Duration) -> std::process::Output {
    let t = Instant::now();
    loop {
        if eid.try_wait().unwrap().is_some() {
            return tokio::task::spawn_blocking(move || eid.wait_with_output().unwrap())
                .await
                .unwrap();
        }
        if t.elapsed() > limit {
            let _ = eid.kill();
            let _ = eid.wait();
            panic!("eid did not stop within {limit:?} after Ctrl-C");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn staging_sets(tools: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(tools)
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(".staging-"))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_during_tool_install_stops_at_once_and_leaves_nothing_behind() {
    let e = Env::new().await;
    // the yt-dlp asset stops half-way for ten minutes: without a working cancel, eid hangs
    let half = fake_ytdlp_bytes().len() as u64 / 2;
    e.srv.set_behavior(
        "/yt-dlp.exe",
        Behavior {
            stall: Some(eid_testserver::Stall {
                after_bytes: half,
                duration: Duration::from_secs(600),
                once: false,
            }),
            ..Behavior::default()
        },
    );
    let cancel_file = e.root.path().join("cancel.flag");
    let out = e.out().display().to_string();
    let mut c = e.cmd(&[
        "video",
        "get",
        &e.url("video/abc", ""),
        "-d",
        &out,
        "--yes-i-agree",
        "-q",
    ]);
    c.env("EID_TEST_CANCEL_FILE", &cancel_file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let eid = c.spawn().unwrap();
    // wait until the tool download is really under way: a staging folder with partial data
    let tools = e.tools_dir();
    let t = Instant::now();
    loop {
        let busy = !e.srv.stats("/yt-dlp.exe").requests.is_empty()
            && staging_sets(&tools).iter().any(|s| {
                std::fs::read_dir(tools.join(s).join("dl")).is_ok_and(|mut d| d.next().is_some())
            });
        if busy {
            break;
        }
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "the tool download never started"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    ctrl_c(&eid, &cancel_file);
    let o = exit_within(eid, Duration::from_secs(20)).await;
    assert_eq!(o.status.code(), Some(130), "{}", text(&o));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("stopped"),
        "{}",
        text(&o)
    );
    // nothing half-installed is left, nothing became current, the tool never ran
    assert!(
        staging_sets(&tools).is_empty(),
        "left behind: {:?}",
        entries(&tools)
    );
    assert!(!tools.join("current.json").exists());
    assert!(e.calls().is_empty(), "the tool ran");
    assert!(entries(&e.out()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_during_media_info_kills_the_tool_and_exits() {
    let e = Env::new().await;
    let flag = e.root.path().join("stall-info.flag");
    std::fs::write(&flag, b"1").unwrap();
    let url = e.url(
        "video/abc",
        &format!("&stall_info={}", pct(&flag.display().to_string())),
    );
    let cancel_file = e.root.path().join("cancel.flag");
    let out = e.out().display().to_string();
    let mut c = e.cmd(&["video", "get", &url, "-d", &out, "--yes-i-agree", "-q"]);
    c.env("EID_TEST_CANCEL_FILE", &cancel_file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let eid = c.spawn().unwrap();
    // the fake has logged its `-J` call and now hangs while the flag file exists
    let t = Instant::now();
    let pid = loop {
        if let Some(c) = e.calls().first() {
            assert!(c["argv"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "--dump-single-json"));
            break c["pid"].as_u64().unwrap() as u32;
        }
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "the -J call never started"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(eid_platform::process::process_is_alive(pid));
    ctrl_c(&eid, &cancel_file);
    let o = exit_within(eid, Duration::from_secs(20)).await;
    assert_eq!(o.status.code(), Some(130), "{}", text(&o));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("stopped"),
        "{}",
        text(&o)
    );
    // the tool was killed (its flag still says "hang"), and the empty staging folder is gone
    let t = Instant::now();
    while eid_platform::process::process_is_alive(pid) {
        assert!(
            t.elapsed() < Duration::from_secs(15),
            "the tool outlived the cancel"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(flag.exists());
    assert!(
        entries(&e.out()).is_empty(),
        "left behind: {:?}",
        entries(&e.out())
    );
    assert_eq!(e.calls().len(), 1, "no download after the cancel");
}

// ------------------------------------------------------------------ remux to MP4 (P4 review M4)

/// The fake's stand-in WebM (same bytes as `webm_bytes` in eid-fake-ytdlp).
fn fake_webm() -> Vec<u8> {
    let mut v = vec![0x1a, 0x45, 0xdf, 0xa3];
    v.extend((0..30_000u32).map(|i| (i % 251) as u8));
    v
}

/// The only staging folder in `out` (panics if there is not exactly one).
fn the_staging(out: &std::path::Path) -> PathBuf {
    let s: Vec<String> = entries(out)
        .into_iter()
        .filter(|n| n.starts_with(".eid-video-"))
        .collect();
    assert_eq!(s.len(), 1, "staging folders: {s:?} in {:?}", entries(out));
    out.join(&s[0])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_file_formats_are_remuxed_to_mp4_and_checked_before_publishing() {
    let e = Env::new().await;
    let out = e.out().display().to_string();
    // the site serves a single WebM file (the `/b` fallback): yt-dlp is asked to remux it
    let url = e.url("video/abc", "&container=webm");
    let o = e
        .run(&["video", "get", &url, "-d", &out, "--yes-i-agree", "-q"])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    let done = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    assert_eq!(done.file_name().unwrap(), "Fake video.mp4");
    // the published file is the remuxed MP4, not the WebM under a new name
    assert_eq!(
        hex(&std::fs::read(&done).unwrap()),
        hex(&fixture("clip.mp4"))
    );
    assert_eq!(entries(&e.out()), ["Fake video.mp4"], "staging left behind");
    let calls = e.calls();
    let argv = |c: &serde_json::Value| -> Vec<String> {
        c["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect()
    };
    assert!(argv(calls.last().unwrap()).contains(&"--remux-video=mp4".to_string()));
    // every video quality asks for the remux; audio (mp3) never does
    for (q, want) in [("best", true), ("720p", true), ("audio", false)] {
        let d = e.root.path().join(format!("q-{q}"));
        std::fs::create_dir(&d).unwrap();
        let o = e
            .run(&[
                "video",
                "get",
                &e.url("video/abc", ""),
                "-d",
                &d.display().to_string(),
                "--quality",
                q,
                "--yes-i-agree",
                "-q",
            ])
            .await;
        assert!(o.status.success(), "{q}: {}", text(&o));
        let last = argv(e.calls().last().unwrap());
        assert_eq!(
            last.iter().any(|a| a == "--remux-video=mp4"),
            want,
            "{q}: {last:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_remux_keeps_the_original_and_the_same_command_finishes_it_later() {
    let e = Env::new().await;
    let out = e.out().display().to_string();
    let fail = e.root.path().join("remux-fails.flag");
    std::fs::write(&fail, b"1").unwrap();
    let url = e.url(
        "video/abc",
        &format!(
            "&container=webm&remux_fail={}",
            pct(&fail.display().to_string())
        ),
    );
    let args = [
        "video",
        "get",
        url.as_str(),
        "-d",
        out.as_str(),
        "--yes-i-agree",
        "-q",
    ];
    let o = e.run(&args).await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("Conversion failed"),
        "{}",
        text(&o)
    );
    // nothing published; the downloaded original is kept whole in the staging folder, next to
    // the tool's partial conversion
    let staging = the_staging(&e.out());
    assert_eq!(
        entries(&e.out()),
        [staging.file_name().unwrap().to_string_lossy().into_owned()]
    );
    assert_eq!(entries(&staging), ["abc.temp.mp4", "abc.webm"]);
    assert_eq!(
        hex(&std::fs::read(staging.join("abc.webm")).unwrap()),
        hex(&fake_webm())
    );
    // the partial conversion is never taken for the result: the same command, once the
    // conversion works, remuxes the kept original (no new download) and publishes a whole MP4
    std::fs::remove_file(&fail).unwrap();
    let o = e.run(&args).await;
    assert!(o.status.success(), "{}", text(&o));
    let done = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    assert_eq!(
        hex(&std::fs::read(&done).unwrap()),
        hex(&fixture("clip.mp4"))
    );
    assert_eq!(
        e.calls().last().unwrap()["cwd"],
        staging.display().to_string()
    );
    assert_eq!(entries(&e.out()), ["Fake video.mp4"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cut_or_foreign_mp4_is_never_published() {
    let e = Env::new().await;
    let out = e.out().display().to_string();
    // the tool reports success but leaves a cut MP4
    let url = e.url("video/abc", "&container=webm&remux=broken");
    let o = e
        .run(&["video", "get", &url, "-d", &out, "--yes-i-agree", "-q"])
        .await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("not a complete MP4"),
        "{}",
        text(&o)
    );
    let staging = the_staging(&e.out());
    assert_eq!(entries(&e.out()).len(), 1, "something was published");
    assert_eq!(entries(&staging), ["abc.mp4"], "kept for inspection");
    // the tool reports success but its ".mp4" is really the WebM under a new name
    let d = e.root.path().join("foreign");
    std::fs::create_dir(&d).unwrap();
    let url = e.url("video/abc", "&container=webm&remux=foreign");
    let o = e
        .run(&[
            "video",
            "get",
            &url,
            "-d",
            &d.display().to_string(),
            "--yes-i-agree",
            "-q",
        ])
        .await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("not an MP4"),
        "{}",
        text(&o)
    );
    let staging = the_staging(&d);
    assert_eq!(
        entries(&d).len(),
        1,
        "something was published: {:?}",
        entries(&d)
    );
    assert_eq!(
        hex(&std::fs::read(staging.join("abc.mp4")).unwrap()),
        hex(&fake_webm())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_during_the_remux_keeps_the_original_and_publishes_nothing() {
    let e = Env::new().await;
    let out = e.out().display().to_string();
    let stall = e.root.path().join("remux-stall.flag");
    std::fs::write(&stall, b"1").unwrap();
    let url = e.url(
        "video/abc",
        &format!(
            "&container=webm&remux_stall={}",
            pct(&stall.display().to_string())
        ),
    );
    let cancel_file = e.root.path().join("cancel.flag");
    let mut c = e.cmd(&["video", "get", &url, "-d", &out, "--yes-i-agree", "-q"]);
    c.env("EID_TEST_CANCEL_FILE", &cancel_file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let eid = c.spawn().unwrap();
    // the download is complete (the WebM is in place, no .part) and the tool hangs in the remux
    let t = Instant::now();
    let (pid, staging) = loop {
        let calls = e.calls();
        if calls.len() >= 2 {
            let cwd = PathBuf::from(calls[1]["cwd"].as_str().unwrap());
            if std::fs::read(cwd.join("abc.webm")).is_ok_and(|b| b == fake_webm())
                && !cwd.join("abc.webm.part").exists()
            {
                break (calls[1]["pid"].as_u64().unwrap() as u32, cwd);
            }
        }
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "the remux never started"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(eid_platform::process::process_is_alive(pid));
    ctrl_c(&eid, &cancel_file);
    let o = exit_within(eid, Duration::from_secs(20)).await;
    assert_eq!(o.status.code(), Some(130), "{}", text(&o));
    let t = Instant::now();
    while eid_platform::process::process_is_alive(pid) {
        assert!(
            t.elapsed() < Duration::from_secs(15),
            "the tool outlived the cancel"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // the original is kept whole, nothing reached the output folder
    assert_eq!(
        entries(&e.out()),
        [staging.file_name().unwrap().to_string_lossy().into_owned()]
    );
    assert_eq!(entries(&staging), ["abc.webm"]);
    assert_eq!(
        hex(&std::fs::read(staging.join("abc.webm")).unwrap()),
        hex(&fake_webm())
    );
    // the same command continues from the kept original
    std::fs::remove_file(&stall).unwrap();
    let o = e
        .run(&["video", "get", &url, "-d", &out, "--yes-i-agree", "-q"])
        .await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(entries(&e.out()), ["Fake video.mp4"]);
    assert_eq!(
        hex(&std::fs::read(e.out().join("Fake video.mp4")).unwrap()),
        hex(&fixture("clip.mp4"))
    );
}
