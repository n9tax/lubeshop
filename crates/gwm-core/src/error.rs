use thiserror::Error;

/// Errors surfaced by the core library. Kept small and `std::error::Error` so
/// front-ends can wrap them with `anyhow` (TUI) or serialise them (future web).
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("greaseweazle CLI is unavailable: {0}")]
    GwUnavailable(String),

    #[error("could not determine application data/config directories")]
    NoAppDirs,

    #[error("{0}")]
    Tool(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
