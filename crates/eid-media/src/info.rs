//! Bounded parsing of yt-dlp `-J` output (`--dump-single-json`).
//!
//! The document comes from the network through the tool, so: the captured stdout is capped
//! by the supervisor (a cut document is refused, never parsed), only the fields we use are
//! read, every list and string is bounded, and nothing here is trusted as a path.

use crate::run::{MAX_ENTRIES, MAX_FORMATS, MAX_TEXT};
use crate::MediaError;
use eid_platform::process::Captured;
use serde::Deserialize;

/// What the user chooses from (single video).
#[derive(Clone, Debug, PartialEq)]
pub struct MediaInfo {
    pub id: String,
    pub title: String,
    pub uploader: Option<String>,
    pub duration_secs: Option<u64>,
    pub is_live: bool,
    pub formats: Vec<Format>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Format {
    pub format_id: String,
    pub ext: String,
    pub height: Option<u32>,
    pub has_video: bool,
    pub has_audio: bool,
    pub filesize: Option<u64>,
}

/// A flat playlist (`--flat-playlist -J`).
#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistInfo {
    pub id: String,
    pub title: String,
    pub uploader: Option<String>,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub title: String,
    /// Page URL of the item (validated again before it is ever passed to the tool).
    pub url: Option<String>,
    /// yt-dlp's availability field (`public`, `private`, `needs_auth`, ...), if present.
    pub availability: Option<String>,
    pub duration_secs: Option<u64>,
}

#[derive(Deserialize)]
struct RawFormat {
    format_id: Option<String>,
    ext: Option<String>,
    height: Option<f64>,
    vcodec: Option<String>,
    acodec: Option<String>,
    filesize: Option<f64>,
    filesize_approx: Option<f64>,
}

#[derive(Deserialize)]
struct RawEntry {
    id: Option<String>,
    title: Option<String>,
    url: Option<String>,
    webpage_url: Option<String>,
    availability: Option<String>,
    duration: Option<f64>,
}

#[derive(Deserialize)]
struct RawInfo {
    id: Option<String>,
    title: Option<String>,
    uploader: Option<String>,
    channel: Option<String>,
    duration: Option<f64>,
    is_live: Option<bool>,
    #[serde(rename = "_type")]
    kind: Option<String>,
    formats: Option<Vec<RawFormat>>,
    entries: Option<Vec<Option<RawEntry>>>,
}

fn bounded(s: Option<String>) -> String {
    let mut s = s.unwrap_or_default();
    if s.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

fn non_neg(f: Option<f64>) -> Option<u64> {
    f.filter(|v| v.is_finite() && *v >= 0.0).map(|v| v as u64)
}

fn parse_raw(out: &Captured, limit: usize) -> Result<RawInfo, MediaError> {
    if out.truncated || out.data.len() > limit {
        return Err(MediaError::OutputTooLarge { limit });
    }
    serde_json::from_slice(&out.data).map_err(|e| MediaError::BadInfo(e.to_string()))
}

/// What one `--flat-playlist -J` of an address is: a single video (with its formats) or a
/// flat playlist.
#[derive(Clone, Debug, PartialEq)]
pub enum Probed {
    Video(MediaInfo),
    Playlist(PlaylistInfo),
}

/// Parse the `--flat-playlist -J` output of an address that may be a video or a playlist
/// (`--flat-playlist` only flattens playlists; a video's document is the full `-J`).
pub fn parse_probe(out: &Captured, limit: usize) -> Result<Probed, MediaError> {
    let r = parse_raw(out, limit)?;
    if r.entries.is_some() || r.kind.as_deref() == Some("playlist") {
        playlist_from(r).map(Probed::Playlist)
    } else {
        video_from(r).map(Probed::Video)
    }
}

fn uploader(r: &mut RawInfo) -> Option<String> {
    r.uploader
        .take()
        .or(r.channel.take())
        .map(|u| bounded(Some(u)))
        .filter(|u| !u.is_empty())
}

/// Parse the `-J` output of a single video.
pub fn parse_video(out: &Captured, limit: usize) -> Result<MediaInfo, MediaError> {
    video_from(parse_raw(out, limit)?)
}

fn video_from(mut r: RawInfo) -> Result<MediaInfo, MediaError> {
    let up = uploader(&mut r);
    if r.kind.as_deref() == Some("playlist") {
        return Err(MediaError::BadInfo("this address is a playlist".into()));
    }
    let id = bounded(r.id);
    if id.is_empty() {
        return Err(MediaError::BadInfo("no id".into()));
    }
    let raw_formats = r.formats.unwrap_or_default();
    if raw_formats.len() > MAX_FORMATS {
        return Err(MediaError::BadInfo(format!(
            "more than {MAX_FORMATS} formats"
        )));
    }
    let formats = raw_formats
        .into_iter()
        .filter_map(|f| {
            let format_id = bounded(f.format_id);
            crate::args::check_format_id(&format_id).ok()?;
            let has = |c: &Option<String>| c.as_deref().is_some_and(|c| c != "none");
            Some(Format {
                format_id,
                ext: bounded(f.ext),
                height: non_neg(f.height).and_then(|h| u32::try_from(h).ok()),
                has_video: has(&f.vcodec),
                has_audio: has(&f.acodec),
                filesize: non_neg(f.filesize).or(non_neg(f.filesize_approx)),
            })
        })
        .collect();
    Ok(MediaInfo {
        id,
        uploader: up,
        title: bounded(r.title),
        duration_secs: non_neg(r.duration),
        is_live: r.is_live.unwrap_or(false),
        formats,
    })
}

/// Parse the `--flat-playlist -J` output of a playlist.
pub fn parse_playlist(out: &Captured, limit: usize) -> Result<PlaylistInfo, MediaError> {
    playlist_from(parse_raw(out, limit)?)
}

fn playlist_from(mut r: RawInfo) -> Result<PlaylistInfo, MediaError> {
    let uploader = uploader(&mut r);
    let raw = r
        .entries
        .ok_or_else(|| MediaError::BadInfo("not a playlist".into()))?;
    if raw.len() > MAX_ENTRIES {
        return Err(MediaError::BadInfo(format!(
            "more than {MAX_ENTRIES} playlist items"
        )));
    }
    let entries = raw
        .into_iter()
        .flatten()
        .map(|e| Entry {
            id: bounded(e.id),
            title: bounded(e.title),
            url: e.webpage_url.or(e.url).map(|u| bounded(Some(u))),
            availability: e.availability.map(|a| bounded(Some(a))),
            duration_secs: non_neg(e.duration),
        })
        .collect();
    Ok(PlaylistInfo {
        id: bounded(r.id),
        title: bounded(r.title),
        uploader,
        entries,
    })
}
