# webcat

A modal terminal web browser. It drives a headless Chromium instance over the Chrome DevTools Protocol (CDP), receives the page as a JPEG screencast, and displays it in the terminal using the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/). Korean and other IME-composed text is fully supported via `Input.insertText`.

**How frames are displayed:** each screencast frame is decoded to RGBA and the raw pixels are handed to Kitty through a POSIX **shared-memory** object (`f=32,t=s`) — only the tiny shm name travels through the terminal, never the pixels — so rendering stays smooth even at high resolution. The image is drawn at `z=-1` (below the text layer) and scaled to the cell grid so it fills the window, with the status bar and hint labels rendered as terminal text on top.

---

## Requirements

- **A compatible terminal** — webcat needs the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/) **with the shared-memory transmission medium** (`t=s`), since it hands raw pixels to the terminal through POSIX shared memory rather than the PTY. See [Supported terminals](#supported-terminals) below. (webcat prints an error and exits if the protocol is not detected.)
- **Google Chrome or Chromium** — a recent version must be installed. webcat launches it in headless mode.
- **Rust toolchain** — only to build from source (stable, 2021 edition). macOS or Linux.

### Supported terminals

Any terminal that implements the Kitty graphics protocol **and** its shared-memory (`t=s`) transmission, running **locally** (shared memory is same-machine only — it does not work over SSH). Confirmed:

| Terminal | Notes |
|----------|-------|
| [kitty](https://sw.kovidgoyal.net/kitty/) | the reference implementation |
| [Ghostty](https://ghostty.org/) | and Ghostty-based terminals (e.g. **cmux**) |
| [WezTerm](https://wezterm.org/) | shared-memory support added in [#1810](https://github.com/wezterm/wezterm/pull/1810) |
| [Konsole](https://konsole.kde.org/) | KDE |
| [Warp](https://www.warp.dev/) | |
| [iTerm2](https://iterm2.com/) | recent versions |

> **Not supported:** terminals without the graphics protocol (Alacritty, Terminal.app, GNOME Terminal, …), and browser-based terminals like xterm.js (no POSIX shared memory). Remote/SSH sessions won't work either — the pixels are passed via local shared memory.

---

## Install

### Homebrew (macOS)

```sh
brew install hoonkim/tap/webcat
```

Runtime dependencies — the Kitty terminal and Google Chrome:

```sh
brew install --cask kitty google-chrome
```

Upgrade later with `brew upgrade webcat`.

### From source

```sh
cargo build --release
```

The binary is written to `target/release/webcat`; copy it anywhere on your `$PATH`.

---

## Usage

```
webcat [OPTIONS] [URL]
```

Opens `URL` in the terminal browser. If no URL is given, opens `about:blank`. The
URL argument is normalised the same way as the `:` URL bar:

```bash
webcat google.com          # → https://google.com
webcat https://naver.com   # used as-is
webcat "rust async book"   # → Google search
```

### Flags

| Flag | Description |
|------|-------------|
| `--zoom <FLOAT>` | Page zoom factor (clamped 0.5–4.0). Defaults to the display's scale factor (2.0 on Retina) so sites open at a natural size. **Lower it for sharper (but smaller) text, raise it for larger text.** |
| `--quality <1-100>` | JPEG screencast quality (default: 92). Higher is sharper at the cost of larger frames. |
| `--profile-dir <PATH>` | Chrome user-data directory (default: `$XDG_DATA_HOME/webcat/profile`). |
| `--chrome <PATH>` | Path to the Chrome/Chromium binary. |

### Environment variables

| Variable | Description |
|----------|-------------|
| `WEBCAT_CHROME` | Path to Chrome/Chromium binary (overridden by `--chrome`). |
| `WEBCAT_PROFILE_DIR` | Chrome profile directory (overridden by `--profile-dir`). |
| `WEBCAT_LOG_LEVEL` | Log level: `trace`, `debug`, `info`, `warn`, `error` (default: `info`). |

---

## Keybindings

webcat is **modal**: Normal mode accepts commands; Insert mode forwards keystrokes to the focused page element.

> **Tip:** commands (`i`, `:`, `f`, …) are English letters. If your OS input method is set to Korean, switch it back to English for Normal-mode commands (the IME composes `i` into `ㅣ`, etc.). Insert mode and the URL bar accept Korean directly.

### Normal mode

| Key | Action |
|-----|--------|
| `i` | Enter Insert mode |
| `:` | Open the URL bar (type a URL or search query, press Enter) |
| `f` | Enter hint mode — labels every clickable element |
| `j` / `k` | Scroll down / up |
| `H` | Go back (browser history) |
| `r` | Reload the page |
| `q` | Quit |
| Mouse click | Click the element under the cursor |
| Mouse scroll | Scroll the page |
| Mouse move | Drives `:hover` states |

### Insert mode

| Key | Action |
|-----|--------|
| `Esc` | Return to Normal mode |
| Any printable key | Forward keystroke to the focused page element (incl. Korean/IME text) |
| Arrows, Enter, Backspace, Tab, Delete | Forward to the focused element |

### URL bar (`:` mode)

Type a URL or search query and press `Enter`. webcat normalises input:
- contains `://` or starts with `about:` → used as-is;
- looks like a hostname (contains `.`, no spaces) → `https://` is prepended;
- otherwise → sent to Google as a search query.

`Backspace` edits, `Esc` cancels.

### Hint mode (`f`)

Labels appear over every clickable element. Type the label to click that element:
- ≤ 26 elements → one-letter labels (`a`, `s`, `d`, …);
- more → two-letter labels (`af`, `ad`, …) — type both letters; matching labels narrow as you type.

`Esc` cancels.

---

## Korean / IME input

1. Press `i` to enter Insert mode.
2. Switch your OS input method to Korean (or any other IME).
3. Type normally — the OS IME composes characters and delivers the committed Unicode text to webcat, which forwards it to Chromium via `Input.insertText`.
4. Press `Esc` to leave Insert mode.

> **Note:** the IME preedit underline (grey text shown while a syllable is being assembled) is not visible in webcat; the final committed text is delivered correctly.

---

## Notes & behaviour

- **Image quality vs. fill** — webcat sets Chrome's device scale to match the terminal display scale by default, so Retina windows render at their natural size without falling back to half-resolution screencast frames. If text looks too large or small for a specific monitor, adjust `--zoom`; if JPEG artifacts are visible, raise `--quality`.
- **Multiple windows / leftover browser** — Chrome allows one instance per profile. When a live webcat browser already holds the profile (a second window, or a previous session that didn't exit), webcat asks at startup whether to **kill** it and reuse your profile (logins, history) or open **anonymously** in a private temporary profile. A stale lock from a crashed instance is cleared automatically (no prompt). When stdin isn't a TTY, it opens anonymously without asking.
- **New tabs stay in-tab** — links that would open a new tab (`target=_blank` / `window.open`) are redirected to the current tab, since webcat captures a single page.
- **Passkeys / native prompts** — WebAuthn/passkey requests and notification-permission prompts (which need native UI that headless Chrome can't show) are declined immediately so the page falls back (e.g. to a password form) instead of freezing.
- **YouTube and video** — the user agent is de-headlessed so sites that refuse automated clients keep serving media.
- **Backpressure** — frame transmission is paced to the terminal's actual draw rate, so a backgrounded/slow Kitty doesn't pile up memory.

---

## Logs

Log output goes to `$XDG_STATE_HOME/webcat/log` (typically `~/.local/state/webcat/log` on Linux or `~/Library/Application Support/webcat/log` on macOS). Set `WEBCAT_LOG_LEVEL=debug` for verbose output.

---

## Known limitations

- **Single page, no tabs** — webcat manages one browser page.
- **Full-frame updates** — CDP screencast sends whole JPEG frames rather than partial damage updates, so very large windows trade sharpness for CPU/bandwidth. `--quality` controls JPEG compression.
- **No in-field IME preedit** — the composition underline isn't shown; committed text appears correctly.
- **Needs a graphics-protocol terminal, locally** — requires the Kitty graphics protocol with shared-memory transmission (see [Supported terminals](#supported-terminals)); shared memory is same-machine only, so it does not work over SSH.

---

## Manual acceptance checklist (run in a supported terminal)

1. **Navigation** — `webcat example.com`, verify the page renders and fills the window.
2. **Scroll** — `j`/`k` and the mouse wheel scroll in both directions.
3. **Click** — left-click a link; the page navigates.
4. **Hints** — press `f`; labels appear; type a label (one or two letters); the target is clicked.
5. **URL bar** — `:`, type `rust-lang.org`, Enter; page navigates.
6. **Search** — `:`, type `rust async book`, Enter; Google search opens.
7. **Korean** — focus a text field, `i`, switch OS IME to Korean, type `안녕하세요`; press `Esc`.
8. **Back / Reload** — `H` goes back; `r` reloads.
9. **Resize** — resize the Kitty window; the page refits and the status bar follows the new bottom row.
10. **Zoom** — relaunch with `--zoom 1` (sharper/smaller) and `--zoom 2.5` (larger); text size changes.
11. **Quit** — `q` and Ctrl-C both fully restore the terminal (cursor visible, alternate screen gone).
