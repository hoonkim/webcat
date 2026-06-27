# MCP Agent Observability & Control — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in embedded MCP server to webcat that exposes console logs, network logs, screenshots, and page control to an AI agent, plus a `~/.webcat/config.yaml` config layer and a `webcat mcp install` subcommand.

**Architecture:** webcat keeps owning the single CDP `Page`. New CDP subscriptions (Network/Runtime/Log) push entries into a shared, bounded `ObservabilityStore`. An `rmcp` streamable-HTTP server runs as a tokio task on `127.0.0.1:<port>`, with tool handlers that read the store and call existing `Browser` methods. Activation is gated behind `--mcp` (read) and `--mcp-allow-control` (write), settable via `~/.webcat/config.yaml`.

**Tech Stack:** Rust, tokio, chromiumoxide 0.7 (CDP), rmcp (MCP SDK, streamable-http + axum), serde_yml (config), clap 4.

## Global Constraints

- Rust edition 2021, rust-version 1.75 (from `Cargo.toml`) — do not use newer-edition-only syntax.
- chromiumoxide is pinned at `0.7` with `default-features = false`, `features = ["tokio-runtime"]` — do not change.
- MCP server binds **only** `127.0.0.1` (never `0.0.0.0`).
- MCP server is **off by default**; read tools require `mcp.enabled`/`--mcp`; write (control) tools additionally require `mcp.allow_control`/`--mcp-allow-control`.
- Config precedence is **CLI/env > `~/.webcat/config.yaml` > built-in default**.
- The MCP HTTP route path is `/mcp`. The advertised URL is `http://127.0.0.1:<port>/mcp`.
- Ring buffers are capped at **2000 entries each** (console, network); oldest dropped on overflow with a `dropped` counter.
- Only `~/.webcat/config.yaml` uses the `~/.webcat` directory; profile/log paths stay on their existing XDG locations.
- Run `cargo test` and `cargo clippy` before every commit; commits must compile.

---

### Task 1: ObservabilityStore (console + network ring buffers)

Pure, dependency-free store. Fully unit-testable without Chrome.

**Files:**
- Create: `src/observability.rs`
- Modify: `src/main.rs` (add `mod observability;`)
- Test: inline `#[cfg(test)]` module in `src/observability.rs`

**Interfaces:**
- Produces:
  - `pub struct ObservabilityStore` with `pub fn new(cap: usize) -> Self`
  - `pub fn push_console(&self, level: String, text: String, url: Option<String>, line: Option<u32>)`
  - `pub fn push_network(&self, e: NetworkEntry)` where the caller builds the entry
  - `pub fn console_since(&self, since_seq: u64, level: Option<&str>, limit: usize) -> ConsolePage`
  - `pub fn network_since(&self, since_seq: u64, status: Option<i64>, limit: usize) -> NetworkPage`
  - structs `ConsoleEntry { seq: u64, ts_ms: u64, level: String, text: String, url: Option<String>, line: Option<u32> }`
  - `NetworkEntry { seq: u64, ts_ms: u64, kind: String, method: Option<String>, url: String, status: Option<i64>, mime: Option<String>, request_id: String }`
  - `ConsolePage { entries: Vec<ConsoleEntry>, dropped: u64, latest_seq: u64 }`, `NetworkPage { entries: Vec<NetworkEntry>, dropped: u64, latest_seq: u64 }`
  - All structs derive `Clone, Debug, serde::Serialize, schemars::JsonSchema`.
  - `push_network` takes a `NetworkEntry` whose `seq`/`ts_ms` fields are **ignored and overwritten** by the store (caller passes 0).

- [ ] **Step 1: Write the failing tests**

Create `src/observability.rs`:

```rust
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Clone, Debug, serde::Serialize, schemars::JsonSchema)]
pub struct ConsoleEntry {
    pub seq: u64,
    pub ts_ms: u64,
    pub level: String,
    pub text: String,
    pub url: Option<String>,
    pub line: Option<u32>,
}

#[derive(Clone, Debug, serde::Serialize, schemars::JsonSchema)]
pub struct NetworkEntry {
    pub seq: u64,
    pub ts_ms: u64,
    /// "request" | "response" | "failed"
    pub kind: String,
    pub method: Option<String>,
    pub url: String,
    pub status: Option<i64>,
    pub mime: Option<String>,
    pub request_id: String,
}

#[derive(Clone, Debug, serde::Serialize, schemars::JsonSchema)]
pub struct ConsolePage {
    pub entries: Vec<ConsoleEntry>,
    pub dropped: u64,
    pub latest_seq: u64,
}

#[derive(Clone, Debug, serde::Serialize, schemars::JsonSchema)]
pub struct NetworkPage {
    pub entries: Vec<NetworkEntry>,
    pub dropped: u64,
    pub latest_seq: u64,
}

struct Buffers {
    console: VecDeque<ConsoleEntry>,
    network: VecDeque<NetworkEntry>,
    console_dropped: u64,
    network_dropped: u64,
}

pub struct ObservabilityStore {
    cap: usize,
    seq: AtomicU64,
    inner: Mutex<Buffers>,
}

impl ObservabilityStore {
    pub fn new(cap: usize) -> Self {
        ObservabilityStore {
            cap,
            seq: AtomicU64::new(0),
            inner: Mutex::new(Buffers {
                console: VecDeque::new(),
                network: VecDeque::new(),
                console_dropped: 0,
                network_dropped: 0,
            }),
        }
    }

    fn next_seq(&self) -> u64 {
        // First seq returned is 1, so `since_seq=0` means "everything".
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn push_console(&self, level: String, text: String, url: Option<String>, line: Option<u32>) {
        let seq = self.next_seq();
        let mut g = self.inner.lock().unwrap();
        g.console.push_back(ConsoleEntry { seq, ts_ms: now_ms(), level, text, url, line });
        while g.console.len() > self.cap {
            g.console.pop_front();
            g.console_dropped += 1;
        }
    }

    pub fn push_network(&self, mut e: NetworkEntry) {
        e.seq = self.next_seq();
        e.ts_ms = now_ms();
        let mut g = self.inner.lock().unwrap();
        g.network.push_back(e);
        while g.network.len() > self.cap {
            g.network.pop_front();
            g.network_dropped += 1;
        }
    }

    pub fn console_since(&self, since_seq: u64, level: Option<&str>, limit: usize) -> ConsolePage {
        let g = self.inner.lock().unwrap();
        let latest_seq = self.seq.load(Ordering::Relaxed);
        let entries: Vec<ConsoleEntry> = g
            .console
            .iter()
            .filter(|e| e.seq > since_seq)
            .filter(|e| level.map_or(true, |l| e.level == l))
            .take(limit)
            .cloned()
            .collect();
        ConsolePage { entries, dropped: g.console_dropped, latest_seq }
    }

    pub fn network_since(&self, since_seq: u64, status: Option<i64>, limit: usize) -> NetworkPage {
        let g = self.inner.lock().unwrap();
        let latest_seq = self.seq.load(Ordering::Relaxed);
        let entries: Vec<NetworkEntry> = g
            .network
            .iter()
            .filter(|e| e.seq > since_seq)
            .filter(|e| status.map_or(true, |s| e.status == Some(s)))
            .take(limit)
            .cloned()
            .collect();
        NetworkPage { entries, dropped: g.network_dropped, latest_seq }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(url: &str, status: Option<i64>) -> NetworkEntry {
        NetworkEntry {
            seq: 0, ts_ms: 0, kind: "response".into(), method: Some("GET".into()),
            url: url.into(), status, mime: None, request_id: "r".into(),
        }
    }

    #[test]
    fn seq_increases_monotonically_across_buffers() {
        let s = ObservabilityStore::new(10);
        s.push_console("log".into(), "a".into(), None, None);
        s.push_network(net("https://x", Some(200)));
        s.push_console("log".into(), "b".into(), None, None);
        let c = s.console_since(0, None, 100);
        let n = s.network_since(0, None, 100);
        assert_eq!(c.entries[0].seq, 1);
        assert_eq!(n.entries[0].seq, 2);
        assert_eq!(c.entries[1].seq, 3);
    }

    #[test]
    fn since_seq_filters_old_entries() {
        let s = ObservabilityStore::new(10);
        s.push_console("log".into(), "a".into(), None, None); // seq 1
        s.push_console("log".into(), "b".into(), None, None); // seq 2
        let page = s.console_since(1, None, 100);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].text, "b");
    }

    #[test]
    fn ring_buffer_drops_oldest_and_counts() {
        let s = ObservabilityStore::new(2);
        for i in 0..5 {
            s.push_console("log".into(), format!("{i}"), None, None);
        }
        let page = s.console_since(0, None, 100);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].text, "3");
        assert_eq!(page.dropped, 3);
    }

    #[test]
    fn level_filter_matches_exact() {
        let s = ObservabilityStore::new(10);
        s.push_console("error".into(), "e".into(), None, None);
        s.push_console("log".into(), "l".into(), None, None);
        let page = s.console_since(0, Some("error"), 100);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].text, "e");
    }

    #[test]
    fn status_filter_matches_network() {
        let s = ObservabilityStore::new(10);
        s.push_network(net("https://ok", Some(200)));
        s.push_network(net("https://err", Some(404)));
        let page = s.network_since(0, Some(404), 100);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].url, "https://err");
    }
}
```

Add to `src/main.rs` near the other `mod` declarations:

```rust
mod observability;
```

- [ ] **Step 2: Run tests to verify they fail to compile (schemars not yet a dependency)**

Run: `cargo test observability:: 2>&1 | head -20`
Expected: FAIL — `schemars` unresolved (added in Task 5's Cargo changes) OR, if you prefer, add `schemars` now. To keep this task self-contained, add the deps in Step 3.

- [ ] **Step 3: Add `schemars` and `serde` derive dependencies**

In `Cargo.toml` `[dependencies]` add (serde is already pulled transitively but make it explicit):

```toml
serde = { version = "1", features = ["derive"] }
schemars = "0.8"
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test observability:: -- --nocapture`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add src/observability.rs src/main.rs Cargo.toml Cargo.lock
git commit -m "feat: add ObservabilityStore ring buffers for console/network"
```

---

### Task 2: Config file layer (`~/.webcat/config.yaml`)

Add `FileConfig` and merge it into `Config::resolve` with correct precedence.

**Files:**
- Modify: `src/config.rs`
- Modify: `Cargo.toml` (add `serde_yml`)
- Test: extend `#[cfg(test)]` in `src/config.rs`

**Interfaces:**
- Consumes: nothing from prior tasks.
- Produces:
  - `pub struct McpConfig { pub enabled: bool, pub port: Option<u16>, pub allow_control: bool }` on `Config`
  - `Config` gains field `pub mcp: McpConfig`
  - `pub fn load_file_config() -> Result<Option<FileConfig>>` reading `~/.webcat/config.yaml`
  - `Config::resolve(cli: Cli, file: Option<FileConfig>) -> Result<Config>` (signature changes — add the `file` arg)

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/config.rs` (and update the existing `base_cli()` if Task 3's new CLI fields already landed; if running this task first, keep `base_cli()` matching current `Cli`):

```rust
    fn file_with_mcp() -> FileConfig {
        FileConfig {
            quality: None,
            zoom: None,
            mcp: Some(FileMcpConfig { enabled: Some(true), port: Some(4470), allow_control: Some(true) }),
        }
    }

    #[test]
    fn file_enables_mcp_when_cli_silent() {
        let cli = base_cli();
        let cfg = Config::resolve(cli, Some(file_with_mcp())).unwrap();
        assert!(cfg.mcp.enabled);
        assert_eq!(cfg.mcp.port, Some(4470));
        assert!(cfg.mcp.allow_control);
    }

    #[test]
    fn cli_flag_overrides_file_for_mcp_enabled() {
        let mut cli = base_cli();
        cli.mcp = true; // CLI explicitly on
        let mut file = file_with_mcp();
        file.mcp.as_mut().unwrap().enabled = Some(false); // file says off
        let cfg = Config::resolve(cli, Some(file)).unwrap();
        assert!(cfg.mcp.enabled, "CLI --mcp must win over file enabled:false");
    }

    #[test]
    fn defaults_mcp_off_when_no_file() {
        let cfg = Config::resolve(base_cli(), None).unwrap();
        assert!(!cfg.mcp.enabled);
        assert!(!cfg.mcp.allow_control);
        assert_eq!(cfg.mcp.port, None);
    }

    #[test]
    fn file_quality_used_when_cli_default() {
        // CLI quality defaults to 92; when the user did not pass --quality the
        // file value should apply. Represented by base_cli() carrying the clap
        // default; resolve treats file as lower precedence than an explicit CLI.
        let mut cli = base_cli();
        cli.quality = 92; // clap default sentinel
        let mut file = file_with_mcp();
        file.quality = Some(50);
        let cfg = Config::resolve(cli, Some(file)).unwrap();
        assert_eq!(cfg.quality, 50);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test config:: 2>&1 | head -20`
Expected: FAIL — `FileConfig`, `FileMcpConfig`, `Config.mcp`, and the 2-arg `resolve` do not exist.

- [ ] **Step 3: Implement the config layer**

In `src/config.rs`, add the structs and rewrite `resolve`. Add imports at the top:

```rust
use serde::Deserialize;
```

Add the file-config types:

```rust
#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileConfig {
    pub quality: Option<u8>,
    pub zoom: Option<f64>,
    pub mcp: Option<FileMcpConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
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

/// Reads `~/.webcat/config.yaml`. Missing file → Ok(None). Parse error → Err.
pub fn load_file_config() -> Result<Option<FileConfig>> {
    let path = match dirs::home_dir() {
        Some(h) => h.join(".webcat").join("config.yaml"),
        None => return Ok(None),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Other(anyhow::anyhow!("reading {}: {e}", path.display()))),
    };
    let cfg: FileConfig = serde_yml::from_str(&text)
        .map_err(|e| Error::Other(anyhow::anyhow!("parsing {}: {e}", path.display())))?;
    Ok(Some(cfg))
}
```

Add `pub mcp: McpConfig` to the `Config` struct, then rewrite `resolve`:

```rust
impl Config {
    pub fn resolve(cli: Cli, file: Option<FileConfig>) -> Result<Config> {
        let file = file.unwrap_or_default();
        let fmcp = file.mcp.unwrap_or_default();

        let profile_dir = cli.profile_dir.unwrap_or_else(default_profile_dir);
        let log_path = default_log_path();

        // quality: explicit CLI (non-default) > file > default.
        // clap default is 92; treat 92 as "unset" for file precedence.
        let quality = if cli.quality != 92 {
            cli.quality
        } else {
            file.quality.unwrap_or(92)
        }
        .clamp(1, 100);

        let zoom = match cli.zoom {
            Some(z) if z > 0.0 => z.clamp(0.5, 4.0),
            _ => match file.zoom {
                Some(z) if z > 0.0 => z.clamp(0.5, 4.0),
                _ => default_zoom(),
            },
        };

        let mcp = McpConfig {
            enabled: cli.mcp || fmcp.enabled.unwrap_or(false),
            port: cli.mcp_port.or(fmcp.port),
            allow_control: cli.mcp_allow_control || fmcp.allow_control.unwrap_or(false),
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
```

> Note: this task assumes `Cli` already has `mcp`, `mcp_port`, `mcp_allow_control` fields (Task 3). If executing Task 2 before Task 3, add those three fields to `Cli` now (see Task 3 Step 3) so this compiles.

Update existing tests' `base_cli()` to include the new fields:

```rust
    fn base_cli() -> Cli {
        Cli {
            command: None,
            url: None, profile_dir: None, chrome: None, quality: 92, zoom: Some(1.0),
            mcp: false, mcp_port: None, mcp_allow_control: false,
        }
    }
```

Update existing `Config::resolve(cli)` call sites in tests to `Config::resolve(cli, None)`.

- [ ] **Step 4: Add `serde_yml` dependency**

In `Cargo.toml` `[dependencies]`:

```toml
serde_yml = "0.0.12"
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test config:: -- --nocapture`
Expected: PASS (all existing + 4 new).

- [ ] **Step 6: Commit**

```bash
git add src/config.rs Cargo.toml Cargo.lock
git commit -m "feat: load ~/.webcat/config.yaml and merge into Config"
```

---

### Task 3: CLI subcommands and MCP flags

Restructure `Cli` to support an optional `mcp` subcommand while preserving the no-subcommand "open URL" behavior.

**Files:**
- Modify: `src/cli.rs`
- Test: inline `#[cfg(test)]` in `src/cli.rs`

**Interfaces:**
- Produces:
  - `Cli` fields: existing + `pub command: Option<Command>`, `pub mcp: bool`, `pub mcp_port: Option<u16>`, `pub mcp_allow_control: bool`
  - `pub enum Command { Mcp(McpCmd) }`
  - `pub struct McpCmd { pub action: McpAction }`
  - `pub enum McpAction { Install(InstallArgs), Status, Uninstall(UninstallArgs) }`
  - `pub struct InstallArgs { pub agent: AgentKind, pub port: Option<u16> }`
  - `pub struct UninstallArgs { pub agent: AgentKind }`
  - `pub enum AgentKind { Claude, Print }` (clap `ValueEnum`, default `Claude`)

- [ ] **Step 1: Write the failing tests**

Add to `src/cli.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn no_subcommand_parses_url() {
        let cli = Cli::parse_from(["webcat", "https://example.com"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn mcp_flags_parse() {
        let cli = Cli::parse_from(["webcat", "--mcp", "--mcp-port", "4470", "--mcp-allow-control"]);
        assert!(cli.mcp);
        assert_eq!(cli.mcp_port, Some(4470));
        assert!(cli.mcp_allow_control);
    }

    #[test]
    fn mcp_install_subcommand_parses() {
        let cli = Cli::parse_from(["webcat", "mcp", "install", "--agent", "print", "--port", "5000"]);
        match cli.command {
            Some(Command::Mcp(McpCmd { action: McpAction::Install(a) })) => {
                assert!(matches!(a.agent, AgentKind::Print));
                assert_eq!(a.port, Some(5000));
            }
            _ => panic!("expected mcp install"),
        }
    }

    #[test]
    fn mcp_install_defaults_agent_to_claude() {
        let cli = Cli::parse_from(["webcat", "mcp", "install"]);
        match cli.command {
            Some(Command::Mcp(McpCmd { action: McpAction::Install(a) })) => {
                assert!(matches!(a.agent, AgentKind::Claude));
            }
            _ => panic!("expected mcp install"),
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test cli:: 2>&1 | head -20`
Expected: FAIL — `command`, `Command`, `McpCmd`, etc. do not exist.

- [ ] **Step 3: Implement the CLI**

Rewrite `src/cli.rs`:

```rust
use std::path::PathBuf;
use clap::{Parser, Subcommand, Args, ValueEnum};

/// webcat — a terminal browser rendering headless Chromium via the Kitty graphics protocol.
#[derive(Parser, Debug, Clone)]
#[command(name = "webcat", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// URL to open on startup (defaults to about:blank).
    pub url: Option<String>,

    /// Dedicated profile directory (overrides $WEBCAT_PROFILE_DIR).
    #[arg(long, env = "WEBCAT_PROFILE_DIR")]
    pub profile_dir: Option<PathBuf>,

    /// Path to the Chrome/Chromium binary (overrides $WEBCAT_CHROME).
    #[arg(long, env = "WEBCAT_CHROME")]
    pub chrome: Option<PathBuf>,

    /// JPEG screencast quality 1-100.
    #[arg(long, default_value_t = 92)]
    pub quality: u8,

    /// Page zoom factor (clamped 0.5–4.0).
    #[arg(long)]
    pub zoom: Option<f64>,

    /// Enable the embedded MCP server (read tools). Overrides config `mcp.enabled`.
    #[arg(long)]
    pub mcp: bool,

    /// Port for the MCP server (default: OS-assigned, or config `mcp.port`).
    #[arg(long)]
    pub mcp_port: Option<u16>,

    /// Allow MCP control (write) tools: navigate/click/type/etc.
    #[arg(long)]
    pub mcp_allow_control: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Manage the MCP integration.
    Mcp(McpCmd),
}

#[derive(Args, Debug, Clone)]
pub struct McpCmd {
    #[command(subcommand)]
    pub action: McpAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpAction {
    /// Register the webcat MCP server with an agent.
    Install(InstallArgs),
    /// Show MCP config and registration status.
    Status,
    /// Remove the webcat MCP server registration from an agent.
    Uninstall(UninstallArgs),
}

#[derive(Args, Debug, Clone)]
pub struct InstallArgs {
    /// Target agent.
    #[arg(long, value_enum, default_value_t = AgentKind::Claude)]
    pub agent: AgentKind,
    /// Stable port to register (written to config). Defaults to config or 4470.
    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Args, Debug, Clone)]
pub struct UninstallArgs {
    #[arg(long, value_enum, default_value_t = AgentKind::Claude)]
    pub agent: AgentKind,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Register via `claude mcp add`.
    Claude,
    /// Print a config snippet only; no side effects.
    Print,
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test cli:: -- --nocapture`
Expected: PASS (4 tests). Note: the whole crate may not yet compile because `main.rs`/`app.rs` use the old `Config::resolve` arity — that is fixed in Task 7. To test this task in isolation, run `cargo test --lib cli::` after confirming `cli.rs` compiles on its own; otherwise land Task 7's main wiring before running the full suite.

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs
git commit -m "feat: add mcp subcommand and --mcp flags to CLI"
```

---

### Task 4: Browser CDP subscriptions + screenshot + selector click

Wire Network/Runtime/Log events into the store, add `capture_screenshot`, and add selector-based clicking. Requires Chrome for full verification.

**Files:**
- Modify: `src/browser/mod.rs`
- Test: manual + a thin unit test for the console-arg flattening helper

**Interfaces:**
- Consumes: `crate::observability::{ObservabilityStore, NetworkEntry}` (Task 1)
- Produces:
  - `Browser::launch` signature gains `store: Arc<ObservabilityStore>` parameter
  - `pub async fn capture_screenshot(&self, jpeg: bool, quality: u8) -> Result<Vec<u8>>` (returns raw image bytes)
  - `pub async fn click_selector(&self, selector: &str) -> Result<()>`
  - `pub fn current_url_sync(&self) -> Option<String>` already exists as `current_url` (async) — reuse it
  - free fn `fn console_args_to_text(args: &[serde_json::Value]) -> String` (unit-tested)

- [ ] **Step 1: Write the failing unit test for the helper**

Add to `src/browser/mod.rs` test module (create one if absent):

```rust
#[cfg(test)]
mod tests {
    use super::console_args_to_text;
    use serde_json::json;

    #[test]
    fn flattens_string_and_number_args() {
        let args = vec![json!({"value": "hello"}), json!({"value": 42})];
        assert_eq!(console_args_to_text(&args), "hello 42");
    }

    #[test]
    fn falls_back_to_description_when_no_value() {
        let args = vec![json!({"description": "Object"})];
        assert_eq!(console_args_to_text(&args), "Object");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test browser::tests:: 2>&1 | head -20`
Expected: FAIL — `console_args_to_text` not defined.

- [ ] **Step 3: Implement helper, subscriptions, screenshot, selector click**

Add imports at the top of `src/browser/mod.rs`:

```rust
use chromiumoxide::cdp::browser_protocol::network::{
    EnableParams as NetworkEnableParams, EventRequestWillBeSent, EventResponseReceived,
    EventLoadingFailed,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    EnableParams as RuntimeEnableParams, EventConsoleApiCalled, EventExceptionThrown,
};
use chromiumoxide::cdp::browser_protocol::log::{
    EnableParams as LogEnableParams, EventEntryAdded,
};
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, CaptureScreenshotParams};
use crate::observability::{ObservabilityStore, NetworkEntry};
```

Add the helper (CDP `RemoteObject` serializes to JSON with `value`/`description` fields):

```rust
/// Best-effort flatten of console API call args into a single line, mirroring
/// how a devtools console renders `console.log(a, b, c)`.
fn console_args_to_text(args: &[serde_json::Value]) -> String {
    args.iter()
        .map(|a| {
            if let Some(v) = a.get("value") {
                match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                }
            } else if let Some(d) = a.get("description").and_then(|d| d.as_str()) {
                d.to_string()
            } else {
                a.get("type").and_then(|t| t.as_str()).unwrap_or("?").to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
```

Change `launch` to accept the store and enable the domains + subscribe. Update the signature:

```rust
    pub async fn launch(
        cfg: &Config,
        chrome: PathBuf,
        window: (u32, u32),
        store: Arc<ObservabilityStore>,
    ) -> Result<(Browser, FrameRx)> {
```

After the page is created and before returning, enable domains:

```rust
        let _ = page.execute(NetworkEnableParams::default()).await;
        let _ = page.execute(RuntimeEnableParams::default()).await;
        let _ = page.execute(LogEnableParams::default()).await;
```

Then add subscription tasks alongside the existing ones (each follows the existing `event_listener` pattern):

```rust
        // Console messages (console.log/warn/error from the page).
        let console_page = page.clone();
        let console_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = console_page.event_listener::<EventConsoleApiCalled>().await {
                while let Some(e) = ev.next().await {
                    let args: Vec<serde_json::Value> = e
                        .args
                        .iter()
                        .map(|a| serde_json::to_value(a).unwrap_or(serde_json::Value::Null))
                        .collect();
                    let text = console_args_to_text(&args);
                    console_store.push_console(format!("{:?}", e.r#type), text, None, None);
                }
            }
        });

        // Uncaught exceptions surface as error-level console entries.
        let exc_page = page.clone();
        let exc_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = exc_page.event_listener::<EventExceptionThrown>().await {
                while let Some(e) = ev.next().await {
                    let text = e
                        .exception_details
                        .exception
                        .as_ref()
                        .and_then(|o| o.description.clone())
                        .unwrap_or_else(|| e.exception_details.text.clone());
                    exc_store.push_console("error".into(), text, None, None);
                }
            }
        });

        // Browser-level log entries (Log domain).
        let log_page = page.clone();
        let log_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = log_page.event_listener::<EventEntryAdded>().await {
                while let Some(e) = ev.next().await {
                    log_store.push_console(
                        format!("{:?}", e.entry.level),
                        e.entry.text.clone(),
                        e.entry.url.clone(),
                        e.entry.line_number.map(|n| n as u32),
                    );
                }
            }
        });

        // Network requests.
        let req_page = page.clone();
        let req_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = req_page.event_listener::<EventRequestWillBeSent>().await {
                while let Some(e) = ev.next().await {
                    req_store.push_network(NetworkEntry {
                        seq: 0, ts_ms: 0, kind: "request".into(),
                        method: Some(e.request.method.clone()),
                        url: e.request.url.clone(), status: None, mime: None,
                        request_id: e.request_id.inner().clone(),
                    });
                }
            }
        });

        // Network responses.
        let resp_page = page.clone();
        let resp_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = resp_page.event_listener::<EventResponseReceived>().await {
                while let Some(e) = ev.next().await {
                    resp_store.push_network(NetworkEntry {
                        seq: 0, ts_ms: 0, kind: "response".into(),
                        method: None, url: e.response.url.clone(),
                        status: Some(e.response.status), mime: Some(e.response.mime_type.clone()),
                        request_id: e.request_id.inner().clone(),
                    });
                }
            }
        });

        // Failed requests.
        let fail_page = page.clone();
        let fail_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = fail_page.event_listener::<EventLoadingFailed>().await {
                while let Some(e) = ev.next().await {
                    fail_store.push_network(NetworkEntry {
                        seq: 0, ts_ms: 0, kind: "failed".into(),
                        method: None, url: String::new(), status: None,
                        mime: None, request_id: e.request_id.inner().clone(),
                    });
                }
            }
        });
```

> Field-name caution: the exact field names on these CDP structs (`request.method`, `response.status`, `response.mime_type`, `entry.level/text/url/line_number`, `request_id.inner()`) are from chromiumoxide_cdp 0.7. If any name mismatches at compile time, inspect the struct in `~/.cargo/registry/src/index.crates.io-*/chromiumoxide_cdp-0.7.0/src/cdp.rs` and adjust — do not guess.

Add the screenshot and selector-click methods (place near `start_screencast` / `click`):

```rust
    pub async fn capture_screenshot(&self, jpeg: bool, quality: u8) -> Result<Vec<u8>> {
        let mut b = CaptureScreenshotParams::builder();
        if jpeg {
            b = b.format(CaptureScreenshotFormat::Jpeg).quality(quality as i64);
        } else {
            b = b.format(CaptureScreenshotFormat::Png);
        }
        let params = b.build();
        let data = self
            .page
            .execute(params)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?
            .result
            .data;
        let b64: &str = data.as_ref();
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))
    }

    /// Resolve a CSS selector to its center point and click it.
    pub async fn click_selector(&self, selector: &str) -> Result<()> {
        let js = format!(
            "(() => {{ const el = document.querySelector({sel}); if (!el) return null; \
             const r = el.getBoundingClientRect(); \
             return JSON.stringify([r.left + r.width/2, r.top + r.height/2]); }})()",
            sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into())
        );
        let res = self.eval_string(&js).await?;
        let coords: [f64; 2] = serde_json::from_str(&res)
            .map_err(|_| Error::Other(anyhow::anyhow!("selector not found: {selector}")))?;
        // CSS px → device px for the click (screencast/input use device pixels).
        self.click(coords[0] * self.zoom, coords[1] * self.zoom, MouseButton::Left).await
    }
```

> `eval_string` returns the JS result as a string; for a `null` (selector not found) it will not parse as `[f64;2]`, yielding the "selector not found" error. Confirm `eval_string`'s exact return shape and adjust parsing if it wraps the value.

- [ ] **Step 4: Run the helper unit test**

Run: `cargo test browser::tests:: -- --nocapture`
Expected: PASS (2 tests). The subscription/screenshot code compiles but is exercised in Task 7 integration + manual run.

- [ ] **Step 5: Commit**

```bash
git add src/browser/mod.rs
git commit -m "feat: subscribe to CDP network/console/log events; add screenshot + selector click"
```

---

### Task 5: MCP server module (rmcp tools)

Implement the rmcp handler exposing read + control tools, gated by `allow_control`.

**Files:**
- Create: `src/mcp/mod.rs`
- Create: `src/mcp/server.rs`
- Modify: `src/main.rs` (`mod mcp;`)
- Modify: `Cargo.toml` (rmcp, axum, tokio listener)
- Test: inline unit tests for arg structs + a tool dispatch smoke test

**Interfaces:**
- Consumes: `Arc<ObservabilityStore>` (Task 1), `Arc<Browser>` (Task 4 — `Browser` must be shareable; wrap in `Arc`)
- Produces:
  - `pub struct WebcatMcp { store: Arc<ObservabilityStore>, browser: Arc<Browser>, allow_control: bool }`
  - `pub async fn serve(addr: std::net::SocketAddr, mcp: WebcatMcp) -> Result<u16>` — binds, returns the actual bound port, spawns the server
  - tool methods (see below)

- [ ] **Step 1: Add dependencies**

In `Cargo.toml` `[dependencies]`:

```toml
rmcp = { version = "0.9", features = ["server", "macros", "transport-streamable-http-server"] }
axum = "0.7"
```

> Verify the current `rmcp` version on crates.io during execution and pin the latest 0.x; adjust feature names if they changed. The streamable-http server type is `rmcp::transport::streamable_http_server::tower::StreamableHttpService` with `LocalSessionManager`.

- [ ] **Step 2: Write the failing test**

Create `src/mcp/server.rs` with the handler and tests. First the test (it will fail until the impl exists):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_disabled_rejects_navigate() {
        // Build a handler with control off and assert the guard helper returns Err.
        // (Pure guard check — no live browser needed.)
        assert!(super::control_guard(false).is_err());
        assert!(super::control_guard(true).is_ok());
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test mcp::server:: 2>&1 | head -20`
Expected: FAIL — `control_guard` undefined.

- [ ] **Step 4: Implement the handler**

`src/mcp/server.rs`:

```rust
use std::sync::Arc;

use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::{tool, tool_router};
use serde::Deserialize;

use crate::browser::Browser;
use crate::observability::{ConsolePage, NetworkPage, ObservabilityStore};

#[derive(Clone)]
pub struct WebcatMcp {
    store: Arc<ObservabilityStore>,
    browser: Arc<Browser>,
    allow_control: bool,
}

impl WebcatMcp {
    pub fn new(store: Arc<ObservabilityStore>, browser: Arc<Browser>, allow_control: bool) -> Self {
        WebcatMcp { store, browser, allow_control }
    }
}

pub(crate) fn control_guard(allow: bool) -> Result<(), ErrorData> {
    if allow {
        Ok(())
    } else {
        Err(ErrorData::invalid_request(
            "control tools are disabled; start webcat with --mcp-allow-control",
            None,
        ))
    }
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct ConsoleQuery {
    pub since_seq: Option<u64>,
    pub level: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct NetworkQuery {
    pub since_seq: Option<u64>,
    pub status: Option<i64>,
    pub limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct NavigateParam { pub url: String }

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct ClickParam { pub x: Option<f64>, pub y: Option<f64>, pub selector: Option<String> }

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct TypeParam { pub text: String }

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct ScrollParam { pub dy: f64, pub x: Option<f64>, pub y: Option<f64> }

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct EvalParam { pub script: String }

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct ShotParam { pub format: Option<String>, pub quality: Option<u8> }

fn to_err<E: std::fmt::Display>(e: E) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

#[tool_router(server_handler)]
impl WebcatMcp {
    #[tool(description = "Get page console logs since a sequence number")]
    async fn get_console_logs(&self, Parameters(q): Parameters<ConsoleQuery>) -> Json<ConsolePage> {
        Json(self.store.console_since(
            q.since_seq.unwrap_or(0),
            q.level.as_deref(),
            q.limit.unwrap_or(200),
        ))
    }

    #[tool(description = "Get network request/response logs since a sequence number")]
    async fn get_network_logs(&self, Parameters(q): Parameters<NetworkQuery>) -> Json<NetworkPage> {
        Json(self.store.network_since(
            q.since_seq.unwrap_or(0),
            q.status,
            q.limit.unwrap_or(200),
        ))
    }

    #[tool(description = "Get current page URL and zoom")]
    async fn get_page_info(&self) -> Json<serde_json::Value> {
        let url = self.browser.current_url().await.unwrap_or_default();
        Json(serde_json::json!({ "url": url }))
    }

    #[tool(description = "Capture a screenshot of the current page (png or jpeg)")]
    async fn capture_screenshot(&self, Parameters(p): Parameters<ShotParam>) -> Result<CallToolResult, ErrorData> {
        let jpeg = p.format.as_deref() == Some("jpeg");
        let bytes = self.browser.capture_screenshot(jpeg, p.quality.unwrap_or(80)).await.map_err(to_err)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let mime = if jpeg { "image/jpeg" } else { "image/png" };
        Ok(CallToolResult::success(vec![Content::image(b64, mime.to_string())]))
    }

    #[tool(description = "Navigate to a URL (requires control)")]
    async fn navigate(&self, Parameters(p): Parameters<NavigateParam>) -> Result<CallToolResult, ErrorData> {
        control_guard(self.allow_control)?;
        self.browser.navigate(&p.url).await.map_err(to_err)?;
        Ok(CallToolResult::success(vec![Content::text("ok")]))
    }

    #[tool(description = "Click at x,y (device px) or a CSS selector (requires control)")]
    async fn click(&self, Parameters(p): Parameters<ClickParam>) -> Result<CallToolResult, ErrorData> {
        control_guard(self.allow_control)?;
        if let Some(sel) = p.selector {
            self.browser.click_selector(&sel).await.map_err(to_err)?;
        } else if let (Some(x), Some(y)) = (p.x, p.y) {
            self.browser
                .click(x, y, crate::terminal::mouse::MouseButton::Left)
                .await
                .map_err(to_err)?;
        } else {
            return Err(ErrorData::invalid_request("provide selector or x,y", None));
        }
        Ok(CallToolResult::success(vec![Content::text("ok")]))
    }

    #[tool(description = "Type text into the focused element (requires control)")]
    async fn type_text(&self, Parameters(p): Parameters<TypeParam>) -> Result<CallToolResult, ErrorData> {
        control_guard(self.allow_control)?;
        self.browser.insert_text(&p.text).await.map_err(to_err)?;
        Ok(CallToolResult::success(vec![Content::text("ok")]))
    }

    #[tool(description = "Scroll by dy at x,y (requires control)")]
    async fn scroll(&self, Parameters(p): Parameters<ScrollParam>) -> Result<CallToolResult, ErrorData> {
        control_guard(self.allow_control)?;
        self.browser.scroll(p.x.unwrap_or(10.0), p.y.unwrap_or(10.0), p.dy).await.map_err(to_err)?;
        Ok(CallToolResult::success(vec![Content::text("ok")]))
    }

    #[tool(description = "Evaluate JavaScript and return the result string (requires control)")]
    async fn eval_js(&self, Parameters(p): Parameters<EvalParam>) -> Result<CallToolResult, ErrorData> {
        control_guard(self.allow_control)?;
        let out = self.browser.eval_string(&p.script).await.map_err(to_err)?;
        Ok(CallToolResult::success(vec![Content::text(out)]))
    }
}
```

> The `Content::image` / `Content::text` / `ErrorData::invalid_request` / `ErrorData::internal_error` constructors and the `#[tool_router(server_handler)]` macro come from the installed `rmcp` version. If a constructor name differs, check `rmcp`'s docs.rs for the pinned version and adjust — the verified macro pattern is `#[tool_router(server_handler)]` over an `impl` block with `#[tool(...)]` methods taking `Parameters<T>`.

`src/mcp/mod.rs`:

```rust
mod server;
pub use server::WebcatMcp;

use std::net::SocketAddr;
use std::sync::Arc;

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{StreamableHttpServerConfig, StreamableHttpService};

use crate::error::{Error, Result};

/// Bind an MCP streamable-HTTP server on `addr`, serving at `/mcp`. Returns the
/// actual bound port and spawns the server on the current runtime.
pub async fn serve(addr: SocketAddr, mcp: WebcatMcp) -> Result<u16> {
    let service = StreamableHttpService::new(
        move || Ok(mcp.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("MCP bind {addr}: {e}")))?;
    let port = listener.local_addr().map_err(|e| Error::Other(anyhow::anyhow!(e)))?.port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(port)
}
```

Add `mod mcp;` to `src/main.rs`.

> The `serve` glue (StreamableHttpService::new signature, LocalSessionManager path) reflects rmcp's streamable-http tower API. Verify the exact module path (`session::local::LocalSessionManager`) and `new` argument order against docs.rs for the pinned version; adjust if needed. This is the one spot most likely to need a small fix on first compile.

- [ ] **Step 5: Run the guard unit test**

Run: `cargo test mcp::server::tests:: -- --nocapture`
Expected: PASS (1 test). Full server behavior is verified in Task 7.

- [ ] **Step 6: Commit**

```bash
git add src/mcp Cargo.toml Cargo.lock src/main.rs
git commit -m "feat: rmcp server exposing observability + control tools"
```

---

### Task 6: `webcat mcp install` / `status` / `uninstall`

Implement the install command logic. Pure of Chrome; testable via output snapshot + arg assembly.

**Files:**
- Create: `src/mcp/install.rs`
- Modify: `src/mcp/mod.rs` (`pub mod install;`)
- Test: inline `#[cfg(test)]` in `src/mcp/install.rs`

**Interfaces:**
- Consumes: `crate::cli::{InstallArgs, UninstallArgs, AgentKind}` (Task 3)
- Produces:
  - `pub fn run_install(args: InstallArgs) -> Result<()>`
  - `pub fn run_status() -> Result<()>`
  - `pub fn run_uninstall(args: UninstallArgs) -> Result<()>`
  - `pub fn mcp_url(port: u16) -> String` → `http://127.0.0.1:<port>/mcp`
  - `fn claude_add_args(port: u16) -> Vec<String>` (unit-tested)
  - `fn print_snippet(port: u16) -> String` (unit-tested)

- [ ] **Step 1: Write the failing tests**

Create `src/mcp/install.rs`:

```rust
use std::process::Command as Proc;

use crate::cli::{AgentKind, InstallArgs, UninstallArgs};
use crate::error::{Error, Result};

const DEFAULT_PORT: u16 = 4470;

pub fn mcp_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

fn claude_add_args(port: u16) -> Vec<String> {
    vec![
        "mcp".into(), "add".into(),
        "--transport".into(), "http".into(),
        "webcat".into(), mcp_url(port),
    ]
}

fn print_snippet(port: u16) -> String {
    format!(
        "webcat MCP server\n  URL: {url}\n  transport: streamable-http\n\nClaude Code:\n  claude mcp add --transport http webcat {url}\n",
        url = mcp_url(port)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_format() {
        assert_eq!(mcp_url(4470), "http://127.0.0.1:4470/mcp");
    }

    #[test]
    fn claude_args_shape() {
        let a = claude_add_args(4470);
        assert_eq!(a, vec![
            "mcp", "add", "--transport", "http", "webcat", "http://127.0.0.1:4470/mcp",
        ]);
    }

    #[test]
    fn snippet_mentions_url_and_claude() {
        let s = print_snippet(5000);
        assert!(s.contains("http://127.0.0.1:5000/mcp"));
        assert!(s.contains("claude mcp add"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test mcp::install:: 2>&1 | head -20`
Expected: FAIL — module not declared / functions missing.

- [ ] **Step 3: Implement the commands**

Add `pub mod install;` to `src/mcp/mod.rs`. Append the public command fns to `src/mcp/install.rs`:

```rust
/// Resolve the stable port: CLI arg > config file > DEFAULT_PORT. Also writes
/// the chosen port + mcp.enabled into ~/.webcat/config.yaml so the server
/// auto-starts next run.
fn resolve_and_persist_port(cli_port: Option<u16>, allow_control: bool) -> Result<u16> {
    let file = crate::config::load_file_config()?.unwrap_or_default();
    let existing = file.mcp.as_ref().and_then(|m| m.port);
    let port = cli_port.or(existing).unwrap_or(DEFAULT_PORT);
    write_config_mcp(port, allow_control)?;
    Ok(port)
}

/// Minimal YAML upsert: rewrite ~/.webcat/config.yaml's mcp block. To avoid a
/// full YAML round-trip dependency surface, load the typed FileConfig, set the
/// fields, and serialize back.
fn write_config_mcp(port: u16, allow_control: bool) -> Result<()> {
    use crate::config::{FileConfig, FileMcpConfig};
    let home = dirs::home_dir().ok_or_else(|| Error::Other(anyhow::anyhow!("no home dir")))?;
    let dir = home.join(".webcat");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Other(anyhow::anyhow!("create {}: {e}", dir.display())))?;
    let path = dir.join("config.yaml");
    let mut cfg: FileConfig = crate::config::load_file_config()?.unwrap_or_default();
    cfg.mcp = Some(FileMcpConfig {
        enabled: Some(true),
        port: Some(port),
        allow_control: Some(allow_control),
    });
    let text = serde_yml::to_string(&cfg)
        .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
    std::fs::write(&path, text)
        .map_err(|e| Error::Other(anyhow::anyhow!("write {}: {e}", path.display())))?;
    Ok(())
}

pub fn run_install(args: InstallArgs) -> Result<()> {
    let port = resolve_and_persist_port(args.port, false)?;
    match args.agent {
        AgentKind::Print => {
            print!("{}", print_snippet(port));
        }
        AgentKind::Claude => {
            let status = Proc::new("claude").args(claude_add_args(port)).status();
            match status {
                Ok(s) if s.success() => {
                    println!("Registered webcat MCP server with Claude Code at {}", mcp_url(port));
                }
                _ => {
                    println!(
                        "Could not run `claude mcp add` automatically. Add it manually:\n\n{}",
                        print_snippet(port)
                    );
                }
            }
        }
    }
    println!(
        "\nNote: webcat must be running (with mcp enabled) for the agent to connect.\n\
         Config written to ~/.webcat/config.yaml (mcp.enabled=true, port={port})."
    );
    Ok(())
}

pub fn run_status() -> Result<()> {
    let file = crate::config::load_file_config()?.unwrap_or_default();
    match file.mcp {
        Some(m) => {
            let port = m.port.unwrap_or(DEFAULT_PORT);
            println!(
                "MCP config (~/.webcat/config.yaml):\n  enabled: {}\n  allow_control: {}\n  url: {}",
                m.enabled.unwrap_or(false),
                m.allow_control.unwrap_or(false),
                mcp_url(port),
            );
        }
        None => println!("No MCP config found in ~/.webcat/config.yaml."),
    }
    Ok(())
}

pub fn run_uninstall(args: UninstallArgs) -> Result<()> {
    match args.agent {
        AgentKind::Claude => {
            let status = Proc::new("claude").args(["mcp", "remove", "webcat"]).status();
            match status {
                Ok(s) if s.success() => println!("Removed webcat MCP server from Claude Code."),
                _ => println!("Could not run `claude mcp remove webcat`. Remove it manually."),
            }
        }
        AgentKind::Print => println!("Nothing to remove for --agent print."),
    }
    Ok(())
}
```

> `write_config_mcp` requires `FileConfig`/`FileMcpConfig` to derive `Serialize` too. Update their derives in `src/config.rs` from `Deserialize` to `Serialize, Deserialize`.

- [ ] **Step 4: Update FileConfig derives**

In `src/config.rs`, change both derive lines to:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
```

and add `use serde::Serialize;` (alongside `Deserialize`).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test mcp::install:: -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add src/mcp/install.rs src/mcp/mod.rs src/config.rs
git commit -m "feat: webcat mcp install/status/uninstall commands"
```

---

### Task 7: Wire everything into main + app (startup, dispatch, status bar)

Dispatch subcommands, load file config, share `Browser` via `Arc`, start the MCP server, and show the control indicator.

**Files:**
- Modify: `src/main.rs`
- Modify: `src/app.rs`
- Modify: `src/ui/mod.rs` (status-bar indicator — adapt to actual UI API)
- Test: integration test `tests/mcp_roundtrip.rs` (gated; runs only with Chrome)

**Interfaces:**
- Consumes: `Config::resolve` (Task 2), `Cli`/`Command` (Task 3), `Browser::launch(... store)` (Task 4), `mcp::serve` + `WebcatMcp` (Task 5), `mcp::install::*` (Task 6)
- Produces: working `webcat --mcp` runtime + `webcat mcp ...` dispatch.

- [ ] **Step 1: Dispatch subcommands in main**

In `src/main.rs`, after parsing `Cli`, branch before the normal run path:

```rust
    let cli = Cli::parse();
    if let Some(cli::Command::Mcp(mcp)) = &cli.command {
        return match &mcp.action {
            cli::McpAction::Install(a) => mcp::install::run_install(a.clone()).map_err(Into::into),
            cli::McpAction::Status => mcp::install::run_status().map_err(Into::into),
            cli::McpAction::Uninstall(a) => mcp::install::run_uninstall(a.clone()).map_err(Into::into),
        };
    }

    let file = config::load_file_config().unwrap_or_else(|e| {
        eprintln!("config error: {e}");
        std::process::exit(2);
    });
    let cfg = Config::resolve(cli, file)?;
```

Adjust to match the existing `main` return type and the existing call that builds `Config`. Replace the old `Config::resolve(cli)?` call with the two lines above.

- [ ] **Step 2: Share Browser via Arc and start the MCP server in app::run**

In `src/app.rs`, construct the store, pass it to `Browser::launch`, wrap the browser in `Arc`, and start the server when enabled. Near the top of `run`:

```rust
    let store = std::sync::Arc::new(crate::observability::ObservabilityStore::new(2000));
    let (browser, frame_rx) = Browser::launch(&cfg, chrome, window, store.clone()).await?;
    let browser = std::sync::Arc::new(browser);
```

> Every existing `browser.method()` call in `app.rs` still works through the `Arc` deref. If any code moved `browser` by value, change it to use the `Arc` clone.

After the browser is up and the viewport/screencast are configured, start the MCP server if enabled:

```rust
    let mut mcp_control_active = false;
    if cfg.mcp.enabled {
        let addr: std::net::SocketAddr = (
            std::net::Ipv4Addr::LOCALHOST,
            cfg.mcp.port.unwrap_or(0),
        ).into();
        let handler = crate::mcp::WebcatMcp::new(store.clone(), browser.clone(), cfg.mcp.allow_control);
        match crate::mcp::serve(addr, handler).await {
            Ok(port) => {
                mcp_control_active = cfg.mcp.allow_control;
                tracing::info!("MCP server on http://127.0.0.1:{port}/mcp (control={})", cfg.mcp.allow_control);
            }
            Err(e) => {
                tracing::warn!("MCP server disabled: {e}");
            }
        }
    }
```

- [ ] **Step 3: Show the control indicator in the status bar**

In `src/ui/mod.rs`, thread a `bool` for "MCP control active" into the status bar rendering and append `" 🤖 MCP"` when true. Follow the existing status-bar string-building code; pass `mcp_control_active` from `app.rs` into the `Ui` render call. (Exact wiring depends on the current `Ui` signature — adapt minimally; do not restructure the UI.)

- [ ] **Step 4: Run the full unit suite + clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings 2>&1 | tail -20`
Expected: all unit tests PASS; clippy clean (fix warnings inline).

- [ ] **Step 5: Manual end-to-end verification**

Run in one terminal:

```bash
cargo run -- --mcp --mcp-port 4470 --mcp-allow-control https://example.com
```

In another terminal, confirm the endpoint responds and tools work (using the `claude` CLI or any MCP client pointed at `http://127.0.0.1:4470/mcp`):

```bash
claude mcp add --transport http webcat http://127.0.0.1:4470/mcp
# then in a claude session: call get_console_logs, capture_screenshot, navigate
```

Expected: `get_console_logs`/`get_network_logs` return entries with increasing `seq`; `capture_screenshot` returns an image; `navigate` changes the page the human sees; with `--mcp` but no `--mcp-allow-control`, control tools return the "control tools are disabled" error.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/app.rs src/ui/mod.rs
git commit -m "feat: wire MCP server into webcat startup and subcommand dispatch"
```

---

### Task 8: README + docs

Document the feature.

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Document `--mcp`, `~/.webcat/config.yaml`, and `webcat mcp install`**

Add a "MCP / AI agent integration" section to `README.md` covering: the flags (`--mcp`, `--mcp-port`, `--mcp-allow-control`), the `~/.webcat/config.yaml` example (from the spec §5.4), the `webcat mcp install` quickstart, the tool list, and the security note that control is opt-in and the server binds localhost only. Mention that `~/.webcat/config.yaml` is the only file under `~/.webcat`.

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: document MCP agent integration and config.yaml"
```

---

## Self-Review Notes

- **Spec coverage:** §3 connection/transport → Task 5; §3 read tools → Tasks 1,5; §3 control tools → Tasks 4,5; §4 architecture (store + server task) → Tasks 1,5,7; §5.1 store → Task 1; §5.2 CDP subs → Task 4; §5.3 tool surface → Task 5; §5.4 config → Task 2; §5.5 CLI → Task 3; §5.6 install → Task 6; §6 concurrency/indicator → Task 7; §7 error handling → Tasks 2 (config parse exit), 5/7 (bind failure keeps running), 5 (tool errors); §8 testing → unit tests across tasks + Task 7 integration/manual; §9 deps → Tasks 1,2,5.
- **Known risk spots (flagged inline, must verify against pinned crate sources, not guessed):** exact chromiumoxide_cdp 0.7 field names (Task 4); exact rmcp 0.x constructor/module names — `Content::image`, `ErrorData::*`, `StreamableHttpService::new`, `LocalSessionManager` path (Tasks 5). These are the only places first-compile fixes are expected.
- **Type consistency:** `ObservabilityStore` methods (`push_console`, `push_network`, `console_since`, `network_since`) and entry/page types are used identically in Tasks 1 and 5. `Config.mcp: McpConfig` defined in Task 2 is consumed in Task 7. `Cli` fields defined in Task 3 are consumed in Tasks 2 and 7. `mcp_url`/`claude_add_args` consistent within Task 6.
