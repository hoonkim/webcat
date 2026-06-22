use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Constructed in app.rs when detect_kitty_graphics returns false.
    // (Integration test compiles this file standalone and won't see app.rs usage.)
    #[allow(dead_code)]
    #[error("terminal is not Kitty-graphics capable: {0}")]
    UnsupportedTerminal(String),

    #[error("chrome binary not found; set $WEBCAT_CHROME or install Google Chrome")]
    ChromeNotFound,

    #[error("profile is locked by another webcat/chrome instance: {0}")]
    ProfileLocked(PathBuf),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
