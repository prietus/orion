use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid configuration in {path}: {msg}")]
    Config { path: PathBuf, msg: String },
    #[error("mailbox not found: {0}")]
    MailboxNotFound(String),
    #[error("message not found: {0}")]
    MessageNotFound(u64),
    #[error("could not parse message {0}")]
    Parse(u64),
    #[error("{0}")]
    Unsupported(&'static str),
    #[error("network: {0}")]
    Network(String),
    #[error("authentication: {0}")]
    Auth(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("cache: {0}")]
    Cache(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io { path: path.into(), source }
    }
}
