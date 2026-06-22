use std::path::{Path, PathBuf};
use crate::error::{Error, Result};

const MAC_CANDIDATES: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
];
const LINUX_CANDIDATES: &[&str] = &[
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
];

pub fn discover_chrome(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return if p.exists() {
            Ok(p.to_path_buf())
        } else {
            Err(Error::ChromeNotFound)
        };
    }
    let candidates = if cfg!(target_os = "macos") {
        MAC_CANDIDATES
    } else {
        LINUX_CANDIDATES
    };
    for c in candidates {
        let p = Path::new(c);
        if p.exists() {
            return Ok(p.to_path_buf());
        }
    }
    Err(Error::ChromeNotFound)
}

pub fn prepare_profile(dir: &Path) -> Result<()> {
    if dir.join("SingletonLock").exists() {
        return Err(Error::ProfileLocked(dir.to_path_buf()));
    }
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn explicit_chrome_must_exist() {
        let p = Path::new("/definitely/not/here/chrome");
        assert!(discover_chrome(Some(p)).is_err());
    }

    #[test]
    fn prepare_creates_profile_dir() {
        let tmp = std::env::temp_dir().join(format!("webcat-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        prepare_profile(&tmp).unwrap();
        assert!(tmp.is_dir());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
