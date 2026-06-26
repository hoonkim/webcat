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

/// If a *live* process currently holds the default profile (Chrome's per-profile
/// `SingletonLock`), return its pid. Returns `None` when the profile is free or
/// the lock is merely stale (the owner died) — that case is handled silently by
/// `resolve_profile`, which clears the stale lock and reuses the profile. The
/// live case is the one worth asking the user about (kill it, or go anonymous).
// Called from app.rs; the integration test compiles this module standalone
// (without app.rs) so it appears unused there.
#[allow(dead_code)]
pub fn detect_conflict(default: &Path) -> Option<i32> {
    match singleton_lock_pid(default) {
        Some(pid) if pid_alive(pid) => Some(pid),
        _ => None,
    }
}

/// Terminate the process holding the profile lock so the default profile can be
/// reused. Tries a graceful SIGTERM first, then escalates to SIGKILL if it does
/// not exit within ~2s. Returns true once the process is gone. The now-stale
/// `SingletonLock` is left for `resolve_profile` to clear on the next launch.
#[allow(dead_code)]
pub fn kill_profile_holder(pid: i32) -> bool {
    if !pid_alive(pid) {
        return true;
    }
    unsafe { libc::kill(pid, libc::SIGTERM) };
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !pid_alive(pid) {
            return true;
        }
    }
    unsafe { libc::kill(pid, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(200));
    !pid_alive(pid)
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

    fn lock_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("webcat-conflict-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_lock(dir: &Path, pid: i32) {
        use std::os::unix::fs::symlink;
        let link = dir.join("SingletonLock");
        let _ = std::fs::remove_file(&link);
        // Chrome records the lock target as "<hostname>-<pid>".
        symlink(format!("somehost-{pid}"), &link).unwrap();
    }

    #[test]
    fn no_lock_is_not_a_conflict() {
        let dir = lock_dir("none");
        assert_eq!(detect_conflict(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_lock_is_not_a_conflict() {
        let dir = lock_dir("stale");
        // A pid that is (almost certainly) not alive — a stale lock from a dead
        // owner is cleared silently by resolve_profile, not surfaced as a conflict.
        write_lock(&dir, 999_999);
        assert_eq!(detect_conflict(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn live_lock_is_a_conflict() {
        let dir = lock_dir("live");
        // Our own pid is alive, so this looks like a live owner.
        let me = std::process::id() as i32;
        write_lock(&dir, me);
        assert_eq!(detect_conflict(&dir), Some(me));
        std::fs::remove_dir_all(&dir).unwrap();
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
