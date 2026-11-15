//! P4 G3: `eid video info|get` end to end with the fake yt-dlp. Tool assets come from the local
//! eid-testserver through the `test-hooks` environment (test builds only); the fake writes a
//! call log (URL parameter `log=`) so the tests can see every tool invocation.
mod video_env;
use std::path::{Path, PathBuf};
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

fn make_dir_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link)
        .expect("cannot create a symlink: this test must not pass without one");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target, link)
        .expect("cannot create a directory symlink: this test must not pass without one");
}

fn make_junction(target: &Path, link: &Path) {
    // a junction needs no privilege; `mklink /J` is the documented way to make one
    let o = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(o.status.success(), "cannot create a junction: {o:?}");
}

fn remove_link(p: &Path) {
    match std::fs::symlink_metadata(p) {
        Ok(md) if md.file_type().is_symlink() => {
            let _ = std::fs::remove_file(p).or_else(|_| std::fs::remove_dir(p));
        }
        Ok(_) => std::fs::remove_dir_all(p).unwrap(),
        Err(_) => {}
    }
    assert!(
        std::fs::symlink_metadata(p).is_err(),
        "could not remove {}",
        p.display()
    );
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
