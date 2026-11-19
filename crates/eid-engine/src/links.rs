//! v0.2: which links are video PAGES (handled by the video download, never by the file engine).
//! Owner's Windows test (2026-11-19): a YouTube link added as a normal download saved the
//! 1.4 MB page itself as a file named `watch`. Only YouTube's own page shapes are recognized
//! here; any other page is caught when it answers (`Config::refuse_web_pages`).

/// What a video page holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPage {
    Video,
    Playlist,
}

/// `Some(kind)` for a YouTube video or playlist page (`watch?v=`, `youtu.be/ID`, `shorts/`,
/// `live/`, `embed/`, `playlist?list=`, on `www.`, `m.`, `music.` and youtube-nocookie.com).
/// http(s) only; a link with a user name or password is never one.
pub fn video_page(link: &str) -> Option<VideoPage> {
    let u = url::Url::parse(link.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some()
    {
        return None;
    }
    let host = u.host_str()?.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    let has = |k: &str| u.query_pairs().any(|(n, v)| n == k && !v.is_empty());
    let mut seg = u.path_segments()?.filter(|s| !s.is_empty());
    let first = seg.next().unwrap_or("");
    let second = seg.next();
    if host == "youtu.be" {
        return (!first.is_empty() && second.is_none()).then_some(VideoPage::Video);
    }
    let yt = ["youtube.com", "youtube-nocookie.com"].iter().any(|d| {
        host == *d
            || ["www.", "m.", "music."]
                .iter()
                .any(|p| host == format!("{p}{d}"))
    });
    if !yt {
        return None;
    }
    match (first, second) {
        ("watch", None) if has("v") => Some(VideoPage::Video),
        ("playlist", None) if has("list") => Some(VideoPage::Playlist),
        ("shorts" | "live" | "embed" | "v", Some(id)) if !id.is_empty() => Some(VideoPage::Video),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{video_page, VideoPage::*};

    #[test]
    fn youtube_video_and_playlist_pages_are_recognized_and_nothing_else() {
        for (l, want) in [
            ("https://www.youtube.com/watch?v=dQw4w9WgXcQ", Some(Video)),
            ("http://youtube.com/watch?v=abc&list=PL1&t=10s", Some(Video)),
            ("https://m.youtube.com/watch?v=abc", Some(Video)),
            ("https://music.youtube.com/watch?v=abc", Some(Video)),
            ("https://WWW.YOUTUBE.COM./watch?v=abc", Some(Video)),
            ("https://youtu.be/abc?si=x", Some(Video)),
            ("https://www.youtube.com/shorts/abc", Some(Video)),
            ("https://www.youtube.com/live/abc", Some(Video)),
            ("https://www.youtube-nocookie.com/embed/abc", Some(Video)),
            (
                "https://www.youtube.com/playlist?list=PL123",
                Some(Playlist),
            ),
            ("  https://www.youtube.com/watch?v=abc  ", Some(Video)),
            // not a video page
            ("https://www.youtube.com/", None),
            ("https://www.youtube.com/watch", None),
            ("https://www.youtube.com/watch?v=", None),
            ("https://www.youtube.com/playlist", None),
            ("https://www.youtube.com/@channel", None),
            ("https://www.youtube.com/shorts/", None),
            ("https://youtu.be/", None),
            ("https://youtu.be/a/b", None),
            ("https://notyoutube.com/watch?v=abc", None),
            ("https://youtube.com.evil.example/watch?v=abc", None),
            ("https://evil.example/www.youtube.com/watch?v=abc", None),
            ("https://u:p@www.youtube.com/watch?v=abc", None),
            ("ftp://www.youtube.com/watch?v=abc", None),
            ("javascript:alert(1)//youtube.com/watch?v=a", None),
            ("https://example.com/video.mp4", None),
            ("not a link", None),
        ] {
            assert_eq!(video_page(l), want, "{l}");
        }
    }
}
