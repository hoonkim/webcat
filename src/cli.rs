use std::path::PathBuf;
use clap::Parser;

/// webcat — a terminal browser rendering headless Chromium via the Kitty graphics protocol.
#[derive(Parser, Debug, Clone)]
#[command(name = "webcat", version)]
pub struct Cli {
    /// URL to open on startup (defaults to about:blank).
    pub url: Option<String>,

    /// Dedicated profile directory (overrides $WEBCAT_PROFILE_DIR).
    #[arg(long, env = "WEBCAT_PROFILE_DIR")]
    pub profile_dir: Option<PathBuf>,

    /// Path to the Chrome/Chromium binary (overrides $WEBCAT_CHROME).
    #[arg(long, env = "WEBCAT_CHROME")]
    pub chrome: Option<PathBuf>,

    /// JPEG screencast quality 1-100.
    #[arg(long, default_value_t = 70)]
    pub quality: u8,

    /// Device pixel ratio for the rendered viewport.
    #[arg(long, default_value_t = 1.0)]
    pub dpr: f64,
}
