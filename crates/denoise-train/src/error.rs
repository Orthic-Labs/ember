//! One error type for the training tooling. Nothing here panics: bad input is an error the operator can act on.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("io: {0}")]
    Fs(#[from] rightkit_fs::FsError),
    #[error("candle: {0}")]
    Candle(#[from] candle_core::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("model: {0}")]
    Model(#[from] lightcraft_nafnet::Error),
    #[error("raw decode of {path}: {why}")]
    Raw { path: String, why: String },
    #[error("data: {0}")]
    Data(String),
    #[error("config: {0}")]
    Config(String),
    #[error("split: {0}")]
    Split(String),
    #[error("training diverged: {0}")]
    Diverged(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn data<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::Data(why.into()))
}

pub(crate) fn config<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::Config(why.into()))
}
