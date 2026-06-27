# webcat MCP Agent Observability & Control — Design

- **Date:** 2026-06-27
- **Status:** Approved (brainstorming) — pending implementation plan
- **Topic:** Expose console logs, network logs, screenshots, and page control to an AI agent over an embedded MCP server.

## 1. Purpose

Let an AI agent (e.g. Claude Code) **observe and co-drive the live webcat session a human is watching** in their terminal. The agent reads console logs, network logs, and screenshots, and can control the page (navigate, click, type, scroll, eval JS) — primarily for debugging / pairing.

webcat already drives Chromium via `chromiumoxide` (Chrome DevTools Protocol). Console / network / screenshot are native CDP capabilities, so this is mostly about **subscribing to events already available on the owned CDP `Page`** and **exposing a control + read surface over MCP**.

## 2. Scope

- **In scope:** read tools (console, network, screenshot, page info); control tools (navigate, click, type, key, scroll, eval JS); embedded MCP server behind opt-in flags; a `~/.webcat/config.yaml` config layer; a `webcat mcp install` subcommand.
- **Out of scope (YAGNI):** multi-instance discovery/registry, push streaming of logs (polling is sufficient over MCP), performance-timeline metrics, agent-only headless sessions (this design targets the human-watched session only).

## 3. Key Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Connection model | **Embedded MCP server in webcat** | webcat owns the single CDP `Page`; embedding avoids a second CDP client racing the screencast (rejected: Chrome `--remote-debugging-port` direct) and avoids a sidecar bridge process. |
| Transport | **streamable-HTTP via `rmcp`** on `127.0.0.1:<port>` | Agent connects to a running process by URL; `rmcp` (official Rust MCP SDK) supports it. Default MCP path `/mcp`. |
| Server activation | **opt-in flag `--mcp` / config `mcp.enabled`** | Off by default for safety; no surprise network surface. |
| Control activation | **second opt-in `--mcp-allow-control` / config `mcp.allow_control`** | Agent mutating a human's live session needs an explicit extra guard. Read tools work with just `--mcp`. |
| Log delivery | **ring buffer + `since_seq` incremental polling** | MCP is request/response; monotonic `seq` lets the agent fetch only new entries and bounds memory. |
| Config file | **`~/.webcat/config.yaml`, parsed with `serde_yml`** | User-requested path and format. `serde_yaml` is unmaintained; `serde_yml` is the maintained fork. This is the only part of webcat that uses `~/.webcat` (profile/log stay on XDG paths). |
| Precedence | **CLI/env > `~/.webcat/config.yaml` > built-in default** | Keep settings persistent by default, allow per-run CLI override. |
| Install command | **`webcat mcp install`, `--agent claude` (default) + `--agent print` fallback** | Registers into Claude Code via `claude mcp add`; prints a paste-able snippet for any other agent. |

## 4. Architecture

```
┌─ webcat process (single) ─────────────────────────────────┐
│                                                           │
│  TUI event loop (app.rs)                                  │
│      │                                                    │
│      ├── Browser (chromiumoxide, owns CDP Page) ──────────┼──> Chromium
│      │     ├─ existing: screencast / input / navigate     │
│      │     └─ new: Network/Runtime/Log event subscriptions│
│      │                                                    │
│      ├── ObservabilityStore (shared ring buffers)         │
│      │     ├─ console deque                                │
│      │     └─ network deque                                │
│      │                                                    │
│      └── MCP server (rmcp, streamable-http)               │
│            127.0.0.1:<port>/mcp ◄─────────────────────────┼──< AI agent (Claude Code, ...)
└───────────────────────────────────────────────────────────┘
```

- Single shared `tokio` runtime (webcat is already `tokio = { features = ["full"] }`). The MCP server runs as a spawned task.
- MCP handlers hold a current-browser handle (`Arc<RwLock<Arc<Browser>>>`) and an `Arc<ObservabilityStore>`. The handle is updated if webcat reconnects Chrome, so the agent keeps observing/controlling the same live session the human sees. Control tools call existing `Browser` methods; read tools read the store.
- The MCP server is independent of the TUI render path — agent calls must not block the frame loop.

## 5. Components

### 5.1 ObservabilityStore
- Two fixed-capacity `VecDeque`s (console, network) behind `Arc<Mutex<…>>`, cap e.g. **2000 entries each** → bounded memory.
- Each entry gets a process-monotonic `seq` (single `AtomicU64`, shared across both buffers so ordering is total). On overflow the oldest entry is dropped and a `dropped` counter increments so the agent can detect gaps.
- Push path (CDP event task) and read path (MCP handler with `since_seq`) take only brief locks.

**Entry shapes**
- console: `{ seq, ts, level, text, url?, line? }`
- network: `{ seq, ts, kind: request|response|failed, method?, url, status?, mime?, size?, timing? }`

### 5.2 CDP subscriptions (in `Browser::launch`)
Alongside the existing 3 listeners (`EventScreencastFrame`, `EventJavascriptDialogOpening`, `EventLoadEventFired`):
- `Network.enable` → `requestWillBeSent`, `responseReceived`, `loadingFailed`
- `Runtime.enable` → `consoleAPICalled`, `exceptionThrown`
- `Log.enable` → `entryAdded`

Each handler maps the CDP payload to a store entry and pushes it.

### 5.3 MCP tool surface

**Read**
| tool | args | returns |
|---|---|---|
| `get_console_logs` | `since_seq?`, `level?`, `limit?` | console entries + `dropped` count |
| `get_network_logs` | `since_seq?`, `status?`, `limit?` | network entries + `dropped` count |
| `capture_screenshot` | `format?` (png/jpeg), `quality?` | MCP image content (base64) via `Page.captureScreenshot` |
| `get_page_info` | — | `url, title, viewport, loading` |

**Control** (registered for discoverability, but returns an MCP error unless `allow_control` is enabled)
| tool | args | action |
|---|---|---|
| `navigate` | `url` | `Browser::navigate` |
| `click` | `x,y` or `selector` | `Browser::click` (selector → coords, reusing the `collect_clickables` pattern) |
| `type_text` | `text` | `Browser::insert_text` |
| `press_key` | `key, mods?` | `Browser::dispatch_key` |
| `scroll` | `dy, x?, y?` | `Browser::scroll` |
| `eval_js` | `script` | `Browser::eval_string` |

### 5.4 Config layer
- New `FileConfig` with all-`Option` fields, deserialized from `~/.webcat/config.yaml` with `serde_yml`. Missing file → ignored (not an error). Missing key → not set.
- `Config::resolve` extended to merge `Cli` + `Option<FileConfig>` with precedence **CLI/env > file > default**.
- New config surface:
  ```yaml
  quality: 92
  zoom: 1.0
  mcp:
    enabled: true        # == --mcp
    port: 4470           # omit → OS-assigned ephemeral port
    allow_control: true  # == --mcp-allow-control
  ```
- Parse failure is surfaced on stderr **before** entering raw mode (same place as the profile-conflict prompt), then exit — no silent fallback to defaults.

### 5.5 CLI restructure (`cli.rs`)
Optional subcommand; no subcommand preserves today's "open URL" behavior.
```
webcat [URL] [flags]
webcat mcp install   [--agent claude|print] [--port N] [--allow-control]
webcat mcp status
webcat mcp uninstall [--agent claude]
```
- New flags on the default command: `--mcp`, `--mcp-port <N>`, `--mcp-allow-control`.

### 5.6 `webcat mcp install`
1. Resolve port: `--port` > config `mcp.port` > propose stable default `4470` and write it into `~/.webcat/config.yaml`.
2. Register the HTTP MCP server with the target agent:
   - `--agent claude` (default): invoke `claude mcp add --transport http webcat http://127.0.0.1:<port>/mcp` if the `claude` CLI is present; otherwise fall back to printing manual instructions.
   - `--agent print`: print the URL + a config snippet only, no side effects.
3. Ensure `mcp.enabled` (and `allow_control` when `--allow-control` is passed) is set in config so the server auto-starts on the next `webcat` run.
4. Print a clear note: **webcat must be running for the agent to connect** (ephemeral embedded server).

## 6. Concurrency & Input Conflict
- Control tools call existing `Browser` methods; CDP serializes commands, so concurrent human + agent input cannot corrupt state (semantic races are still possible).
- `allow_control` opt-in is the primary guard.
- Status bar shows `MCP control active` while control is enabled so the human is aware an agent may intervene.

## 7. Error Handling
- **Config parse failure** → stderr message before raw mode, exit.
- **MCP port bind failure** → webcat keeps running with browser features intact; one warning line + status-bar indicator; MCP disabled for the session.
- **Tool argument / eval errors** (bad selector, JS exception) → returned as MCP error responses; never crash webcat.
- **Agent disconnect** → server stays up; reconnection allowed.

## 8. Testing
- **Unit:** `ObservabilityStore` (seq monotonicity, ring-buffer drop + `dropped` count, `since_seq` filtering); `Config` merge precedence (CLI > file > default); `FileConfig` deserialization; `mcp install` command assembly.
- **Integration:** launch webcat with `--mcp`, drive an MCP client through `get_console_logs` / `navigate` / `capture_screenshot` round-trips; assert write tools are rejected when `allow_control` is off; assert read tools still work.
- **Install:** snapshot test of `--agent print` output (no side effects); unit test of the `claude mcp add` argument vector.

## 9. New Dependencies
- `rmcp` (official Rust MCP SDK, streamable-http server feature).
- `serde_yml` (maintained YAML parser) + `serde` derive.
- Likely a minimal HTTP server dep that `rmcp` pulls in (e.g. `axum`/`hyper`) — confirm during planning.

## 10. Open Items for the Implementation Plan
- Confirm `rmcp` server transport API surface and exact route path.
- Confirm `chromiumoxide` event enum names for Network/Runtime/Log events on the pinned 0.7 version.
- Verify latest dependency APIs from primary sources before implementation: `rmcp` from docs.rs/crates.io latest compatible release, and `chromiumoxide`/`chromiumoxide_cdp` from the pinned 0.7 docs/source.
- Use an updateable current-browser handle (`Arc<RwLock<Arc<Browser>>>`) so MCP survives webcat's existing browser reconnect path.
