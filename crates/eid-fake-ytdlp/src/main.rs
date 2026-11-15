//! `fake-yt-dlp`: a test double for yt-dlp (P4). Never shipped, never touches the network.
//!
//! It behaves like the small part of yt-dlp that eid uses, driven by the page URL (the last
//! argument, after `--`), because the child environment is an allowlist and cannot carry test
//! configuration:
//!
//! * `/argv`               print the received arguments as a JSON array
//! * `/env`                print the received environment as a JSON object
//! * `/flood-json/MIB`     print a JSON document of about MIB MiB (bounded-output tests)
//! * `/error/KIND`         print a yt-dlp-style error for KIND and exit 1
//! * `/sleep/SECS`         sleep, then exit 0 (cancellation tests)
//! * `/video/ID?title=T`   `--dump-single-json`: print video info; otherwise "download":
//!   write the fixture clip (mp4, or mp3 with `--extract-audio`) as `ID.EXT` in the working
//!   directory, printing progress lines in our progress template
//!
//! It refuses (exit 9) to run without the flags eid must always pass, and it only understands
//! the constant output template `%(id)s.%(ext)s`, so a regression in the argument builder fails
//! loudly here instead of passing silently.
use std::io::Write;
use std::time::Duration;

const CLIP_MP4: &[u8] = include_bytes!("../fixtures/clip.mp4");
const CLIP_MP3: &[u8] = include_bytes!("../fixtures/clip.mp3");
const MANDATORY: &[&str] = &[
    "--ignore-config",
    "--no-cache-dir",
    "--no-plugin-dirs",
    "--no-remote-components",
    "--no-js-runtimes",
    "--no-update",
];

fn die(code: i32, msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(code);
}

fn out(s: &str) {
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(s.as_bytes());
    let _ = o.write_all(b"\n");
    let _ = o.flush();
}

fn value<'a>(flags: &'a [String], name: &str) -> Option<&'a str> {
    let p = format!("{name}=");
    flags.iter().find_map(|f| f.strip_prefix(p.as_str()))
}

fn error_text(kind: &str) -> String {
    match kind {
        "drm" => "ERROR: [fake] abc123: This video is DRM protected".into(),
        "bot" => "ERROR: [youtube] abc123: Sign in to confirm you\u{2019}re not a bot. Use \
                  --cookies-from-browser or --cookies for the authentication."
            .into(),
        "age" => "ERROR: [fake] abc123: Sign in to confirm your age. This video may be \
                  inappropriate for some users."
            .into(),
        "429" => "ERROR: [fake] abc123: Unable to download webpage: HTTP Error 429: Too Many \
                  Requests"
            .into(),
        "geo" => "ERROR: [fake] abc123: The uploader has not made this video available in your \
                  country"
            .into(),
        "private" => "ERROR: [fake] abc123: Private video. Sign in if you've been granted \
                      access to this video"
            .into(),
        "js" => "WARNING: [youtube] No supported JavaScript runtime could be found.\nERROR: \
                 [youtube] abc123: Requested format is not available"
            .into(),
        // sentinels for the redaction tests: tokens in the query, credentials in the
        // userinfo, a media URL with the user's IP, a quoted cookie header
        "secrets" => "WARNING: retrying https://user:SENTINEL_PASS@example.org/watch?v=x&token=\
                      SENTINEL_TOKEN\nERROR: [fake] abc123: Unable to download: \
                      'https://rr3---sn-SENTINELHOST.googlevideo.com/videoplayback?ip=203.0.113.77\
                      &sig=SENTINEL_SIG' (caused by HTTP Error 403)\nCookie: SID=SENTINEL_COOKIE"
            .into(),
        other => format!("ERROR: [fake] unknown error kind {other}"),
    }
}

fn info_json(id: &str, title: &str) -> String {
    serde_json::json!({
        "_type": "video",
        "id": id,
        "title": title,
        "duration": 2.0,
        "is_live": false,
        "webpage_url": format!("https://fake.test/video/{id}"),
        "formats": [
            {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2",
             "filesize": 8255},
            {"format_id": "134", "ext": "mp4", "height": 360, "vcodec": "mp4v",
             "acodec": "none", "filesize_approx": 30000},
            {"format_id": "18", "ext": "mp4", "height": 90, "vcodec": "mp4v",
             "acodec": "mp4a.40.2", "filesize": CLIP_MP4.len()},
            {"format_id": "$(evil)", "ext": "mp4", "vcodec": "mp4v", "acodec": "none"}
        ]
    })
    .to_string()
}

fn download(flags: &[String], id: &str) {
    match value(flags, "--output") {
        Some("%(id)s.%(ext)s") => {}
        other => die(
            9,
            &format!("fake-yt-dlp: unexpected output template {other:?}"),
        ),
    }
    let audio = flags.iter().any(|f| f == "--extract-audio");
    let (data, ext) = if audio {
        (CLIP_MP3, "mp3")
    } else {
        (CLIP_MP4, "mp4")
    };
    let progress = value(flags, "--progress-template").is_some();
    let total = data.len();
    let name = format!("{id}.{ext}");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&name)
        .unwrap_or_else(|e| die(1, &format!("ERROR: unable to open {name}: {e}")));
    for (i, chunk) in data.chunks(total.div_ceil(4)).enumerate() {
        f.write_all(chunk).unwrap();
        if progress {
            let done = ((i + 1) * total.div_ceil(4)).min(total);
            out(&format!("[eid-progress] {done} {total} NA 1000.0 0"));
        }
    }
    f.sync_all().unwrap();
}

fn main() {
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| {
            a.into_string()
                .unwrap_or_else(|_| die(9, "fake-yt-dlp: non-Unicode argument"))
        })
        .collect();
    let sep = argv
        .iter()
        .position(|a| a == "--")
        .unwrap_or_else(|| die(9, "fake-yt-dlp: no '--' before the URL"));
    let (flags, rest) = (&argv[..sep], &argv[sep + 1..]);
    if rest.len() != 1 {
        die(9, "fake-yt-dlp: exactly one URL must follow '--'");
    }
    for m in MANDATORY {
        if !flags.iter().any(|f| f == m) {
            die(9, &format!("fake-yt-dlp: mandatory flag {m} missing"));
        }
    }
    let url = url::Url::parse(&rest[0]).unwrap_or_else(|_| die(9, "fake-yt-dlp: bad URL"));
    let segs: Vec<String> = url
        .path_segments()
        .map(|s| s.map(str::to_string).collect())
        .unwrap_or_default();
    let query = |k: &str| {
        url.query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
    };
    match segs.first().map(String::as_str) {
        Some("argv") => out(&serde_json::to_string(&argv).unwrap()),
        Some("env") => {
            let m: serde_json::Map<String, serde_json::Value> = std::env::vars_os()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned().into(),
                    )
                })
                .collect();
            out(&serde_json::Value::Object(m).to_string());
        }
        Some("flood-json") => {
            let mib: usize = segs.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(b"{\"id\":\"flood\",\"pad\":\"");
            let block = vec![b'x'; 1024 * 1024];
            for _ in 0..mib {
                if o.write_all(&block).is_err() {
                    std::process::exit(0);
                }
            }
            let _ = o.write_all(b"\"}\n");
        }
        Some("error") => {
            eprintln!(
                "{}",
                error_text(segs.get(1).map(String::as_str).unwrap_or(""))
            );
            std::process::exit(1);
        }
        Some("sleep") => {
            let s: u64 = segs.get(1).and_then(|s| s.parse().ok()).unwrap_or(600);
            std::thread::sleep(Duration::from_secs(s));
        }
        Some("video") => {
            let id = segs.get(1).cloned().unwrap_or_default();
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                die(9, "fake-yt-dlp: bad id");
            }
            if flags.iter().any(|f| f == "--dump-single-json") {
                let title = query("title").unwrap_or_else(|| "Fake video".into());
                out(&info_json(&id, &title));
            } else {
                download(flags, &id);
            }
        }
        _ => die(9, &format!("fake-yt-dlp: unknown fake URL {}", url.path())),
    }
}
