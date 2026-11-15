//! P4 G1: the yt-dlp rules of eid-media, checked with the fake yt-dlp (crate
//! eid-fake-ytdlp, built by `cargo test --workspace`).
use eid_media::args::{
    self, Action, MediaUrl, Quality, ToolPaths, ALLOWED, ARCHIVE_FILE, EJS_JITLESS,
    OUTPUT_TEMPLATE, PROGRESS_TEMPLATE,
};
use eid_media::hints::Hint;
use eid_media::run::{Runner, INFO_STDOUT_LIMIT};
use eid_media::MediaError;
use eid_platform::process as pp;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn fake_ytdlp() -> PathBuf {
    let name = format!("fake-yt-dlp{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().unwrap();
    exe.ancestors()
        .map(|a| a.join(&name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| {
            panic!(
                "{name} not built: run `cargo test --workspace` (or build -p eid-fake-ytdlp) \
                 so the fake tool exists next to {}",
                exe.display()
            )
        })
}

/// Tool paths with the real fake yt-dlp and ffmpeg/deno paths in a folder whose name has
/// spaces and Arabic (they are only passed as arguments; the fake never runs them).
fn tools(dir: &Path) -> ToolPaths {
    let t = dir.join("أدوات eid");
    ToolPaths {
        ytdlp: fake_ytdlp(),
        ffmpeg: t.join("ffmpeg.exe"),
        deno: t.join("deno.exe"),
    }
}

fn runner(dir: &Path) -> Runner {
    Runner::new(tools(dir), |k| std::env::var_os(k)).unwrap()
}

fn url(s: &str) -> MediaUrl {
    MediaUrl::parse(s).unwrap()
}

/// Every action/quality/limit/archive combination the builder supports.
fn all_actions() -> Vec<Action> {
    let mut v = vec![Action::Info, Action::PlaylistInfo];
    for q in [
        Quality::Best,
        Quality::MaxHeight(720),
        Quality::AudioMp3,
        Quality::Format("18".into()),
    ] {
        for limit_rate in [None, Some(1), Some(u64::MAX)] {
            for archive in [false, true] {
                v.push(Action::Download {
                    quality: q.clone(),
                    limit_rate,
                    archive,
                });
            }
        }
    }
    v
}

fn value_of<'a>(a: &'a [OsString], flag: &str) -> Option<&'a str> {
    let p = format!("{flag}=");
    a.iter()
        .take_while(|x| *x != "--")
        .find_map(|x| x.to_str()?.strip_prefix(p.as_str()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn media_args_reach_the_tool_byte_exact() {
    let d = tempfile::tempdir().unwrap();
    let r = runner(d.path());
    // quotes, backslashes (including a trailing one: the classic Windows quoting bug),
    // Arabic, percent-escapes and shell metacharacters; the tool paths have spaces + Arabic
    let raw = "https://fake.test/argv/a\"b\\c\\\\\"d/عربي/$(id)`x`;&|?q=\"x\"\\&t=%41&o=-x\\";
    let u = url(raw);
    for action in all_actions() {
        let built = args::build(&r.tools, &u, &action).unwrap();
        let f = pp::spawn(r.spec(d.path(), built.clone(), 1 << 20))
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert!(
            f.status.success(),
            "{}",
            String::from_utf8_lossy(&f.stderr.data)
        );
        let got: Vec<String> = serde_json::from_slice(&f.stdout.data).unwrap();
        let want: Vec<String> = built
            .iter()
            .map(|a| a.to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            got, want,
            "arguments changed on the way to the tool ({action:?})"
        );
        assert_eq!(
            got.last().unwrap(),
            raw,
            "the URL must arrive exactly as given"
        );
    }
    // what cannot be passed at all: a URL that looks like a flag, RTL override, control
    // characters, whitespace, credentials, other schemes
    for bad in [
        "-https://fake.test/x",
        "--exec=calc",
        "https://fake.test/\u{202E}4pm.exe",
        "https://fake.test/a\nb",
        "https://fake.test/a b",
        "https://user:pass@fake.test/x",
        "file:///etc/passwd",
        "ftp://fake.test/x",
        "",
    ] {
        assert!(MediaUrl::parse(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn no_user_text_reaches_a_template_or_path_option() {
    let d = tempfile::tempdir().unwrap();
    let t = tools(d.path());
    let ffmpeg = t.ffmpeg.to_str().unwrap().to_string();
    let deno = format!("deno:{}", t.deno.to_str().unwrap());
    let hostile = [
        "https://fake.test/%(title)s/%(uploader)s.%(ext)s",
        "https://fake.test/$HOME/${PATH}/%USERPROFILE%/..%2F..%2Fx",
        "https://fake.test/x?o=--output=C:/Windows/x&-P=..&--exec=calc",
        "https://fake.test/..\\..\\..\\Windows\\System32",
    ];
    for h in hostile {
        let u = url(h);
        for action in all_actions() {
            let a = args::build(&t, &u, &action).unwrap();
            args::check(&a).unwrap();
            assert_eq!(a.last().unwrap(), h);
            assert_eq!(a[a.len() - 2], "--");
            // the URL text appears nowhere else
            let marker = &h["https://fake.test/".len()..];
            for x in &a[..a.len() - 1] {
                assert!(
                    !x.to_str().unwrap().contains(marker),
                    "{x:?} carries URL text"
                );
            }
            // template and path options are constants or our own tool paths
            if let Some(v) = value_of(&a, "--output") {
                assert_eq!(v, OUTPUT_TEMPLATE);
            }
            if let Some(v) = value_of(&a, "--download-archive") {
                assert_eq!(v, ARCHIVE_FILE);
            }
            if let Some(v) = value_of(&a, "--progress-template") {
                assert_eq!(v, PROGRESS_TEMPLATE);
            }
            assert_eq!(value_of(&a, "--ffmpeg-location"), Some(ffmpeg.as_str()));
            assert_eq!(value_of(&a, "--js-runtimes"), Some(deno.as_str()));
            assert_eq!(value_of(&a, "--extractor-args"), Some(EJS_JITLESS));
        }
    }
    // a format id from the network is never a format-selection expression or a flag
    for bad in [
        "$(evil)",
        "-x",
        "--exec",
        "bv*+ba",
        "18/best",
        "a b",
        "",
        &"9".repeat(65),
    ] {
        let e = args::build(
            &t,
            &url("https://fake.test/video/x"),
            &Action::Download {
                quality: Quality::Format(bad.to_string()),
                limit_rate: None,
                archive: false,
            },
        );
        assert!(
            matches!(e, Err(MediaError::BadChoice(_))),
            "{bad:?} -> {e:?}"
        );
    }
    for bad in [0, args::MAX_HEIGHT + 1] {
        let e = args::build(
            &t,
            &url("https://fake.test/video/x"),
            &Action::Download {
                quality: Quality::MaxHeight(bad),
                limit_rate: None,
                archive: false,
            },
        );
        assert!(e.is_err(), "height {bad}");
    }
}

#[test]
fn percent_and_dollar_in_paths_never_reach_ytdlp() {
    let d = tempfile::tempdir().unwrap();
    for bad in ["%TEMP%", "$HOME", "${x}", "100%", "a$b", "$(whoami)"] {
        let dir = d.path().join(bad);
        for which in 0..3 {
            let mut t = tools(d.path());
            match which {
                0 => t.ffmpeg = dir.join("ffmpeg.exe"),
                1 => t.deno = dir.join("deno.exe"),
                _ => t.ytdlp = dir.join("yt-dlp.exe"),
            }
            let e = args::build(&t, &url("https://fake.test/video/x"), &Action::Info);
            assert!(
                matches!(e, Err(MediaError::BadToolPath(_))),
                "{bad:?}: {e:?}"
            );
            let r = Runner::new(t, |k| std::env::var_os(k));
            assert!(r.is_err(), "Runner must refuse {bad:?}");
            let msg = r.unwrap_err().to_string();
            assert!(msg.contains("'$' or '%'"), "{msg}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_environment_is_an_allowlist() {
    let d = tempfile::tempdir().unwrap();
    // a hostile parent environment: Python hooks, yt-dlp hooks, a PATH full of planted tools
    let ours: HashMap<&str, &str> = [
        ("SystemRoot", r"C:\Windows"),
        ("TEMP", r"C:\Temp"),
        ("TMP", r"C:\Temp"),
        ("TMPDIR", "/tmp"),
        ("PATH", "/planted/bin"),
        ("PYTHONPATH", "/evil"),
        ("PYTHONHOME", "/evil"),
        ("PYTHONSTARTUP", "/evil/start.py"),
        ("PYTHONINSPECT", "1"),
        ("YTDLP_HOME", "/evil"),
        ("XDG_CONFIG_HOME", "/evil"),
        ("APPDATA", r"C:\evil"),
        ("HOME", "/evil"),
        ("EID_SECRET", "SENTINEL_ENV"),
        ("HTTPS_PROXY", "http://proxy.example:3128"),
        ("no_proxy", "localhost"),
    ]
    .into_iter()
    .collect();
    let r = Runner::new(tools(d.path()), |k| ours.get(k).map(OsString::from)).unwrap();
    let allowed: Vec<String> = eid_media::env::allowed_names()
        .iter()
        .map(|s| s.to_ascii_uppercase())
        .collect();
    let names: Vec<String> = r
        .env
        .iter()
        .map(|(k, _)| k.to_string_lossy().to_ascii_uppercase())
        .collect();
    for n in &names {
        assert!(allowed.contains(n), "{n} is not in the allowlist");
    }
    for must in ["PATH", "YTDLP_NO_PLUGINS", "HTTPS_PROXY", "NO_PROXY"] {
        assert!(names.iter().any(|n| n == must), "{must} missing: {names:?}");
    }
    let get = |k: &str| {
        r.env
            .iter()
            .find(|(n, _)| n.to_string_lossy().eq_ignore_ascii_case(k))
            .map(|(_, v)| v.clone())
    };
    assert_eq!(get("YTDLP_NO_PLUGINS"), Some("1".into()));
    let path = get("PATH").unwrap();
    let first = std::env::split_paths(&path).next().unwrap();
    assert_eq!(
        first,
        r.tools.ytdlp.parent().unwrap(),
        "our tools come first"
    );
    assert!(
        !path.to_string_lossy().contains("planted"),
        "the parent PATH leaked"
    );
    // end to end: the tool sees exactly this environment, nothing more
    let a = args::build(&r.tools, &url("https://fake.test/env"), &Action::Info).unwrap();
    let f = pp::spawn(r.spec(d.path(), a, 1 << 20))
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(f.status.success());
    let seen: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&f.stdout.data).unwrap();
    let mut seen_names: Vec<String> = seen.keys().map(|k| k.to_ascii_uppercase()).collect();
    seen_names.sort();
    let mut want = names.clone();
    want.sort();
    assert_eq!(seen_names, want);
    let text = serde_json::to_string(&seen).unwrap();
    for leaked in ["SENTINEL_ENV", "/evil", "planted"] {
        assert!(!text.contains(leaked), "{leaked} reached the tool: {text}");
    }
}

#[test]
fn tool_locations_are_explicit() {
    let d = tempfile::tempdir().unwrap();
    let r = runner(d.path());
    for action in all_actions() {
        let a = args::build(&r.tools, &url("https://fake.test/video/x"), &action).unwrap();
        let flags = args::emitted_flags(&a);
        let no = flags.iter().position(|f| f == "--no-js-runtimes").unwrap();
        let yes = flags.iter().position(|f| f == "--js-runtimes").unwrap();
        assert!(
            no < yes,
            "the default runtime search must be disabled first"
        );
        let deno = value_of(&a, "--js-runtimes").unwrap();
        let deno = Path::new(deno.strip_prefix("deno:").unwrap());
        assert!(deno.is_absolute());
        assert_eq!(deno, r.tools.deno);
        let ff = Path::new(value_of(&a, "--ffmpeg-location").unwrap());
        assert!(ff.is_absolute());
        assert_eq!(ff, r.tools.ffmpeg);
    }
    let spec = r.spec(d.path(), vec![], 1);
    assert!(spec.program.is_absolute());
    assert_eq!(spec.program, r.tools.ytdlp);
    // relative tool paths are refused outright
    let mut t = tools(d.path());
    t.ytdlp = PathBuf::from("yt-dlp.exe");
    assert!(Runner::new(t, |_| None).is_err());
    let mut t = tools(d.path());
    t.ffmpeg = PathBuf::from("ffmpeg");
    assert!(args::build(&t, &url("https://fake.test/x"), &Action::Info).is_err());
}

#[test]
fn every_emitted_flag_is_in_the_allowlist() {
    let d = tempfile::tempdir().unwrap();
    let t = tools(d.path());
    let names: Vec<&str> = ALLOWED.iter().map(|(n, _)| *n).collect();
    for action in all_actions() {
        let a = args::build(&t, &url("https://fake.test/video/x"), &action).unwrap();
        args::check(&a).unwrap();
        for f in args::emitted_flags(&a) {
            assert!(names.contains(&f.as_str()), "{f} not in the allowlist");
        }
        for m in [
            "--ignore-config",
            "--no-cache-dir",
            "--no-plugin-dirs",
            "--no-remote-components",
            "--no-js-runtimes",
            "--js-runtimes",
            "--ffmpeg-location",
            "--no-update",
            "--extractor-args",
        ] {
            assert!(
                args::emitted_flags(&a).iter().any(|f| f == m),
                "{m} missing for {action:?}"
            );
        }
        if let Action::Download { limit_rate, .. } = &action {
            assert_eq!(
                value_of(&a, "--limit-rate").map(str::to_string),
                limit_rate.map(|r| r.to_string())
            );
        }
    }
}

#[test]
fn forbidden_flags_are_never_emitted() {
    for (name, _) in ALLOWED {
        assert!(
            !args::is_forbidden(name),
            "{name} is both allowed and forbidden"
        );
    }
    for f in [
        "--exec",
        "--exec-before-download",
        "--netrc",
        "--netrc-location",
        "--no-check-certificates",
        "--legacy-server-connect",
        "--enable-file-urls",
        "--cookies",
        "--cookies-from-browser",
        "-U",
        "--update",
        "--update-to",
        "--remote-components",
        "--plugin-dirs",
        "--load-info-json",
        "-a",
        "--batch-file",
        "--config-locations",
        "--downloader",
        "--downloader-args",
        "--external-downloader",
        "--postprocessor-args",
        "--use-postprocessor",
        "--allow-unplayable-formats",
        "--username",
        "--password",
        "-P",
        "--paths",
    ] {
        assert!(args::is_forbidden(f), "{f} must be forbidden");
    }
    let d = tempfile::tempdir().unwrap();
    let t = tools(d.path());
    for action in all_actions() {
        let a = args::build(&t, &url("https://fake.test/video/x"), &action).unwrap();
        for f in args::emitted_flags(&a) {
            assert!(!args::is_forbidden(&f), "{f} emitted for {action:?}");
        }
        // the structural check rejects every way a forbidden or malformed flag could slip in
        let sep = a.len() - 2;
        for inject in [
            "--exec=calc",
            "--cookies=c.txt",
            "-a",
            "--config-locations=x",
            "--output",
            "--no-such-flag",
        ] {
            let mut b = a.clone();
            b.insert(sep, inject.into());
            assert!(args::check(&b).is_err(), "{inject} accepted");
        }
        let mut b = a.clone();
        b.push("https://second.test/".into());
        assert!(args::check(&b).is_err(), "two arguments after --");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_dash_j_is_refused_without_oom() {
    let d = tempfile::tempdir().unwrap();
    let r = runner(d.path());
    // 64 MiB of "JSON": four times the cap; the supervisor keeps at most the cap
    let u = url("https://fake.test/flood-json/64");
    let a = args::build(&r.tools, &u, &Action::Info).unwrap();
    let f = pp::spawn(r.spec(d.path(), a, INFO_STDOUT_LIMIT))
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(f.stdout.truncated);
    assert_eq!(f.stdout.data.len(), INFO_STDOUT_LIMIT);
    assert!(f.stdout.total > 4 * INFO_STDOUT_LIMIT as u64 - 1024);
    let e = r.info(&u, d.path()).await.unwrap_err();
    assert!(
        matches!(e, MediaError::OutputTooLarge { limit } if limit == INFO_STDOUT_LIMIT),
        "{e:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secrets_are_redacted_from_tool_text() {
    let d = tempfile::tempdir().unwrap();
    let r = runner(d.path());
    let e = r
        .info(&url("https://fake.test/error/secrets"), d.path())
        .await
        .unwrap_err();
    let MediaError::ToolFailed { message, .. } = &e else {
        panic!("{e:?}")
    };
    let shown = format!("{e} {e:?}");
    for s in [
        "SENTINEL_PASS",
        "SENTINEL_TOKEN",
        "SENTINELHOST",
        "SENTINEL_SIG",
        "SENTINEL_COOKIE",
        "203.0.113.77",
    ] {
        assert!(!shown.contains(s), "{s} leaked: {shown}");
    }
    assert!(message.contains("https://example.org/watch"), "{message}");
    assert!(message.contains("<media-url>"), "{message}");
    assert!(message.contains("Cookie: <redacted>"), "{message}");
    // the same rules as a pure function, with the awkward spellings
    let t = eid_media::redact::redact_tool_text(
        "HTTPS://U:SENTINEL_PASS@Example.org/p?k=SENTINEL_TOKEN#SENTINEL_FRAG \
         (http://x.googlevideo.com/videoplayback?ip=203.0.113.77) \
         authorization: Bearer SENTINEL_BEARER\nsee <https://h.test/a?b=SENTINEL_Q>",
    );
    for s in [
        "SENTINEL_PASS",
        "SENTINEL_TOKEN",
        "SENTINEL_FRAG",
        "203.0.113.77",
        "SENTINEL_BEARER",
        "SENTINEL_Q",
    ] {
        assert!(!t.contains(s), "{s} leaked: {t}");
    }
    assert!(t.contains("https://example.org/p"), "{t}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_errors_get_hints() {
    let d = tempfile::tempdir().unwrap();
    let r = runner(d.path());
    for (kind, want) in [
        ("drm", Some(Hint::Drm)),
        ("bot", Some(Hint::BotCheck)),
        ("age", Some(Hint::SignInRequired)),
        ("429", Some(Hint::RateLimited)),
        ("geo", Some(Hint::Geo)),
        ("private", Some(Hint::Unavailable)),
        ("js", Some(Hint::MissingJsRuntime)),
        ("nothing-known", None),
    ] {
        let e = r
            .info(&url(&format!("https://fake.test/error/{kind}")), d.path())
            .await
            .unwrap_err();
        match e {
            MediaError::ToolFailed { hint, .. } => assert_eq!(hint, want, "{kind}"),
            other => panic!("{kind}: {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dash_j_is_parsed_and_hostile_format_ids_are_dropped() {
    let d = tempfile::tempdir().unwrap();
    let r = runner(d.path());
    let i = r
        .info(
            &url(
                "https://fake.test/video/abc_123?title=%D8%B9%D9%86%D9%88%D8%A7%D9%86%20%E2%80%AE",
            ),
            d.path(),
        )
        .await
        .unwrap();
    assert_eq!(i.id, "abc_123");
    assert_eq!(
        i.title, "عنوان \u{202E}",
        "titles are data, kept verbatim here"
    );
    let ids: Vec<&str> = i.formats.iter().map(|f| f.format_id.as_str()).collect();
    assert_eq!(
        ids,
        ["140", "134", "18"],
        "the hostile format id is dropped"
    );
    assert!(i.formats[2].has_video && i.formats[2].has_audio);
    assert!(!i.formats[0].has_video && i.formats[0].has_audio);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fake_download_writes_the_clip_into_the_staging_dir() {
    let d = tempfile::tempdir().unwrap();
    let staging = d.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let r = runner(d.path());
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    r.download(
        &url("https://fake.test/video/abc"),
        &staging,
        Quality::Best,
        Some(1_000_000),
        false,
        Some(tx),
    )
    .await
    .unwrap();
    let mut samples = vec![];
    while let Ok(p) = rx.try_recv() {
        samples.push(p);
    }
    let names: Vec<String> = std::fs::read_dir(&staging)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["abc.mp4"]);
    let got = std::fs::read(staging.join("abc.mp4")).unwrap();
    let want = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../eid-fake-ytdlp/fixtures/clip.mp4"),
    )
    .unwrap();
    assert!(got == want, "the staged file is not the fixture clip");
    let last = samples.last().expect("no progress samples");
    assert_eq!(last.downloaded, Some(want.len() as u64));
    assert_eq!(last.total, Some(want.len() as u64));
    r.download(
        &url("https://fake.test/video/abc"),
        &staging,
        Quality::AudioMp3,
        None,
        false,
        None,
    )
    .await
    .unwrap();
    assert!(staging.join("abc.mp3").is_file());
}
