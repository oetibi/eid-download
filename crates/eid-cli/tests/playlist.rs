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
    // the state file is lost: the archive alone stops a second download of finished items
    std::fs::remove_file(folder.join(".eid-playlist.json")).unwrap();
    let o = playlist(&e, &url, &["-q"]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(visible(&folder).len(), 5, "{:?}", visible(&folder));
    assert_eq!(
        std::fs::read_to_string(folder.join(".eid-archive.txt")).unwrap(),
        archive
    );
    assert!(folder.join(".eid-playlist.json").exists());
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
            },
            Item {
                id: "b".into(),
                number: 2,
                status: Status::Pending,
                file: None,
            },
        ],
    }
}

/// The state file as eid writes it: schema, state and the SHA-256 of the state's JSON.
fn state_file_bytes(s: &eid_media::playlist::State) -> Vec<u8> {
    let d = hex(&serde_json::to_vec(s).unwrap());
    serde_json::to_vec_pretty(&json!({"schema": 1, "state": s, "sha256": d})).unwrap()
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
    t["schema"] = json!(2);
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
    let mut l = vec![
        item("a", "Alpha"),
        item_with("b", "Removed", "error", "private"),
        item_with("c", "Private", "availability", "private"),
        item_with("d", "Protected", "error", "drm"),
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
    assert_eq!(visible(&folder), ["01 - Alpha.mp4", "05 - Echo.mp4"]);
    for (id, n) in [("b", 2), ("c", 3), ("d", 4)] {
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
    assert!(
        err.contains("3 downloaded") || err.contains("2 downloaded"),
        "{err}"
    );
    assert!(err.contains("3 unavailable"), "{err}");
    // a later run: the unavailable ones are not asked again, a new item gets the next number
    l.push(item("f", "Foxtrot"));
    write_list(&e, &l);
    let before = e.calls().len();
    let o = playlist(&e, &url(&e), &[]).await;
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(download_ids(&e.calls()[before..]), ["f"]);
    assert_eq!(
        visible(&folder),
        ["01 - Alpha.mp4", "05 - Echo.mp4", "06 - Foxtrot.mp4"]
    );
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(
        err.contains("2: unavailable")
            && err.contains("3: unavailable")
            && err.contains("4: unavailable"),
        "{err}"
    );
    // a temporary failure (rate limit) is a failure, not "unavailable": exit code 1, the
    // number is kept and the next run tries again
    l.push(item_with("g", "Golf", "error", "429"));
    write_list(&e, &l);
    for _ in 0..2 {
        let o = playlist(&e, &url(&e), &["-q"]).await;
        assert_eq!(o.status.code(), Some(1), "{}", text(&o));
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("429"),
            "{}",
            text(&o)
        );
        let g = item_state(&folder, "g");
        assert_eq!(
            (g["number"].as_u64(), g["status"].as_str()),
            (Some(7), Some("pending"))
        );
    }
    assert_eq!(visible(&folder).len(), 3);
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
