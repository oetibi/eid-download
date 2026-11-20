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
//! * `/playlist/NAME?list=FILE&ptitle=T`   `--flat-playlist -J` of the entries in FILE
//! * `/video/ID?title=T`   `--dump-single-json`: print video info; otherwise "download":
//!   write the fixture clip (mp4, or mp3 with `--extract-audio`) as `ID.EXT` in the working
//!   directory, printing progress lines in our progress template
//! * `&heights=360,1080,4320` (video and playlist URLs): the video formats the item offers
//!   (one video-only format per height, plus audio). `--format` is evaluated against them with
//!   the subset of yt-dlp's selection language eid emits (`/` alternatives, `+` merges,
//!   `bv*` `wv*` `b` `w` `ba`, a format id, `[height<=N]` and `[height<=?N]`), and the chosen
//!   video height is printed as the last progress field (`%(info.height)s`). No match is
//!   "Requested format is not available", exit 1, like yt-dlp.
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
        // P4 review M2: a warning that mentions an explicit restriction, then a vague error
        // (YouTube also answers so while it limits requests): must stay retryable
        "vague" => "WARNING: [fake] abc123: Private video in the related list\nERROR: [fake] \
                    abc123: Video unavailable. This content isn't available."
            .into(),
        "removed" => "ERROR: [fake] abc123: Video unavailable. This video has been removed by \
                      the uploader"
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

/// One format the fake offers.
#[derive(Clone, Debug)]
struct Fmt {
    id: String,
    height: Option<u32>,
    video: bool,
    audio: bool,
}

/// The formats of a fake item: the fixed fixture list, or `heights=` (see the module doc).
fn formats(heights: Option<&str>) -> Vec<Fmt> {
    let f = |id: &str, height, video, audio| Fmt {
        id: id.into(),
        height,
        video,
        audio,
    };
    match heights {
        None => vec![
            f("140", None, false, true),
            f("134", Some(360), true, false),
            f("18", Some(90), true, true),
        ],
        Some(list) => {
            let mut v = vec![f("140", None, false, true)];
            for h in list.split(',').filter(|h| !h.is_empty()) {
                let h: u32 = h
                    .parse()
                    .unwrap_or_else(|_| die(9, "fake-yt-dlp: bad heights"));
                v.push(f(&format!("v{h}"), Some(h), true, false));
            }
            v
        }
    }
}

/// `[height<=N]` / `[height<=?N]` filters of one selector atom.
fn passes(f: &Fmt, filters: &str) -> bool {
    filters.split(']').filter(|x| !x.is_empty()).all(|flt| {
        let flt = flt.trim_start_matches('[');
        // v0.2 `ba[ext=m4a]`: video formats are mp4, audio-only ones m4a (as in info_json)
        if let Some(e) = flt.strip_prefix("ext=") {
            return e == if f.video { "mp4" } else { "m4a" };
        }
        let (lenient, n) = match flt.strip_prefix("height<=?") {
            Some(n) => (true, n),
            None => (
                false,
                flt.strip_prefix("height<=")
                    .unwrap_or_else(|| die(9, &format!("fake-yt-dlp: filter {flt}"))),
            ),
        };
        let n: u32 = n
            .parse()
            .unwrap_or_else(|_| die(9, "fake-yt-dlp: filter number"));
        match f.height {
            Some(h) => h <= n,
            None => lenient,
        }
    })
}

fn pick<'a>(fs: &'a [Fmt], atom: &str) -> Option<&'a Fmt> {
    let (name, filters) = match atom.find('[') {
        Some(i) => atom.split_at(i),
        None => (atom, ""),
    };
    let ok: Vec<&Fmt> = fs.iter().filter(|f| passes(f, filters)).collect();
    let key = |f: &&Fmt| f.height.unwrap_or(0);
    match name {
        "bv*" => ok.into_iter().filter(|f| f.video).max_by_key(key),
        "wv*" => ok.into_iter().filter(|f| f.video).min_by_key(key),
        "b" => ok
            .into_iter()
            .filter(|f| f.video && f.audio)
            .max_by_key(key),
        "w" => ok
            .into_iter()
            .filter(|f| f.video && f.audio)
            .min_by_key(key),
        "ba" => ok.into_iter().find(|f| f.audio && !f.video),
        id => ok.into_iter().find(|f| f.id == id),
    }
}

/// The video height `--format` selects from `fs` (`Some(None)`: a selection without a known
/// height); `None` when nothing matches.
fn select(fs: &[Fmt], spec: &str) -> Option<Option<u32>> {
    selected(fs, spec).map(|p| p.iter().filter(|f| f.video).filter_map(|f| f.height).max())
}

/// The formats of the first alternative of `spec` that matches.
fn selected<'a>(fs: &'a [Fmt], spec: &str) -> Option<Vec<&'a Fmt>> {
    spec.split('/')
        .find_map(|alt| alt.split('+').map(|a| pick(fs, a)).collect())
}

/// The extension of the file a download writes: mp3 with `--extract-audio`, m4a when the
/// selection is audio only (v0.2 `m4a`), else mp4 (or the WebM source, see [`Remux`]).
fn out_ext(flags: &[String], heights: Option<&str>) -> &'static str {
    if flags.iter().any(|f| f == "--extract-audio") {
        return "mp3";
    }
    let fs = formats(heights);
    match value(flags, "--format").and_then(|s| selected(&fs, s)) {
        Some(p) if !p.is_empty() && p.iter().all(|f| !f.video) => "m4a",
        _ => "mp4",
    }
}

fn info_json(id: &str, title: &str, heights: Option<&str>) -> String {
    if heights.is_some() {
        let fs: Vec<serde_json::Value> = formats(heights)
            .iter()
            .map(|f| {
                // v0.2 details for the download menu: 60 fps from 720p, sizes and bitrates
                // that grow with the height, audio 128 kbit/s
                let h = f.height.unwrap_or(0);
                serde_json::json!({
                    "format_id": f.id, "ext": if f.video { "mp4" } else { "m4a" },
                    "height": f.height,
                    "vcodec": if f.video { "avc1.64001f" } else { "none" },
                    "acodec": if f.audio { "mp4a.40.2" } else { "none" },
                    "fps": if f.video { Some(if h >= 720 { 60.0 } else { 30.0 }) } else { None },
                    "dynamic_range": if f.video { Some("SDR") } else { None },
                    "filesize": if f.video { u64::from(h) * 10_000 } else { 5_000 },
                    "tbr": if f.video { f64::from(h) } else { 128.0 },
                    "abr": if f.audio { Some(128.0) } else { None },
                })
            })
            .collect();
        return serde_json::json!({
            "_type": "video", "id": id, "title": title, "duration": 2.0,
            "uploader": "Fake uploader", "is_live": false,
            "webpage_url": format!("https://fake.test/video/{id}"), "formats": fs,
        })
        .to_string();
    }
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

/// Remux behaviour of the fake (review M4), from the URL. `container=webm` makes the source a
/// single-file WebM; with `--remux-video=mp4` it is then remuxed the way yt-dlp's remuxer does
/// it: `<id>.temp.mp4`, renamed to `<id>.mp4`, the source removed. `remux_fail=<flag>`: while
/// the file exists, a partial `<id>.temp.mp4` is left, "Conversion failed!", exit 1, the source
/// kept. `remux=broken`: a cut `<id>.mp4`, exit 0; `remux=foreign`: the WebM renamed to `.mp4`, exit 0. `remux_stall=<flag>`: hang before the remux
/// while the file exists (cancel tests). Without `--remux-video` the WebM stays as it is.
#[derive(Default)]
struct Remux {
    container: Option<String>,
    mode: Option<String>,
    fail_flag: Option<String>,
    stall: Option<String>,
}

/// A stand-in WebM: the EBML magic, then filler. Not an MP4 (the product's check refuses it).
fn webm_bytes() -> Vec<u8> {
    let mut v = vec![0x1a, 0x45, 0xdf, 0xa3];
    v.extend((0..30_000u32).map(|i| (i % 251) as u8));
    v
}

fn remux_to_mp4(flags: &[String], id: &str, source: &str, r: &Remux) {
    if value(flags, "--remux-video") != Some("mp4") {
        return; // no remux asked: the source container stays (as yt-dlp does)
    }
    if let Some(flag) = &r.stall {
        out("[VideoRemuxer] Remuxing video");
        let t = std::time::Instant::now();
        while std::path::Path::new(flag).exists() && t.elapsed().as_secs() < 600 {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let temp = format!("{id}.temp.mp4");
    if r.fail_flag
        .as_deref()
        .is_some_and(|f| std::path::Path::new(f).exists())
    {
        std::fs::write(&temp, &CLIP_MP4[..CLIP_MP4.len() / 3]).unwrap();
        die(1, "ERROR: Postprocessing: Conversion failed!");
    }
    match r.mode.as_deref() {
        // a "conversion" that only renamed the WebM
        Some("foreign") => std::fs::rename(source, format!("{id}.mp4")).unwrap(),
        Some("broken") => {
            std::fs::write(format!("{id}.mp4"), &CLIP_MP4[..CLIP_MP4.len() / 2]).unwrap();
            std::fs::remove_file(source).unwrap();
        }
        _ => {
            out(&format!(
                "[VideoRemuxer] Remuxing video from webm to mp4; Destination: {id}.mp4"
            ));
            std::fs::write(&temp, CLIP_MP4).unwrap();
            std::fs::rename(&temp, format!("{id}.mp4")).unwrap();
            std::fs::remove_file(source).unwrap();
        }
    }
}

/// "Download" like yt-dlp: write `<id>.<ext>.part` in the working directory, continue an
/// existing `.part` from its length (resume), optionally stall half-way while `stall_flag`
/// exists (cancellation tests), then rename to `<id>.<ext>` (and remux, see [`Remux`]).
/// Returns the resume offset.
fn download(
    flags: &[String],
    id: &str,
    stall_flag: Option<&str>,
    remux: &Remux,
    heights: Option<&str>,
) -> u64 {
    match value(flags, "--output") {
        Some("%(id)s.%(ext)s") => {}
        other => die(
            9,
            &format!("fake-yt-dlp: unexpected output template {other:?}"),
        ),
    }
    let audio = flags.iter().any(|f| f == "--extract-audio");
    let webm = webm_bytes();
    let (data, ext): (&[u8], &str) = if audio {
        (CLIP_MP3, "mp3")
    } else if out_ext(flags, heights) == "m4a" {
        // the fixture clip is an MP4 file; as audio only it stands in for an M4A stream
        (CLIP_MP4, "m4a")
    } else if remux.container.as_deref() == Some("webm") {
        (&webm, "webm")
    } else {
        (CLIP_MP4, "mp4")
    };
    let progress = value(flags, "--progress-template").is_some();
    // `%(info.height)s` in the template: the selected video height as the last field
    let with_height =
        value(flags, "--progress-template").is_some_and(|t| t.contains("info.height"));
    let chosen = match value(flags, "--format") {
        Some(spec) if !audio => match select(&formats(heights), spec) {
            Some(h) => h,
            None => die(
                1,
                &format!("ERROR: [fake] {id}: Requested format is not available"),
            ),
        },
        _ => None,
    };
    let hfield = if with_height {
        format!(" {}", chosen.map_or("NA".to_string(), |h| h.to_string()))
    } else {
        String::new()
    };
    let total = data.len();
    let name = format!("{id}.{ext}");
    let part = format!("{name}.part");
    if ext == "webm" && std::fs::symlink_metadata(&name).is_ok() {
        // like yt-dlp: a finished source is not downloaded again, only post-processed
        out(&format!("[download] {name} has already been downloaded"));
        remux_to_mp4(flags, id, &name, remux);
        append_archive(flags, id);
        return 0;
    }
    // --download-archive: an id already recorded is skipped (no file, exit 0), like yt-dlp
    let archive = value(flags, "--download-archive");
    let line = format!("fake {id}");
    if let Some(a) = archive {
        if std::fs::read_to_string(a).is_ok_and(|s| s.lines().any(|l| l == line)) {
            out(&format!(
                "[download] {id}: has already been recorded in the archive"
            ));
            std::process::exit(0);
        }
    }
    if std::fs::symlink_metadata(&name).is_ok() {
        die(1, &format!("ERROR: {name} already exists"));
    }
    let resumed = std::fs::symlink_metadata(&part)
        .map(|m| m.len().min(total as u64))
        .unwrap_or(0);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part)
        .unwrap_or_else(|e| die(1, &format!("ERROR: unable to open {part}: {e}")));
    let step = total.div_ceil(4);
    let mut done = resumed as usize;
    while done < total {
        if let Some(flag) = stall_flag {
            if done >= total / 2 && std::path::Path::new(flag).exists() {
                f.sync_all().unwrap();
                out(&format!("[eid-progress] {done} {total} NA 0 NA{hfield}"));
                std::thread::sleep(Duration::from_secs(600));
            }
        }
        let n = step
            .min(total - done)
            .min(if stall_flag.is_some() && done < total / 2 {
                total / 2 - done
            } else {
                usize::MAX
            });
        f.write_all(&data[done..done + n]).unwrap();
        done += n;
        if progress {
            out(&format!(
                "[eid-progress] {done} {total} NA 1000.0 0{hfield}"
            ));
        }
    }
    f.sync_all().unwrap();
    drop(f);
    std::fs::rename(&part, &name).unwrap();
    if ext == "webm" {
        remux_to_mp4(flags, id, &name, remux);
    }
    // like yt-dlp, an item is recorded only after its post-processing succeeded
    append_archive(flags, id);
    resumed
}

fn append_archive(flags: &[String], id: &str) {
    if let Some(a) = value(flags, "--download-archive") {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(a)
            .unwrap_or_else(|e| die(1, &format!("ERROR: archive {a}: {e}")));
        let _ = writeln!(f, "fake {id}");
    }
}

/// `--flat-playlist -J` of a fake playlist whose entries come from the JSON file named by the
/// URL's `list` parameter: `[{"id": .., "title": .., "availability": ..}]`. Entry URLs keep the
/// playlist URL's other parameters (log, stall, ...); unavailable entries point at an error.
fn playlist_json(name: &str, url: &url::Url) -> String {
    let q = |k: &str| {
        url.query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
    };
    let list = q("list").unwrap_or_else(|| die(9, "fake-yt-dlp: playlist without list="));
    let text = std::fs::read_to_string(&list)
        .unwrap_or_else(|e| die(9, &format!("fake-yt-dlp: list {list}: {e}")));
    let items: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    let mut rest = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in url.query_pairs() {
        if k != "list" && k != "ptitle" {
            rest.append_pair(&k, &v);
        }
    }
    let rest = rest.finish();
    let entries: Vec<serde_json::Value> = items
        .iter()
        .map(|it| {
            let id = it["id"].as_str().unwrap();
            let avail = it["availability"].as_str();
            // `"error": KIND` makes the item fail with that error kind when downloaded
            let path = match (it["error"].as_str(), avail) {
                (Some(kind), _) => format!("error/{kind}"),
                (None, Some("private" | "needs_auth" | "unavailable")) => {
                    "error/private".to_string()
                }
                _ => format!("video/{id}"),
            };
            // `"nourl": true`: a flat entry that carries only its id (some extractors do this)
            if it["nourl"].as_bool() == Some(true) {
                return serde_json::json!({"_type": "url", "id": id, "title": it["title"]});
            }
            // `"heights": "360,720"`: this item's formats (overrides the playlist's)
            let rest = match it["heights"].as_str() {
                Some(h) => {
                    let mut r = url::form_urlencoded::Serializer::new(String::new());
                    for (k, v) in url::form_urlencoded::parse(rest.as_bytes()) {
                        if k != "heights" {
                            r.append_pair(&k, &v);
                        }
                    }
                    r.append_pair("heights", h);
                    r.finish()
                }
                None => rest.clone(),
            };
            serde_json::json!({
                "_type": "url", "id": id, "title": it["title"],
                "url": format!("https://fake.test/{path}?{rest}"),
                "availability": avail,
            })
        })
        .collect();
    serde_json::json!({
        "_type": "playlist", "id": name,
        "title": q("ptitle").unwrap_or_else(|| "Fake playlist".into()),
        "entries": entries,
    })
    .to_string()
}

/// Append one JSON line per invocation to the file named by the URL's `log` parameter.
fn log_call(path: &str, argv: &[String], resumed_from: Option<u64>) {
    let line = serde_json::json!({
        "pid": std::process::id(),
        "argv": argv,
        "cwd": std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(),
        "resumed_from": resumed_from,
    });
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|e| die(9, &format!("fake-yt-dlp: cannot open log {path}: {e}")));
    // one write for the whole line (and its newline), so a reader never sees half of it
    let _ = f.write_all(format!("{line}\n").as_bytes());
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
        Some("playlist") => {
            let name = segs.get(1).cloned().unwrap_or_default();
            if !flags.iter().any(|f| f == "--flat-playlist") {
                die(
                    9,
                    "fake-yt-dlp: playlists are only listed (--flat-playlist)",
                );
            }
            if let Some(l) = query("log") {
                log_call(&l, &argv, None);
            }
            // `stall_info=<flag file>`: hang in the listing while the file exists (cancel
            // tests; logged first, so a test sees this process while it hangs)
            if let Some(flag) = query("stall_info") {
                let t = std::time::Instant::now();
                while std::path::Path::new(&flag).exists() && t.elapsed().as_secs() < 600 {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            out(&playlist_json(&name, &url));
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
            let log = query("log");
            if flags.iter().any(|f| f == "--dump-single-json") {
                if let Some(l) = &log {
                    log_call(l, &argv, None);
                }
                // `stall_info=<flag file>`: hang in `-J` while the file exists (cancel tests;
                // logged first, so a test sees this process while it hangs)
                if let Some(flag) = query("stall_info") {
                    let t = std::time::Instant::now();
                    while std::path::Path::new(&flag).exists() && t.elapsed().as_secs() < 600 {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                let title = query("title").unwrap_or_else(|| "Fake video".into());
                out(&info_json(&id, &title, query("heights").as_deref()));
            } else {
                // log first (with the resume offset), so a test can see this process even if
                // it is killed while it stalls
                let ext = out_ext(flags, query("heights").as_deref());
                let resumed = std::fs::symlink_metadata(format!("{id}.{ext}.part"))
                    .map(|m| m.len())
                    .unwrap_or(0);
                if let Some(l) = &log {
                    log_call(l, &argv, Some(resumed));
                }
                // `stall_id=X`: only item X stalls (playlist tests)
                let stall = query("stall").filter(|_| query("stall_id").is_none_or(|s| s == id));
                let remux = Remux {
                    container: query("container"),
                    mode: query("remux"),
                    fail_flag: query("remux_fail"),
                    stall: query("remux_stall"),
                };
                download(
                    flags,
                    &id,
                    stall.as_deref(),
                    &remux,
                    query("heights").as_deref(),
                );
            }
        }
        _ => die(9, &format!("fake-yt-dlp: unknown fake URL {}", url.path())),
    }
}
