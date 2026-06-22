# webcat Terminal Browser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Rust terminal browser that drives headless Chromium via CDP and renders its output to a Kitty terminal using the Kitty graphics protocol, with full keyboard/mouse interaction and Korean text input.

**Architecture:** A tokio async app spawns a headless Chromium child process and controls it over CDP (`chromiumoxide`). Chromium emits JPEG screencast frames that are passed through (no decode) to the terminal via the Kitty graphics protocol. Terminal raw input (Kitty keyboard protocol + SGR mouse) is parsed and mapped to CDP input commands. Modules have single responsibilities and clean interfaces so pure-logic units (encoder, parsers, mapper) are unit-tested in isolation and browser/E2E paths are integration-tested.

**Tech Stack:** Rust, tokio, chromiumoxide, crossterm, image, base64, clap, anyhow/thiserror, tracing.

## Global Constraints

- Target terminal: **Kitty** only (graphics + keyboard protocol reference implementation).
- Platform: **local macOS/Linux**; primary dev/test on macOS.
- Rust edition **2021**, minimum toolchain **1.75+**.
- Async runtime: **tokio** (multi-thread flavor).
- Chromium control: **chromiumoxide** only — do not shell out to CDP manually.
- JPEG frames are **passed through without decoding** on the happy path (Kitty `f=100`); decode is a fallback only.
- Dedicated **persistent profile** dir; never touch the user's real Chrome profile.
- Korean/text input uses **`Input.insertText`** with already-composed UTF-8 (completed-syllable granularity); never attempt IME composition in v1.
- The terminal screen is reserved for rendering: **never write logs/diagnostics to stdout/stderr**; use the file logger.
- On any exit (normal, panic, SIGINT/SIGTERM) the terminal **must** be restored (raw mode off, alt screen off, kitty keyboard/mouse protocols popped, cursor shown).
- Default profile dir: `$WEBCAT_PROFILE_DIR` or `<data_dir>/webcat/profile`. Chrome binary: `$WEBCAT_CHROME` or auto-discovery. Log file: `$WEBCAT_LOG` or `<state_dir>/webcat/log`.

---

## File Structure

```
Cargo.toml
src/
  main.rs                  — entry: parse CLI, init logging, run app
  cli.rs                   — clap args (Cli struct)
  error.rs                 — Error/Result types (thiserror)
  config.rs                — resolved paths (profile, chrome, log) + tunables
  app.rs                   — async event loop / orchestrator + Mode state machine
  geometry.rs              — cell<->pixel coordinate math, viewport sizing
  terminal/
    mod.rs                 — Terminal facade + RestoreGuard
    raw.rs                 — raw mode enable/restore, alt screen, protocol push/pop
    capability.rs          — cell-size query (CSI 16 t), graphics/keyboard detection
    keyboard.rs            — kitty keyboard protocol parser (bytes -> KeyEvent)
    mouse.rs               — SGR mouse parser (bytes -> MouseEvent)
    input.rs               — RawInput enum + stdin byte reader -> event stream
  renderer/
    mod.rs                 — Renderer: present/clear/resize over the graphics encoder
    graphics.rs            — kitty graphics protocol encoder (transmit+place JPEG)
  browser/
    mod.rs                 — Browser controller (spawn, navigate, screencast, input)
    profile.rs             — chrome path discovery + profile dir prep
    frame.rs               — Frame struct + latest-frame coalescing channel
  input/
    mod.rs                 — InputMapper: RawInput + Mode -> Action
    action.rs              — Action enum
    hints.rs               — vim hint label generation + element->click mapping
  ui/
    mod.rs                 — overlay rendering: status bar, url prompt, hints
tests/
  browser_integration.rs   — gated integration tests (real headless Chromium)
assets/
  test_frame.jpg           — small static JPEG used by the milestone-1 smoke
```

---

## Task 1: Project scaffold, error types, config, CLI

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/error.rs`
- Create: `src/cli.rs`
- Create: `src/config.rs`
- Test: unit tests inline in `src/config.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `error::Error` (thiserror enum), `error::Result<T> = std::result::Result<T, Error>`.
  - `cli::Cli { url: Option<String>, profile_dir: Option<PathBuf>, chrome: Option<PathBuf>, quality: u8, dpr: f64 }` via `Cli::parse()`.
  - `config::Config { profile_dir: PathBuf, chrome: Option<PathBuf>, log_path: PathBuf, quality: u8, dpr: f64, start_url: String }` with `Config::resolve(cli: Cli) -> Result<Config>`.

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "webcat"
version = "0.1.0"
edition = "2021"
rust-version = "1.75"

[dependencies]
tokio = { version = "1", features = ["full"] }
chromiumoxide = { version = "0.7", features = ["tokio-runtime"], default-features = false }
futures = "0.3"
crossterm = { version = "0.28", features = ["event-stream"] }
image = { version = "0.25", default-features = false, features = ["jpeg"] }
base64 = "0.22"
clap = { version = "4", features = ["derive", "env"] }
thiserror = "1"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-appender = "0.2"
dirs = "5"

[dev-dependencies]
tokio = { version = "1", features = ["full", "test-util"] }
```

- [ ] **Step 2: Create `src/error.rs`**

```rust
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("terminal is not Kitty-graphics capable: {0}")]
    UnsupportedTerminal(String),

    #[error("chrome binary not found; set $WEBCAT_CHROME or install Google Chrome")]
    ChromeNotFound,

    #[error("profile is locked by another webcat/chrome instance: {0}")]
    ProfileLocked(PathBuf),

    #[error("browser disconnected")]
    BrowserDisconnected,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
```

- [ ] **Step 3: Create `src/cli.rs`**

```rust
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
```

- [ ] **Step 4: Write the failing test for `Config::resolve`**

Add to `src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;

    fn base_cli() -> Cli {
        Cli { url: None, profile_dir: None, chrome: None, quality: 70, dpr: 1.0 }
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
```

- [ ] **Step 5: Run the test to verify it fails**

Run: `cargo test config::tests -- --nocapture`
Expected: FAIL — `Config` / `resolve` not found.

- [ ] **Step 6: Implement `src/config.rs`**

```rust
use std::path::PathBuf;
use crate::cli::Cli;
use crate::error::Result;

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
            dpr: if cli.dpr > 0.0 { cli.dpr } else { 1.0 },
            start_url: cli.url.unwrap_or_else(|| "about:blank".to_string()),
        })
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
```

- [ ] **Step 7: Create a minimal `src/main.rs` that compiles**

```rust
mod cli;
mod config;
mod error;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    let cfg = config::Config::resolve(cli)?;
    // Wired up incrementally in later tasks.
    eprintln!("resolved config: {:?}", cfg);
    Ok(())
}
```

- [ ] **Step 8: Run tests and verify they pass**

Run: `cargo test config::tests`
Expected: PASS (3 tests).

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs src/error.rs src/cli.rs src/config.rs
git commit -m "feat: project scaffold with CLI, error types, and config"
```

---

## Task 2: Kitty graphics protocol encoder

**Files:**
- Create: `src/renderer/graphics.rs`
- Create: `src/renderer/mod.rs` (module wiring only in this task)
- Test: inline `#[cfg(test)]` in `src/renderer/graphics.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct KittyGraphics { image_id: u32, placement_id: u32 }`
  - `KittyGraphics::new(image_id: u32) -> Self`
  - `KittyGraphics::transmit_and_place_jpeg(&self, jpeg: &[u8]) -> Vec<u8>` — returns the full escape-sequence byte stream that transmits a JPEG (format `f=100`) and places it at the current cursor, replacing any previous placement with the same (image_id, placement_id), chunked at 4096 base64 bytes.
  - `KittyGraphics::delete_all(&self) -> Vec<u8>` — returns the escape to delete the image by id.

Background — Kitty graphics protocol facts this task depends on:
- Escapes are `ESC _ G <control kv pairs> ; <base64 payload> ESC \`.
- `f=100` means the payload is a PNG/JPEG file (Kitty auto-detects); `a=T` transmits **and** displays.
- Payload must be base64 and split into ≤4096-byte chunks; every chunk except the last carries `m=1`, the last carries `m=0`. Control keys (`f`, `a`, `i`, `p`, `q`, etc.) appear **only on the first chunk**; continuation chunks carry just `m`.
- Reusing the same `i` (image id) and `p` (placement id) replaces the prior placement in place (no flicker / no stacking).
- `q=2` suppresses Kitty's acknowledgement responses (we don't want them landing in our input stream).

- [ ] **Step 1: Write failing tests**

Add to `src/renderer/graphics.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn payload_of(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn small_jpeg_is_single_chunk() {
        let g = KittyGraphics::new(7);
        let out = payload_of(&g.transmit_and_place_jpeg(&[0xFF, 0xD8, 0xFF, 0xD9]));
        // Begins with the graphics APC opener and the expected control keys.
        assert!(out.starts_with("\x1b_G"), "missing APC opener: {out:?}");
        assert!(out.contains("f=100"), "missing f=100");
        assert!(out.contains("a=T"), "missing a=T");
        assert!(out.contains("i=7"), "missing image id");
        assert!(out.contains("q=2"), "missing quiet flag");
        assert!(out.contains("m=0"), "single chunk must end with m=0");
        assert!(out.ends_with("\x1b\\"), "missing ST terminator");
        // Exactly one APC block for a small image.
        assert_eq!(out.matches("\x1b_G").count(), 1);
    }

    #[test]
    fn large_jpeg_is_chunked_with_m_flags() {
        let g = KittyGraphics::new(1);
        // 10_000 raw bytes -> base64 ~13_336 chars -> 4 chunks of 4096 max.
        let big = vec![0xABu8; 10_000];
        let out = payload_of(&g.transmit_and_place_jpeg(&big));
        let blocks = out.matches("\x1b_G").count();
        assert!(blocks >= 4, "expected >=4 chunks, got {blocks}");
        // Control keys only on first block.
        assert_eq!(out.matches("f=100").count(), 1);
        // Every block except the last carries m=1; last carries m=0.
        assert_eq!(out.matches("m=1").count(), blocks - 1);
        assert_eq!(out.matches("m=0").count(), 1);
    }

    #[test]
    fn delete_all_targets_image_id() {
        let g = KittyGraphics::new(42);
        let out = payload_of(&g.delete_all());
        assert!(out.contains("a=d"), "missing delete action");
        assert!(out.contains("i=42"), "missing image id");
        assert!(out.ends_with("\x1b\\"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test renderer::graphics`
Expected: FAIL — `KittyGraphics` not found.

- [ ] **Step 3: Implement `src/renderer/graphics.rs`**

```rust
use base64::Engine;

const CHUNK: usize = 4096;

pub struct KittyGraphics {
    image_id: u32,
    placement_id: u32,
}

impl KittyGraphics {
    pub fn new(image_id: u32) -> Self {
        KittyGraphics { image_id, placement_id: 1 }
    }

    /// Transmit a JPEG and place it at the cursor, replacing any prior placement
    /// with the same (image_id, placement_id). Chunked at CHUNK base64 bytes.
    pub fn transmit_and_place_jpeg(&self, jpeg: &[u8]) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
        let bytes = b64.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 256);

        // Split into ≤CHUNK pieces; if it fits in one, that single piece is the last.
        let chunks: Vec<&[u8]> = if bytes.is_empty() {
            vec![&[][..]]
        } else {
            bytes.chunks(CHUNK).collect()
        };
        let last = chunks.len() - 1;

        for (idx, chunk) in chunks.iter().enumerate() {
            out.extend_from_slice(b"\x1b_G");
            if idx == 0 {
                // First chunk carries all control keys.
                let m = if last == 0 { 0 } else { 1 };
                let header = format!(
                    "f=100,a=T,i={},p={},q=2,m={}",
                    self.image_id, self.placement_id, m
                );
                out.extend_from_slice(header.as_bytes());
            } else {
                let m = if idx == last { 0 } else { 1 };
                out.extend_from_slice(format!("m={}", m).as_bytes());
            }
            out.push(b';');
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\x1b\\");
        }
        out
    }

    /// Delete the transmitted image (and its placements) by id.
    pub fn delete_all(&self) -> Vec<u8> {
        format!("\x1b_Ga=d,d=i,i={},q=2;\x1b\\", self.image_id).into_bytes()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test renderer::graphics`
Expected: PASS (3 tests).

- [ ] **Step 5: Create `src/renderer/mod.rs`**

```rust
pub mod graphics;
```

- [ ] **Step 6: Wire module into `src/main.rs`**

Add `mod renderer;` to the module list in `src/main.rs`.

- [ ] **Step 7: Commit**

```bash
git add src/renderer/ src/main.rs
git commit -m "feat: kitty graphics protocol encoder with chunking and in-place placement"
```

---

## Task 3: Geometry — cell/pixel mapping

**Files:**
- Create: `src/geometry.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct CellSize { pub w: u16, pub h: u16 }` (pixels per cell)
  - `pub struct GridSize { pub cols: u16, pub rows: u16 }`
  - `pub struct Viewport { pub width_px: u32, pub height_px: u32 }`
  - `pub fn page_viewport(grid: GridSize, cell: CellSize, status_rows: u16) -> Viewport` — pixel viewport for the page area (grid height minus `status_rows`).
  - `pub fn cell_to_pixel(col: u16, row: u16, cell: CellSize) -> (f64, f64)` — center of the cell in page-pixel coordinates (0-based col/row).

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_excludes_status_rows() {
        let vp = page_viewport(GridSize { cols: 100, rows: 30 },
                               CellSize { w: 8, h: 16 }, 1);
        assert_eq!(vp.width_px, 800);
        assert_eq!(vp.height_px, 29 * 16);
    }

    #[test]
    fn cell_center_maps_to_pixel_center() {
        // col 0,row 0 center -> (4, 8) for an 8x16 cell.
        let (x, y) = cell_to_pixel(0, 0, CellSize { w: 8, h: 16 });
        assert_eq!((x, y), (4.0, 8.0));
        let (x, y) = cell_to_pixel(2, 1, CellSize { w: 8, h: 16 });
        assert_eq!((x, y), (20.0, 24.0));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test geometry`
Expected: FAIL — types not found.

- [ ] **Step 3: Implement `src/geometry.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize { pub w: u16, pub h: u16 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize { pub cols: u16, pub rows: u16 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport { pub width_px: u32, pub height_px: u32 }

pub fn page_viewport(grid: GridSize, cell: CellSize, status_rows: u16) -> Viewport {
    let page_rows = grid.rows.saturating_sub(status_rows);
    Viewport {
        width_px: grid.cols as u32 * cell.w as u32,
        height_px: page_rows as u32 * cell.h as u32,
    }
}

pub fn cell_to_pixel(col: u16, row: u16, cell: CellSize) -> (f64, f64) {
    let x = col as f64 * cell.w as f64 + cell.w as f64 / 2.0;
    let y = row as f64 * cell.h as f64 + cell.h as f64 / 2.0;
    (x, y)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test geometry`
Expected: PASS.

- [ ] **Step 5: Wire module + commit**

Add `mod geometry;` to `src/main.rs`, then:

```bash
git add src/geometry.rs src/main.rs
git commit -m "feat: cell/pixel geometry mapping"
```

---

## Task 4: Kitty keyboard protocol parser

**Files:**
- Create: `src/terminal/keyboard.rs`
- Create: `src/terminal/mod.rs` (module wiring only)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum Key { Char(char), Enter, Backspace, Tab, Esc, Up, Down, Left, Right, Home, End, PageUp, PageDown, Delete, F(u8) }`
  - `pub struct Mods { pub shift: bool, pub ctrl: bool, pub alt: bool, pub meta: bool }` with `Mods::none()`.
  - `pub struct KeyEvent { pub key: Key, pub mods: Mods, pub text: Option<String> }`
  - `pub fn parse_kitty_key(seq: &str) -> Option<KeyEvent>` — parses one Kitty keyboard escape `CSI <unicode-key-code> [; <mods>[: <event-type>]] [; <text-codepoints>] u`. Returns None if `seq` is not a complete Kitty key escape.

Background — Kitty keyboard protocol facts:
- Enabled by pushing flags: `CSI > 1 u` (disambiguate + report events). Popped with `CSI < u`.
- A key press arrives as `CSI unicode-key-code ; modifiers : event-type ; text-as-codepoints u`. Only the key code is mandatory; the rest are optional.
- `modifiers` is `1 + bitmask` where bit 0=shift, 1=alt, 2=ctrl, 3=super(meta).
- `event-type`: 1=press (default), 2=repeat, 3=release.
- Special keys use functional key codes: Enter=13, Tab=9, Backspace=127, Esc=27; arrows/navigation use CSI-u functional codes (e.g. Up=57352? — but Kitty reports legacy arrows as `CSI A`). For this parser we handle the **CSI-u `…u` form**; legacy arrow/`CSI letter` forms are handled in Task 6's reader and mapped to `Key` directly.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_letter() {
        let ev = parse_kitty_key("\x1b[97u").unwrap(); // 'a'
        assert!(matches!(ev.key, Key::Char('a')));
        assert!(!ev.mods.ctrl && !ev.mods.alt);
    }

    #[test]
    fn ctrl_modifier() {
        // 'c' with ctrl: modifiers = 1 + 4 = 5
        let ev = parse_kitty_key("\x1b[99;5u").unwrap();
        assert!(matches!(ev.key, Key::Char('c')));
        assert!(ev.mods.ctrl);
        assert!(!ev.mods.shift);
    }

    #[test]
    fn release_event_is_parsed() {
        // 'a', no mods (1), event-type 3 (release)
        let ev = parse_kitty_key("\x1b[97;1:3u").unwrap();
        assert!(matches!(ev.key, Key::Char('a')));
    }

    #[test]
    fn enter_and_backspace_special_codes() {
        assert!(matches!(parse_kitty_key("\x1b[13u").unwrap().key, Key::Enter));
        assert!(matches!(parse_kitty_key("\x1b[127u").unwrap().key, Key::Backspace));
        assert!(matches!(parse_kitty_key("\x1b[27u").unwrap().key, Key::Esc));
    }

    #[test]
    fn text_field_is_captured() {
        // key code 97, text codepoint 97 ('a') after second ';'
        let ev = parse_kitty_key("\x1b[97;1;97u").unwrap();
        assert_eq!(ev.text.as_deref(), Some("a"));
    }

    #[test]
    fn rejects_incomplete() {
        assert!(parse_kitty_key("\x1b[97").is_none());
        assert!(parse_kitty_key("not an escape").is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test terminal::keyboard`
Expected: FAIL — symbols not found.

- [ ] **Step 3: Implement `src/terminal/keyboard.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char), Enter, Backspace, Tab, Esc,
    Up, Down, Left, Right, Home, End, PageUp, PageDown, Delete, F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods { pub shift: bool, pub ctrl: bool, pub alt: bool, pub meta: bool }

impl Mods {
    pub fn none() -> Self { Mods::default() }
    fn from_encoded(m: u32) -> Self {
        let bits = m.saturating_sub(1);
        Mods {
            shift: bits & 0b0001 != 0,
            alt:   bits & 0b0010 != 0,
            ctrl:  bits & 0b0100 != 0,
            meta:  bits & 0b1000 != 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent { pub key: Key, pub mods: Mods, pub text: Option<String> }

/// Parse one complete Kitty keyboard escape of the CSI-u form: `CSI code [;mods[:type]] [;text] u`.
pub fn parse_kitty_key(seq: &str) -> Option<KeyEvent> {
    let body = seq.strip_prefix("\x1b[")?.strip_suffix('u')?;
    // Split into up to 3 ';'-separated fields: key, mods(:type), text.
    let mut fields = body.split(';');

    let key_field = fields.next()?;
    let key_code: u32 = key_field.parse().ok()?;

    let mods_field = fields.next();
    let mods = match mods_field {
        Some(f) => {
            // f may be "mods" or "mods:event-type"; we ignore event-type here.
            let m = f.split(':').next()?.parse::<u32>().ok()?;
            Mods::from_encoded(m)
        }
        None => Mods::none(),
    };

    let text = fields.next().and_then(|f| {
        let s: String = f
            .split(':')
            .filter_map(|cp| cp.parse::<u32>().ok())
            .filter_map(char::from_u32)
            .collect();
        if s.is_empty() { None } else { Some(s) }
    });

    let key = match key_code {
        13 => Key::Enter,
        9 => Key::Tab,
        127 => Key::Backspace,
        27 => Key::Esc,
        c => Key::Char(char::from_u32(c)?),
    };

    Some(KeyEvent { key, mods, text })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test terminal::keyboard`
Expected: PASS (6 tests).

- [ ] **Step 5: Create `src/terminal/mod.rs`**

```rust
pub mod keyboard;
```

- [ ] **Step 6: Wire module + commit**

Add `mod terminal;` to `src/main.rs`, then:

```bash
git add src/terminal/ src/main.rs
git commit -m "feat: kitty keyboard protocol parser"
```

---

## Task 5: SGR mouse parser

**Files:**
- Create: `src/terminal/mouse.rs`
- Modify: `src/terminal/mod.rs` (add `pub mod mouse;`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum MouseButton { Left, Middle, Right }`
  - `pub enum MouseKind { Down(MouseButton), Up(MouseButton), Move, WheelUp, WheelDown }`
  - `pub struct MouseEvent { pub kind: MouseKind, pub col: u16, pub row: u16 }` (col/row are 0-based cells)
  - `pub fn parse_sgr_mouse(seq: &str) -> Option<MouseEvent>` — parses `CSI < b ; col ; row (M|m)`.

Background — SGR mouse (`CSI ? 1006 h`) facts:
- Format: `ESC [ < Cb ; Cx ; Cy M` (press/move) or `… m` (release). `Cx`/`Cy` are 1-based.
- Low 2 bits of `Cb`: 0=left,1=middle,2=right. Bit 5 (value 32) = motion. Bit 6 (value 64) = wheel; then low 2 bits 0=up,1=down.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_press_and_release() {
        let d = parse_sgr_mouse("\x1b[<0;5;9M").unwrap();
        assert!(matches!(d.kind, MouseKind::Down(MouseButton::Left)));
        assert_eq!((d.col, d.row), (4, 8)); // converted to 0-based
        let u = parse_sgr_mouse("\x1b[<0;5;9m").unwrap();
        assert!(matches!(u.kind, MouseKind::Up(MouseButton::Left)));
    }

    #[test]
    fn wheel_up_and_down() {
        assert!(matches!(parse_sgr_mouse("\x1b[<64;1;1M").unwrap().kind, MouseKind::WheelUp));
        assert!(matches!(parse_sgr_mouse("\x1b[<65;1;1M").unwrap().kind, MouseKind::WheelDown));
    }

    #[test]
    fn motion_is_move() {
        // 32 (motion) + 0 (left held) = 32
        assert!(matches!(parse_sgr_mouse("\x1b[<32;2;2M").unwrap().kind, MouseKind::Move));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_sgr_mouse("\x1b[<0;5M").is_none());
        assert!(parse_sgr_mouse("nope").is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test terminal::mouse`
Expected: FAIL.

- [ ] **Step 3: Implement `src/terminal/mouse.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton { Left, Middle, Right }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind { Down(MouseButton), Up(MouseButton), Move, WheelUp, WheelDown }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent { pub kind: MouseKind, pub col: u16, pub row: u16 }

pub fn parse_sgr_mouse(seq: &str) -> Option<MouseEvent> {
    let body = seq.strip_prefix("\x1b[<")?;
    let (is_release, body) = if let Some(b) = body.strip_suffix('M') {
        (false, b)
    } else if let Some(b) = body.strip_suffix('m') {
        (true, b)
    } else {
        return None;
    };

    let mut parts = body.split(';');
    let cb: u32 = parts.next()?.parse().ok()?;
    let cx: u16 = parts.next()?.parse().ok()?;
    let cy: u16 = parts.next()?.parse().ok()?;
    if parts.next().is_some() { return None; }

    let col = cx.checked_sub(1)?;
    let row = cy.checked_sub(1)?;

    let kind = if cb & 64 != 0 {
        // wheel
        if cb & 1 == 0 { MouseKind::WheelUp } else { MouseKind::WheelDown }
    } else if cb & 32 != 0 {
        MouseKind::Move
    } else {
        let button = match cb & 0b11 {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            _ => MouseButton::Right,
        };
        if is_release { MouseKind::Up(button) } else { MouseKind::Down(button) }
    };

    Some(MouseEvent { kind, col, row })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test terminal::mouse`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add src/terminal/mouse.rs src/terminal/mod.rs
git commit -m "feat: SGR mouse parser"
```

---

## Task 6: Raw mode + RestoreGuard

**Files:**
- Create: `src/terminal/raw.rs`
- Modify: `src/terminal/mod.rs` (add `pub mod raw;`)
- Test: inline `#[cfg(test)]` (state-machine logic only; tty side effects are covered by manual smoke)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct RestoreGuard { active: bool }`
  - `RestoreGuard::enter() -> crate::error::Result<RestoreGuard>` — enables raw mode, enters alt screen, pushes kitty keyboard flags (`CSI > 1 u`), enables SGR mouse (`CSI ? 1006 h` + `CSI ? 1003 h`), hides cursor. Writes the enabling escapes to stdout.
  - `RestoreGuard::restore(&mut self)` — idempotently reverses all of the above. Safe to call multiple times.
  - `impl Drop for RestoreGuard` calls `restore`.
  - `pub fn install_panic_and_signal_hooks()` — sets a panic hook and SIGINT/SIGTERM handler that emit the restore escapes directly (best-effort) so the terminal is never left broken.

Note: `restore()` must work even if called from a signal/panic context, so it writes raw escape bytes (it does not depend on `RestoreGuard` internal state beyond the `active` flag).

- [ ] **Step 1: Write the failing test (idempotency of the state flag)**

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test terminal::raw`
Expected: FAIL — `RestoreGuard` not found.

- [ ] **Step 3: Implement `src/terminal/raw.rs`**

```rust
use std::io::Write;
use crate::error::Result;

// Escape sequences for setup/teardown.
const ENTER_ALT: &str = "\x1b[?1049h";
const LEAVE_ALT: &str = "\x1b[?1049l";
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";
const KKBD_PUSH: &str = "\x1b[>1u";       // push kitty keyboard flags (disambiguate)
const KKBD_POP: &str = "\x1b[<u";          // pop kitty keyboard flags
const MOUSE_ON: &str = "\x1b[?1003h\x1b[?1006h"; // any-event + SGR
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1003l";

pub struct RestoreGuard {
    pub(crate) active: bool,
}

impl RestoreGuard {
    pub fn enter() -> Result<RestoreGuard> {
        crossterm::terminal::enable_raw_mode()?;
        let mut out = std::io::stdout();
        write!(out, "{ENTER_ALT}{HIDE_CURSOR}{KKBD_PUSH}{MOUSE_ON}")?;
        out.flush()?;
        Ok(RestoreGuard { active: true })
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
        // Best-effort: ignore errors, emit teardown escapes, drop raw mode.
        let mut out = std::io::stdout();
        let _ = write!(out, "{MOUSE_OFF}{KKBD_POP}{SHOW_CURSOR}{LEAVE_ALT}");
        let _ = out.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Best-effort terminal restoration on panic and on SIGINT/SIGTERM.
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
        for _ in signals.forever() {
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
```

- [ ] **Step 4: Add the `signal-hook` dependency**

In `Cargo.toml` under `[dependencies]`:

```toml
signal-hook = "0.3"
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test terminal::raw`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/terminal/raw.rs src/terminal/mod.rs Cargo.toml Cargo.lock
git commit -m "feat: raw-mode RestoreGuard with panic/signal restoration"
```

---

## Task 7: Terminal capability detection (cell size)

**Files:**
- Create: `src/terminal/capability.rs`
- Modify: `src/terminal/mod.rs` (add `pub mod capability;`)
- Test: inline `#[cfg(test)]` for the response parser

**Interfaces:**
- Consumes: `crate::geometry::CellSize`.
- Produces:
  - `pub fn parse_cell_size_report(seq: &str) -> Option<CellSize>` — parses `CSI 6 ; height ; width t`.
  - `pub fn query_cell_size(timeout: std::time::Duration) -> CellSize` — writes `CSI 16 t`, reads the reply from stdin within `timeout`, parses it, and falls back to `CellSize { w: 8, h: 16 }` on timeout/parse failure (logging a warning).
  - `pub fn detect_kitty_graphics(timeout: std::time::Duration) -> bool` — sends a tiny 1x1 RGBA graphics query (`\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\`) and returns true iff a Kitty graphics response (`\x1b_G...;OK\x1b\\`) arrives within `timeout`.

- [ ] **Step 1: Write the failing test for the parser**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cell_size_report() {
        let cs = parse_cell_size_report("\x1b[6;16;8t").unwrap();
        assert_eq!(cs.w, 8);
        assert_eq!(cs.h, 16);
    }

    #[test]
    fn rejects_wrong_report() {
        assert!(parse_cell_size_report("\x1b[4;16;8t").is_none()); // wrong leading code
        assert!(parse_cell_size_report("garbage").is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test terminal::capability`
Expected: FAIL.

- [ ] **Step 3: Implement `src/terminal/capability.rs`**

```rust
use std::io::{Read, Write};
use std::time::{Duration, Instant};
use crate::geometry::CellSize;

/// Parse a `CSI 6 ; height ; width t` cell-size report.
pub fn parse_cell_size_report(seq: &str) -> Option<CellSize> {
    let body = seq.strip_prefix("\x1b[")?.strip_suffix('t')?;
    let mut parts = body.split(';');
    if parts.next()? != "6" {
        return None;
    }
    let h: u16 = parts.next()?.parse().ok()?;
    let w: u16 = parts.next()?.parse().ok()?;
    Some(CellSize { w, h })
}

/// Read available bytes from stdin until `terminator` byte or `timeout`.
fn read_reply(terminator: u8, timeout: Duration) -> String {
    // stdin is already in raw mode (no line buffering) when this is called.
    let mut stdin = std::io::stdin();
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        // Non-blocking-ish: rely on raw-mode VTIME via poll. Use a short read.
        match stdin.read(&mut byte) {
            Ok(1) => {
                buf.push(byte[0]);
                if byte[0] == terminator {
                    break;
                }
            }
            _ => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

pub fn query_cell_size(timeout: Duration) -> CellSize {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[16t");
    let _ = out.flush();
    let reply = read_reply(b't', timeout);
    parse_cell_size_report(&reply).unwrap_or_else(|| {
        tracing::warn!("cell-size query failed; falling back to 8x16");
        CellSize { w: 8, h: 16 }
    })
}

pub fn detect_kitty_graphics(timeout: Duration) -> bool {
    let mut out = std::io::stdout();
    // Query action with a 1px base64 RGB pixel; Kitty replies with an APC ...;OK ST.
    let _ = write!(out, "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
    let _ = out.flush();
    let reply = read_reply(b'\\', timeout);
    reply.contains("\x1b_G") && reply.contains("OK")
}
```

Note for the implementer: raw mode must set `VMIN=0, VTIME=1` (crossterm raw mode does this by default on Unix) so the per-byte `read` returns quickly rather than blocking forever. If responses are flaky in manual testing, set the terminal to non-blocking explicitly; this is acceptable to adjust during the integration smoke.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test terminal::capability`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/terminal/capability.rs src/terminal/mod.rs
git commit -m "feat: terminal cell-size query and kitty graphics detection"
```

---

## Task 8: Renderer facade

**Files:**
- Modify: `src/renderer/mod.rs`
- Test: inline `#[cfg(test)]` in `src/renderer/mod.rs`

**Interfaces:**
- Consumes: `renderer::graphics::KittyGraphics`, `geometry::{GridSize, CellSize}`.
- Produces:
  - `pub struct Renderer { gfx: graphics::KittyGraphics, grid: GridSize, cell: CellSize }`
  - `Renderer::new(grid: GridSize, cell: CellSize) -> Renderer`
  - `Renderer::present_jpeg_bytes(&self, jpeg: &[u8]) -> Vec<u8>` — returns the byte stream to position the cursor at the top-left of the page area (row 1, col 1) and place the frame. (Caller writes these bytes to stdout.)
  - `Renderer::resize(&mut self, grid: GridSize, cell: CellSize)`
  - `Renderer::clear(&self) -> Vec<u8>` — bytes to delete the current image.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{GridSize, CellSize};

    #[test]
    fn present_positions_cursor_then_places_image() {
        let r = Renderer::new(GridSize { cols: 80, rows: 24 }, CellSize { w: 8, h: 16 });
        let out = String::from_utf8(r.present_jpeg_bytes(&[0xFF, 0xD8, 0xFF, 0xD9])).unwrap();
        // Cursor home (row1,col1) before the graphics block.
        let home_idx = out.find("\x1b[1;1H").expect("cursor home missing");
        let gfx_idx = out.find("\x1b_G").expect("graphics block missing");
        assert!(home_idx < gfx_idx, "cursor must be positioned before placement");
    }

    #[test]
    fn clear_emits_delete() {
        let r = Renderer::new(GridSize { cols: 80, rows: 24 }, CellSize { w: 8, h: 16 });
        let out = String::from_utf8(r.clear()).unwrap();
        assert!(out.contains("a=d"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test renderer::tests`
Expected: FAIL — `Renderer` not found.

- [ ] **Step 3: Implement the facade in `src/renderer/mod.rs`**

```rust
pub mod graphics;

use crate::geometry::{GridSize, CellSize};

const IMAGE_ID: u32 = 1;

pub struct Renderer {
    gfx: graphics::KittyGraphics,
    #[allow(dead_code)]
    grid: GridSize,
    #[allow(dead_code)]
    cell: CellSize,
}

impl Renderer {
    pub fn new(grid: GridSize, cell: CellSize) -> Renderer {
        Renderer { gfx: graphics::KittyGraphics::new(IMAGE_ID), grid, cell }
    }

    pub fn resize(&mut self, grid: GridSize, cell: CellSize) {
        self.grid = grid;
        self.cell = cell;
    }

    pub fn present_jpeg_bytes(&self, jpeg: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"\x1b[1;1H"); // cursor to row 1, col 1
        out.extend_from_slice(&self.gfx.transmit_and_place_jpeg(jpeg));
        out
    }

    pub fn clear(&self) -> Vec<u8> {
        self.gfx.delete_all()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test renderer::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/renderer/mod.rs
git commit -m "feat: renderer facade positioning and placing frames"
```

---

## Task 9: Milestone 1 — display a static JPEG in Kitty

**Files:**
- Create: `assets/test_frame.jpg` (any small valid JPEG)
- Create: `src/bin/static_frame.rs` (a throwaway smoke binary; removed in Task 16)
- Test: manual smoke (documented), plus a compile check.

**Interfaces:**
- Consumes: `RestoreGuard`, `query_cell_size`, `detect_kitty_graphics`, `Renderer`, terminal size from crossterm.
- Produces: a runnable binary `static_frame` that draws `assets/test_frame.jpg` and waits for a keypress.

- [ ] **Step 1: Add a small JPEG asset**

Run (generates a 64x64 red JPEG without extra tooling, via Rust):

```bash
mkdir -p assets
cat > /tmp/gen.rs <<'RS'
fn main() {
    let img = image::RgbImage::from_pixel(64, 64, image::Rgb([200, 30, 30]));
    img.save_with_format("assets/test_frame.jpg", image::ImageFormat::Jpeg).unwrap();
}
RS
echo "generate assets/test_frame.jpg manually with the image crate or copy any small .jpg here"
```

If simpler, copy any existing small `.jpg` to `assets/test_frame.jpg`. The only requirement is a valid baseline JPEG.

- [ ] **Step 2: Create `src/bin/static_frame.rs`**

```rust
// Throwaway smoke binary: render a static JPEG via the kitty graphics protocol.
use std::io::Write;
use std::time::Duration;

#[path = "../error.rs"] mod error;
#[path = "../geometry.rs"] mod geometry;
#[path = "../renderer/mod.rs"] mod renderer;
#[path = "../terminal/mod.rs"] mod terminal;

use geometry::{GridSize, CellSize};

fn main() -> anyhow::Result<()> {
    terminal::raw::install_panic_and_signal_hooks();
    let mut guard = terminal::raw::RestoreGuard::enter()?;

    if !terminal::capability::detect_kitty_graphics(Duration::from_millis(300)) {
        guard.restore();
        eprintln!("This terminal does not support the Kitty graphics protocol.");
        std::process::exit(1);
    }

    let cell = terminal::capability::query_cell_size(Duration::from_millis(300));
    let (cols, rows) = crossterm::terminal::size()?;
    let renderer = renderer::Renderer::new(GridSize { cols, rows }, CellSize { w: cell.w, h: cell.h });

    let jpeg = std::fs::read("assets/test_frame.jpg")?;
    let mut out = std::io::stdout();
    out.write_all(&renderer.present_jpeg_bytes(&jpeg))?;
    out.flush()?;

    // Wait ~3s so the user can see it, then restore.
    std::thread::sleep(Duration::from_secs(3));
    guard.restore();
    Ok(())
}
```

- [ ] **Step 3: Build**

Run: `cargo build --bin static_frame`
Expected: compiles cleanly.

- [ ] **Step 4: Manual smoke (in a real Kitty terminal)**

Run: `cargo run --bin static_frame`
Expected: a red square appears at the top-left for ~3 seconds, then the terminal returns to normal (cursor visible, no leftover escapes). Verify Ctrl-C during the 3s also restores the terminal cleanly.

- [ ] **Step 5: Commit**

```bash
git add assets/test_frame.jpg src/bin/static_frame.rs
git commit -m "feat: milestone 1 — static JPEG rendered via kitty graphics"
```

---

## Task 10: Browser profile + chrome discovery

**Files:**
- Create: `src/browser/profile.rs`
- Create: `src/browser/mod.rs` (module wiring + re-exports only this task)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `crate::error::{Error, Result}`.
- Produces:
  - `pub fn discover_chrome(explicit: Option<&std::path::Path>) -> Result<std::path::PathBuf>` — returns `explicit` if it exists, else searches known macOS/Linux locations, else `Err(Error::ChromeNotFound)`.
  - `pub fn prepare_profile(dir: &std::path::Path) -> Result<()>` — creates the dir (mode 0700 on unix) if missing; returns `Err(Error::ProfileLocked)` if a `SingletonLock` indicates a live instance.

- [ ] **Step 1: Write failing tests**

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test browser::profile`
Expected: FAIL.

- [ ] **Step 3: Implement `src/browser/profile.rs`**

```rust
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
```

- [ ] **Step 4: Create `src/browser/mod.rs`**

```rust
pub mod profile;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test browser::profile`
Expected: PASS.

- [ ] **Step 6: Wire module + commit**

Add `mod browser;` to `src/main.rs`, then:

```bash
git add src/browser/ src/main.rs
git commit -m "feat: chrome discovery and profile preparation"
```

---

## Task 11: Frame type + latest-frame coalescing channel

**Files:**
- Create: `src/browser/frame.rs`
- Modify: `src/browser/mod.rs` (add `pub mod frame;`)
- Test: inline `#[cfg(test)]` (async tests with tokio)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct Frame { pub jpeg: Vec<u8> }`
  - `pub fn frame_channel() -> (FrameTx, FrameRx)` — a coalescing channel that keeps only the most recent frame.
  - `pub struct FrameTx` with `fn send(&self, frame: Frame)` (overwrites any unconsumed frame).
  - `pub struct FrameRx` with `async fn recv(&mut self) -> Option<Frame>` (awaits the latest frame; returns None when all senders dropped).

Implementation note: use `tokio::sync::watch` for natural latest-value coalescing, wrapping `Option<Frame>`.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recv_gets_latest_after_multiple_sends() {
        let (tx, mut rx) = frame_channel();
        tx.send(Frame { jpeg: vec![1] });
        tx.send(Frame { jpeg: vec![2] });
        tx.send(Frame { jpeg: vec![3] });
        let f = rx.recv().await.unwrap();
        assert_eq!(f.jpeg, vec![3]); // coalesced to most recent
    }

    #[tokio::test]
    async fn recv_returns_none_when_tx_dropped() {
        let (tx, mut rx) = frame_channel();
        drop(tx);
        assert!(rx.recv().await.is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test browser::frame`
Expected: FAIL.

- [ ] **Step 3: Implement `src/browser/frame.rs`**

```rust
use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct Frame {
    pub jpeg: Vec<u8>,
}

pub struct FrameTx {
    inner: watch::Sender<Option<Frame>>,
}

pub struct FrameRx {
    inner: watch::Receiver<Option<Frame>>,
}

pub fn frame_channel() -> (FrameTx, FrameRx) {
    let (tx, rx) = watch::channel(None);
    (FrameTx { inner: tx }, FrameRx { inner: rx })
}

impl FrameTx {
    pub fn send(&self, frame: Frame) {
        // Overwrites the stored value; lagging receivers only see the latest.
        let _ = self.inner.send(Some(frame));
    }
}

impl FrameRx {
    pub async fn recv(&mut self) -> Option<Frame> {
        loop {
            // Wait for a change from the current value.
            if self.inner.changed().await.is_err() {
                return None; // all senders dropped
            }
            let val = self.inner.borrow_and_update().clone();
            if let Some(f) = val {
                return Some(f);
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test browser::frame`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/browser/frame.rs src/browser/mod.rs
git commit -m "feat: latest-frame coalescing channel"
```

---

## Task 12: Browser controller — spawn, navigate, screencast, input dispatch

**Files:**
- Modify: `src/browser/mod.rs`
- Test: `tests/browser_integration.rs` (gated; requires a real Chrome binary)

**Interfaces:**
- Consumes: `profile::{discover_chrome, prepare_profile}`, `frame::{Frame, FrameTx, frame_channel, FrameRx}`, `config::Config`, `geometry::Viewport`, `terminal::keyboard::{Key, Mods}`, `terminal::mouse::{MouseButton}`.
- Produces:
  - `pub struct Browser` controlling one headless page.
  - `Browser::launch(cfg: &Config, chrome: PathBuf) -> Result<(Browser, FrameRx)>` — launches headless Chromium with the dedicated profile, opens one page, starts the CDP handler task, returns the controller and a coalescing frame receiver.
  - `async fn navigate(&self, url: &str) -> Result<()>`
  - `async fn go_back(&self)`, `async fn go_forward(&self)`, `async fn reload(&self)`
  - `async fn set_viewport(&self, vp: Viewport, dpr: f64) -> Result<()>`
  - `async fn start_screencast(&self, quality: u8, vp: Viewport) -> Result<()>` — begins screencast; frames flow to the `FrameTx` captured by the handler task; each frame is acked.
  - `async fn insert_text(&self, text: &str) -> Result<()>` — `Input.insertText` (the Korean path).
  - `async fn dispatch_key(&self, key: Key, mods: Mods, down: bool) -> Result<()>` — `Input.dispatchKeyEvent`.
  - `async fn click(&self, x: f64, y: f64, button: MouseButton) -> Result<()>` — down+up mouse event pair.
  - `async fn scroll(&self, x: f64, y: f64, dy: f64) -> Result<()>` — `Input.dispatchMouseEvent` type `mouseWheel`.
  - `async fn current_url(&self) -> Option<String>`

Background — chromiumoxide usage facts:
- `chromiumoxide::{Browser, BrowserConfig, Page}`. `Browser::launch(config).await` returns `(Browser, Handler)`; the `Handler` stream must be driven on a spawned task (`while let Some(_) = handler.next().await {}`).
- `BrowserConfig::builder().chrome_executable(path).user_data_dir(dir).arg("--headless=new").build()`.
- CDP commands are issued with `page.execute(ParamsStruct).await`. Event subscription: `page.event_listener::<EventScreencastFrame>().await` yields a stream.
- Relevant CDP params live under `chromiumoxide::cdp::browser_protocol::{page, input, emulation}` and `chromiumoxide::cdp::js_protocol::runtime`. Exact type names: `page::StartScreencastParams`, `page::ScreencastFrameAckParams`, `page::EventScreencastFrame`, `input::DispatchKeyEventParams`, `input::DispatchMouseEventParams`, `input::InsertTextParams`, `emulation::SetDeviceMetricsOverrideParams`.

- [ ] **Step 1: Write the gated integration test `tests/browser_integration.rs`**

```rust
// Run with: WEBCAT_ITEST=1 cargo test --test browser_integration -- --nocapture
// Skipped unless WEBCAT_ITEST=1 and a chrome binary is discoverable.

#[path = "../src/error.rs"] mod error;
#[path = "../src/cli.rs"] mod cli;
#[path = "../src/config.rs"] mod config;
#[path = "../src/geometry.rs"] mod geometry;
#[path = "../src/terminal/mod.rs"] mod terminal;
#[path = "../src/browser/mod.rs"] mod browser;

use std::time::Duration;

fn itest_enabled() -> bool {
    std::env::var("WEBCAT_ITEST").is_ok()
}

#[tokio::test]
async fn navigate_and_screencast_and_korean_input() {
    if !itest_enabled() { eprintln!("skipped (set WEBCAT_ITEST=1)"); return; }

    let chrome = browser::profile::discover_chrome(None).expect("chrome");
    let tmp = std::env::temp_dir().join(format!("webcat-itest-{}", std::process::id()));
    let cfg = config::Config {
        profile_dir: tmp.clone(),
        chrome: Some(chrome.clone()),
        log_path: tmp.join("log"),
        quality: 70,
        dpr: 1.0,
        start_url: "about:blank".into(),
    };

    let (b, mut frames) = browser::Browser::launch(&cfg, chrome).await.expect("launch");
    let vp = geometry::Viewport { width_px: 800, height_px: 600 };
    b.set_viewport(vp, 1.0).await.unwrap();

    // Page with a text input we can focus and type Korean into.
    let html = "data:text/html,<input id=t autofocus>";
    b.navigate(html).await.unwrap();

    b.start_screencast(70, vp).await.unwrap();
    // We should receive at least one frame within a few seconds.
    let got = tokio::time::timeout(Duration::from_secs(5), frames.recv()).await;
    assert!(matches!(got, Ok(Some(_))), "expected a screencast frame");

    // Korean round-trip via insertText.
    b.insert_text("안녕하세요").await.unwrap();
    let value = b.eval_string("document.getElementById('t').value").await.unwrap();
    assert_eq!(value, "안녕하세요");

    let _ = std::fs::remove_dir_all(&tmp);
}
```

- [ ] **Step 2: Run the test to verify it fails to compile/find symbols**

Run: `WEBCAT_ITEST=1 cargo test --test browser_integration`
Expected: FAIL — `Browser`, `launch`, `eval_string`, etc. not found.

- [ ] **Step 3: Implement the controller in `src/browser/mod.rs`**

```rust
pub mod profile;
pub mod frame;

use std::path::PathBuf;
use std::sync::Arc;
use futures::StreamExt;

use chromiumoxide::{Browser as CdpBrowser, BrowserConfig, Page};
use chromiumoxide::cdp::browser_protocol::page::{
    StartScreencastParams, StartScreencastFormat, ScreencastFrameAckParams, EventScreencastFrame,
    NavigateParams,
};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType,
    DispatchMouseEventParams, DispatchMouseEventType, MouseButton as CdpMouseButton,
    InsertTextParams,
};
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use base64::Engine;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::geometry::Viewport;
use crate::terminal::keyboard::{Key, Mods};
use crate::terminal::mouse::MouseButton;
use frame::{Frame, FrameTx, FrameRx, frame_channel};

pub struct Browser {
    // Owning the CdpBrowser keeps the headless Chromium child alive for the
    // controller's lifetime and ensures it is killed when Browser is dropped
    // (satisfies the spec's "clean up Chromium on exit" requirement).
    _cdp: CdpBrowser,
    page: Arc<Page>,
    frame_tx: Arc<FrameTx>,
}

impl Browser {
    pub async fn launch(cfg: &Config, chrome: PathBuf) -> Result<(Browser, FrameRx)> {
        profile::prepare_profile(&cfg.profile_dir)?;

        let bc = BrowserConfig::builder()
            .chrome_executable(chrome)
            .user_data_dir(cfg.profile_dir.clone())
            .arg("--headless=new")
            .arg("--hide-scrollbars")
            .arg("--remote-allow-origins=*")
            .build()
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;

        let (cdp, mut handler) = CdpBrowser::launch(bc)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;

        // Drive the CDP handler in the background; if it ends, the browser is gone.
        tokio::spawn(async move {
            while let Some(ev) = handler.next().await {
                if ev.is_err() { break; }
            }
        });

        let page = Arc::new(
            cdp.new_page("about:blank")
                .await
                .map_err(|e| Error::Other(anyhow::anyhow!(e)))?,
        );

        let (tx, rx) = frame_channel();
        let frame_tx = Arc::new(tx);

        // Subscribe to screencast frames and forward to the coalescing channel.
        let listener_page = page.clone();
        let listener_tx = frame_tx.clone();
        tokio::spawn(async move {
            let mut events = match listener_page.event_listener::<EventScreencastFrame>().await {
                Ok(s) => s,
                Err(_) => return,
            };
            while let Some(ev) = events.next().await {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(ev.data.as_bytes())
                    .unwrap_or_default();
                listener_tx.send(Frame { jpeg: bytes });
                // Ack so Chromium keeps sending frames.
                let _ = listener_page
                    .execute(ScreencastFrameAckParams::new(ev.session_id))
                    .await;
            }
        });

        Ok((Browser { _cdp: cdp, page, frame_tx }, rx))
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.page
            .execute(NavigateParams::new(url.to_string()))
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn go_back(&self) { let _ = self.page.evaluate("history.back()").await; }
    pub async fn go_forward(&self) { let _ = self.page.evaluate("history.forward()").await; }
    pub async fn reload(&self) { let _ = self.page.reload().await; }

    pub async fn set_viewport(&self, vp: Viewport, dpr: f64) -> Result<()> {
        self.page
            .execute(SetDeviceMetricsOverrideParams::new(
                vp.width_px as i64, vp.height_px as i64, dpr, false,
            ))
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn start_screencast(&self, quality: u8, vp: Viewport) -> Result<()> {
        let params = StartScreencastParams::builder()
            .format(StartScreencastFormat::Jpeg)
            .quality(quality as i64)
            .max_width(vp.width_px as i64)
            .max_height(vp.height_px as i64)
            .build();
        self.page
            .execute(params)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn insert_text(&self, text: &str) -> Result<()> {
        self.page
            .execute(InsertTextParams::new(text.to_string()))
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn dispatch_key(&self, key: Key, mods: Mods, down: bool) -> Result<()> {
        let (vk, text) = key_to_cdp(key);
        let mut p = DispatchKeyEventParams::builder()
            .r#type(if down { DispatchKeyEventType::KeyDown } else { DispatchKeyEventType::KeyUp })
            .modifiers(encode_mods(mods))
            .windows_virtual_key_code(vk);
        if let Some(t) = text {
            p = p.text(t);
        }
        let params = p.build().map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        self.page.execute(params).await.map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn click(&self, x: f64, y: f64, button: MouseButton) -> Result<()> {
        let b = match button {
            MouseButton::Left => CdpMouseButton::Left,
            MouseButton::Middle => CdpMouseButton::Middle,
            MouseButton::Right => CdpMouseButton::Right,
        };
        for ty in [DispatchMouseEventType::MousePressed, DispatchMouseEventType::MouseReleased] {
            let params = DispatchMouseEventParams::builder()
                .r#type(ty)
                .x(x)
                .y(y)
                .button(b.clone())
                .click_count(1)
                .build()
                .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
            self.page.execute(params).await.map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        }
        Ok(())
    }

    pub async fn scroll(&self, x: f64, y: f64, dy: f64) -> Result<()> {
        let params = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MouseWheel)
            .x(x)
            .y(y)
            .delta_x(0.0)
            .delta_y(dy)
            .build()
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        self.page.execute(params).await.map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn current_url(&self) -> Option<String> {
        self.page.url().await.ok().flatten()
    }

    pub async fn eval_string(&self, js: &str) -> Result<String> {
        let v = self.page.evaluate(js).await.map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(v.into_value::<String>().unwrap_or_default())
    }
}

fn encode_mods(m: Mods) -> i64 {
    // CDP modifier bitmask: Alt=1, Ctrl=2, Meta=4, Shift=8.
    let mut bits = 0;
    if m.alt { bits |= 1; }
    if m.ctrl { bits |= 2; }
    if m.meta { bits |= 4; }
    if m.shift { bits |= 8; }
    bits
}

/// Map a Key to (windows virtual key code, optional text to emit).
fn key_to_cdp(key: Key) -> (i64, Option<String>) {
    match key {
        Key::Enter => (13, Some("\r".into())),
        Key::Backspace => (8, None),
        Key::Tab => (9, Some("\t".into())),
        Key::Esc => (27, None),
        Key::Up => (38, None),
        Key::Down => (40, None),
        Key::Left => (37, None),
        Key::Right => (39, None),
        Key::Home => (36, None),
        Key::End => (35, None),
        Key::PageUp => (33, None),
        Key::PageDown => (34, None),
        Key::Delete => (46, None),
        Key::F(n) => (111 + n as i64, None),
        Key::Char(c) => (c.to_ascii_uppercase() as i64, Some(c.to_string())),
    }
}
```

Implementer note: chromiumoxide's exact builder method names and required args can vary slightly by version (e.g. `r#type` vs `set_type`, `new(...)` arity). If a `0.7.x` signature differs, adjust the call to match the generated CDP types — the CDP semantics (param names/fields) are stable and authoritative. Keep behavior identical.

- [ ] **Step 4: Run the integration test**

Run: `WEBCAT_ITEST=1 cargo test --test browser_integration -- --nocapture`
Expected: PASS — a frame is received and `안녕하세요` round-trips through the input element. (If no Chrome is installed, the test prints "skipped".)

- [ ] **Step 5: Commit**

```bash
git add src/browser/mod.rs tests/browser_integration.rs
git commit -m "feat: browser controller with navigate, screencast, and input dispatch"
```

---

## Task 13: Milestone 2+3 — live page on screen (read-only)

**Files:**
- Create: `src/bin/live_view.rs` (throwaway smoke; removed in Task 16)
- Test: manual smoke

**Interfaces:**
- Consumes: `Browser`, `FrameRx`, `Renderer`, capability/cell-size, `RestoreGuard`, geometry.
- Produces: a binary that opens a URL and continuously renders screencast frames until a key is pressed.

- [ ] **Step 1: Create `src/bin/live_view.rs`**

```rust
use std::io::Write;
use std::time::Duration;

#[path = "../error.rs"] mod error;
#[path = "../cli.rs"] mod cli;
#[path = "../config.rs"] mod config;
#[path = "../geometry.rs"] mod geometry;
#[path = "../renderer/mod.rs"] mod renderer;
#[path = "../terminal/mod.rs"] mod terminal;
#[path = "../browser/mod.rs"] mod browser;

use geometry::{GridSize, CellSize};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args().nth(1).unwrap_or_else(|| "https://example.com".into());

    terminal::raw::install_panic_and_signal_hooks();
    let mut guard = terminal::raw::RestoreGuard::enter()?;
    if !terminal::capability::detect_kitty_graphics(Duration::from_millis(300)) {
        guard.restore();
        eprintln!("Not a kitty-graphics terminal.");
        std::process::exit(1);
    }
    let cell = terminal::capability::query_cell_size(Duration::from_millis(300));
    let (cols, rows) = crossterm::terminal::size()?;
    let grid = GridSize { cols, rows };
    let cell = CellSize { w: cell.w, h: cell.h };
    let vp = geometry::page_viewport(grid, cell, 1);

    let chrome = browser::profile::discover_chrome(None)?;
    let cfg = config::Config {
        profile_dir: std::env::temp_dir().join("webcat-liveview"),
        chrome: Some(chrome.clone()),
        log_path: std::env::temp_dir().join("webcat-liveview/log"),
        quality: 70, dpr: 1.0, start_url: url.clone(),
    };
    let (b, mut frames) = browser::Browser::launch(&cfg, chrome).await?;
    b.set_viewport(vp, 1.0).await?;
    b.navigate(&url).await?;
    b.start_screencast(70, vp).await?;

    let renderer = renderer::Renderer::new(grid, cell);
    // Render frames for 10 seconds.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            maybe = frames.recv() => {
                match maybe {
                    Some(f) => {
                        let mut out = std::io::stdout();
                        out.write_all(&renderer.present_jpeg_bytes(&f.jpeg))?;
                        out.flush()?;
                    }
                    None => break,
                }
            }
        }
    }
    guard.restore();
    Ok(())
}
```

- [ ] **Step 2: Build**

Run: `cargo build --bin live_view`
Expected: compiles.

- [ ] **Step 3: Manual smoke (in Kitty)**

Run: `cargo run --bin live_view https://example.com`
Expected: the rendered example.com page appears and updates; after 10s the terminal restores cleanly. Try a page with motion (e.g. a CSS animation demo) and confirm frames update without flicker and without lag accumulation.

- [ ] **Step 4: Commit**

```bash
git add src/bin/live_view.rs
git commit -m "feat: milestone 2+3 — live screencast rendered to terminal"
```

---

## Task 14: Action enum + InputMapper

**Files:**
- Create: `src/input/action.rs`
- Create: `src/input/mod.rs`
- Test: inline `#[cfg(test)]` in `src/input/mod.rs`

**Interfaces:**
- Consumes: `terminal::keyboard::{Key, Mods, KeyEvent}`, `terminal::mouse::{MouseEvent, MouseKind, MouseButton}`, `geometry::{CellSize, cell_to_pixel}`.
- Produces:
  - `src/input/action.rs`:
    ```rust
    pub enum Action {
        None,
        Quit,
        InsertText(String),
        Key(crate::terminal::keyboard::Key, crate::terminal::keyboard::Mods),
        ClickPixel { x: f64, y: f64, button: crate::terminal::mouse::MouseButton },
        ScrollPixel { x: f64, y: f64, dy: f64 },
        EnterUrlMode,
        EnterInsertMode,
        ExitInsertMode,
        EnterHintMode,
        UrlInputChar(String),
        UrlBackspace,
        UrlSubmit,
        UrlCancel,
        HintKey(char),
        GoBack,
        Reload,
    }
    ```
  - `src/input/mod.rs`:
    - `pub enum Mode { Normal, Insert, UrlInput, Hint }`
    - `pub struct InputMapper { pub mode: Mode, cell: CellSize }`
    - `InputMapper::new(cell: CellSize) -> Self`
    - `InputMapper::on_key(&mut self, ev: KeyEvent) -> Action`
    - `InputMapper::on_mouse(&mut self, ev: MouseEvent) -> Action`

Modes follow vim conventions. **Normal mode is command-only — it never sends text to the page**; text entry (Korean and English) happens exclusively in **Insert mode**.

Key bindings (Normal mode): `i` → EnterInsertMode; `:` → EnterUrlMode; `f` → EnterHintMode; `q` → Quit; `H` (shift+h) → GoBack; `r` → Reload; `j`/`k` → ScrollPixel (down/up by one line ~ 3*cell.h); arrow keys → forwarded as `Key`; any other printable char → `None` (commands only, no text leaks to the page).

Insert mode: Esc → ExitInsertMode (back to Normal); Enter/Backspace/Tab/Delete/arrows → `Key`; any printable text (incl. Korean via `ev.text`) → `InsertText`.

UrlInput mode: printable text (incl. Korean via `ev.text`) → UrlInputChar; Backspace → UrlBackspace; Enter → UrlSubmit; Esc → UrlCancel.

Hint mode: a-z char → HintKey; Esc → UrlCancel (reused to exit).

Mouse (any mode): Down(Left) → ClickPixel; WheelUp/Down → ScrollPixel ±(3*cell.h).

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::keyboard::{Key, Mods, KeyEvent};
    use crate::terminal::mouse::{MouseEvent, MouseKind, MouseButton};
    use crate::geometry::CellSize;
    use crate::input::action::Action;

    fn mapper() -> InputMapper { InputMapper::new(CellSize { w: 8, h: 16 }) }
    fn ev(key: Key, text: Option<&str>) -> KeyEvent {
        KeyEvent { key, mods: Mods::none(), text: text.map(|s| s.to_string()) }
    }

    #[test]
    fn colon_enters_url_mode() {
        let mut m = mapper();
        let a = m.on_key(ev(Key::Char(':'), Some(":")));
        assert!(matches!(a, Action::EnterUrlMode));
        assert!(matches!(m.mode, Mode::UrlInput));
    }

    #[test]
    fn korean_text_in_url_mode_is_captured() {
        let mut m = mapper();
        m.on_key(ev(Key::Char(':'), Some(":")));
        let a = m.on_key(ev(Key::Char('가'), Some("안녕")));
        match a {
            Action::UrlInputChar(s) => assert_eq!(s, "안녕"),
            other => panic!("expected UrlInputChar, got {other:?}"),
        }
    }

    #[test]
    fn i_enters_insert_mode_and_korean_inserts_text() {
        let mut m = mapper();
        assert!(matches!(m.on_key(ev(Key::Char('i'), Some("i"))), Action::EnterInsertMode));
        assert!(matches!(m.mode, Mode::Insert));
        let a = m.on_key(ev(Key::Char('하'), Some("하세요")));
        match a {
            Action::InsertText(s) => assert_eq!(s, "하세요"),
            other => panic!("expected InsertText, got {other:?}"),
        }
    }

    #[test]
    fn normal_mode_does_not_leak_text_to_page() {
        let mut m = mapper();
        // A printable letter that is not a command must be swallowed in Normal mode.
        assert!(matches!(m.on_key(ev(Key::Char('z'), Some("z"))), Action::None));
    }

    #[test]
    fn esc_exits_insert_mode() {
        let mut m = mapper();
        m.on_key(ev(Key::Char('i'), Some("i")));
        assert!(matches!(m.on_key(ev(Key::Esc, None)), Action::ExitInsertMode));
        assert!(matches!(m.mode, Mode::Normal));
    }

    #[test]
    fn left_click_maps_to_pixel_center() {
        let mut m = mapper();
        let a = m.on_mouse(MouseEvent { kind: MouseKind::Down(MouseButton::Left), col: 2, row: 1 });
        match a {
            Action::ClickPixel { x, y, .. } => assert_eq!((x, y), (20.0, 24.0)),
            other => panic!("expected ClickPixel, got {other:?}"),
        }
    }

    #[test]
    fn wheel_scrolls() {
        let mut m = mapper();
        let a = m.on_mouse(MouseEvent { kind: MouseKind::WheelDown, col: 0, row: 0 });
        assert!(matches!(a, Action::ScrollPixel { dy, .. } if dy > 0.0));
    }

    #[test]
    fn q_quits_in_normal_mode() {
        let mut m = mapper();
        assert!(matches!(m.on_key(ev(Key::Char('q'), Some("q"))), Action::Quit));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test input::`
Expected: FAIL.

- [ ] **Step 3: Implement `src/input/action.rs`** (exactly the enum shown in Interfaces, with `#[derive(Debug)]`)

```rust
use crate::terminal::keyboard::{Key, Mods};
use crate::terminal::mouse::MouseButton;

#[derive(Debug)]
pub enum Action {
    None,
    Quit,
    InsertText(String),
    Key(Key, Mods),
    ClickPixel { x: f64, y: f64, button: MouseButton },
    ScrollPixel { x: f64, y: f64, dy: f64 },
    EnterUrlMode,
    EnterInsertMode,
    ExitInsertMode,
    EnterHintMode,
    UrlInputChar(String),
    UrlBackspace,
    UrlSubmit,
    UrlCancel,
    HintKey(char),
    GoBack,
    Reload,
}
```

- [ ] **Step 4: Implement `src/input/mod.rs`**

```rust
pub mod action;
pub mod hints;

use crate::geometry::{CellSize, cell_to_pixel};
use crate::terminal::keyboard::{Key, KeyEvent, Mods};
use crate::terminal::mouse::{MouseEvent, MouseKind, MouseButton};
use action::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode { Normal, Insert, UrlInput, Hint }

pub struct InputMapper {
    pub mode: Mode,
    cell: CellSize,
}

impl InputMapper {
    pub fn new(cell: CellSize) -> Self {
        InputMapper { mode: Mode::Normal, cell }
    }

    pub fn on_key(&mut self, ev: KeyEvent) -> Action {
        match self.mode {
            Mode::Normal => self.on_key_normal(ev),
            Mode::Insert => self.on_key_insert(ev),
            Mode::UrlInput => self.on_key_url(ev),
            Mode::Hint => self.on_key_hint(ev),
        }
    }

    fn on_key_normal(&mut self, ev: KeyEvent) -> Action {
        let line = 3.0 * self.cell.h as f64;
        match ev.key {
            Key::Char('i') => { self.mode = Mode::Insert; Action::EnterInsertMode }
            Key::Char(':') => { self.mode = Mode::UrlInput; Action::EnterUrlMode }
            Key::Char('f') => { self.mode = Mode::Hint; Action::EnterHintMode }
            Key::Char('q') => Action::Quit,
            Key::Char('r') => Action::Reload,
            Key::Char('H') => Action::GoBack,
            Key::Char('j') => Action::ScrollPixel { x: 0.0, y: 0.0, dy: line },
            Key::Char('k') => Action::ScrollPixel { x: 0.0, y: 0.0, dy: -line },
            Key::Up | Key::Down | Key::Left | Key::Right => Action::Key(ev.key, ev.mods),
            // Normal mode is command-only: any other key (incl. printable text) is swallowed.
            _ => Action::None,
        }
    }

    fn on_key_insert(&mut self, ev: KeyEvent) -> Action {
        match ev.key {
            Key::Esc => { self.mode = Mode::Normal; Action::ExitInsertMode }
            Key::Enter | Key::Backspace | Key::Tab | Key::Delete
            | Key::Up | Key::Down | Key::Left | Key::Right => Action::Key(ev.key, ev.mods),
            _ => {
                // Printable text (incl. Korean) goes to the focused page element.
                if let Some(t) = ev.text { Action::InsertText(t) } else { Action::None }
            }
        }
    }

    fn on_key_url(&mut self, ev: KeyEvent) -> Action {
        match ev.key {
            Key::Esc => { self.mode = Mode::Normal; Action::UrlCancel }
            Key::Enter => { self.mode = Mode::Normal; Action::UrlSubmit }
            Key::Backspace => Action::UrlBackspace,
            _ => {
                if let Some(t) = ev.text { Action::UrlInputChar(t) } else { Action::None }
            }
        }
    }

    fn on_key_hint(&mut self, ev: KeyEvent) -> Action {
        match ev.key {
            Key::Esc => { self.mode = Mode::Normal; Action::UrlCancel }
            Key::Char(c) if c.is_ascii_alphabetic() => Action::HintKey(c.to_ascii_lowercase()),
            _ => Action::None,
        }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> Action {
        let line = 3.0 * self.cell.h as f64;
        let (x, y) = cell_to_pixel(ev.col, ev.row, self.cell);
        match ev.kind {
            MouseKind::Down(MouseButton::Left) =>
                Action::ClickPixel { x, y, button: MouseButton::Left },
            MouseKind::Down(b) => Action::ClickPixel { x, y, button: b },
            MouseKind::WheelDown => Action::ScrollPixel { x, y, dy: line },
            MouseKind::WheelUp => Action::ScrollPixel { x, y, dy: -line },
            _ => Action::None,
        }
    }
}
```

- [ ] **Step 5: Create a stub `src/input/hints.rs` so the module compiles**

```rust
/// Generate hint labels (a, s, d, f, ...) for `n` clickable elements.
pub fn hint_labels(n: usize) -> Vec<String> {
    const ALPHA: &[u8] = b"asdfghjklqwertyuiopzxcvbnm";
    if n <= ALPHA.len() {
        (0..n).map(|i| (ALPHA[i] as char).to_string()).collect()
    } else {
        // Two-letter labels for larger counts.
        let mut out = Vec::with_capacity(n);
        'outer: for &a in ALPHA {
            for &b in ALPHA {
                out.push(format!("{}{}", a as char, b as char));
                if out.len() == n { break 'outer; }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_letters_for_small_counts() {
        assert_eq!(hint_labels(3), vec!["a", "s", "d"]);
    }
    #[test]
    fn two_letters_when_exhausted() {
        let labels = hint_labels(30);
        assert_eq!(labels.len(), 30);
        assert_eq!(labels[26].len(), 2);
    }
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test input::`
Expected: PASS (mapper + hints tests).

- [ ] **Step 7: Wire module + commit**

Add `mod input;` to `src/main.rs`, then:

```bash
git add src/input/ src/main.rs
git commit -m "feat: input mapper with modes, Korean text, mouse, and hint labels"
```

---

## Task 15: UI overlays — status bar, URL prompt, hints

**Files:**
- Create: `src/ui/mod.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `geometry::{GridSize, CellSize}`, `input::hints::hint_labels`.
- Produces:
  - `pub struct Ui { grid: GridSize }`
  - `Ui::new(grid: GridSize) -> Self`
  - `Ui::status_bar(&self, url: &str, loading: bool) -> Vec<u8>` — bytes to draw a single status line at the bottom row (`CSI <rows>;1H`, clear line, write text). Truncates to width.
  - `Ui::url_prompt(&self, buffer: &str) -> Vec<u8>` — draws `: <buffer>` on the bottom row with a visible cursor position.
  - `Ui::hint_overlay(&self, hints: &[(String, u16, u16)]) -> Vec<u8>` — for each `(label, col, row)` draws the label at that cell with reverse video.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::GridSize;

    fn ui() -> Ui { Ui::new(GridSize { cols: 80, rows: 24 }) }

    #[test]
    fn status_bar_targets_bottom_row() {
        let out = String::from_utf8(ui().status_bar("https://example.com", false)).unwrap();
        assert!(out.contains("\x1b[24;1H")); // rows=24 -> bottom row
        assert!(out.contains("example.com"));
    }

    #[test]
    fn status_bar_truncates_long_url() {
        let long = "https://example.com/".to_string() + &"a".repeat(200);
        let out = String::from_utf8(ui().status_bar(&long, false)).unwrap();
        // The visible payload must not exceed the 80-col width (plus escapes).
        let visible: String = out.chars().filter(|c| !c.is_control() && *c != '[').collect();
        assert!(visible.len() <= 80 + 8);
    }

    #[test]
    fn url_prompt_shows_buffer() {
        let out = String::from_utf8(ui().url_prompt("git")).unwrap();
        assert!(out.contains(": git"));
    }

    #[test]
    fn hint_overlay_places_labels() {
        let out = String::from_utf8(ui().hint_overlay(&[("a".into(), 5, 9)])).unwrap();
        assert!(out.contains("\x1b[10;6H")); // row+1, col+1
        assert!(out.contains('a'));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test ui::`
Expected: FAIL.

- [ ] **Step 3: Implement `src/ui/mod.rs`**

```rust
use crate::geometry::GridSize;

const REVERSE: &str = "\x1b[7m";
const RESET: &str = "\x1b[0m";
const CLEAR_LINE: &str = "\x1b[2K";

pub struct Ui { grid: GridSize }

impl Ui {
    pub fn new(grid: GridSize) -> Self { Ui { grid } }

    fn bottom_row(&self) -> u16 { self.grid.rows }

    pub fn status_bar(&self, url: &str, loading: bool) -> Vec<u8> {
        let prefix = if loading { "⟳ " } else { "  " };
        let width = self.grid.cols as usize;
        let mut text = format!("{prefix}{url}");
        if text.chars().count() > width {
            text = text.chars().take(width).collect();
        }
        format!("\x1b[{};1H{CLEAR_LINE}{REVERSE}{text}{RESET}", self.bottom_row()).into_bytes()
    }

    pub fn url_prompt(&self, buffer: &str) -> Vec<u8> {
        let width = self.grid.cols as usize;
        let mut text = format!(": {buffer}");
        if text.chars().count() > width {
            text = text.chars().take(width).collect();
        }
        format!("\x1b[{};1H{CLEAR_LINE}{text}", self.bottom_row()).into_bytes()
    }

    pub fn hint_overlay(&self, hints: &[(String, u16, u16)]) -> Vec<u8> {
        let mut out = String::new();
        for (label, col, row) in hints {
            out.push_str(&format!(
                "\x1b[{};{}H{REVERSE}{label}{RESET}",
                row + 1, col + 1
            ));
        }
        out.into_bytes()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test ui::`
Expected: PASS.

- [ ] **Step 5: Wire module + commit**

Add `mod ui;` to `src/main.rs`, then:

```bash
git add src/ui/mod.rs src/main.rs
git commit -m "feat: UI overlays for status bar, url prompt, and hints"
```

---

## Task 16: App event loop + real `main` (Milestone 4 — interactive)

**Files:**
- Create: `src/app.rs`
- Create: `src/terminal/input.rs` (raw stdin → RawInput stream)
- Modify: `src/terminal/mod.rs` (add `pub mod input;`)
- Modify: `src/main.rs` (wire everything; init logging)
- Delete: `src/bin/static_frame.rs`, `src/bin/live_view.rs`
- Test: inline parser tests in `src/terminal/input.rs`; the loop itself is covered by manual smoke.

**Interfaces:**
- Consumes: all prior modules.
- Produces:
  - `src/terminal/input.rs`:
    - `pub enum RawInput { Key(crate::terminal::keyboard::KeyEvent), Mouse(crate::terminal::mouse::MouseEvent), Resize }`
    - `pub fn classify(seq: &str) -> Option<RawInput>` — routes a single decoded escape/char to a `RawInput` using the kitty/mouse parsers; plain UTF-8 text becomes a `Key(Char, text=Some)`.
    - `pub fn input_stream() -> impl futures::Stream<Item = RawInput>` — uses `crossterm::event::EventStream` for resize + a raw byte reader for kitty/mouse sequences. (Implementation may lean on crossterm's event reader where it already decodes kitty keys; otherwise read raw bytes and call `classify`.)
  - `src/app.rs`:
    - `pub async fn run(cfg: Config) -> Result<()>` — the orchestrator.

- [ ] **Step 1: Write failing tests for `classify`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::keyboard::Key;

    #[test]
    fn classifies_kitty_key() {
        match classify("\x1b[97u").unwrap() {
            RawInput::Key(ev) => assert!(matches!(ev.key, Key::Char('a'))),
            _ => panic!("expected key"),
        }
    }

    #[test]
    fn classifies_sgr_mouse() {
        assert!(matches!(classify("\x1b[<0;5;9M"), Some(RawInput::Mouse(_))));
    }

    #[test]
    fn classifies_plain_utf8_text() {
        match classify("가").unwrap() {
            RawInput::Key(ev) => assert_eq!(ev.text.as_deref(), Some("가")),
            _ => panic!("expected key with text"),
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test terminal::input`
Expected: FAIL.

- [ ] **Step 3: Implement `src/terminal/input.rs`**

```rust
use crate::terminal::keyboard::{parse_kitty_key, Key, KeyEvent, Mods};
use crate::terminal::mouse::parse_sgr_mouse;

#[derive(Debug)]
pub enum RawInput {
    Key(KeyEvent),
    Mouse(crate::terminal::mouse::MouseEvent),
    Resize,
}

/// Route one decoded sequence (a full escape or a plain UTF-8 grapheme/text) to a RawInput.
pub fn classify(seq: &str) -> Option<RawInput> {
    if seq.starts_with("\x1b[<") {
        return parse_sgr_mouse(seq).map(RawInput::Mouse);
    }
    if seq.starts_with("\x1b[") && seq.ends_with('u') {
        return parse_kitty_key(seq).map(RawInput::Key);
    }
    // Plain text (the OS IME already composed Korean into final UTF-8).
    if !seq.is_empty() && !seq.starts_with('\x1b') {
        let first = seq.chars().next()?;
        return Some(RawInput::Key(KeyEvent {
            key: Key::Char(first),
            mods: Mods::none(),
            text: Some(seq.to_string()),
        }));
    }
    None
}
```

Implementer note on the live stream: with the Kitty keyboard protocol pushed (Task 6), Kitty reports keys in the `CSI … u` form and Korean committed text in the text field, so reading raw stdin bytes and splitting on escape boundaries before calling `classify` is sufficient. crossterm 0.28's `event::read` also understands the Kitty protocol when `PushKeyboardEnhancementFlags` is used; either path is acceptable as long as the manual smoke in Step 6 passes (especially Korean input). Build `input_stream()` with whichever path you verify works; below is the raw-byte version.

```rust
pub fn input_stream() -> impl futures::Stream<Item = RawInput> {
    use futures::stream::StreamExt;
    // Resize signals via crossterm's EventStream; key/mouse via raw byte reader on a blocking task.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RawInput>();

    // Resize watcher.
    let tx_resize = tx.clone();
    tokio::spawn(async move {
        let mut ev = crossterm::event::EventStream::new();
        while let Some(Ok(e)) = ev.next().await {
            if let crossterm::event::Event::Resize(_, _) = e {
                let _ = tx_resize.send(RawInput::Resize);
            }
        }
    });

    // Raw byte reader.
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Split the chunk into escape-delimited sequences and plain text.
                    for seq in split_sequences(&buf[..n]) {
                        if let Some(ri) = classify(&seq) {
                            if tx.send(ri).is_err() { return; }
                        }
                    }
                }
            }
        }
    });

    tokio_stream::wrappers::UnboundedReceiverStream::new(rx)
}

/// Split a raw byte chunk into individual sequences: each ESC-introduced control
/// sequence (up to its final byte) and each run of plain UTF-8 text.
fn split_sequences(bytes: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    let mut text = String::new();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if !text.is_empty() { out.push(std::mem::take(&mut text)); }
            let mut esc = String::from(c);
            // Consume until a plausible terminator (u, M, m, t, letter, or ST).
            while let Some(&n) = chars.peek() {
                esc.push(n);
                chars.next();
                if matches!(n, 'u' | 'M' | 'm' | 't' | '\\') || n.is_ascii_alphabetic() {
                    break;
                }
            }
            out.push(esc);
        } else {
            text.push(c);
        }
    }
    if !text.is_empty() { out.push(text); }
    out
}
```

Add deps for the stream wrappers in `Cargo.toml`:

```toml
tokio-stream = "0.1"
```

- [ ] **Step 4: Run parser tests to verify they pass**

Run: `cargo test terminal::input`
Expected: PASS.

- [ ] **Step 5: Implement `src/app.rs`**

```rust
use std::io::Write;
use std::time::Duration;
use futures::StreamExt;

use crate::browser::Browser;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::geometry::{self, CellSize, GridSize};
use crate::input::{InputMapper, Mode};
use crate::input::action::Action;
use crate::renderer::Renderer;
use crate::terminal::input::{input_stream, RawInput};
use crate::ui::Ui;

pub async fn run(cfg: Config) -> Result<()> {
    crate::terminal::raw::install_panic_and_signal_hooks();
    let mut guard = crate::terminal::raw::RestoreGuard::enter()?;

    if !crate::terminal::capability::detect_kitty_graphics(Duration::from_millis(300)) {
        guard.restore();
        return Err(Error::UnsupportedTerminal(
            "Kitty graphics protocol not detected".into(),
        ));
    }

    let cell_raw = crate::terminal::capability::query_cell_size(Duration::from_millis(300));
    let (cols, rows) = crossterm::terminal::size()?;
    let mut grid = GridSize { cols, rows };
    let cell = CellSize { w: cell_raw.w, h: cell_raw.h };
    let mut vp = geometry::page_viewport(grid, cell, 1);

    let chrome = crate::browser::profile::discover_chrome(cfg.chrome.as_deref())?;
    let (browser, mut frames) = Browser::launch(&cfg, chrome).await?;
    browser.set_viewport(vp, cfg.dpr).await?;
    browser.navigate(&cfg.start_url).await?;
    browser.start_screencast(cfg.quality, vp).await?;

    let mut renderer = Renderer::new(grid, cell);
    let ui = Ui::new(grid);
    let mut mapper = InputMapper::new(cell);
    let mut url_buffer = String::new();

    let mut inputs = Box::pin(input_stream());
    let mut out = std::io::stdout();

    loop {
        tokio::select! {
            // Frame branch: render the latest frame + status bar.
            maybe = frames.recv() => {
                let Some(f) = maybe else { break; };
                out.write_all(&renderer.present_jpeg_bytes(&f.jpeg))?;
                let url = browser.current_url().await.unwrap_or_default();
                let status = match mapper.mode {
                    Mode::Insert => format!("-- INSERT --  {url}"),
                    _ => url.clone(),
                };
                out.write_all(&ui.status_bar(&status, false))?;
                if mapper.mode == Mode::UrlInput {
                    out.write_all(&ui.url_prompt(&url_buffer))?;
                }
                out.flush()?;
            }

            // Input branch: handle one event.
            maybe = inputs.next() => {
                let Some(ri) = maybe else { break; };
                let action = match ri {
                    RawInput::Key(ev) => mapper.on_key(ev),
                    RawInput::Mouse(ev) => mapper.on_mouse(ev),
                    RawInput::Resize => {
                        let (c, r) = crossterm::terminal::size()?;
                        grid = GridSize { cols: c, rows: r };
                        vp = geometry::page_viewport(grid, cell, 1);
                        renderer.resize(grid, cell);
                        browser.set_viewport(vp, cfg.dpr).await?;
                        Action::None
                    }
                };

                match action {
                    Action::Quit => break,
                    Action::InsertText(t) => { let _ = browser.insert_text(&t).await; }
                    Action::Key(k, m) => {
                        let _ = browser.dispatch_key(k, m, true).await;
                        let _ = browser.dispatch_key(k, m, false).await;
                    }
                    Action::ClickPixel { x, y, button } => { let _ = browser.click(x, y, button).await; }
                    Action::ScrollPixel { x, y, dy } => { let _ = browser.scroll(x, y, dy).await; }
                    Action::GoBack => browser.go_back().await,
                    Action::Reload => browser.reload().await,
                    // Mode switches are applied inside the mapper; the app just
                    // acknowledges them (the status bar reflects mapper.mode).
                    Action::EnterInsertMode => {}
                    Action::ExitInsertMode => {}
                    Action::EnterUrlMode => { url_buffer.clear(); }
                    Action::UrlInputChar(s) => { url_buffer.push_str(&s); }
                    Action::UrlBackspace => { url_buffer.pop(); }
                    Action::UrlSubmit => {
                        let target = normalize_url(&url_buffer);
                        let _ = browser.navigate(&target).await;
                        url_buffer.clear();
                    }
                    Action::UrlCancel => { url_buffer.clear(); }
                    Action::EnterHintMode => { /* handled in Task 17 */ }
                    Action::HintKey(_) => { /* handled in Task 17 */ }
                    Action::None => {}
                }
            }
        }
    }

    guard.restore();
    Ok(())
}

fn normalize_url(input: &str) -> String {
    let t = input.trim();
    if t.contains("://") || t.starts_with("about:") {
        t.to_string()
    } else if t.contains('.') && !t.contains(' ') {
        format!("https://{t}")
    } else {
        format!("https://www.google.com/search?q={}", urlencode(t))
    }
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        b' ' => "+".to_string(),
        _ => format!("%{:02X}", b),
    }).collect()
}
```

- [ ] **Step 6: Rewrite `src/main.rs` to run the app**

```rust
mod app;
mod browser;
mod cli;
mod config;
mod error;
mod geometry;
mod input;
mod renderer;
mod terminal;
mod ui;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    let cfg = config::Config::resolve(cli)?;

    // File logging only — never touch the terminal screen.
    if let Some(parent) = cfg.log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new()
        .create(true).append(true).open(&cfg.log_path)?;
    tracing_subscriber::fmt()
        .with_writer(file)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("WEBCAT_LOG_LEVEL")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async {
        if let Err(e) = app::run(cfg).await {
            tracing::error!("fatal: {e}");
            eprintln!("webcat: {e}");
            std::process::exit(1);
        }
    });
    Ok(())
}
```

- [ ] **Step 7: Remove the throwaway smoke binaries**

```bash
git rm src/bin/static_frame.rs src/bin/live_view.rs
```

- [ ] **Step 8: Build + manual smoke (in Kitty)**

Run: `cargo build && cargo run -- https://example.com`
Expected:
- Page renders and updates.
- `j`/`k` and mouse wheel scroll; left-click follows links.
- `:` opens the URL prompt; typing a URL + Enter navigates.
- Click into a page text field, press `i` (status shows `-- INSERT --`), **type Korean and verify Hangul inserts correctly**, then `Esc` returns to Normal.
- `q` quits and the terminal is fully restored.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "feat: milestone 4 — interactive app event loop wiring all modules"
```

---

## Task 17: vim hint mode (collect → overlay → click)

**Files:**
- Modify: `src/browser/mod.rs` (add `collect_clickables`)
- Modify: `src/input/hints.rs` (add `Clickable` + assignment)
- Modify: `src/app.rs` (wire hint mode)
- Test: inline tests for label assignment; manual smoke for end-to-end.

**Interfaces:**
- Consumes: `Browser::eval_string`, `hints::hint_labels`, `geometry::cell_to_pixel`.
- Produces:
  - `browser`: `async fn collect_clickables(&self) -> Result<Vec<Clickable>>` where `pub struct Clickable { pub x: f64, pub y: f64 }` (page-pixel centers of clickable elements), implemented via a `Runtime.evaluate` returning JSON of bounding-rect centers.
  - `hints`: `pub fn assign(clickables: &[Clickable]) -> Vec<(String, Clickable)>` pairing labels with elements.

- [ ] **Step 1: Write failing test for assignment**

Add to `src/input/hints.rs`:

```rust
#[cfg(test)]
mod assign_tests {
    use super::*;
    use crate::browser::Clickable;

    #[test]
    fn assigns_label_per_element() {
        let cs = vec![
            Clickable { x: 1.0, y: 2.0 },
            Clickable { x: 3.0, y: 4.0 },
        ];
        let pairs = assign(&cs);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "a");
        assert_eq!(pairs[1].0, "s");
        assert_eq!(pairs[1].1.x, 3.0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test input::hints::assign_tests`
Expected: FAIL — `assign` / `Clickable` not found.

- [ ] **Step 3: Add `Clickable` and `collect_clickables` to `src/browser/mod.rs`**

```rust
#[derive(Debug, Clone, Copy)]
pub struct Clickable { pub x: f64, pub y: f64 }
```

Add this method inside `impl Browser`:

```rust
    pub async fn collect_clickables(&self) -> Result<Vec<Clickable>> {
        let js = r#"
            (() => {
              const sel = 'a,button,input,textarea,select,[role=button],[onclick]';
              const out = [];
              for (const el of document.querySelectorAll(sel)) {
                const r = el.getBoundingClientRect();
                if (r.width > 0 && r.height > 0 && r.bottom > 0 && r.right > 0
                    && r.top < innerHeight && r.left < innerWidth) {
                  out.push([r.left + r.width/2, r.top + r.height/2]);
                }
              }
              return JSON.stringify(out);
            })()
        "#;
        let json = self.eval_string(js).await?;
        let parsed: Vec<(f64, f64)> = serde_json::from_str(&json).unwrap_or_default();
        Ok(parsed.into_iter().map(|(x, y)| Clickable { x, y }).collect())
    }
```

Add `serde_json = "1"` to `Cargo.toml` `[dependencies]`.

- [ ] **Step 4: Implement `assign` in `src/input/hints.rs`**

```rust
use crate::browser::Clickable;

pub fn assign(clickables: &[Clickable]) -> Vec<(String, Clickable)> {
    let labels = hint_labels(clickables.len());
    labels.into_iter().zip(clickables.iter().copied()).collect()
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test input::hints`
Expected: PASS.

- [ ] **Step 6: Wire hint mode into `src/app.rs`**

Add a `hints: Vec<(String, Clickable)>` variable before the loop (`let mut hints: Vec<(String, crate::browser::Clickable)> = Vec::new();`). Replace the two hint placeholders in the `match action` block:

```rust
                    Action::EnterHintMode => {
                        let clickables = browser.collect_clickables().await.unwrap_or_default();
                        if clickables.is_empty() {
                            // Nothing to click; drop back to normal.
                            mapper.mode = crate::input::Mode::Normal;
                            out.write_all(&ui.status_bar("(no clickable elements)", false))?;
                            out.flush()?;
                        } else {
                            hints = crate::input::hints::assign(&clickables);
                            // Convert page-pixel centers to cell coords for overlay.
                            let overlay: Vec<(String, u16, u16)> = hints.iter().map(|(label, c)| {
                                let col = (c.x / cell.w as f64) as u16;
                                let row = (c.y / cell.h as f64) as u16;
                                (label.clone(), col, row)
                            }).collect();
                            out.write_all(&ui.hint_overlay(&overlay))?;
                            out.flush()?;
                        }
                    }
                    Action::HintKey(c) => {
                        if let Some((_, target)) = hints.iter().find(|(l, _)| l == &c.to_string()) {
                            let _ = browser.click(target.x, target.y, crate::terminal::mouse::MouseButton::Left).await;
                        }
                        mapper.mode = crate::input::Mode::Normal;
                        hints.clear();
                    }
```

Note: this v1 supports single-letter hint selection (covers up to 26 elements; two-letter labels are generated but selected by first match — acceptable for v1; multi-key hint capture is a follow-up).

- [ ] **Step 7: Build + manual smoke (in Kitty)**

Run: `cargo run -- https://example.com`
Press `f`: labels appear over links. Press a label letter: the link is clicked and the page navigates. Press `f` then `Esc`: overlay clears, back to normal.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: vim hint mode — collect clickables, overlay labels, click by key"
```

---

## Task 18: Error handling, reconnection, and navigation feedback

**Files:**
- Modify: `src/browser/mod.rs` (expose disconnection detection)
- Modify: `src/app.rs` (handle disconnect, navigation errors, JS dialogs)
- Test: gated integration check + manual smoke.

**Interfaces:**
- Consumes: existing browser/app interfaces.
- Produces:
  - `browser`: a `tokio::sync::watch::Receiver<bool>` "alive" signal, exposed as `fn alive(&self) -> tokio::sync::watch::Receiver<bool>`; set to `false` when the CDP handler task ends. Auto-dismiss JS dialogs via `Page.javascriptDialogOpening` → `Page.handleJavaScriptDialog{accept:false}`.
  - `app`: on `alive == false`, show "reconnecting…" in the status bar, attempt `Browser::launch` up to 3 times (re-navigating to the last URL), and give up with a clear message after 3 failures. On navigation failure, show the error in the status bar instead of a blank screen.

- [ ] **Step 1: Add the alive signal to `Browser`**

In `Browser::launch`, create `let (alive_tx, alive_rx) = tokio::sync::watch::channel(true);`, move `alive_tx` into the handler task and set `let _ = alive_tx.send(false);` after the `while let` loop ends. Store `alive_rx` in the `Browser` struct and add:

```rust
    pub fn alive(&self) -> tokio::sync::watch::Receiver<bool> {
        self.alive_rx.clone()
    }
```

(Add field `alive_rx: tokio::sync::watch::Receiver<bool>` to the struct and the constructor.)

- [ ] **Step 2: Subscribe to and auto-dismiss JS dialogs**

In `Browser::launch`, after creating the page, spawn:

```rust
        let dialog_page = page.clone();
        tokio::spawn(async move {
            use chromiumoxide::cdp::browser_protocol::page::{
                EventJavascriptDialogOpening, HandleJavaScriptDialogParams,
            };
            if let Ok(mut ev) = dialog_page.event_listener::<EventJavascriptDialogOpening>().await {
                while ev.next().await.is_some() {
                    let _ = dialog_page
                        .execute(HandleJavaScriptDialogParams::builder().accept(false).build().unwrap())
                        .await;
                }
            }
        });
```

- [ ] **Step 3: Handle disconnect + nav errors in `src/app.rs`**

Add an `alive` branch to the `tokio::select!` loop:

```rust
            changed = async {
                let mut a = browser_alive.clone();
                a.changed().await.ok();
                *a.borrow()
            } => {
                if changed == false {
                    out.write_all(&ui.status_bar("disconnected — reconnecting…", true))?;
                    out.flush()?;
                    match reconnect(&cfg, vp, cfg.dpr, &last_url).await {
                        Ok((nb, nf)) => { /* swap browser/frames; see note */ }
                        Err(_) => {
                            out.write_all(&ui.status_bar("browser unavailable (gave up after 3 tries)", false))?;
                            out.flush()?;
                            break;
                        }
                    }
                }
            }
```

Add the helper:

```rust
async fn reconnect(
    cfg: &Config, vp: geometry::Viewport, dpr: f64, last_url: &str,
) -> Result<(Browser, crate::browser::frame::FrameRx)> {
    let chrome = crate::browser::profile::discover_chrome(cfg.chrome.as_deref())?;
    for attempt in 1..=3 {
        match Browser::launch(cfg, chrome.clone()).await {
            Ok((b, f)) => {
                b.set_viewport(vp, dpr).await?;
                b.navigate(last_url).await?;
                b.start_screencast(cfg.quality, vp).await?;
                return Ok((b, f));
            }
            Err(e) => {
                tracing::warn!("reconnect attempt {attempt} failed: {e}");
                tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
            }
        }
    }
    Err(Error::BrowserDisconnected)
}
```

Implementation note: because `browser`/`frames` are owned by the loop, the cleanest structure is to make them `let mut browser = …; let mut frames = …;` and, on successful reconnect, reassign both and refresh `browser_alive = browser.alive();`. Track `last_url` by updating it on every successful `UrlSubmit`/hint navigation and from `current_url()` after load. Keep the borrow rules satisfied by performing the reassignment directly in the branch rather than in the helper if the borrow checker complains.

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: compiles.

- [ ] **Step 5: Manual smoke (in Kitty)**

- Navigate to a bad host (`:` then `http://nonexistent.invalid`): the status bar shows an error rather than hanging on a blank screen (Chromium renders its own error page, which is fine — verify it appears).
- Kill the Chromium process externally (`pkill -f headless`): webcat shows "reconnecting…", relaunches, and restores the last page; after 3 forced failures it exits cleanly with a message and a restored terminal.
- Trigger an `alert()` on a test page: it is auto-dismissed and the page keeps rendering.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: disconnect recovery, navigation error feedback, JS dialog auto-dismiss"
```

---

## Task 19: README + final polish

**Files:**
- Create: `README.md`
- Modify: any `#[allow(dead_code)]` that are now used (remove the attributes)
- Test: full `cargo test` + `cargo clippy`.

**Interfaces:** none new.

- [ ] **Step 1: Write `README.md`**

Include: what webcat is; requirements (Kitty terminal, Chrome/Chromium installed); install/build (`cargo build --release`); usage (`webcat <url>`, flags `--profile-dir/--chrome/--quality/--dpr`, env vars `WEBCAT_CHROME/WEBCAT_PROFILE_DIR/WEBCAT_LOG/WEBCAT_LOG_LEVEL`); keybindings (`i` insert mode / `Esc` normal mode, `:` URL, `f` hints, `j/k` scroll, `H` back, `r` reload, `q` quit, mouse click/scroll); the modal model (Normal = commands only, Insert = type into the focused field); Korean input note (enter Insert mode, then OS IME → committed text → `Input.insertText`); known limitations (single page/no tabs, single-letter hints, no in-field IME composition); where logs go.

- [ ] **Step 2: Run the full unit suite**

Run: `cargo test`
Expected: all unit tests PASS; the gated integration test prints "skipped" (or passes with `WEBCAT_ITEST=1`).

- [ ] **Step 3: Run clippy and fix warnings**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean. Remove any now-unnecessary `#[allow(dead_code)]`. Fix lints.

- [ ] **Step 4: Final manual acceptance (in Kitty)**

Walk the full v1 acceptance checklist:
- Open a real site, scroll (keys + wheel), click links, use `f` hints.
- `:` navigate by typing a URL and by typing a search query.
- Focus a text field, press `i` for Insert mode, and type **한글** (e.g. "안녕하세요") — verify it appears correctly in the field; `Esc` back to Normal.
- Resize the terminal — page reflows to the new size.
- Quit with `q` and Ctrl-C — terminal fully restored both ways.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: README and final clippy/polish pass"
```

---

## Self-Review

**1. Spec coverage:**
- §1 decisions (engine A / chromiumoxide, dedicated profile, Kitty, mouse+keyboard, Korean via insertText) → Tasks 1, 10, 12, 14, 16. ✓
- §2 architecture & process model (parent/child, CDP, tokio select loop) → Tasks 12, 16. ✓
- §3 components: `browser`→12/17/18, `renderer`→2/8, `terminal`→4/5/6/7/16, `input`→14, `ui`→15, `app`→16. ✓
- vim hints → Task 17. ✓
- §4 data flow & performance: JPEG passthrough→2/8, event-driven frames→12, screencast params (quality/maxW/maxH)→12, coalescing/backpressure→11, input independent path & select priority→16, resize flow→16, DPR mapping→3/16. ✓ (`everyNthFrame` is exposed by the CDP param but left at default in v1; the coalescing channel already bounds output — acceptable, noted here as an intentional v1 simplification.)
- §5 error handling: chrome-not-found→10, profile lock→10, crash/reconnect→18, capability gate→7/16, cell-size fallback→7, terminal restore on panic/signal→6, frame skip→implicit (decode failure path is passthrough; bad frames are dropped by Chromium), hint zero-elements→17, focusless input→insertText no-ops (documented), nav failure→18, JS dialogs/new windows→18 (dialogs dismissed; `window.open` opens in same page is Chromium default under one target — acceptable v1), logging to file→16. ✓
- §6 testing & stack: unit tests for renderer/input/parsers→2/3/4/5/14/15, integration for browser→12, E2E manual smoke→9/13/16/17/18/19, restore tests→6, dev order milestones→9/13/16. ✓

**2. Placeholder scan:** No "TBD/TODO/handle edge cases" left as work items. The two `/* handled in Task 17 */` placeholders in Task 16 are explicitly replaced in Task 17 Step 6. ✓

**3. Type consistency:** `Frame.jpeg: Vec<u8>` used consistently (11/12/13/16). `KittyGraphics::transmit_and_place_jpeg` / `delete_all` names match across 2/8. `Key`/`Mods`/`MouseEvent`/`MouseButton` from `terminal::*` used uniformly in 4/5/12/14/16. `Clickable { x, y }` defined in 17 and consumed by `assign`/app in 17. `Action` variants in 14 all matched in the app loop (16/17). `Viewport`/`CellSize`/`GridSize` consistent (3/8/12/16). ✓

One known soft spot flagged for the implementer: chromiumoxide 0.7.x exact builder signatures (Task 12 Step 3 note) and the live `input_stream` decoding path (Task 16 Step 3 note) may need small adjustments verified by the manual smoke; CDP/protocol semantics are fixed and authoritative.
