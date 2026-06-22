# webcat

A modal terminal web browser that renders web pages as JPEG screencasts from a headless Chromium instance and displays them in the terminal using the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/). Keyboard and mouse input is forwarded to the browser via the Chrome DevTools Protocol (CDP). Korean and other IME-composed text is fully supported via `Input.insertText`.

---

## Requirements

- **Kitty terminal** — the Kitty graphics protocol is required; other terminals are not supported (webcat will print an error and exit if the protocol is not detected).
- **Google Chrome or Chromium** — a recent version must be installed. webcat launches it in headless mode.
- **Rust toolchain** — to build from source (stable, 2021 edition).

---

## Build

```bash
cargo build --release
```

The binary is written to `target/release/webcat`. Copy it anywhere on your `$PATH`.

---

## Usage

```
webcat [OPTIONS] [URL]
```

Opens `URL` in the terminal browser. If no URL is given, opens `about:blank`.

### Flags

| Flag | Description |
|------|-------------|
| `--profile-dir <PATH>` | Chrome user-data directory (default: `$XDG_DATA_HOME/webcat/profile`) |
| `--chrome <PATH>` | Path to the Chrome/Chromium binary |
| `--quality <1-100>` | JPEG screencast quality (default: 70) |
| `--dpr <FLOAT>` | Device pixel ratio for HiDPI screens (default: 1.0) |

### Environment variables

| Variable | Description |
|----------|-------------|
| `WEBCAT_CHROME` | Path to Chrome/Chromium binary (overridden by `--chrome`) |
| `WEBCAT_PROFILE_DIR` | Chrome profile directory (overridden by `--profile-dir`) |
| `WEBCAT_LOG` | Path for the log file (default: `$XDG_STATE_HOME/webcat/log`) |
| `WEBCAT_LOG_LEVEL` | Log level: `trace`, `debug`, `info`, `warn`, `error` (default: `warn`) |

---

## Keybindings

webcat is **modal**: Normal mode accepts commands; Insert mode forwards keystrokes to the focused page element.

### Normal mode

| Key | Action |
|-----|--------|
| `i` | Enter Insert mode |
| `:` | Open URL bar (type a URL or search query, press Enter) |
| `f` | Enter hint mode — labels clickable elements with single letters |
| `j` | Scroll down |
| `k` | Scroll up |
| `H` | Go back (browser history) |
| `r` | Reload the page |
| `q` | Quit |
| Mouse click | Click the element under the cursor |
| Mouse scroll | Scroll the page |

### Insert mode

| Key | Action |
|-----|--------|
| `Esc` | Return to Normal mode |
| Any printable key | Forward keystroke to the focused page element |
| Arrow keys, Enter, Backspace, Tab, Delete | Forward to the focused element |

### URL bar (`:` mode)

Type a URL or search query and press `Enter`. webcat normalises input:
- If it contains `://` or starts with `about:`, it is used as-is.
- If it looks like a hostname (contains `.`, no spaces), `https://` is prepended.
- Otherwise the input is sent to Google as a search query.

Press `Esc` to cancel.

### Hint mode (`f`)

Single-letter labels appear over every clickable element. Press the displayed letter to click that element. Press `Esc` to cancel.

---

## Korean / IME input

1. Press `i` to enter Insert mode.
2. Switch your OS input method to Korean (or any other IME).
3. Type normally — the OS IME composes characters and delivers the committed Unicode text to webcat, which forwards it to Chromium via `Input.insertText`.
4. Press `Esc` to leave Insert mode.

> **Note:** In-field IME preedit composition (the grey underlined text shown while a syllable is being assembled) is not visible in webcat. The final committed text is delivered correctly.

---

## Logs

Log output is written to `$XDG_STATE_HOME/webcat/log` (typically `~/.local/state/webcat/log` on Linux or `~/Library/Application Support/webcat/log` on macOS). Set `WEBCAT_LOG_LEVEL=debug` or `WEBCAT_LOG_LEVEL=trace` for verbose output.

---

## Known limitations

- **Single page, no tabs** — webcat manages one browser page. Tab support is planned for v2.
- **Single-letter hints only** — the hint overlay assigns one letter per clickable element. If there are more than 26 clickable elements, only the first 26 are reachable via hints; the rest require mouse clicks.
- **No in-field IME preedit** — the IME composition underline is not shown. Committed (final) text appears correctly.
- **No in-page navigation controls** — there is no visible address bar or back/forward buttons; use `:` and `H` instead.
- **Requires Kitty terminal** — the Kitty graphics protocol is not available in other terminals (xterm, iTerm2, GNOME Terminal, etc.).

---

## Manual acceptance checklist (run in Kitty)

1. **Basic navigation** — `webcat https://example.com`, verify the page renders.
2. **Scroll** — press `j`/`k` and use the mouse scroll wheel; page scrolls in both directions.
3. **Click** — left-click a link; the page navigates.
4. **Hint navigation** — press `f`; letter labels appear over links; press a letter; the target is clicked.
5. **URL bar** — press `:`, type a hostname (e.g. `rust-lang.org`), press Enter; page navigates.
6. **Search** — press `:`, type a plain phrase (e.g. `rust async book`), press Enter; Google search opens.
7. **Insert mode / Korean** — click or tab to a text field, press `i`, switch OS IME to Korean, type `안녕하세요`; verify it appears in the field; press `Esc`.
8. **Go back** — press `H`; browser history goes back.
9. **Reload** — press `r`; page reloads.
10. **Resize** — resize the Kitty window; the page reflows to the new size within a frame or two.
11. **Quit with `q`** — press `q`; terminal is fully restored (cursor visible, alternate screen gone).
12. **Quit with Ctrl-C** — relaunch, press Ctrl-C; terminal is fully restored.
