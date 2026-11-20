//! v0.2 quality fallback, checked with the REAL yt-dlp format selector, offline: yt-dlp reads a
//! crafted info JSON (`--load-info-json`, no network, no site) and prints the format it would
//! download for each of our selectors. This proves the selector strings mean what we say; it is
//! NOT proof that any real site (YouTube) works.
//!
//! Needs a yt-dlp executable: `EID_YTDLP=/path/to/yt-dlp cargo test -p eid-media --test
//! g5_fallback_real_tool -- --ignored`. Without `EID_YTDLP` the test fails loudly (it is
//! `#[ignore]`d so a normal `cargo test` does not need the tool). Results go to docs/STATUS.md.

use eid_media::args::{exact_selector, height_selector, higher_selector};
use std::path::Path;

/// One video with the given heights (video-only formats `v<h>`) plus one audio format `a`.
fn info_json(dir: &Path, heights: &[u32]) -> std::path::PathBuf {
    let mut formats: Vec<serde_json::Value> = heights
        .iter()
        .map(|h| {
            serde_json::json!({
                "format_id": format!("v{h}"), "url": format!("https://media.invalid/v{h}"),
                "ext": "mp4", "protocol": "https", "vcodec": "avc1", "acodec": "none",
                "height": h, "width": h * 16 / 9, "tbr": f64::from(*h)
            })
        })
        .collect();
    formats.push(serde_json::json!({
        "format_id": "a", "url": "https://media.invalid/a", "ext": "m4a", "protocol": "https",
        "vcodec": "none", "acodec": "mp4a", "abr": 128.0
    }));
    let info = serde_json::json!({
        "id": "x", "title": "t", "extractor": "generic", "extractor_key": "Generic",
        "webpage_url": "https://media.invalid/page", "_type": "video", "formats": formats
    });
    let p = dir.join(format!("info-{}.json", heights.len()));
    std::fs::write(&p, serde_json::to_vec(&info).unwrap()).unwrap();
    p
}

/// The format ids yt-dlp picks (`v720+a`), or `None` if it refuses ("not available").
fn pick(ytdlp: &str, info: &Path, sel: &str) -> Option<String> {
    let out = eid_platform::process::command(
        ytdlp,
        [
            "--ignore-config",
            "--no-update",
            "--simulate",
            "--load-info-json",
            &info.to_string_lossy(),
            "--format",
            sel,
            "--print",
            "format_id",
        ],
    )
    .output()
    .expect("yt-dlp starts");
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("format is not available"),
            "unexpected yt-dlp error: {err}"
        );
        None
    }
}

#[test]
#[ignore = "needs a real yt-dlp: set EID_YTDLP (results in docs/STATUS.md)"]
fn real_ytdlp_picks_what_each_fallback_promises() {
    let ytdlp = std::env::var("EID_YTDLP").expect("EID_YTDLP must point to a yt-dlp executable");
    let d = tempfile::tempdir().unwrap();
    let v = |s: &str| Some(s.to_string());

    // 360, 1080, 4320 (8K) available; the user asked for 720
    let gap = info_json(d.path(), &[360, 1080, 4320]);
    assert_eq!(
        pick(&ytdlp, &gap, &height_selector(720)),
        v("v360+a"),
        "lower"
    );
    assert_eq!(
        pick(&ytdlp, &gap, &higher_selector(720)),
        v("v1080+a"),
        "higher"
    );
    assert_eq!(pick(&ytdlp, &gap, &exact_selector(720)), None, "skip");

    // the asked class exists (a 1012-pixel crop counts as 1080p): every policy takes it
    let crop = info_json(d.path(), &[480, 1012, 2160, 4320]);
    for sel in [
        height_selector(1080),
        higher_selector(1080),
        exact_selector(1080),
    ] {
        assert_eq!(pick(&ytdlp, &crop, &sel), v("v1012+a"), "{sel}");
    }

    // owner 2026-11-20: 8K is the tallest; "higher" from 4K takes 8K, never anything taller
    let tall = info_json(d.path(), &[480, 4320, 8640]);
    assert_eq!(pick(&ytdlp, &tall, &higher_selector(2160)), v("v4320+a"));
    assert_eq!(pick(&ytdlp, &tall, &height_selector(2160)), v("v480+a"));
    assert_eq!(pick(&ytdlp, &tall, &exact_selector(2160)), None);
    // only above 8K above the asked height: falls back to the shorter one
    let above8k = info_json(d.path(), &[480, 8640, 8641]);
    assert_eq!(pick(&ytdlp, &above8k, &higher_selector(2160)), v("v480+a"));
    assert_eq!(pick(&ytdlp, &above8k, &height_selector(4320)), v("v480+a"));
}
