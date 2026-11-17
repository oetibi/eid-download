//! P4 G4: `eid video playlist` end to end with the fake yt-dlp (F11a (1)-(5), numbering rules D8,
//! state file and archive inside the playlist folder D13, folder lock). Playlists are described
//! by a JSON file (`list=`); the fake logs every tool call (`log=`), which is how the tests see
//! what was downloaded, resumed or skipped.
mod video_env;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};
use video_env::*;

// ------------------------------------------------------------------ helpers

fn item(id: &str, title: &str) -> Value {
    json!({"id": id, "title": title})
}

fn item_with(id: &str, title: &str, key: &str, value: &str) -> Value {
    json!({"id": id, "title": title, key: value})
}

fn items(n: usize, title: impl Fn(usize) -> String) -> Vec<Value> {
    (1..=n).map(|i| item(&format!("i{i}"), &title(i))).collect()
}

fn write_list(e: &Env, items: &[Value]) -> PathBuf {
    let p = e.root.path().join("list.json");
    std::fs::write(&p, serde_json::to_vec(items).unwrap()).unwrap();
    p
}

/// A playlist page address: `NAME` is the playlist id, `ptitle` its title.
fn purl(e: &Env, list: &Path, name: &str, ptitle: &str, extra: &str) -> String {
    e.url(
        &format!("playlist/{name}"),
        &format!(
            "&list={}&ptitle={}{extra}",
            pct(&list.display().to_string()),
            pct(ptitle)
        ),
    )
}

async fn playlist(e: &Env, url: &str, more: &[&str]) -> Output {
    playlist_in(e, &e.out(), url, more).await
}

async fn playlist_in(e: &Env, dir: &Path, url: &str, more: &[&str]) -> Output {
    let out = dir.display().to_string();
    let mut a = vec!["video", "playlist", url, "-d", &out, "--yes-i-agree"];
    a.extend_from_slice(more);
    e.run(&a).await
}

/// The folder a finished run printed (last stdout line).
fn folder_of(o: &Output) -> PathBuf {
    let s = String::from_utf8_lossy(&o.stdout);
    PathBuf::from(s.lines().last().unwrap_or("").trim())
}

/// File names a user sees: everything except our dot files and staging folders.
fn visible(dir: &Path) -> Vec<String> {
    entries(dir)
        .into_iter()
        .filter(|n| !n.starts_with('.'))
        .collect()
}

fn staging_dirs(dir: &Path) -> Vec<String> {
    entries(dir)
        .into_iter()
        .filter(|n| n.starts_with(".eid-video-"))
        .collect()
}

fn is_download(c: &Value) -> bool {
    !c["argv"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "--dump-single-json")
}

/// The item id a logged download call was for (`https://fake.test/video/ID?...`).
fn item_id(c: &Value) -> String {
    let last = c["argv"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .as_str()
        .unwrap();
    let rest = last.split("/video/").nth(1).unwrap_or("");
    rest.split('?').next().unwrap().to_string()
}

fn download_ids(calls: &[Value]) -> Vec<String> {
    calls
        .iter()
        .filter(|c| is_download(c))
        .map(item_id)
        .collect()
}

fn state_json(folder: &Path) -> Value {
    let t = std::fs::read_to_string(folder.join(".eid-playlist.json")).unwrap();
    serde_json::from_str::<Value>(&t).unwrap()["state"].clone()
}

fn item_state(folder: &Path, id: &str) -> Value {
    state_json(folder)["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id)
        .unwrap_or_else(|| panic!("no item {id} in the state"))
        .clone()
}

fn has_bidi(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
}

/// Start `eid video playlist` in the background (for the stall tests).
fn spawn_eid(e: &Env, args: &[&str]) -> std::process::Child {
    let mut c = e.cmd(args);
    c.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    c.spawn().unwrap()
}

/// Wait until the download of `id` has stalled half-way; returns (pid of the tool, `.part`).
async fn wait_for_stall(e: &Env, id: &str) -> (u32, PathBuf) {
    let t = Instant::now();
    loop {
        for c in e.calls() {
            if is_download(&c) && item_id(&c) == id {
                let part = PathBuf::from(c["cwd"].as_str().unwrap()).join(format!("{id}.mp4.part"));
                if part
                    .metadata()
                    .is_ok_and(|m| m.len() as usize >= fixture("clip.mp4").len() / 2)
                {
                    return (c["pid"].as_u64().unwrap() as u32, part);
                }
            }
        }
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "the download of {id} never stalled"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Stop eid like Ctrl-C (Unix) or a hard kill (Windows: the job object must take the tool
/// down), wait for it and for the tool to be gone.
async fn stop_eid(mut eid: std::process::Child, tool_pid: u32) {
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
    while eid_platform::process::process_is_alive(tool_pid) {
        assert!(
            t.elapsed() < Duration::from_secs(15),
            "the tool outlived eid"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ------------------------------------------------------------------ (1) selection

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playlist_selects_every_item_by_default_with_one_quality_and_a_deselected_item_is_skipped()
{
    let e = Env::new().await;
    let list = write_list(
        &e,
        &[item("a", "Alpha"), item("b", "Beta"), item("c", "Gamma")],
    );
    let url = purl(&e, &list, "pl1", "Mix", "");
    // the plan: nothing is downloaded by --list, and the deselected item is shown as such
    let o = playlist(&e, &url, &["--list", "--skip", "2"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let plan = String::from_utf8_lossy(&o.stdout).into_owned();
    let line = |n: &str| plan.lines().find(|l| l.starts_with(n)).unwrap().to_string();
    assert!(line("01").contains("to download"), "{plan}");
    assert!(line("02").contains("deselected"), "{plan}");
    assert!(line("03").contains("to download"), "{plan}");
    assert!(download_ids(&e.calls()).is_empty(), "--list downloaded");
    let folder = e.out().join("Mix");
    assert!(visible(&folder).is_empty());
    // item 2 is skipped; all others use the one chosen quality
    let o = playlist(&e, &url, &["--skip", "2", "--quality", "720p", "-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(visible(&folder), ["01 - Alpha.mp4", "03 - Gamma.mp4"]);
    let calls = e.calls();
    assert_eq!(download_ids(&calls), ["a", "c"]);
    let formats: std::collections::HashSet<String> = calls
        .iter()
        .filter(|c| is_download(c))
        .flat_map(|c| c["argv"].as_array().unwrap().clone())
        .filter_map(|a| a.as_str().map(str::to_string))
        .filter(|a| a.starts_with("--format="))
        .collect();
    assert_eq!(
        formats,
        ["--format=bv*[height<=720]+ba/b[height<=720]".to_string()].into()
    );
    // the deselected item keeps its number and is downloaded when it is no longer skipped
    assert_eq!(item_state(&folder, "b")["number"], 2);
    assert_eq!(item_state(&folder, "b")["status"], "pending");
    let o = playlist(&e, &url, &["--quality", "720p", "-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        visible(&folder),
        ["01 - Alpha.mp4", "02 - Beta.mp4", "03 - Gamma.mp4"]
    );
    assert_eq!(download_ids(&e.calls()), ["a", "c", "b"]);
    for f in visible(&folder) {
        assert_eq!(
            hex(&std::fs::read(folder.join(&f)).unwrap()),
            hex(&fixture("clip.mp4"))
        );
    }
}

// ------------------------------------------------------------------ (2) folder name

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playlist_folder_name_is_sanitized_and_confined() {
    let e = Env::new().await;
    let list = write_list(&e, &[item("a", "Alpha")]);
    let titles = [
        "../x",
        "..\\..\\x",
        "/abs/path",
        "CON",
        "nul.txt",
        "\u{202E}gpj.exe قائمة",
        "x:ads",
        ".hidden",
        "...",
        "",
        "a<b>c|d?e*f\"g",
    ];
    // one output folder per title: two playlists whose names sanitize to the same folder name
    // are refused (see the test below), which is not what is checked here
    for (i, t) in titles.iter().enumerate() {
        let dir = e.out().join(format!("case{i}"));
        std::fs::create_dir(&dir).unwrap();
        let url = purl(&e, &list, &format!("pl{i}"), t, "");
        let o = playlist_in(&e, &dir, &url, &["-q"]).await;
        assert!(o.status.success(), "{t:?}: {}", text(&o));
        let folder = folder_of(&o);
        assert_eq!(folder.parent().unwrap(), dir, "{t:?} escaped: {folder:?}");
        let name = folder.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            !name.contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*']),
            "{name:?}"
        );
        assert!(!has_bidi(&name), "{name:?}");
        assert!(
            !name.starts_with('.') && !name.ends_with(['.', ' ']),
            "{name:?}"
        );
        let stem = name.split('.').next().unwrap().to_ascii_uppercase();
        assert!(
            !["CON", "NUL"].contains(&stem.as_str()),
            "reserved: {name:?}"
        );
        assert_eq!(visible(&folder), ["01 - Alpha.mp4"], "{t:?}");
    }
    // nothing was created beside the output folder
    let mut top = entries(e.root.path());
    top.retain(|n| {
        ![
            "cfg",
            "data",
            "out",
            "list.json",
            "manifest.json",
            "calls.jsonl",
        ]
        .contains(&n.as_str())
    });
    assert!(top.is_empty(), "{top:?}");
    // a link planted where the folder would be is never followed
    let elsewhere = e.root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let list = write_list(&e, &[item("a", "Alpha")]);
    let url = purl(&e, &list, "plink", "Planted", "");
    let planted = e.out().join("Planted");
    let mut kinds = vec!["dir-symlink"];
    if eid_platform::IS_WINDOWS {
        kinds.push("junction");
    }
    for kind in kinds {
        if kind == "junction" {
            make_junction(&elsewhere, &planted);
        } else {
            make_dir_link(&elsewhere, &planted);
        }
        let before = e.calls().len();
        let o = playlist(&e, &url, &["-q"]).await;
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
        assert!(
            download_ids(&e.calls()[before..]).is_empty(),
            "{kind}: the tool downloaded"
        );
        remove_link(&planted);
    }
}

// ------------------------------------------------------------------ (3) numbering

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playlist_numbering_is_zero_padded_in_playlist_order() {
    let e = Env::new().await;
    // identical, Arabic and hostile titles: numbers keep the names unique and in order
    let titles = [
        "Same",
        "Same",
        "عنوان عربي",
        "../../evil:x\u{202E}exe.mp4",
        "CON",
    ];
    let list = write_list(
        &e,
        &titles
            .iter()
            .enumerate()
            .map(|(i, t)| item(&format!("v{i}"), t))
            .collect::<Vec<_>>(),
    );
    let o = playlist(&e, &purl(&e, &list, "small", "Small", ""), &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let folder = folder_of(&o);
    let names = visible(&folder);
    assert_eq!(names.len(), 5, "{names:?}");
    assert_eq!(names[0], "01 - Same.mp4");
    assert_eq!(names[1], "02 - Same.mp4");
    assert_eq!(names[2], "03 - عنوان عربي.mp4");
    assert!(
        names[3].starts_with("04 - ") && names[3].ends_with(".mp4"),
        "{names:?}"
    );
    assert_eq!(names[4], "05 - _CON.mp4");
    for n in &names {
        assert!(!n.contains(['/', '\\', ':']) && !has_bidi(n), "{n:?}");
        assert_eq!(folder.join(n).parent().unwrap(), folder);
    }
    assert_eq!(state_json(&folder)["width"], 2);
    // 120 items: three digits, in the order of the list, one tool call per item
    let list = write_list(&e, &items(120, |_| "T".into()));
    let o = playlist(&e, &purl(&e, &list, "big", "Big", ""), &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let folder = folder_of(&o);
    let want: Vec<String> = (1..=120).map(|n| format!("{n:03} - T.mp4")).collect();
    assert_eq!(visible(&folder), want);
    assert_eq!(state_json(&folder)["width"], 3);
    let ids = download_ids(&e.calls());
    assert_eq!(ids.len(), 5 + 120);
    assert_eq!(
        ids[5..],
        (1..=120).map(|n| format!("i{n}")).collect::<Vec<_>>()[..],
        "downloads are not in playlist order"
    );
    assert!(staging_dirs(&folder).is_empty());
}

// ------------------------------------------------------------------ (4) kill then resume

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playlist_kill_then_resume_skips_finished_items() {
    let e = Env::new().await;
    let list = write_list(
        &e,
        &[item("a", "Alpha"), item("b", "Beta"), item("c", "Gamma")],
    );
    let stall = e.root.path().join("stall.flag");
    std::fs::write(&stall, b"1").unwrap();
    let url = purl(
        &e,
        &list,
        "pl4",
        "Resume",
        &format!("&stall={}&stall_id=b", pct(&stall.display().to_string())),
    );
    let out = e.out().display().to_string();
    let eid = spawn_eid(
        &e,
        &["video", "playlist", &url, "-d", &out, "--yes-i-agree", "-q"],
    );
    let (pid, part) = wait_for_stall(&e, "b").await;
    stop_eid(eid, pid).await;
    let folder = e.out().join("Resume");
    // item a is finished and published; b is partial in its staging folder; c never started
    assert_eq!(visible(&folder), ["01 - Alpha.mp4"]);
    assert_eq!(item_state(&folder, "a")["status"], "done");
    assert_eq!(staging_dirs(&folder).len(), 1);
    let half = part.metadata().unwrap().len();
    assert!(half > 0);
    let before = e.calls().len();
    // run it again: only b (continued from its partial data) and c are downloaded
    std::fs::remove_file(&stall).unwrap();
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let calls = e.calls();
    let new = &calls[before..];
    assert_eq!(download_ids(new), ["b", "c"], "a was downloaded again");
    let b = new
        .iter()
        .find(|c| is_download(c) && item_id(c) == "b")
        .unwrap();
    assert_eq!(b["resumed_from"], half, "b was not continued");
    assert_eq!(
        visible(&folder),
        ["01 - Alpha.mp4", "02 - Beta.mp4", "03 - Gamma.mp4"]
    );
    assert!(staging_dirs(&folder).is_empty());
    for f in visible(&folder) {
        assert_eq!(
            hex(&std::fs::read(folder.join(&f)).unwrap()),
            hex(&fixture("clip.mp4")),
            "{f}"
        );
    }
    // a third run has nothing left to download
    let before = e.calls().len();
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert!(download_ids(&e.calls()[before..]).is_empty());
}

// ------------------------------------------------------------------ (5) sync via the archive

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playlist_sync_downloads_only_new_items_via_the_archive() {
    let e = Env::new().await;
    let list = write_list(
        &e,
        &[item("a", "Alpha"), item("b", "Beta"), item("c", "Gamma")],
    );
    let url = purl(&e, &list, "pl5", "Sync", "");
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let folder = folder_of(&o);
    // the owner of the playlist adds one item on top and one at the end
    write_list(
        &e,
        &[
            item("x", "Xray"),
            item("a", "Alpha"),
            item("b", "Beta"),
            item("c", "Gamma"),
            item("y", "Yankee"),
        ],
    );
    let before = e.calls().len();
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let calls = e.calls();
    assert_eq!(download_ids(&calls[before..]), ["x", "y"]);
    assert_eq!(
        visible(&folder),
        [
            "01 - Alpha.mp4",
            "02 - Beta.mp4",
            "03 - Gamma.mp4",
            "04 - Xray.mp4",
            "05 - Yankee.mp4"
        ]
    );
    // every download goes through the archive inside the playlist folder
    for c in calls.iter().filter(|c| is_download(c)) {
        assert!(c["argv"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "--download-archive=../.eid-archive.txt"));
        assert_eq!(
            Path::new(c["cwd"].as_str().unwrap()).parent().unwrap(),
            folder
        );
    }
    let archive = std::fs::read_to_string(folder.join(".eid-archive.txt")).unwrap();
    let mut lines: Vec<&str> = archive.lines().collect();
    lines.sort();
    assert_eq!(
        lines,
        ["fake a", "fake b", "fake c", "fake x", "fake y"],
        "each finished item once"
    );
    // the state file is lost (review M3, owner decision): the run is refused before any
    // download and before a new state is written; files and archive stay as they are
    std::fs::remove_file(folder.join(".eid-playlist.json")).unwrap();
    let before = e.calls().len();
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(!o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains(".eid-playlist.json is missing"),
        "{}",
        text(&o)
    );
    assert!(download_ids(&e.calls()[before..]).is_empty());
    assert_eq!(visible(&folder).len(), 5, "{:?}", visible(&folder));
    assert_eq!(
        std::fs::read_to_string(folder.join(".eid-archive.txt")).unwrap(),
        archive
    );
    assert!(!folder.join(".eid-playlist.json").exists());
}

/// Every entry of `dir` (dot files too) with the SHA-256 of each file's bytes.
fn snapshot(dir: &Path) -> Vec<(String, String)> {
    entries(dir)
        .into_iter()
        .map(|n| {
            let p = dir.join(&n);
            let h = if p.is_file() {
                hex(&std::fs::read(&p).unwrap())
            } else {
                "<dir>".to_string()
            };
            (n, h)
        })
        .collect()
}

// review M3 (owner decision): a lost `.eid-playlist.json` next to earlier downloads stops the
// run with a clear way on; `--list` still reads, with a warning; nothing is renamed or changed
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playlist_lost_state_with_archive_and_numbered_files_is_refused() {
    let e = Env::new().await;
    let list = write_list(&e, &[item("a", "Alpha"), item("b", "Beta")]);
    let url = purl(&e, &list, "pl-m3", "Lost", "");
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let folder = folder_of(&o);
    // a later item as audio: MP4 and MP3 numbered files side by side
    write_list(
        &e,
        &[item("a", "Alpha"), item("b", "Beta"), item("c", "Gamma")],
    );
    let o = playlist(&e, &url, &["-q", "--quality", "audio"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        visible(&folder),
        ["01 - Alpha.mp4", "02 - Beta.mp4", "03 - Gamma.mp3"]
    );
    assert!(!std::fs::read_to_string(folder.join(".eid-archive.txt"))
        .unwrap()
        .trim()
        .is_empty());
    // the state is lost, and an item is added on top: numbering again would give it 01
    std::fs::remove_file(folder.join(".eid-playlist.json")).unwrap();
    write_list(
        &e,
        &[
            item("z", "Zulu"),
            item("a", "Alpha"),
            item("b", "Beta"),
            item("c", "Gamma"),
        ],
    );
    let before = snapshot(&folder);
    for q in [
        &["-q"][..],
        &["-q", "--quality", "audio"],
        &["--retry-failed", "all"],
        &["-q", "--skip", "1"],
    ] {
        let calls = e.calls().len();
        let o = playlist(&e, &url, q).await;
        let t = text(&o);
        assert_eq!(o.status.code(), Some(1), "{q:?}: {t}");
        for want in [
            ".eid-playlist.json is missing",
            "download archive",
            "3 numbered file(s) (01 - Alpha.mp4, 02 - Beta.mp4, 03 - Gamma.mp3)",
            "nothing was downloaded",
            "from a backup",
            "-d <another folder>",
        ] {
            assert!(t.contains(want), "{q:?}: {want:?} not in {t}");
        }
        // refused after the listing, before any download or new state
        assert!(download_ids(&e.calls()[calls..]).is_empty(), "{q:?}");
        assert!(!folder.join(".eid-playlist.json").exists(), "{q:?}");
        assert!(staging_dirs(&folder).is_empty(), "{q:?}");
        // no file is created, renamed, removed or changed (names and SHA-256)
        assert_eq!(snapshot(&folder), before, "{q:?}");
    }
    // `--list` only reads: it shows the listing, warns, and writes nothing
    let o = playlist(&e, &url, &["--list"]).await;
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("warning: .eid-playlist.json is missing"), "{t}");
    assert!(
        t.contains("not the numbers of the files already there"),
        "{t}"
    );
    assert!(t.contains("Zulu"), "{t}");
    assert_eq!(snapshot(&folder), before);
    // a damaged state file is still the load error (never treated as "missing")
    std::fs::write(folder.join(".eid-playlist.json"), b"{ not json").unwrap();
    let o = playlist(&e, &url, &["-q"]).await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("is invalid"), "{}", text(&o));
    assert!(!text(&o).contains("is missing"), "{}", text(&o));
    assert_eq!(
        std::fs::read(folder.join(".eid-playlist.json")).unwrap(),
        b"{ not json"
    );
    std::fs::remove_file(folder.join(".eid-playlist.json")).unwrap(); // written by this test
    assert_eq!(snapshot(&folder), before);
    // numbered files alone (no archive) are enough to refuse (the archive alone: eid-media
    // `playlist_lost_state_leftovers_are_found_read_only`)
    std::fs::rename(
        folder.join(".eid-archive.txt"),
        e.root.path().join("archive-aside"),
    )
    .unwrap();
    let o = playlist(&e, &url, &["-q"]).await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(!text(&o).contains("download archive"), "{}", text(&o));
    assert!(!folder.join(".eid-playlist.json").exists());
    // the other way on: a new folder starts its own numbering, the old one is not touched
    let fresh = e.out().join("Elsewhere");
    std::fs::create_dir(&fresh).unwrap();
    let o = playlist_in(&e, &fresh, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        visible(&folder_of(&o)),
        [
            "01 - Zulu.mp4",
            "02 - Alpha.mp4",
            "03 - Beta.mp4",
            "04 - Gamma.mp4"
        ]
    );
    // the old folder was left exactly as it was (the archive was moved aside by this test)
    let mut left = before.clone();
    left.retain(|(n, _)| n != ".eid-archive.txt");
    assert_eq!(snapshot(&folder), left);
}

// names found on disk are shown neutralised; an archive that cannot be judged (a hard link,
// which the no-follow open refuses on Unix) stops the run but never `--list`
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playlist_lost_state_message_neutralises_names_and_list_survives_an_odd_archive() {
    let e = Env::new().await;
    let list = write_list(&e, &[item("a", "Alpha")]);
    let url = purl(&e, &list, "pl-m3d", "Odd", "");
    let folder = e.out().join("Odd");
    std::fs::create_dir(&folder).unwrap();
    let hostile = "01 - \u{202E}4pm.exe\u{200F}\u{2028}\u{200B}\u{061C}x\u{85}y.mp4";
    std::fs::write(folder.join(hostile), b"mine").unwrap();
    let outside = e.root.path().join("archive-elsewhere.txt");
    std::fs::write(&outside, b"fake old\n").unwrap();
    std::fs::hard_link(&outside, folder.join(".eid-archive.txt"))
        .expect("this test needs a hard link; it must not pass without one");
    // the folder lock is the one file every run opens (created if missing; Round 16 limit 3)
    let user_files = |d: &Path| {
        let mut s = snapshot(d);
        s.retain(|(n, _)| n != ".eid-lock");
        s
    };
    let before = user_files(&folder);
    let o = playlist(&e, &url, &["-q"]).await;
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(t.contains("download archive"), "{t}");
    assert!(
        t.contains("1 numbered file(s) (01 - ?4pm.exe????x?y.mp4)"),
        "{t}"
    );
    for bad in [
        '\u{202E}', '\u{200F}', '\u{2028}', '\u{200B}', '\u{061C}', '\u{85}',
    ] {
        assert!(!t.contains(bad), "{bad:?} printed: {t:?}");
    }
    assert!(download_ids(&e.calls()).is_empty());
    let o = playlist(&e, &url, &["--list"]).await;
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("warning: .eid-playlist.json is missing"), "{t}");
    assert!(!t.contains('\u{202E}'), "{t:?}");
    assert_eq!(user_files(&folder), before);
    assert!(!folder.join(".eid-playlist.json").exists());
    assert_eq!(std::fs::read(&outside).unwrap(), b"fake old\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playlist_lost_state_restored_from_backup_continues_with_the_old_numbers() {
    let e = Env::new().await;
    let list = write_list(&e, &[item("a", "Alpha"), item("b", "Beta")]);
    let url = purl(&e, &list, "pl-m3b", "Backup", "");
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let folder = folder_of(&o);
    let backup = std::fs::read(folder.join(".eid-playlist.json")).unwrap();
    std::fs::remove_file(folder.join(".eid-playlist.json")).unwrap();
    write_list(
        &e,
        &[item("z", "Zulu"), item("a", "Alpha"), item("b", "Beta")],
    );
    let o = playlist(&e, &url, &["-q"]).await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    // the user follows the message: the backup goes back, the same command continues
    std::fs::write(folder.join(".eid-playlist.json"), &backup).unwrap();
    let before = e.calls().len();
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(download_ids(&e.calls()[before..]), ["z"]);
    assert_eq!(
        visible(&folder),
        ["01 - Alpha.mp4", "02 - Beta.mp4", "03 - Zulu.mp4"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playlist_without_state_in_a_folder_without_earlier_runs_starts_normally() {
    let e = Env::new().await;
    let list = write_list(&e, &[item("a", "Alpha"), item("b", "Beta")]);
    let url = purl(&e, &list, "pl-m3c", "Fresh", "");
    // a folder with the playlist's name already exists, with unnumbered files of the user
    let folder = e.out().join("Fresh");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("notes.txt"), b"mine").unwrap();
    std::fs::write(folder.join(".eid-archive.txt"), b"\n").unwrap();
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(folder_of(&o), folder);
    assert_eq!(
        visible(&folder),
        ["01 - Alpha.mp4", "02 - Beta.mp4", "notes.txt"]
    );
    assert_eq!(std::fs::read(folder.join("notes.txt")).unwrap(), b"mine");
}

// ------------------------------------------------------------------ D8 numbering over time

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn numbering_is_stable_when_new_items_are_prepended() {
    let e = Env::new().await;
    let url = |e: &Env| purl(e, &e.root.path().join("list.json"), "pl6", "Stable", "");
    write_list(&e, &[item("a", "Alpha"), item("b", "Beta")]);
    let o = playlist(&e, &url(&e), &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let folder = folder_of(&o);
    let snapshot = |f: &str| hex(&std::fs::read(folder.join(f)).unwrap());
    let a_hash = snapshot("01 - Alpha.mp4");
    // a new item on top does not shift the old ones: it takes the next free number
    write_list(
        &e,
        &[item("z", "Zulu"), item("a", "Alpha"), item("b", "Beta")],
    );
    let o = playlist(&e, &url(&e), &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        visible(&folder),
        ["01 - Alpha.mp4", "02 - Beta.mp4", "03 - Zulu.mp4"]
    );
    assert_eq!(snapshot("01 - Alpha.mp4"), a_hash);
    // an item that disappears keeps its number and file; its number is never reused
    write_list(
        &e,
        &[item("z", "Zulu"), item("a", "Alpha"), item("q", "Quebec")],
    );
    let o = playlist(&e, &url(&e), &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        visible(&folder),
        [
            "01 - Alpha.mp4",
            "02 - Beta.mp4",
            "03 - Zulu.mp4",
            "04 - Quebec.mp4"
        ]
    );
    assert_eq!(item_state(&folder, "b")["number"], 2);
    // a re-titled item is not downloaded again and not renamed
    write_list(
        &e,
        &[
            item("a", "Renamed Alpha"),
            item("z", "Zulu"),
            item("q", "Quebec"),
        ],
    );
    let before = e.calls().len();
    let o = playlist(&e, &url(&e), &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert!(download_ids(&e.calls()[before..]).is_empty());
    assert_eq!(visible(&folder).len(), 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn growing_past_99_keeps_old_names_and_never_collides() {
    let e = Env::new().await;
    let url = purl(&e, &e.root.path().join("list.json"), "pl7", "Grow", "");
    // deselect all but the last two so the test stays fast; numbers are kept for every item
    let skip: Vec<String> = (1..=97).map(|n| n.to_string()).collect();
    let skip = skip.join(",");
    write_list(&e, &items(99, |i| format!("T{i}")));
    let o = playlist(&e, &url, &["--skip", &skip, "-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let folder = folder_of(&o);
    assert_eq!(visible(&folder), ["98 - T98.mp4", "99 - T99.mp4"]);
    assert_eq!(state_json(&folder)["width"], 2, "99 items: two digits");
    let old: Vec<String> = ["98 - T98.mp4", "99 - T99.mp4"]
        .iter()
        .map(|f| hex(&std::fs::read(folder.join(f)).unwrap()))
        .collect();
    // the list grows to 101: the width stays 2, the new numbers keep their natural digits
    write_list(&e, &items(101, |i| format!("T{i}")));
    let o = playlist(&e, &url, &["--skip", &skip, "-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        visible(&folder),
        [
            "100 - T100.mp4",
            "101 - T101.mp4",
            "98 - T98.mp4",
            "99 - T99.mp4"
        ]
    );
    assert_eq!(state_json(&folder)["width"], 2, "the width never changes");
    for (f, h) in ["98 - T98.mp4", "99 - T99.mp4"].iter().zip(old) {
        assert_eq!(
            hex(&std::fs::read(folder.join(f)).unwrap()),
            h,
            "{f} changed"
        );
    }
    // a foreign file already carrying the next name is never overwritten: the item gets the
    // collision suffix and the real name is recorded
    std::fs::write(folder.join("102 - T102.mp4"), b"foreign").unwrap();
    write_list(&e, &items(102, |i| format!("T{i}")));
    let o = playlist(&e, &url, &["--skip", &skip, "-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        std::fs::read(folder.join("102 - T102.mp4")).unwrap(),
        b"foreign"
    );
    let recorded = item_state(&folder, "i102")["file"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(recorded, "102 - T102.mp4");
    assert!(recorded.starts_with("102 - T102"), "{recorded}");
    assert_eq!(
        hex(&std::fs::read(folder.join(&recorded)).unwrap()),
        hex(&fixture("clip.mp4"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_video_in_a_playlist_downloads_once() {
    let e = Env::new().await;
    let list = write_list(
        &e,
        &[
            item("a", "Alpha"),
            item("b", "Beta"),
            item("a", "Alpha again"),
            item("c", "Gamma"),
            item("b", "Beta again"),
        ],
    );
    let o = playlist(&e, &purl(&e, &list, "pl8", "Dups", ""), &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let folder = folder_of(&o);
    // one file per video, numbered by the first appearance; c is the third distinct video
    assert_eq!(
        visible(&folder),
        ["01 - Alpha.mp4", "02 - Beta.mp4", "03 - Gamma.mp4"]
    );
    assert_eq!(download_ids(&e.calls()), ["a", "b", "c"]);
}

// ------------------------------------------------------------------ hostile state file

fn good_state() -> eid_media::playlist::State {
    use eid_media::playlist::{Item, State, Status};
    State {
        playlist_id: "pl9".into(),
        width: 2,
        items: vec![
            Item {
                id: "a".into(),
                number: 1,
                status: Status::Done,
                file: Some("01 - Alpha.mp4".into()),
                attempts: 0,
            },
            Item {
                id: "b".into(),
                number: 2,
                status: Status::Pending,
                file: None,
                attempts: 0,
            },
        ],
    }
}

/// The state file as eid writes it: schema, state and the SHA-256 of the state's JSON.
fn state_file_bytes(s: &eid_media::playlist::State) -> Vec<u8> {
    let d = hex(&serde_json::to_vec(s).unwrap());
    serde_json::to_vec_pretty(
        &json!({"schema": eid_media::playlist::SCHEMA, "state": s, "sha256": d}),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_playlist_state_file_is_rejected_and_confined() {
    use eid_media::playlist::{State, Status};
    let e = Env::new().await;
    let list = write_list(&e, &[item("a", "Alpha"), item("b", "Beta")]);
    let url = purl(&e, &list, "pl9", "Hostile", "");
    let folder = e.out().join("Hostile");
    std::fs::create_dir(&folder).unwrap();
    let elsewhere = e.root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("victim.mp4"), b"victim").unwrap();
    std::fs::write(folder.join("01 - Alpha.mp4"), b"mine").unwrap();
    let state_path = folder.join(".eid-playlist.json");

    // the control: the good state is accepted (the rejections below are about the hostile part)
    std::fs::write(&state_path, state_file_bytes(&good_state())).unwrap();
    let o = playlist(&e, &url, &["--list"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("done"),
        "{}",
        text(&o)
    );

    let with = |f: &dyn Fn(&mut State)| {
        let mut s = good_state();
        f(&mut s);
        state_file_bytes(&s)
    };
    let done_file = |name: &str| {
        let name = name.to_string();
        with(&move |s: &mut State| s.items[0].file = Some(name.clone()))
    };
    let mut cases: Vec<(String, Vec<u8>)> = vec![];
    for n in [
        "../victim.mp4",
        "..\\victim.mp4",
        "/etc/passwd",
        "C:\\Windows\\x.mp4",
        "\\\\server\\share\\x.mp4",
        "sub/x.mp4",
        "CON",
        "x.mp4:ads",
        "trailing. ",
        "bidi\u{202E}4pm.exe",
        "ctl\u{7}.mp4",
        ".eid-playlist.json",
        ".eid-lock",
        "",
    ] {
        cases.push((format!("file {n:?}"), done_file(n)));
    }
    cases.push(("number 0".into(), with(&|s| s.items[0].number = 0)));
    cases.push((
        "duplicate number".into(),
        with(&|s| s.items[1].number = s.items[0].number),
    ));
    cases.push((
        "huge number".into(),
        with(&|s| s.items[1].number = u32::MAX),
    ));
    cases.push(("duplicate id".into(), with(&|s| s.items[1].id = "a".into())));
    cases.push(("empty id".into(), with(&|s| s.items[1].id = String::new())));
    cases.push(("width 0".into(), with(&|s| s.width = 0)));
    cases.push(("width 99".into(), with(&|s| s.width = 99)));
    // the attempt counter (schema 2) must agree with the status
    cases.push((
        "failed with too few attempts".into(),
        with(&|s| s.items[1].status = Status::Failed),
    ));
    cases.push((
        "pending past the attempt limit".into(),
        with(&|s| s.items[1].attempts = eid_media::playlist::MAX_ATTEMPTS),
    ));
    cases.push((
        "huge attempt count".into(),
        with(&|s| s.items[1].attempts = u32::MAX),
    ));
    cases.push((
        "file on a pending item".into(),
        with(&|s| s.items[1].file = Some("02 - Beta.mp4".into())),
    ));
    cases.push((
        "same file twice".into(),
        with(&|s| {
            s.items[1].status = Status::Done;
            s.items[1].file = Some("01 - ALPHA.mp4".into());
        }),
    ));
    // a damaged or edited file: the checksum no longer matches
    let mut t: Value = serde_json::from_slice(&state_file_bytes(&good_state())).unwrap();
    t["state"]["items"][1]["number"] = json!(7);
    cases.push((
        "edited after writing".into(),
        serde_json::to_vec(&t).unwrap(),
    ));
    let mut t: Value = serde_json::from_slice(&state_file_bytes(&good_state())).unwrap();
    t["state"]["items"][0]["extra"] = json!(1);
    cases.push(("unknown field".into(), serde_json::to_vec(&t).unwrap()));
    let mut t: Value = serde_json::from_slice(&state_file_bytes(&good_state())).unwrap();
    t["schema"] = json!(9);
    cases.push(("unknown schema".into(), serde_json::to_vec(&t).unwrap()));
    cases.push(("garbage".into(), b"\x00\x01not json".to_vec()));
    cases.push(("empty".into(), vec![]));
    cases.push((
        "another playlist's folder".into(),
        with(&|s| s.playlist_id = "someone-else".into()),
    ));
    // larger than the 8 MiB bound
    let mut big = state_file_bytes(&good_state());
    big.extend(std::iter::repeat_n(b' ', 9 * 1024 * 1024));
    cases.push(("oversized".into(), big));

    let before_files = visible(&folder);
    for (name, bytes) in &cases {
        std::fs::write(&state_path, bytes).unwrap();
        let calls = e.calls().len();
        let o = playlist(&e, &url, &["-q"]).await;
        assert_eq!(o.status.code(), Some(1), "{name}: {}", text(&o));
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("playlist"),
            "{name}: {}",
            text(&o)
        );
        assert_eq!(
            &std::fs::read(&state_path).unwrap(),
            bytes,
            "{name}: state rewritten"
        );
        assert!(
            download_ids(&e.calls()[calls..]).is_empty(),
            "{name}: the tool downloaded"
        );
        assert_eq!(visible(&folder), before_files, "{name}: files changed");
        assert!(staging_dirs(&folder).is_empty(), "{name}");
        assert_eq!(
            std::fs::read(elsewhere.join("victim.mp4")).unwrap(),
            b"victim"
        );
        assert_eq!(entries(&elsewhere), ["victim.mp4"], "{name}");
        assert_eq!(
            std::fs::read(folder.join("01 - Alpha.mp4")).unwrap(),
            b"mine"
        );
    }
    // a link in place of the state file is never followed, even to a valid state
    std::fs::remove_file(&state_path).unwrap();
    std::fs::write(
        elsewhere.join("state.json"),
        state_file_bytes(&good_state()),
    )
    .unwrap();
    eid_platform::fs::symlink_file(&elsewhere.join("state.json"), &state_path)
        .expect("cannot create a symlink: this test must not pass without one");
    let calls = e.calls().len();
    let o = playlist(&e, &url, &["-q"]).await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(download_ids(&e.calls()[calls..]).is_empty());
    assert_eq!(entries(&elsewhere), ["state.json", "victim.mp4"]);
    remove_link(&state_path);
}

// ------------------------------------------------------------------ the folder lock

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_runs_on_one_playlist_folder_cannot_overlap() {
    let e = Env::new().await;
    let list = write_list(&e, &[item("a", "Alpha"), item("b", "Beta")]);
    let stall = e.root.path().join("stall.flag");
    std::fs::write(&stall, b"1").unwrap();
    let url = purl(
        &e,
        &list,
        "pl10",
        "Locked",
        &format!("&stall={}&stall_id=b", pct(&stall.display().to_string())),
    );
    let out = e.out().display().to_string();
    let first = spawn_eid(
        &e,
        &["video", "playlist", &url, "-d", &out, "--yes-i-agree", "-q"],
    );
    let (pid, _) = wait_for_stall(&e, "b").await;
    let folder = e.out().join("Locked");
    let files = visible(&folder);
    let downloads = download_ids(&e.calls());
    // the second run on the same folder is refused at once and changes nothing; if it were
    // not refused it would stall like the first, so it is watched and killed after a deadline
    let mut c = e.cmd(&["video", "playlist", &url, "-d", &out, "--yes-i-agree", "-q"]);
    c.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut second = c.spawn().unwrap();
    let t = Instant::now();
    let finished = loop {
        if second.try_wait().unwrap().is_some() {
            break true;
        }
        if t.elapsed() > Duration::from_secs(30) {
            let _ = second.kill();
            break false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let o = tokio::task::spawn_blocking(move || second.wait_with_output().unwrap())
        .await
        .unwrap();
    assert!(
        finished,
        "the second run was not refused and kept running: {}",
        text(&o)
    );
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("already working"),
        "{}",
        text(&o)
    );
    assert_eq!(download_ids(&e.calls()), downloads);
    assert_eq!(visible(&folder), files);
    assert!(
        eid_platform::process::process_is_alive(pid),
        "the first run was disturbed"
    );
    // once the first run is gone the lock is free (it belongs to the process, not to a file)
    stop_eid(first, pid).await;
    std::fs::remove_file(&stall).unwrap();
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(visible(&folder), ["01 - Alpha.mp4", "02 - Beta.mp4"]);
}

// ------------------------------------------------------------------ private / deleted items

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_or_deleted_item_is_skipped_and_keeps_its_number() {
    let e = Env::new().await;
    let url = |e: &Env| purl(e, &e.root.path().join("list.json"), "pl11", "Mixed", "");
    // explicit restrictions only (owner decision M2): removed by the uploader, private in the
    // listing, DRM, private by the tool's own error
    let mut l = vec![
        item("a", "Alpha"),
        item_with("b", "Removed", "error", "removed"),
        item_with("c", "Private", "availability", "private"),
        item_with("d", "Protected", "error", "drm"),
        item_with("p", "Hidden", "error", "private"),
        item("e", "Echo"),
    ];
    write_list(&e, &l);
    let o = playlist(&e, &url(&e), &[]).await;
    assert!(
        o.status.success(),
        "unavailable items must not fail the run: {}",
        text(&o)
    );
    let folder = folder_of(&o);
    assert_eq!(visible(&folder), ["01 - Alpha.mp4", "06 - Echo.mp4"]);
    for (id, n) in [("b", 2), ("c", 3), ("d", 4), ("p", 5)] {
        let s = item_state(&folder, id);
        assert_eq!(
            (s["number"].as_u64(), s["status"].as_str()),
            (Some(n), Some("unavailable")),
            "{id}"
        );
        assert!(s["file"].is_null());
    }
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(err.contains("DRM"), "{err}");
    assert!(err.contains("removed"), "{err}");
    assert!(
        err.contains("3 downloaded") || err.contains("2 downloaded"),
        "{err}"
    );
    assert!(err.contains("4 unavailable"), "{err}");
    // a later run: the unavailable ones are not asked again, a new item gets the next number
    l.push(item("f", "Foxtrot"));
    write_list(&e, &l);
    let before = e.calls().len();
    let o = playlist(&e, &url(&e), &[]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(download_ids(&e.calls()[before..]), ["f"]);
    assert_eq!(
        visible(&folder),
        ["01 - Alpha.mp4", "06 - Echo.mp4", "07 - Foxtrot.mp4"]
    );
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    for n in 2..=5 {
        assert!(err.contains(&format!("{n}: unavailable")), "{n}: {err}");
        assert!(err.contains(&format!("--retry-failed {n}")), "{n}: {err}");
    }
    // a temporary failure (rate limit) is a failure, not "unavailable": exit code 1, the
    // number is kept and the next run tries again
    l.push(item_with("g", "Golf", "error", "429"));
    write_list(&e, &l);
    for k in 1..=2 {
        let o = playlist(&e, &url(&e), &["-q"]).await;
        assert_eq!(o.status.code(), Some(1), "{}", text(&o));
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("429"),
            "{}",
            text(&o)
        );
        let g = item_state(&folder, "g");
        assert_eq!(
            (
                g["number"].as_u64(),
                g["status"].as_str(),
                g["attempts"].as_u64()
            ),
            (Some(8), Some("pending"), Some(k))
        );
    }
    assert_eq!(visible(&folder).len(), 3);
    // conditions change: the removed video is back; the user asks again by hand for 2, 3, 4.
    // Item 2 is downloaded; 3 is still private in the listing, so the tool is not even asked;
    // 4 is still DRM-protected and refused again. Nothing is worked around; 5 was not named and
    // is not asked.
    l[1] = item("b", "Removed");
    write_list(&e, &l);
    let before = e.calls().len();
    let o = playlist(&e, &url(&e), &["--retry-failed", "2,3,4"]).await;
    // the fake logs only real downloads; "<n>: <title>" is printed each time the tool is asked
    assert_eq!(download_ids(&e.calls()[before..]), ["b"], "{}", text(&o));
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(err.contains("4: Protected") && err.contains("DRM"), "{err}");
    assert!(
        !err.contains("3: Private") && !err.contains("5: Hidden"),
        "the tool was asked for an item it must not be: {err}"
    );
    // g (pending, third attempt) also runs and fails: exit code 1
    assert!(err.contains("8: Golf"), "{err}");
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert_eq!(
        visible(&folder),
        [
            "01 - Alpha.mp4",
            "02 - Removed.mp4",
            "06 - Echo.mp4",
            "07 - Foxtrot.mp4"
        ]
    );
    for (id, n, status) in [
        ("b", 2, "done"),
        ("c", 3, "unavailable"),
        ("d", 4, "unavailable"),
        ("p", 5, "unavailable"),
        ("g", 8, "failed"),
    ] {
        let s = item_state(&folder, id);
        assert_eq!(
            (s["number"].as_u64(), s["status"].as_str()),
            (Some(n), Some(status)),
            "{id}"
        );
    }
}

// ------------------------------------------------------------------ retries (P4 review M2)

/// P4 review m3: one item that cannot be published does not end the run. The items after it
/// are still downloaded and the summary is printed; the item counts as a temporary failure
/// (M2) and keeps its download in staging, so the next run publishes it. The publish error is
/// injected with the `test-hooks` hook `EID_TEST_PUBLISH_FAIL` (an I/O error at that name: a
/// real one, such as a full disk or a refused path, cannot be caused per item portably).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_item_that_cannot_be_published_does_not_end_the_playlist_run() {
    let e = Env::new().await;
    let list = e.root.path().join("list.json");
    let url = purl(&e, &list, "pl20", "Pub", "");
    write_list(
        &e,
        &[item("a", "Alpha"), item("b", "Bravo"), item("c", "Charlie")],
    );
    let out = e.out().display().to_string();
    let mut c = e.cmd(&["video", "playlist", &url, "-d", &out, "--yes-i-agree"]);
    c.env("EID_TEST_PUBLISH_FAIL", "02 - Bravo.mp4");
    let o = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    let folder = folder_of(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(download_ids(&e.calls()), ["a", "b", "c"], "{err}");
    assert!(err.contains("2: cannot publish"), "{err}");
    assert!(
        err.contains("2 downloaded, 0 deselected, 0 unavailable, 1 failed"),
        "no summary: {err}"
    );
    assert_eq!(visible(&folder), ["01 - Alpha.mp4", "03 - Charlie.mp4"]);
    let b = item_state(&folder, "b");
    assert_eq!(
        (
            b["status"].as_str(),
            b["attempts"].as_u64(),
            b["number"].as_u64()
        ),
        (Some("pending"), Some(1), Some(2)),
        "{err}"
    );
    assert_eq!(item_state(&folder, "c")["status"], "done");
    assert_eq!(staging_dirs(&folder).len(), 1, "{:?}", entries(&folder));
    // the next run (no fault) publishes the kept download under its number
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        visible(&folder),
        ["01 - Alpha.mp4", "02 - Bravo.mp4", "03 - Charlie.mp4"]
    );
    assert_eq!(
        std::fs::read(folder.join("02 - Bravo.mp4")).unwrap(),
        fixture("clip.mp4")
    );
    assert_eq!(item_state(&folder, "b")["status"], "done");
    assert!(staging_dirs(&folder).is_empty(), "{:?}", entries(&folder));
}

/// P4 review m4: a listing entry without a usable address (some extractors give only an id)
/// is a temporary failure like any other (owner decision M2): counted per run, `failed` after
/// MAX_ATTEMPTS runs and then not asked again, keeping its number; the other items are fine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_without_an_address_stops_being_retried_after_three_runs() {
    let e = Env::new().await;
    let list = e.root.path().join("list.json");
    let url = purl(&e, &list, "pl21", "NoUrl", "");
    let mut bare = item("x", "Bare");
    bare["nourl"] = json!(true);
    write_list(&e, &[item("a", "Alpha"), bare, item("c", "Charlie")]);
    let folder = e.out().join("NoUrl");
    for k in 1..=4u64 {
        let o = playlist(&e, &url, &["-q"]).await;
        let err = String::from_utf8_lossy(&o.stderr).into_owned();
        assert_eq!(o.status.code(), Some(1), "run {k}: {err}");
        let x = item_state(&folder, "x");
        let want = match k {
            1 | 2 => (Some("pending"), Some(k)),
            _ => (Some("failed"), Some(3)),
        };
        assert_eq!(
            (x["status"].as_str(), x["attempts"].as_u64()),
            want,
            "run {k}"
        );
        assert_eq!(x["number"], 2, "run {k}");
        if k <= 3 {
            assert!(
                err.contains("2: the playlist gave no usable address"),
                "run {k}: {err}"
            );
        } else {
            assert!(!err.contains("no usable address"), "run {k}: {err}");
            assert!(err.contains("--retry-failed 2"), "run {k}: {err}");
        }
    }
    assert_eq!(download_ids(&e.calls()), ["a", "c"]);
    assert_eq!(visible(&folder), ["01 - Alpha.mp4", "03 - Charlie.mp4"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn temporary_failures_stop_after_three_runs_and_retry_failed_resets_only_the_named_items() {
    let e = Env::new().await;
    let list = e.root.path().join("list.json");
    let url = purl(&e, &list, "pl13", "Retry", "");
    let mut l = vec![
        item("a", "Alpha"),
        // a warning about a private video, then a vague error: must stay retryable (M2)
        item_with("v", "Vague", "error", "vague"),
        item_with("r", "Rated", "error", "429"),
    ];
    write_list(&e, &l);
    let folder = e.out().join("Retry");
    for k in 1..=3u64 {
        let before = e.calls().len();
        let o = playlist(&e, &url, &[]).await;
        assert_eq!(o.status.code(), Some(1), "run {k}: {}", text(&o));
        let want: &[&str] = if k == 1 { &["a"] } else { &[] };
        assert_eq!(download_ids(&e.calls()[before..]), want, "run {k}");
        // the fake logs only real downloads; "<n>: <title>" is printed each time the tool is asked
        let err = String::from_utf8_lossy(&o.stderr).into_owned();
        assert!(
            err.contains("2: Vague") && err.contains("3: Rated"),
            "run {k}: {err}"
        );
        for (id, n) in [("v", 2), ("r", 3)] {
            let s = item_state(&folder, id);
            let status = if k < 3 { "pending" } else { "failed" };
            assert_eq!(
                (
                    s["number"].as_u64(),
                    s["status"].as_str(),
                    s["attempts"].as_u64()
                ),
                (Some(n), Some(status), Some(k)),
                "run {k}, {id}"
            );
        }
        if k == 3 {
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            assert!(err.contains("--retry-failed 2"), "{err}");
            assert!(err.contains("--retry-failed 3"), "{err}");
        }
    }
    // run 4: neither is asked again automatically, and the failure is still shown (exit 1)
    let before = e.calls().len();
    let o = playlist(&e, &url, &[]).await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(download_ids(&e.calls()[before..]).is_empty());
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(
        err.contains("not retried automatically any more: 2, 3")
            && err.contains("--retry-failed 2,3"),
        "{err}"
    );
    assert!(
        !err.contains("2: Vague") && !err.contains("3: Rated"),
        "asked again automatically: {err}"
    );
    // conditions change for item 3; the user retries 3 only: 2 is left as it is
    l[2] = item("r", "Rated");
    write_list(&e, &l);
    let before = e.calls().len();
    let o = playlist(&e, &url, &["--retry-failed", "3", "-q"]).await;
    assert_eq!(download_ids(&e.calls()[before..]), ["r"], "{}", text(&o));
    assert_eq!(
        o.status.code(),
        Some(1),
        "item 2 still failed: {}",
        text(&o)
    );
    assert_eq!(visible(&folder), ["01 - Alpha.mp4", "03 - Rated.mp4"]);
    let v = item_state(&folder, "v");
    assert_eq!(
        (v["status"].as_str(), v["attempts"].as_u64()),
        (Some("failed"), Some(3))
    );
    // a number that is not failed or unavailable: nothing changes
    let before = e.calls().len();
    let o = playlist(&e, &url, &["--retry-failed", "1,9", "-q"]).await;
    assert!(download_ids(&e.calls()[before..]).is_empty());
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(
        err.contains("1: not a failed or unavailable item")
            && err.contains("9: not a failed or unavailable item"),
        "{err}"
    );
    assert_eq!(item_state(&folder, "a")["status"], "done");
    // `all`: the remaining failed item starts over from attempt 1
    let before = e.calls().len();
    let o = playlist(&e, &url, &["--retry-failed", "all"]).await;
    assert!(
        download_ids(&e.calls()[before..]).is_empty(),
        "{}",
        text(&o)
    );
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(
        err.contains("2: Vague") && !err.contains("3: Rated"),
        "{err}"
    );
    let v = item_state(&folder, "v");
    assert_eq!(
        (
            v["number"].as_u64(),
            v["status"].as_str(),
            v["attempts"].as_u64()
        ),
        (Some(2), Some("pending"), Some(1))
    );
    // --retry-failed is for playlists only, and never with --list
    for bad in [
        vec!["video", "get", &url, "--retry-failed", "2"],
        vec!["video", "playlist", &url, "--retry-failed", "2", "--list"],
        vec!["video", "playlist", &url, "--retry-failed", "0"],
        vec!["video", "playlist", &url, "--retry-failed"],
    ] {
        let o = e.run(&bad).await;
        assert_eq!(o.status.code(), Some(2), "{bad:?}: {}", text(&o));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playlist_state_schema_1_is_upgraded_in_place_without_losing_numbers_or_files() {
    let e = Env::new().await;
    let list = write_list(
        &e,
        &[
            item("a", "Alpha"),
            item("b", "Beta"),
            item("c", "Gamma"),
            item("d", "Delta"),
        ],
    );
    let url = purl(&e, &list, "pl-v1", "Old", "");
    // a folder left by the previous version: its state file (written by the pre-M2 code), a
    // published file and the archive
    let folder = e.out().join("Old");
    std::fs::create_dir(&folder).unwrap();
    let v1 = include_bytes!("../../eid-media/tests/fixtures/playlist-state-v1.json");
    std::fs::write(folder.join(".eid-playlist.json"), v1).unwrap();
    std::fs::write(folder.join("01 - Alpha.mp4"), b"published earlier").unwrap();
    std::fs::write(folder.join(".eid-archive.txt"), "fake a\nfake d\n").unwrap();
    // --list reads it without writing anything
    let o = playlist(&e, &url, &["--list"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        std::fs::read(folder.join(".eid-playlist.json")).unwrap(),
        v1
    );
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    // a (done) and d (done, archive only) are not downloaded again; b (an old "unavailable",
    // from the classifier that M2 replaced) and c (pending) are
    assert_eq!(download_ids(&e.calls()), ["b", "c"]);
    assert_eq!(
        visible(&folder),
        ["01 - Alpha.mp4", "02 - Beta.mp4", "03 - Gamma.mp4"]
    );
    assert_eq!(
        std::fs::read(folder.join("01 - Alpha.mp4")).unwrap(),
        b"published earlier"
    );
    // the state is schema 2 now, with every number, file and done item kept
    let st: Value =
        serde_json::from_slice(&std::fs::read(folder.join(".eid-playlist.json")).unwrap()).unwrap();
    assert_eq!(st["schema"], 2);
    let items: Vec<(String, u64, String, Value)> = st["state"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["id"].as_str().unwrap().to_string(),
                i["number"].as_u64().unwrap(),
                i["status"].as_str().unwrap().to_string(),
                i["file"].clone(),
            )
        })
        .collect();
    assert_eq!(
        items,
        [
            ("a".into(), 1, "done".into(), json!("01 - Alpha.mp4")),
            ("b".into(), 2, "done".into(), json!("02 - Beta.mp4")),
            ("c".into(), 3, "done".into(), json!("03 - Gamma.mp4")),
            ("d".into(), 7, "done".into(), Value::Null),
        ]
    );
}

// ------------------------------------------------------------------ secrets

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guard_secrets_never_reach_outputs_logs_state_archive_or_staging() {
    const SECRETS: [&str; 6] = [
        "SENTINEL_PASS",
        "SENTINEL_TOKEN",
        "SENTINEL_SIG",
        "SENTINEL_COOKIE",
        "SENTINELHOST",
        "203.0.113.77",
    ];
    let e = Env::new().await;
    let list = write_list(
        &e,
        &[
            item("a", "Alpha"),
            item_with("b", "Leaky", "error", "secrets"),
            item("c", "Gamma"),
        ],
    );
    // a token in the query reaches the tool (it needs the address) but nothing else
    let url = purl(&e, &list, "pl12", "Secrets", "&token=SENTINEL_TOKEN");
    let mut shown = vec![];
    let o = playlist(&e, &url, &[]).await;
    shown.push(text(&o));
    assert_eq!(
        o.status.code(),
        Some(1),
        "the leaky item fails the run: {}",
        text(&o)
    );
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("<redacted>"),
        "{}",
        text(&o)
    );
    // user:password in the address is refused before anything starts
    let calls = e.calls().len();
    let with_userinfo = url.replace("https://fake.test", "https://user:SENTINEL_PASS@fake.test");
    let o = playlist(&e, &with_userinfo, &[]).await;
    shown.push(text(&o));
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert_eq!(
        e.calls().len(),
        calls,
        "the tool ran with credentials in the address"
    );
    // the same through --list and a second normal run
    for more in [&["--list"][..], &[]] {
        let o = playlist(&e, &url, more).await;
        shown.push(text(&o));
    }
    // the positive control: the secrets did reach the (fake) tool, so the checks below can fail
    let log = std::fs::read_to_string(e.log()).unwrap();
    assert!(
        log.contains("SENTINEL_TOKEN"),
        "the token never reached the tool"
    );
    // nothing we wrote or showed contains a secret: outputs, the playlist folder with its
    // state, archive and staging folders, the settings and the tool folders
    let mut hay: Vec<(String, Vec<u8>)> = shown
        .into_iter()
        .map(|s| ("output".to_string(), s.into_bytes()))
        .collect();
    let mut stack = vec![
        e.out(),
        e.root.path().join("cfg"),
        e.root.path().join("data"),
    ];
    let mut files = 0;
    while let Some(d) = stack.pop() {
        for ent in std::fs::read_dir(&d).unwrap() {
            let p = ent.unwrap().path();
            let md = std::fs::symlink_metadata(&p).unwrap();
            if md.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "exe") {
                // the installed (fake) tools: the fake's own code contains the sentinels
                continue;
            } else {
                files += 1;
                hay.push((p.display().to_string(), std::fs::read(&p).unwrap()));
            }
        }
    }
    assert!(files >= 5, "the scan found only {files} files");
    for (name, bytes) in &hay {
        for s in SECRETS {
            assert!(
                !bytes.windows(s.len()).any(|w| w == s.as_bytes()),
                "{s} found in {name}"
            );
        }
    }
    let folder = e.out().join("Secrets");
    assert!(folder.join(".eid-playlist.json").exists());
    assert!(folder.join(".eid-archive.txt").exists());
}

// ------------------------------------------------------------------ remux to MP4 (P4 review M4)

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playlist_items_in_another_container_are_remuxed_and_a_bad_result_is_a_failed_item() {
    let e = Env::new().await;
    let list = write_list(&e, &[item("a", "Alpha"), item("b", "Beta")]);
    // every item is a single WebM file: remuxed by the tool, checked, published as .mp4
    let url = purl(&e, &list, "pl14", "Remuxed", "&container=webm");
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    let folder = folder_of(&o);
    assert_eq!(visible(&folder), ["01 - Alpha.mp4", "02 - Beta.mp4"]);
    for f in visible(&folder) {
        assert_eq!(
            hex(&std::fs::read(folder.join(&f)).unwrap()),
            hex(&fixture("clip.mp4")),
            "{f}"
        );
    }
    assert!(staging_dirs(&folder).is_empty());
    // the tool "succeeds" with a cut MP4: nothing is published, every item is tried (the run
    // is not stopped by the first one), each counts as a failed attempt and keeps its number
    let url = purl(&e, &list, "pl15", "Broken", "&container=webm&remux=broken");
    let o = playlist(&e, &url, &[]).await;
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let folder = e.out().join("Broken");
    assert!(visible(&folder).is_empty(), "{:?}", visible(&folder));
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(err.contains("1: Alpha") && err.contains("2: Beta"), "{err}");
    assert!(err.contains("not a complete MP4"), "{err}");
    for (id, n) in [("a", 1), ("b", 2)] {
        let s = item_state(&folder, id);
        assert_eq!(
            (
                s["number"].as_u64(),
                s["status"].as_str(),
                s["attempts"].as_u64()
            ),
            (Some(n), Some("pending"), Some(1)),
            "{id}"
        );
    }
    // the bad files stay in their staging folders (never published, never deleted by us)
    assert_eq!(staging_dirs(&folder).len(), 2);
}

/// P4 review m9: a `--skip` number that no item has is reported (not silently ignored), and
/// the run goes on with the numbers that exist.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skip_numbers_that_no_item_has_are_reported_and_the_rest_apply() {
    let e = Env::new().await;
    let list = e.root.path().join("list.json");
    let url = purl(&e, &list, "pl22", "Skip", "");
    write_list(&e, &items(3, |i| format!("T{i}")));
    let o = playlist(&e, &url, &["-q", "--skip", "2,9"]).await;
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        err.contains("--skip 9: no item of this playlist has that number"),
        "{err}"
    );
    assert!(!err.contains("--skip 2:"), "{err}");
    assert_eq!(download_ids(&e.calls()), ["i1", "i3"]);
    assert!(err.contains("2 downloaded, 1 deselected"), "{err}");
}
