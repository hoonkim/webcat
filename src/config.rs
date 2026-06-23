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
    pub dpr: f64,
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
            dpr: match cli.dpr {
                Some(d) if d > 0.0 => d,
                _ => default_dpr(),
            },
            start_url: cli.url.unwrap_or_else(|| "about:blank".to_string()),
        })
    }
}

/// Default render scale. kitty maps a graphics image's pixels 1:1 onto the
/// logical terminal cell grid, so a frame sized to the page viewport
/// (cols×rows of cells) fills the window exactly. dpr>1 renders the page at a
/// larger device resolution (sharper on HiDPI) but the placed image then
/// overflows unless the terminal scales it down, so 1.0 is the safe default;
/// override with --dpr to experiment.
fn default_dpr() -> f64 {
    1.0
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
        Cli { url: None, profile_dir: None, chrome: None, quality: 70, dpr: Some(1.0) }
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
}
