//! Turning a tool error into a hint the user can act on (PLAN P4; ARCHITECTURE section 6).
//! Matching is on the wording of yt-dlp's public error messages; the text shown to the user
//! for each hint lives in the CLI/UI translation files, not here.
//!
//! Rules (P4 review M2, owner decision 2026-11-16):
//! * only `ERROR:` lines decide, the last one first: a warning printed on the way (yt-dlp
//!   prints many) can never turn a failure into "do not retry";
//! * warnings may only add the informational [`Hint::MissingJsRuntime`], never a hint that
//!   stops automatic retries;
//! * only an **explicit** access restriction stops automatic retries
//!   ([`Hint::blocks_auto_retry`]): DRM, private, removed by the uploader or with a terminated
//!   account, sign-in / age / membership / premium, and a country restriction. A bare "Video
//!   unavailable" or "This content isn't available" is not explicit (YouTube also answers so
//!   while it is limiting requests): it is a retryable [`Hint::Unavailable`].

use serde::Serialize;

/// What most likely went wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Hint {
    /// DRM-protected media: refused, never worked around.
    Drm,
    /// The site asks to prove the user is not a bot.
    BotCheck,
    /// Sign-in, age, membership or premium required: not supported in v1 (no cookies).
    SignInRequired,
    /// The item is private (the site says so explicitly).
    Private,
    /// The item was removed (by the uploader, or its account was terminated).
    Removed,
    /// Unavailable without an explicit reason: may be temporary, so it is retried.
    Unavailable,
    /// HTTP 429 / rate limit.
    RateLimited,
    /// Not available in the user's country.
    Geo,
    /// yt-dlp found no JavaScript runtime.
    MissingJsRuntime,
    /// The site is not supported by yt-dlp.
    Unsupported,
    /// v0.2: the chosen quality is not available and the user chose "skip" for that case
    /// (or nothing at all matches). Retrying the same choice cannot help.
    QualityMissing,
}

impl Hint {
    /// An explicit access restriction (or DRM): retrying automatically cannot help and would
    /// only knock on the same door. The user may still retry by hand when things change; no
    /// restriction is ever worked around.
    pub fn blocks_auto_retry(self) -> bool {
        matches!(
            self,
            Hint::Drm
                | Hint::Private
                | Hint::Removed
                | Hint::SignInRequired
                | Hint::Geo
                | Hint::QualityMissing
        )
    }
}

/// Classify the (redacted) error output of a failed run: the `ERROR:` lines, last first; see
/// the module rules.
pub fn classify(text: &str) -> Option<Hint> {
    let errors: Vec<&str> = text
        .lines()
        .filter(|l| l.trim_start().starts_with("ERROR:"))
        .collect();
    let no_runtime = text
        .to_lowercase()
        .contains("no supported javascript runtime");
    if let Some(h) = errors.iter().rev().find_map(|l| classify_error_line(l)) {
        if !(h == Hint::QualityMissing && no_runtime) {
            return Some(h);
        }
    }
    // informational only: never stops a retry
    if text
        .to_lowercase()
        .contains("no supported javascript runtime")
    {
        return Some(Hint::MissingJsRuntime);
    }
    None
}

/// One `ERROR:` line. Order matters: the most important (DRM) and the most specific first; the
/// generic "unavailable" last.
fn classify_error_line(line: &str) -> Option<Hint> {
    let t = line.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| t.contains(n));
    if has(&["drm protected", "drm-protected", "[drm]"]) {
        return Some(Hint::Drm);
    }
    if has(&[
        "not a bot",
        "confirm you're not a robot",
        "confirm you’re not a bot",
    ]) {
        return Some(Hint::BotCheck);
    }
    // before the generic "unavailable": "This content isn't available, try again later"
    if has(&[
        "http error 429",
        "too many requests",
        "rate-limit",
        "rate limit",
        "try again later",
    ]) {
        return Some(Hint::RateLimited);
    }
    if has(&[
        "available in your country",
        "geo restricted",
        "geo-restricted",
        "not available from your location",
    ]) {
        return Some(Hint::Geo);
    }
    if has(&[
        "sign in to confirm your age",
        "login required",
        "requires authentication",
        "members-only",
        "join this channel",
        "premium members",
        "requires payment",
    ]) {
        return Some(Hint::SignInRequired);
    }
    if has(&["private video"]) {
        return Some(Hint::Private);
    }
    if has(&[
        "has been removed",
        "account associated with this video has been terminated",
        "no longer available because",
    ]) {
        return Some(Hint::Removed);
    }
    if has(&["unsupported url"]) {
        return Some(Hint::Unsupported);
    }
    // yt-dlp's own text when no format matches the selector. Not when the JavaScript runtime
    // is missing (then formats are missing for that reason): see `classify`.
    if has(&["requested format is not available"]) {
        return Some(Hint::QualityMissing);
    }
    if has(&["no supported javascript runtime"]) {
        return Some(Hint::MissingJsRuntime);
    }
    if has(&[
        "video unavailable",
        "this video is not available",
        "content isn't available",
        "content isn’t available",
        "does not exist",
    ]) {
        return Some(Hint::Unavailable);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_error_lines_decide_and_warnings_never_block_a_retry() {
        // a warning matching a "do not retry" hint, then a temporary error: retryable
        let t = "WARNING: [youtube] x: Private video in the related list\n\
                 ERROR: [youtube] x: HTTP Error 429: Too Many Requests";
        assert_eq!(classify(t), Some(Hint::RateLimited));
        // a warning alone never blocks
        assert_eq!(
            classify("WARNING: This video is DRM protected (one format)"),
            None
        );
        assert!(
            !classify("WARNING: Private video\nERROR: boom").is_some_and(Hint::blocks_auto_retry)
        );
        // the last error wins
        let t = "ERROR: [youtube] x: Video unavailable\nERROR: [youtube] x: Private video";
        assert_eq!(classify(t), Some(Hint::Private));
        // the missing runtime is only informational, from a warning
        assert_eq!(
            classify("WARNING: No supported JavaScript runtime could be found.\nERROR: Requested format is not available"),
            Some(Hint::MissingJsRuntime)
        );
    }

    #[test]
    fn explicit_restrictions_block_auto_retry_and_vague_ones_do_not() {
        for (line, hint) in [
            ("ERROR: [youtube] x: This video is DRM protected", Hint::Drm),
            ("ERROR: [youtube] x: Private video. Sign in if you've been granted access to this video", Hint::Private),
            ("ERROR: [youtube] x: Video unavailable. This video has been removed by the uploader", Hint::Removed),
            ("ERROR: [youtube] x: Video unavailable. This video is no longer available because the YouTube account associated with this video has been terminated.", Hint::Removed),
            ("ERROR: [youtube] x: Sign in to confirm your age. This video may be inappropriate for some users.", Hint::SignInRequired),
            ("ERROR: [youtube] x: Join this channel to get access to members-only content like this video", Hint::SignInRequired),
            ("ERROR: [youtube] x: The uploader has not made this video available in your country", Hint::Geo),
        ] {
            assert_eq!(classify(line), Some(hint), "{line}");
            assert!(hint.blocks_auto_retry(), "{line}");
        }
        for (line, hint) in [
            // YouTube answers so while it limits requests
            ("ERROR: [youtube] x: Video unavailable. This content isn't available, try again later.", Some(Hint::RateLimited)),
            ("ERROR: [youtube] x: Video unavailable. This content isn’t available.", Some(Hint::Unavailable)),
            ("ERROR: [youtube] x: Video unavailable", Some(Hint::Unavailable)),
            ("ERROR: [youtube] x: Sign in to confirm you're not a bot. Use --cookies-from-browser or --cookies for the authentication.", Some(Hint::BotCheck)),
            ("ERROR: [generic] Unsupported URL: https://example.org/", Some(Hint::Unsupported)),
            ("ERROR: unable to download video data: <urlopen error timed out>", None),
        ] {
            assert_eq!(classify(line), hint, "{line}");
            assert!(!hint.is_some_and(Hint::blocks_auto_retry), "{line}");
        }
        // v0.2 "skip": the same choice cannot succeed by retrying
        let q = "ERROR: [youtube] abc: Requested format is not available. Use --list-formats for a list of available formats";
        assert_eq!(classify(q), Some(Hint::QualityMissing));
        assert!(Hint::QualityMissing.blocks_auto_retry());
        // ... unless the formats are missing because yt-dlp had no JavaScript runtime
        let js = "WARNING: No supported JavaScript runtime could be found.\nERROR: Requested format is not available";
        assert_eq!(classify(js), Some(Hint::MissingJsRuntime));
    }
}
