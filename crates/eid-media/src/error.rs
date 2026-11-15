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
    #[error("could not run the tool: {0}")]
    Io(#[from] std::io::Error),
}
