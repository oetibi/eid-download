//! v0.2 (owner's request 2026-11-20): the options of the download menu, each one an exact pair
//! of formats from the tool's own `-J` output, so what the menu shows (height, frame rate,
//! codec, HDR, audio, size) is what is downloaded.
//!
//! One option per height class of [`crate::args::POLICY_HEIGHTS`], tallest first. In a class the
//! choice is: the highest frame rate; then SDR before HDR (HDR only when the class has nothing
//! else, and then it says so); then the most compatible codec for an MP4 file (H.264, then VP9,
//! then AV1, then anything else); then the highest bitrate. A video-only format is paired with
//! the best audio (AAC in M4A first, for MP4); a format with its own audio is used alone; a
//! class with no audio at all says "no audio". Sizes are the tool's numbers added up, or
//! unknown: never guessed.
//!
//! Nothing here comes from the user: every format id was checked by
//! [`crate::args::check_format_id`] when the document was parsed.

use crate::args::{class_floor, MAX_HEIGHT, POLICY_HEIGHTS};
use crate::info::Format;

/// One line of the "video" part of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoOption {
    /// The class (`1080` for anything in 721..=1080).
    pub height: u32,
    /// The real height of the chosen format.
    pub real_height: u32,
    /// `--format` value: `video+audio`, or `video` when it carries its own audio.
    pub format: String,
    pub fps: Option<u32>,
    /// Display name: `H.264`, `VP9`, `AV1`, `H.265`, or the tool's first codec word.
    pub codec: String,
    /// `HDR10`, `HLG`, ... when the chosen format is not SDR.
    pub hdr: Option<String>,
    /// The file has sound (its own, or the best audio merged in).
    pub audio: bool,
    /// Sum of the tool's sizes (video + audio), when every part has one.
    pub size: Option<u64>,
}

/// One line of the "audio" part of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioOption {
    /// The policy quality: `m4a` (the original AAC stream, not converted) or `audio` (MP3,
    /// converted by FFmpeg from the best audio).
    pub quality: &'static str,
    /// `true` when the file is the stream as published (no conversion).
    pub original: bool,
    /// `--format` value for `m4a` (the exact stream); `None` for `audio`.
    pub format: Option<String>,
    /// `AAC` / `MP3`.
    pub codec: &'static str,
    /// kbit/s of the stream used (for MP3: of its source).
    pub kbps: Option<u32>,
    /// The tool's size of the original stream; `None` for MP3 (the converted size is not
    /// known before converting).
    pub size: Option<u64>,
}

/// Display name of a codec string.
pub fn codec_name(c: &str) -> String {
    let l = c.to_ascii_lowercase();
    let n = if l.starts_with("avc1") || l.starts_with("avc3") || l == "h264" {
        "H.264"
    } else if l.starts_with("vp09") || l.starts_with("vp9") {
        "VP9"
    } else if l.starts_with("av01") || l == "av1" {
        "AV1"
    } else if l.starts_with("hev1") || l.starts_with("hvc1") || l == "h265" || l == "hevc" {
        "H.265"
    } else if l.starts_with("mp4a") || l == "aac" {
        "AAC"
    } else if l == "opus" {
        "Opus"
    } else {
        return c
            .split('.')
            .next()
            .unwrap_or("")
            .chars()
            .take(12)
            .collect::<String>()
            .to_ascii_uppercase();
    };
    n.into()
}

/// Lower is more compatible inside an MP4 file.
fn codec_rank(c: Option<&str>) -> u8 {
    match c.map(codec_name).as_deref() {
        Some("H.264") => 0,
        Some("VP9") => 1,
        Some("AV1") => 2,
        _ => 3,
    }
}

fn is_hdr(f: &Format) -> bool {
    f.dynamic_range
        .as_deref()
        .is_some_and(|d| !d.eq_ignore_ascii_case("sdr"))
}

fn is_aac(f: &Format) -> bool {
    f.ext == "m4a" || f.acodec.as_deref().is_some_and(|a| codec_name(a) == "AAC")
}

/// The best audio-only stream to merge with a video-only one: AAC/M4A first (MP4-friendly),
/// then the highest bitrate.
fn best_audio(formats: &[Format]) -> Option<&Format> {
    formats
        .iter()
        .filter(|f| f.has_audio && !f.has_video)
        .max_by_key(|f| {
            (
                is_aac(f),
                f.abr.or(f.tbr).unwrap_or(0),
                f.filesize.unwrap_or(0),
            )
        })
}

/// The menu's video options, tallest first, one per height class the video really has.
pub fn video_options(formats: &[Format]) -> Vec<VideoOption> {
    let audio = best_audio(formats);
    let mut out = vec![];
    for &h in POLICY_HEIGHTS.iter().rev() {
        let floor = class_floor(h);
        let pick = formats
            .iter()
            .filter(|f| f.has_video)
            .filter(|f| {
                f.class_height()
                    .is_some_and(|x| x > floor && x <= h && x <= MAX_HEIGHT)
            })
            .max_by_key(|f| {
                (
                    f.fps.unwrap_or(0),
                    !is_hdr(f),
                    3 - codec_rank(f.vcodec.as_deref()),
                    f.tbr.unwrap_or(0),
                    f.filesize.unwrap_or(0),
                )
            });
        let Some(v) = pick else { continue };
        let (format, has_audio, size) = if v.has_audio {
            (v.format_id.clone(), true, v.filesize)
        } else if let Some(a) = audio {
            (
                format!("{}+{}", v.format_id, a.format_id),
                true,
                v.filesize
                    .zip(a.filesize)
                    .and_then(|(x, y)| x.checked_add(y)),
            )
        } else {
            (v.format_id.clone(), false, v.filesize)
        };
        out.push(VideoOption {
            height: h,
            real_height: v.class_height().unwrap_or(h),
            format,
            fps: v.fps,
            codec: v.vcodec.as_deref().map(codec_name).unwrap_or_default(),
            hdr: is_hdr(v).then(|| v.dynamic_range.clone().unwrap_or_default()),
            audio: has_audio,
            size,
        });
    }
    out
}

/// The menu's audio options: the original AAC stream (when the video has one) and MP3
/// (converted, when the video has any audio).
pub fn audio_options(formats: &[Format]) -> Vec<AudioOption> {
    let mut out = vec![];
    let aac = formats
        .iter()
        .filter(|f| f.has_audio && !f.has_video && f.ext == "m4a")
        .max_by_key(|f| (f.abr.or(f.tbr).unwrap_or(0), f.filesize.unwrap_or(0)));
    if let Some(a) = aac {
        out.push(AudioOption {
            quality: "m4a",
            original: true,
            format: Some(a.format_id.clone()),
            codec: "AAC",
            kbps: a.abr.or(a.tbr),
            size: a.filesize,
        });
    }
    if formats.iter().any(|f| f.has_audio) {
        out.push(AudioOption {
            quality: "audio",
            original: false,
            format: None,
            codec: "MP3",
            kbps: best_audio(formats).and_then(|a| a.abr.or(a.tbr)),
            size: None,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(id: &str, h: u32, fps: u32, vc: &str, dr: &str, tbr: u32, size: Option<u64>) -> Format {
        Format {
            format_id: id.into(),
            ext: if vc.starts_with("avc1") {
                "mp4"
            } else {
                "webm"
            }
            .into(),
            height: Some(h),
            width: None,
            has_video: true,
            has_audio: false,
            filesize: size,
            fps: Some(fps),
            vcodec: Some(vc.into()),
            acodec: None,
            dynamic_range: Some(dr.into()),
            abr: None,
            tbr: Some(tbr),
        }
    }
    fn a(id: &str, ext: &str, ac: &str, abr: u32, size: Option<u64>) -> Format {
        Format {
            format_id: id.into(),
            ext: ext.into(),
            height: None,
            width: None,
            has_video: false,
            has_audio: true,
            filesize: size,
            fps: None,
            vcodec: None,
            acodec: Some(ac.into()),
            dynamic_range: None,
            abr: Some(abr),
            tbr: Some(abr),
        }
    }

    /// A YouTube-like set: several codecs per height, 60 fps above 720p, HDR at 4K, 8K in AV1.
    fn sample() -> Vec<Format> {
        vec![
            v("160", 144, 30, "avc1.4d400c", "SDR", 80, Some(1_000_000)),
            v("133", 240, 30, "avc1.4d4015", "SDR", 150, Some(2_000_000)),
            v("134", 360, 30, "avc1.4d401e", "SDR", 300, Some(4_000_000)),
            v("243", 360, 30, "vp09.00.21.08", "SDR", 280, Some(3_500_000)),
            v("135", 480, 30, "avc1.4d401f", "SDR", 600, Some(8_000_000)),
            v("136", 720, 30, "avc1.4d401f", "SDR", 1200, Some(15_000_000)),
            v("298", 720, 60, "avc1.4d4020", "SDR", 2000, Some(25_000_000)),
            v(
                "302",
                720,
                60,
                "vp09.00.40.08",
                "SDR",
                1800,
                Some(22_000_000),
            ),
            v(
                "299",
                1080,
                60,
                "avc1.64002a",
                "SDR",
                4000,
                Some(50_000_000),
            ),
            v(
                "303",
                1080,
                60,
                "vp09.00.41.08",
                "SDR",
                3500,
                Some(45_000_000),
            ),
            v("308", 1440, 60, "vp09.00.50.08", "SDR", 9000, None),
            v(
                "337",
                2160,
                60,
                "vp09.02.51.10",
                "HDR10",
                20000,
                Some(300_000_000),
            ),
            v(
                "402",
                4320,
                60,
                "av01.0.17M.08",
                "SDR",
                40000,
                Some(600_000_000),
            ),
            a("140", "m4a", "mp4a.40.2", 129, Some(3_000_000)),
            a("251", "webm", "opus", 135, Some(3_100_000)),
            // a muxed low format, and a storyboard-like entry with neither stream
            Format {
                has_audio: true,
                ..v("18", 360, 30, "avc1.42001E", "SDR", 500, Some(9_000_000))
            },
        ]
    }

    #[test]
    fn video_options_are_exact_format_pairs_tallest_first_with_real_details() {
        let o = video_options(&sample());
        let hs: Vec<u32> = o.iter().map(|x| x.height).collect();
        assert_eq!(hs, [4320, 2160, 1440, 1080, 720, 480, 360, 240, 144]);
        let by = |h: u32| o.iter().find(|x| x.height == h).unwrap();
        // 8K: the only format, AV1, merged with the AAC audio; sizes added up
        let k8 = by(4320);
        assert_eq!(k8.format, "402+140");
        assert_eq!(
            (k8.codec.as_str(), k8.fps, k8.audio),
            ("AV1", Some(60), true)
        );
        assert_eq!(k8.size, Some(603_000_000));
        // 4K exists only in HDR: offered, and it says so
        assert_eq!(by(2160).hdr.as_deref(), Some("HDR10"));
        // 1440p: the tool gives no size: unknown, never guessed
        assert_eq!(by(1440).size, None);
        // 1080p60: H.264 before VP9 at the same frame rate (MP4-friendly)
        assert_eq!(by(1080).format, "299+140");
        assert_eq!(by(1080).codec, "H.264");
        // 720p: 60 fps before 30 fps, then H.264 before VP9
        assert_eq!(
            (by(720).format.as_str(), by(720).fps),
            ("298+140", Some(60))
        );
        // a format with its own audio may win its class, and is then used alone
        assert_eq!(by(360).format, "18");
        assert!(by(360).audio);
        assert!(o.iter().all(|x| x.hdr.is_none() || x.height == 2160));
    }

    #[test]
    fn sdr_wins_over_hdr_and_no_audio_is_said_not_hidden() {
        let f = vec![
            v("1", 2160, 60, "vp09.02.51.10", "HDR10", 20000, Some(1)),
            v("2", 2160, 60, "vp09.00.51.08", "SDR", 15000, Some(2)),
            v("3", 1080, 30, "avc1", "SDR", 1, None),
        ];
        let o = video_options(&f);
        assert_eq!(o[0].format, "2");
        assert!(o[0].hdr.is_none());
        assert!(
            o.iter().all(|x| !x.audio),
            "no audio stream at all: never 'with audio'"
        );
        assert_eq!(o[0].size, Some(2), "video only: its own size");
        assert!(audio_options(&f).is_empty());
    }

    #[test]
    fn classes_crops_and_heights_outside_the_policy() {
        let f = vec![
            v("crop", 1012, 30, "avc1", "SDR", 1, None),
            v("tiny", 90, 30, "avc1", "SDR", 1, None),
            v("16k", 8640, 30, "av01", "SDR", 1, None),
            Format {
                height: None,
                ..v("noh", 0, 30, "avc1", "SDR", 1, None)
            },
        ];
        let o = video_options(&f);
        assert_eq!(
            o.iter()
                .map(|x| (x.height, x.real_height))
                .collect::<Vec<_>>(),
            [(1080, 1012), (144, 90)]
        );
        assert!(video_options(&[]).is_empty());
    }

    #[test]
    fn audio_options_original_aac_and_converted_mp3() {
        let o = audio_options(&sample());
        assert_eq!(o.len(), 2);
        assert_eq!(
            (
                o[0].quality,
                o[0].original,
                o[0].format.as_deref(),
                o[0].codec
            ),
            ("m4a", true, Some("140"), "AAC")
        );
        assert_eq!((o[0].kbps, o[0].size), (Some(129), Some(3_000_000)));
        assert_eq!(
            (o[1].quality, o[1].original, o[1].codec),
            ("audio", false, "MP3")
        );
        assert_eq!(
            o[1].size, None,
            "the converted size is not known before converting"
        );
        // Opus only: no original AAC line, MP3 still offered
        let opus = vec![a("251", "webm", "opus", 135, None)];
        let o = audio_options(&opus);
        assert_eq!(o.iter().map(|x| x.quality).collect::<Vec<_>>(), ["audio"]);
    }

    /// Owner's feedback 2026-11-23 ("qualities higher than the video's"): a vertical video is
    /// named by its short side, so a 1080x1920 Short is 1080p, never 4K.
    #[test]
    fn a_vertical_video_is_named_by_its_short_side() {
        let vert = |id: &str, w: u32, h: u32| Format {
            width: Some(w),
            ..v(id, h, 30, "avc1", "SDR", w, None)
        };
        let f = vec![
            vert("617", 1080, 1920),
            vert("616", 720, 1280),
            vert("605", 360, 640),
            a("140", "m4a", "mp4a.40.2", 129, None),
        ];
        let o = video_options(&f);
        assert_eq!(
            o.iter()
                .map(|x| (x.height, x.real_height))
                .collect::<Vec<_>>(),
            [(1080, 1080), (720, 720), (360, 360)]
        );
        assert_eq!(o[0].format, "617+140");
        // a horizontal video keeps its height; an ultra-wide one is named by its short side
        let wide = Format {
            width: Some(2560),
            ..v("w", 1080, 30, "avc1", "SDR", 1, None)
        };
        assert_eq!(wide.class_height(), Some(1080));
        assert_eq!(
            v("n", 720, 30, "avc1", "SDR", 1, None).class_height(),
            Some(720)
        );
    }

    #[test]
    fn codec_names() {
        for (c, n) in [
            ("avc1.640028", "H.264"),
            ("vp09.00.51.08", "VP9"),
            ("vp9", "VP9"),
            ("av01.0.08M.08", "AV1"),
            ("hev1.1.6.L93.B0", "H.265"),
            ("mp4a.40.2", "AAC"),
            ("opus", "Opus"),
            ("theora.x", "THEORA"),
            ("", ""),
        ] {
            assert_eq!(codec_name(c), n, "{c}");
        }
    }
}
