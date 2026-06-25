use std::path::PathBuf;
use crate::cli::Cli;
use crate::error::Result;

// Fields are read in app.rs; the integration test uses a struct literal and may
// not read all fields, triggering dead_code in that compilation unit.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Config {
    pub profile_dir: PathBuf,
    pub chrome: Option<PathBuf>,
    pub log_path: PathBuf,
    pub quality: u8,
    /// Page zoom factor. >1 lays the page out at a smaller CSS viewport so text
    /// and content render larger (the capture still fills the terminal grid).
    pub zoom: f64,
    pub start_url: String,
}

impl Config {
    pub fn resolve(cli: Cli) -> Result<Config> {
        let profile_dir = cli.profile_dir.unwrap_or_else(default_profile_dir);
        let log_path = default_log_path();
        Ok(Config {
            profile_dir,
            chrome: cli.chrome,
            log_path,
            quality: cli.quality.clamp(1, 100),
            zoom: match cli.zoom {
                Some(z) if z > 0.0 => z.clamp(0.5, 4.0),
                _ => default_zoom(),
            },
            start_url: cli.url.unwrap_or_else(|| "about:blank".to_string()),
        })
    }
}

/// Default zoom. The terminal reports its size in *device* pixels, so on a
/// HiDPI/Retina display the page would otherwise lay out at a huge CSS width and
/// render tiny — like opening a non-Retina Chrome window twice the size. We
/// default zoom to the display's backing scale factor (2.0 on Retina), which
/// makes the page lay out at the logical size — the natural size you'd get from
/// a Chrome window of the terminal's dimensions (matching awrit). Override with
/// --zoom for a different size or on an external non-HiDPI monitor.
fn default_zoom() -> f64 {
    display_scale().clamp(1.0, 3.0)
}

/// The main display's backing scale factor (device pixels ÷ logical points).
/// 2.0 on Retina, 1.0 on standard displays. Falls back to 1.0 off macOS or if
/// the query fails.
fn display_scale() -> f64 {
    #[cfg(target_os = "macos")]
    {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGMainDisplayID() -> u32;
            fn CGDisplayCopyDisplayMode(display: u32) -> *mut std::ffi::c_void;
            fn CGDisplayModeGetWidth(mode: *mut std::ffi::c_void) -> usize;
            fn CGDisplayModeGetPixelWidth(mode: *mut std::ffi::c_void) -> usize;
            fn CGDisplayModeRelease(mode: *mut std::ffi::c_void);
        }
        unsafe {
            let mode = CGDisplayCopyDisplayMode(CGMainDisplayID());
            if mode.is_null() {
                return 1.0;
            }
            let points = CGDisplayModeGetWidth(mode);
            let pixels = CGDisplayModeGetPixelWidth(mode);
            CGDisplayModeRelease(mode);
            if points == 0 {
                return 1.0;
            }
            (pixels as f64 / points as f64).max(1.0)
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        1.0
    }
}

fn default_profile_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("webcat")
        .join("profile")
}

fn default_log_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("webcat")
        .join("log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;

    fn base_cli() -> Cli {
        Cli { url: None, profile_dir: None, chrome: None, quality: 70, zoom: Some(1.0) }
    }

    #[test]
    fn defaults_url_to_about_blank() {
        let cfg = Config::resolve(base_cli()).unwrap();
        assert_eq!(cfg.start_url, "about:blank");
    }

    #[test]
    fn explicit_paths_win() {
        let mut cli = base_cli();
        cli.url = Some("https://example.com".into());
        cli.profile_dir = Some("/tmp/p".into());
        let cfg = Config::resolve(cli).unwrap();
        assert_eq!(cfg.start_url, "https://example.com");
        assert_eq!(cfg.profile_dir, std::path::PathBuf::from("/tmp/p"));
    }

    #[test]
    fn clamps_quality() {
        let mut cli = base_cli();
        cli.quality = 200;
        let cfg = Config::resolve(cli).unwrap();
        assert_eq!(cfg.quality, 100);
    }

    #[test]
    fn zoom_defaults_and_clamps() {
        let mut cli = base_cli();
        // Unset → the auto-detected display scale, clamped to a sane range.
        cli.zoom = None;
        let auto = Config::resolve(cli.clone()).unwrap().zoom;
        assert!((1.0..=3.0).contains(&auto), "auto zoom {auto} out of range");
        // Explicit values win and are clamped to 0.5–4.0.
        cli.zoom = Some(10.0);
        assert_eq!(Config::resolve(cli.clone()).unwrap().zoom, 4.0);
        cli.zoom = Some(0.1);
        assert_eq!(Config::resolve(cli).unwrap().zoom, 0.5);
    }
}
