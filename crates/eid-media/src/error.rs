use crate::hints::Hint;

/// Errors of the media layer. Every text in here is already redacted.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("unsupported or unsafe address: {0}")]
    BadUrl(String),
    #[error("unsafe tool location: {0}")]
    BadToolPath(String),
    #[error("invalid choice: {0}")]
    BadChoice(String),
    #[error("the tool printed more than {limit} bytes; refused")]
    OutputTooLarge { limit: usize },
    #[error("the tool printed something that is not valid media information: {0}")]
    BadInfo(String),
    #[error("the tool failed ({hint:?}): {message}")]
    ToolFailed {
        hint: Option<Hint>,
        /// Redacted tail of the tool's error output.
        message: String,
    },
    #[error("invalid tool manifest: {0}")]
    BadManifest(String),
    #[error("video features are off: consent was not given (or the consent text changed)")]
    NoConsent,
    #[error("settings file: {0}")]
    Settings(String),
    #[error("the video tools are not installed")]
    NotInstalled,
    #[error("{file}: SHA-256 does not match the pinned value; refused")]
    HashMismatch { file: String },
    #[error("playlist: {0}")]
    Playlist(String),
    #[error("archive: {0}")]
    Archive(String),
    #[error("tool download failed: {0}")]
    Fetch(String),
    #[error("{file} is in use by another program (open for writing or deleting); refused")]
    ToolBusy { file: String },
    #[error("could not run the tool: {0}")]
    Io(#[from] std::io::Error),
}
