//! One error type for the NAFNet model. Nothing here panics: a bad file or contract is an error to act on.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("candle: {0}")]
    Candle(#[from] candle_core::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("data: {0}")]
    Data(String),
    #[error("config: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn config<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::Config(why.into()))
}
