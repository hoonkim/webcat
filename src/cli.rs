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

    /// JPEG screencast quality 1-100. Higher is sharper (less compression blur)
    /// at the cost of bigger frames; 92 is a good default for crisp text.
    #[arg(long, default_value_t = 92)]
    pub quality: u8,

    /// Page zoom factor (clamped 0.5–4.0). Defaults to the display's scale factor
    /// (2.0 on Retina) so sites open at their natural size — like a Chrome window
    /// of the terminal's dimensions. Raise for bigger text, or pass --zoom 1 on a
    /// non-HiDPI external monitor.
    #[arg(long)]
    pub zoom: Option<f64>,
}
