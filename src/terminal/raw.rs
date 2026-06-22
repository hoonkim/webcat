use std::io::Write;
use crate::error::Result;

// Escape sequences for setup/teardown.
// Used by enter() and emit_restore_escapes() (called from app.rs).
// The integration test includes this file standalone and doesn't call enter(),
// so these constants appear unused in that compilation unit.
#[allow(dead_code)]
const ENTER_ALT: &str = "\x1b[?1049h";
const LEAVE_ALT: &str = "\x1b[?1049l";
#[allow(dead_code)]
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";
#[allow(dead_code)]
const KKBD_PUSH: &str = "\x1b[>1u";       // push kitty keyboard flags (disambiguate)
const KKBD_POP: &str = "\x1b[<u";          // pop kitty keyboard flags
#[allow(dead_code)]
const MOUSE_ON: &str = "\x1b[?1003h\x1b[?1006h"; // any-event + SGR
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1003l";

pub struct RestoreGuard {
    pub(crate) active: bool,
}

impl RestoreGuard {
    // Called from app.rs; appears unused to integration test's standalone compile.
    #[allow(dead_code)]
    pub fn enter() -> Result<RestoreGuard> {
        crossterm::terminal::enable_raw_mode()?;
        // Construct the guard before the fallible writes so an early return
        // (e.g. a failed escape write) still triggers Drop -> restore().
        let guard = RestoreGuard { active: true };
        let mut out = std::io::stdout();
        write!(out, "{ENTER_ALT}{HIDE_CURSOR}{KKBD_PUSH}{MOUSE_ON}")?;
        out.flush()?;
        Ok(guard)
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> RestoreGuard {
        RestoreGuard { active: true }
    }

    pub fn restore(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        emit_restore_escapes();
    }
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Best-effort terminal restoration on panic and on SIGINT/SIGTERM.
// Called from app.rs; appears unused to integration test's standalone compile.
#[allow(dead_code)]
pub fn install_panic_and_signal_hooks() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        emit_restore_escapes();
        prev(info);
    }));

    // Spawn a blocking thread that waits for SIGINT/SIGTERM and restores.
    std::thread::spawn(|| {
        let mut signals = match signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
        ]) {
            Ok(s) => s,
            Err(_) => return,
        };
        if signals.forever().next().is_some() {
            emit_restore_escapes();
            std::process::exit(130);
        }
    });
}

fn emit_restore_escapes() {
    let mut out = std::io::stdout();
    let _ = write!(out, "{MOUSE_OFF}{KKBD_POP}{SHOW_CURSOR}{LEAVE_ALT}");
    let _ = out.flush();
    let _ = crossterm::terminal::disable_raw_mode();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_is_idempotent_on_flag() {
        // Construct without touching the real tty by using the test-only ctor.
        let mut g = RestoreGuard::for_test();
        assert!(g.active);
        g.restore();
        assert!(!g.active);
        g.restore(); // second call is a no-op, must not panic
        assert!(!g.active);
    }
}
