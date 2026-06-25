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

/// Pick the profile dir to actually use. Chrome allows only one instance per
/// profile (enforced by a `SingletonLock`), so running webcat in several windows
/// against one shared profile makes every instance after the first hang and time
/// out (the black screen). If the default profile is held by a *live* Chrome,
/// fall back to a private per-instance temp profile so this window works too. If
/// the lock is stale (the owning process died — e.g. webcat was killed), clear
/// it so the default profile can be reused.
pub fn resolve_profile(default: &Path) -> PathBuf {
    match singleton_lock_pid(default) {
        Some(pid) if pid_alive(pid) => {
            std::env::temp_dir().join(format!("webcat-profile-{}", std::process::id()))
        }
        Some(_) => {
            clear_singleton_files(default);
            default.to_path_buf()
        }
        None => default.to_path_buf(),
    }
}

/// The pid recorded in the profile's `SingletonLock` symlink (`host-pid`), if any.
fn singleton_lock_pid(dir: &Path) -> Option<i32> {
    let target = std::fs::read_link(dir.join("SingletonLock")).ok()?;
    target.to_str()?.rsplit('-').next()?.parse::<i32>().ok()
}

fn pid_alive(pid: i32) -> bool {
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

fn clear_singleton_files(dir: &Path) {
    for f in ["SingletonLock", "SingletonCookie", "SingletonSocket"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
}

pub fn prepare_profile(dir: &Path) -> Result<()> {
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
