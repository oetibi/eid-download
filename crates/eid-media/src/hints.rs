//! Turning a tool error into a hint the user can act on (PLAN P4; ARCHITECTURE section 6).
//! Matching is on the wording of yt-dlp's public error messages; the text shown to the user
//! for each hint lives in the CLI/UI translation files, not here.

use serde::Serialize;

/// What most likely went wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Hint {
    /// DRM-protected media: refused, never worked around.
    Drm,
    /// The site asks to prove the user is not a bot.
    BotCheck,
    /// Sign-in, age or membership required: not supported in v1 (no cookies).
    SignInRequired,
    /// Private, removed or otherwise unavailable item.
    Unavailable,
    /// HTTP 429 / rate limit.
    RateLimited,
    /// Not available in the user's country.
    Geo,
    /// yt-dlp found no JavaScript runtime.
    MissingJsRuntime,
    /// The site is not supported by yt-dlp.
    Unsupported,
}

/// Classify the (redacted) error text of a failed run. Order matters: the most specific and
/// the most important (DRM) first.
pub fn classify(text: &str) -> Option<Hint> {
    let t = text.to_lowercase();
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
    if has(&["no supported javascript runtime", "javascript runtime"]) {
        return Some(Hint::MissingJsRuntime);
    }
    if has(&[
        "http error 429",
        "too many requests",
        "rate-limit",
        "rate limit",
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
        "use --cookies",
        "--username",
    ]) {
        return Some(Hint::SignInRequired);
    }
    if has(&["unsupported url"]) {
        return Some(Hint::Unsupported);
    }
    if has(&[
        "private video",
        "video unavailable",
        "has been removed",
        "this video is not available",
        "does not exist",
    ]) {
        return Some(Hint::Unavailable);
    }
    None
}
