//! The fixture clips are pinned by SHA-256 (fixtures/README.md) and, where ffprobe is
//! available, checked to contain what the README says.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const MP4_SHA256: &str = "40df818f303c01b7a110e8f8a59a6a442f7f39041cfdce76533962611dad67b9";
const MP3_SHA256: &str = "364397438bf967720de9ed65f98dcdee20de65846a8828858fd190eba42adb11";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

fn sha256_hex(p: &Path) -> String {
    let d = Sha256::digest(std::fs::read(p).unwrap());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn fixture_clips_match_their_pinned_sha256() {
    assert_eq!(sha256_hex(&fixture("clip.mp4")), MP4_SHA256);
    assert_eq!(sha256_hex(&fixture("clip.mp3")), MP3_SHA256);
}

/// `ffprobe` from `EID_FFPROBE` (a path; CI installs the pinned BtbN build, D3). With
/// `EID_REQUIRE_FFPROBE=1` a missing ffprobe is a failure, never a skip.
fn ffprobe() -> Option<PathBuf> {
    let p = std::env::var_os("EID_FFPROBE").map(PathBuf::from);
    let required = std::env::var("EID_REQUIRE_FFPROBE").as_deref() == Ok("1");
    match p {
        Some(p) if p.is_file() => Some(p),
        _ if required => panic!("EID_REQUIRE_FFPROBE=1 but EID_FFPROBE is not an ffprobe file"),
        _ => {
            eprintln!(
                "NOT TESTED: ffprobe not available (set EID_FFPROBE; CI sets \
                 EID_REQUIRE_FFPROBE=1 so this cannot pass silently there)"
            );
            None
        }
    }
}

fn streams(probe: &Path, file: &Path) -> Vec<String> {
    let o = std::process::Command::new(probe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name",
            "-of",
            "compact=p=0:nk=1",
        ])
        .arg(file)
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    String::from_utf8(o.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn fake_clip_has_video_and_audio_by_ffprobe() {
    let Some(p) = ffprobe() else { return };
    let s = streams(&p, &fixture("clip.mp4"));
    assert!(
        s.iter()
            .any(|l| l.ends_with("|video") || l.contains("video")),
        "{s:?}"
    );
    assert!(s.iter().any(|l| l.contains("audio")), "{s:?}");
    assert_eq!(s.len(), 2, "exactly one video and one audio stream: {s:?}");
    let s = streams(&p, &fixture("clip.mp3"));
    assert_eq!(s, ["mp3|audio"], "{s:?}");
}
