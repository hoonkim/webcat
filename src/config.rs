use crate::cli::Cli;
use crate::error::Error;
use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileConfig {
    pub quality: Option<u8>,
    pub zoom: Option<f64>,
    pub mcp: Option<FileMcpConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileMcpConfig {
    pub enabled: Option<bool>,
    pub port: Option<u16>,
    pub allow_control: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct McpConfig {
    pub enabled: bool,
    pub port: Option<u16>,
    pub allow_control: bool,
}

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
    pub mcp: McpConfig,
}

impl Config {
    pub fn resolve(cli: Cli, file: Option<FileConfig>) -> Result<Config> {
        let file = file.unwrap_or_default();
        let fmcp = file.mcp.unwrap_or_default();
        let cli_mcp = cli.mcp();
        let cli_mcp_allow_control = cli.mcp_allow_control();
        let profile_dir = cli.profile_dir.unwrap_or_else(default_profile_dir);
        let log_path = default_log_path();
        let quality = cli.quality.or(file.quality).unwrap_or(92).clamp(1, 100);
        let zoom = match cli.zoom {
            Some(z) if z > 0.0 => z.clamp(0.5, 4.0),
            _ => match file.zoom {
                Some(z) if z > 0.0 => z.clamp(0.5, 4.0),
                _ => default_zoom(),
            },
        };
        let mcp = McpConfig {
            enabled: cli_mcp.or(fmcp.enabled).unwrap_or(false),
            port: cli.mcp_port.or(fmcp.port),
            allow_control: cli_mcp_allow_control
                .or(fmcp.allow_control)
                .unwrap_or(false),
        };
        Ok(Config {
            profile_dir,
            chrome: cli.chrome,
            log_path,
            quality,
            zoom,
            start_url: cli.url.unwrap_or_else(|| "about:blank".to_string()),
            mcp,
        })
    }
}

#[allow(dead_code)]
pub fn load_file_config() -> Result<Option<FileConfig>> {
    let path = match dirs::home_dir() {
        Some(h) => h.join(".webcat").join("config.yaml"),
        None => return Ok(None),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::Other(anyhow::anyhow!(
                "reading {}: {e}",
                path.display()
            )))
        }
    };
    let cfg = serde_yml::from_str(&text)
        .map_err(|e| Error::Other(anyhow::anyhow!("parsing {}: {e}", path.display())))?;
    Ok(Some(cfg))
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
        Cli {
            command: None,
            url: None,
            profile_dir: None,
            chrome: None,
            quality: Some(70),
            zoom: Some(1.0),
            mcp_on: false,
            mcp_off: false,
            mcp_port: None,
            mcp_allow_control_on: false,
            mcp_allow_control_off: false,
        }
    }

    #[test]
    fn defaults_url_to_about_blank() {
        let cfg = Config::resolve(base_cli(), None).unwrap();
        assert_eq!(cfg.start_url, "about:blank");
    }

    #[test]
    fn explicit_paths_win() {
        let mut cli = base_cli();
        cli.url = Some("https://example.com".into());
        cli.profile_dir = Some("/tmp/p".into());
        let cfg = Config::resolve(cli, None).unwrap();
        assert_eq!(cfg.start_url, "https://example.com");
        assert_eq!(cfg.profile_dir, std::path::PathBuf::from("/tmp/p"));
    }

    #[test]
    fn clamps_quality() {
        let mut cli = base_cli();
        cli.quality = Some(200);
        let cfg = Config::resolve(cli, None).unwrap();
        assert_eq!(cfg.quality, 100);
    }

    #[test]
    fn zoom_defaults_and_clamps() {
        let mut cli = base_cli();
        // Unset → the auto-detected display scale, clamped to a sane range.
        cli.zoom = None;
        let auto = Config::resolve(cli.clone(), None).unwrap().zoom;
        assert!((1.0..=3.0).contains(&auto), "auto zoom {auto} out of range");
        // Explicit values win and are clamped to 0.5–4.0.
        cli.zoom = Some(10.0);
        assert_eq!(Config::resolve(cli.clone(), None).unwrap().zoom, 4.0);
        cli.zoom = Some(0.1);
        assert_eq!(Config::resolve(cli, None).unwrap().zoom, 0.5);
    }

    fn file_with_mcp() -> FileConfig {
        FileConfig {
            quality: None,
            zoom: None,
            mcp: Some(FileMcpConfig {
                enabled: Some(true),
                port: Some(4470),
                allow_control: Some(true),
            }),
        }
    }

    #[test]
    fn file_enables_mcp_when_cli_silent() {
        let mut cli = base_cli();
        cli.quality = None;
        let cfg = Config::resolve(cli, Some(file_with_mcp())).unwrap();
        assert!(cfg.mcp.enabled);
        assert_eq!(cfg.mcp.port, Some(4470));
        assert!(cfg.mcp.allow_control);
    }

    #[test]
    fn cli_flag_overrides_file_for_mcp_enabled() {
        let mut cli = base_cli();
        cli.mcp_on = true;
        let mut file = file_with_mcp();
        file.mcp.as_mut().unwrap().enabled = Some(false);
        let cfg = Config::resolve(cli, Some(file)).unwrap();
        assert!(cfg.mcp.enabled);
    }

    #[test]
    fn cli_no_mcp_overrides_file_enabled_true() {
        let mut cli = base_cli();
        cli.mcp_off = true;
        let cfg = Config::resolve(cli, Some(file_with_mcp())).unwrap();
        assert!(!cfg.mcp.enabled);
    }

    #[test]
    fn defaults_mcp_off_when_no_file() {
        let mut cli = base_cli();
        cli.quality = None;
        let cfg = Config::resolve(cli, None).unwrap();
        assert!(!cfg.mcp.enabled);
        assert!(!cfg.mcp.allow_control);
        assert_eq!(cfg.mcp.port, None);
    }

    #[test]
    fn file_quality_used_when_cli_default() {
        let mut cli = base_cli();
        cli.quality = None;
        let mut file = file_with_mcp();
        file.quality = Some(50);
        let cfg = Config::resolve(cli, Some(file)).unwrap();
        assert_eq!(cfg.quality, 50);
    }

    #[test]
    fn explicit_cli_default_quality_still_overrides_file() {
        let mut cli = base_cli();
        cli.quality = Some(92);
        let mut file = file_with_mcp();
        file.quality = Some(50);
        let cfg = Config::resolve(cli, Some(file)).unwrap();
        assert_eq!(cfg.quality, 92);
    }
}
