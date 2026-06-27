use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::browser::Browser;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::geometry::{self, CellSize, GridSize};
use crate::input::action::Action;
use crate::input::{InputMapper, Mode};
use crate::renderer::Renderer;
use crate::terminal::input::{input_stream, RawInput};
use crate::ui::Ui;

pub async fn run(cfg: Config) -> Result<()> {
    // Resolve a leftover-browser conflict before touching the terminal: a
    // previous webcat browser that never exited still holds the profile lock.
    // Ask whether to kill it (and reuse the real profile) or open anonymously.
    // Done here, before raw mode, so the prompt prints/reads on a normal screen.
    if !resolve_profile_conflict(&cfg) {
        return Ok(());
    }

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
    let cell = CellSize {
        w: cell_raw.w,
        h: cell_raw.h,
    };
    // `vp` is the page capture area in DEVICE pixels — the size the screencast
    // captures and the renderer places 1:1, so it fills the terminal grid. The
    // page's CSS layout size is derived from this and the zoom factor inside the
    // browser layer (set_viewport), which also converts input coordinates.
    let mut vp = geometry::page_viewport(grid, cell, 1);

    let chrome = crate::browser::profile::discover_chrome(cfg.chrome.as_deref())?;
    let store = std::sync::Arc::new(crate::observability::ObservabilityStore::new(2000));
    let (browser, mut frames) =
        Browser::launch(&cfg, chrome, window_for(vp), store.clone()).await?;
    let mut browser = std::sync::Arc::new(browser);
    let current_browser: crate::mcp::CurrentBrowser =
        std::sync::Arc::new(tokio::sync::RwLock::new(browser.clone()));
    // Startup CDP calls are best-effort with a short timeout: Chrome 149 emits
    // some CDP responses chromiumoxide 0.7.0 can't deserialize, so a request can
    // occasionally never see its reply and hang. The command is still delivered
    // (e.g. the navigation happens), so on timeout we log and continue rather
    // than killing the app — the screencast loop renders the page regardless.
    // Normalize the CLI start URL the same way the `:` URL bar does, so a
    // scheme-less argument like `google.com` (or a search term) still opens.
    let start_url = normalize_url(&cfg.start_url);
    best_effort("set_viewport", browser.set_viewport(vp)).await;
    best_effort("navigate", browser.navigate(&start_url)).await;
    best_effort(
        "start_screencast",
        browser.start_screencast(cfg.quality, vp),
    )
    .await;

    let mut mcp_control_active = false;
    if cfg.mcp.enabled {
        let addr: std::net::SocketAddr =
            (std::net::Ipv4Addr::LOCALHOST, cfg.mcp.port.unwrap_or(0)).into();
        let handler = crate::mcp::WebcatMcp::new(
            store.clone(),
            current_browser.clone(),
            cfg.mcp.allow_control,
        );
        match crate::mcp::serve(addr, handler).await {
            Ok(port) => {
                mcp_control_active = cfg.mcp.allow_control;
                tracing::info!(
                    "MCP server on http://127.0.0.1:{port}/mcp (control={})",
                    cfg.mcp.allow_control
                );
            }
            Err(e) => tracing::warn!("MCP server disabled: {e}"),
        }
    }

    let mut renderer = Renderer::new(grid, cell);
    let mut ui = Ui::new(grid);
    let mut mapper = InputMapper::new(cell);
    let mut url_buffer = String::new();

    // Cached URL for status bar (avoids a per-frame round-trip to Chrome).
    // Also used as last_url for reconnection.
    let mut current_url = start_url.clone();

    let mut inputs = input_stream();
    let mut out = std::io::stdout();
    let mut hints: Vec<(String, crate::browser::Clickable)> = Vec::new();
    // Hint labels currently painted on screen (label, col, row). The z=-1 frame
    // no longer hides them, so we must erase these cells when leaving hint mode.
    let mut drawn_hints: Vec<(String, u16, u16)> = Vec::new();
    // Keystrokes typed so far toward selecting a (possibly multi-char) hint.
    let mut hint_buffer = String::new();
    // Last mouse position in device pixels. Keyboard scroll commands reuse this
    // so sites with internal scrollers (Gmail) receive the wheel event over the
    // same pane the user last interacted with; before any mouse input, fall back
    // to the viewport center.
    let mut last_mouse_pos: Option<(f64, f64)> = None;
    // Backpressure: bound how many transmitted frames may be awaiting kitty's
    // graphics ack. A backgrounded/slow kitty acks slowly, so we stop sending
    // (and let frames coalesce) instead of piling up multi-MB shm buffers.
    // SHM_POOL is 4, so keep in-flight below that to never overwrite a buffer
    // kitty is still reading. Backpressure only engages once we've actually seen
    // an ack, so terminals that don't respond keep rendering every frame.
    const MAX_IN_FLIGHT: u32 = 2;
    // If kitty stalls (no acks) for this long while we're blocked, force one
    // frame through so a transient hiccup can't freeze the screen permanently.
    let stall_timeout = Duration::from_millis(750);
    let mut in_flight: u32 = 0;
    let mut gfx_acked = false;
    let mut last_transmit = std::time::Instant::now();

    // alive receiver — watch this for Chrome disconnect.
    let mut browser_alive = browser.alive();
    // navigation counter — re-sync viewport + screencast after each page load.
    let mut browser_nav = browser.navigated();
    let mut browser_loading = browser.loading();
    let mut page_loading = *browser_loading.borrow();
    let mut loading_phase: u16 = 0;
    let mut loading_tick = tokio::time::interval(Duration::from_millis(80));
    loading_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Frame counter for throttled URL refresh (every 30 frames).
    let mut frame_count: u64 = 0;

    // One-shot re-sync ~500ms after launch, plus a re-sync when the capture size
    // changes (relayout). These keep the page filling the terminal.
    let startup_resync = tokio::time::sleep(Duration::from_millis(500));
    tokio::pin!(startup_resync);
    let mut resynced = false;
    let mut last_frame_dims: Option<(u32, u32)> = None;
    let mut last_resync = std::time::Instant::now();

    loop {
        tokio::select! {
            // Frame branch: render the latest frame + status bar.
            maybe = frames.recv() => {
                let Some(f) = maybe else { break; };
                // Backpressure: if too many frames are awaiting kitty's ack, drop
                // this one (the channel keeps the latest, so we render the newest
                // frame once a slot frees). If acks dry up for longer than the
                // stall timeout, assume they were lost, reset the counter, and
                // force a frame through so a hiccup can't freeze the screen.
                if gfx_acked && in_flight >= MAX_IN_FLIGHT {
                    if last_transmit.elapsed() < stall_timeout {
                        continue;
                    }
                    in_flight = 0;
                }
                if let Some((fw, fh)) = jpeg_dims(&f.jpeg) {
                    let changed = matches!(last_frame_dims, Some((lw, lh))
                        if (fw as i64 - lw as i64).abs() > 16 || (fh as i64 - lh as i64).abs() > 16);
                    last_frame_dims = Some((fw, fh));
                    if changed && last_resync.elapsed() > Duration::from_millis(400) {
                        last_resync = std::time::Instant::now();
                        best_effort("set_viewport", browser.set_viewport(vp)).await;
                        best_effort(
                            "start_screencast",
                            browser.start_screencast(cfg.quality, vp),
                        )
                        .await;
                    }
                }
                let frame_bytes = renderer.present_jpeg_bytes(&f.jpeg);
                if !frame_bytes.is_empty() {
                    out.write_all(&frame_bytes)?;
                    in_flight += 1;
                    last_transmit = std::time::Instant::now();
                }
                // Throttled URL refresh: poll Chrome every 30 frames to catch
                // in-page navigation (link clicks, GoBack, JS redirects, etc.)
                // without incurring a round-trip on every frame.
                frame_count += 1;
                if frame_count % 30 == 0 {
                    if let Some(u) = browser.current_url().await {
                        current_url = u;
                    }
                }
                // Use cached current_url — no per-frame round-trip to Chrome.
                write_status(&mut out, &ui, mapper.mode, &current_url, &url_buffer, mcp_control_active, page_loading, loading_phase)?;
                out.flush()?;
            }

            // Alive branch: handle Chrome disconnect.
            res = browser_alive.changed() => {
                // changed() resolves when the value changes; an Err means the
                // sender (alive_tx) was dropped, which is itself a disconnect.
                let _ = res;
                if !*browser_alive.borrow() {
                    out.write_all(&ui.status_bar("disconnected — reconnecting…", true))?;
                    out.flush()?;

                    let chrome = crate::browser::profile::discover_chrome(cfg.chrome.as_deref())?;
                    let mut reconnected = false;
                    for attempt in 1u64..=3 {
                        match Browser::launch(&cfg, chrome.clone(), window_for(vp), store.clone()).await {
                            Ok((nb, nf)) => {
                                if nb.set_viewport(vp).await.is_ok()
                                    && nb.navigate(&current_url).await.is_ok()
                                    && nb.start_screencast(cfg.quality, vp).await.is_ok()
                                {
                                    let nb = std::sync::Arc::new(nb);
                                    browser_alive = nb.alive();
                                    browser_nav = nb.navigated();
                                    browser_loading = nb.loading();
                                    page_loading = *browser_loading.borrow();
                                    browser = nb.clone();
                                    *current_browser.write().await = nb;
                                    frames = nf;
                                    reconnected = true;
                                    break;
                                } else {
                                    tracing::warn!("reconnect attempt {attempt}: post-launch setup failed");
                                }
                            }
                            Err(e) => {
                                tracing::warn!("reconnect attempt {attempt} failed: {e}");
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
                    }

                    if !reconnected {
                        out.write_all(&ui.status_bar("browser unavailable (gave up after 3 tries)", false))?;
                        out.flush()?;
                        break;
                    }
                }
            }

            // One-shot startup re-sync (fires once ~500ms after launch).
            _ = &mut startup_resync, if !resynced => {
                resynced = true;
                let (c, r) = crossterm::terminal::size()?;
                grid = GridSize { cols: c, rows: r };
                vp = geometry::page_viewport(grid, cell, 1);
                renderer.resize(grid, cell);
                best_effort("set_viewport", browser.set_viewport(vp)).await;
                best_effort(
                    "start_screencast",
                    browser.start_screencast(cfg.quality, vp),
                )
                .await;
            }

            // Re-sync after a navigation completes so the new page fills.
            res = browser_nav.changed() => {
                if res.is_ok() {
                    best_effort("set_viewport", browser.set_viewport(vp)).await;
                    best_effort(
                        "start_screencast",
                        browser.start_screencast(cfg.quality, vp),
                    )
                    .await;
                }
            }

            res = browser_loading.changed() => {
                if res.is_ok() {
                    page_loading = *browser_loading.borrow();
                    write_status(&mut out, &ui, mapper.mode, &current_url, &url_buffer, mcp_control_active, page_loading, loading_phase)?;
                    out.flush()?;
                }
            }

            _ = loading_tick.tick(), if page_loading => {
                loading_phase = loading_phase.wrapping_add(1);
                write_status(&mut out, &ui, mapper.mode, &current_url, &url_buffer, mcp_control_active, true, loading_phase)?;
                out.flush()?;
            }

            // Input branch: drain the entire pending backlog in one iteration so
            // a click is never starved behind a flood of mouse-move events (and
            // interleaved frame renders). Mouse moves are coalesced to the latest
            // and dispatched once per batch as a hover.
            maybe = inputs.recv() => {
                let Some(first) = maybe else { break; };
                let mut batch = vec![first];
                while let Ok(ev) = inputs.try_recv() {
                    batch.push(ev);
                    if batch.len() >= 512 { break; }
                }

                let mut pending_move: Option<(f64, f64)> = None;
                let mut quit = false;
                for ri in batch {
                    let action = match ri {
                        RawInput::Key(ev) => mapper.on_key(ev),
                        RawInput::Mouse(ev) => mapper.on_mouse(ev),
                        RawInput::Resize => {
                            let (c, r) = crossterm::terminal::size()?;
                            // Ignore no-op resizes (e.g. the spurious SIGWINCH some
                            // terminals send at startup). Restarting the screencast
                            // resets Chrome's capture from device resolution back to
                            // logical (half-res, blurry), so only do it on a real
                            // size change.
                            if (c, r) != (grid.cols, grid.rows) {
                                grid = GridSize { cols: c, rows: r };
                                vp = geometry::page_viewport(grid, cell, 1);
                                renderer.resize(grid, cell);
                                ui.resize(grid);
                                // Clear stale text (e.g. the old status bar row,
                                // now mid-screen); the image (z=-1) survives ED,
                                // and the next frame redraws status at the new row.
                                out.write_all(b"\x1b[2J")?;
                                out.flush()?;
                                best_effort("set_viewport", browser.set_viewport(vp)).await;
                                best_effort(
                                    "start_screencast",
                                    browser.start_screencast(cfg.quality, vp),
                                )
                                .await;
                            }
                            Action::None
                        }
                        // Graphics ack from kitty: it finished processing a
                        // transmitted frame, so free one in-flight slot.
                        RawInput::GfxAck => {
                            if !gfx_acked {
                                tracing::info!("backpressure engaged: terminal graphics acks detected");
                            }
                            gfx_acked = true;
                            in_flight = in_flight.saturating_sub(1);
                            Action::None
                        }
                    };

                    match action {
                        Action::Quit => { quit = true; break; }
                        // Coalesce hover moves; dispatched once after the batch.
                        Action::MoveMouse { x, y } => {
                            last_mouse_pos = Some((x, y));
                            pending_move = Some((x, y));
                        }
                        Action::InsertText(t) => { let _ = browser.insert_text(&t).await; }
                        Action::Key(k, m) => {
                            let _ = browser.dispatch_key(k, m, true).await;
                            let _ = browser.dispatch_key(k, m, false).await;
                        }
                        Action::ClickPixel { x, y, button } => {
                            last_mouse_pos = Some((x, y));
                            // The click moves to its own point; drop a stale hover.
                            pending_move = None;
                            let _ = browser.click(x, y, button).await;
                        }
                        Action::ScrollPixel { x, y, dy } => {
                            let (sx, sy) = scroll_target(x, y, last_mouse_pos, vp);
                            last_mouse_pos = Some((sx, sy));
                            let _ = browser.scroll(sx, sy, dy).await;
                        }
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
                            browser.begin_loading();
                            let nav_browser = browser.clone();
                            let nav_target = target.clone();
                            tokio::spawn(async move {
                                let _ = nav_browser.navigate(&nav_target).await;
                            });
                            // Show the submitted URL immediately; Chrome events
                            // will reconcile it once navigation progresses.
                            current_url = target;
                            url_buffer.clear();
                        }
                        Action::UrlCancel => {
                            url_buffer.clear();
                            // Esc out of hint mode also routes here — erase labels.
                            hint_buffer.clear();
                            out.write_all(&renderer.clear_hint_boxes())?;
                            if !drawn_hints.is_empty() {
                                out.write_all(&ui.clear_hints(&drawn_hints))?;
                                drawn_hints.clear();
                            }
                        }
                        Action::EnterHintMode => {
                            let clickables = browser.collect_clickables().await.unwrap_or_default();
                            if clickables.is_empty() {
                                mapper.mode = crate::input::Mode::Normal;
                                out.write_all(&ui.status_bar("(no clickable elements)", false))?;
                                out.flush()?;
                            } else {
                                hints = crate::input::hints::assign(&clickables);
                                let overlay: Vec<(String, u16, u16)> = hints.iter().map(|(label, c)| {
                                    let col = (c.x / cell.w as f64) as u16;
                                    let row = (c.y / cell.h as f64) as u16;
                                    (label.clone(), col, row)
                                }).collect();
                                // Bake the label backgrounds into the frame (z=-1
                                // hides cell backgrounds), then draw the letters.
                                out.write_all(&renderer.set_hint_boxes(hint_boxes(&overlay, cell)))?;
                                out.write_all(&ui.hint_overlay(&overlay))?;
                                out.flush()?;
                                drawn_hints = overlay;
                                hint_buffer.clear();
                            }
                        }
                        Action::HintKey(c) => {
                            // Accumulate keystrokes: labels can be multi-char
                            // (>26 elements -> two letters), so a single key is
                            // usually just a prefix, not a complete selection.
                            hint_buffer.push(c);
                            if let Some((_, target)) = hints.iter().find(|(l, _)| l.as_str() == hint_buffer.as_str()) {
                                // Full label typed: click it and leave hint mode.
                                let _ = browser.click(target.x, target.y, crate::terminal::mouse::MouseButton::Left).await;
                                if let Some(url) = browser.current_url().await {
                                    current_url = url;
                                }
                                mapper.mode = crate::input::Mode::Normal;
                                hints.clear();
                                hint_buffer.clear();
                                out.write_all(&renderer.clear_hint_boxes())?;
                                if !drawn_hints.is_empty() {
                                    out.write_all(&ui.clear_hints(&drawn_hints))?;
                                    drawn_hints.clear();
                                }
                                out.flush()?;
                            } else {
                                // Narrow to labels still matching the prefix.
                                let matching: Vec<(String, u16, u16)> = drawn_hints
                                    .iter()
                                    .filter(|(l, _, _)| l.starts_with(&hint_buffer))
                                    .cloned()
                                    .collect();
                                if matching.is_empty() {
                                    // Stray key: ignore it, keep the prior prefix.
                                    hint_buffer.pop();
                                } else {
                                    out.write_all(&renderer.set_hint_boxes(hint_boxes(&matching, cell)))?;
                                    out.write_all(&ui.clear_hints(&drawn_hints))?;
                                    out.write_all(&ui.hint_overlay(&matching))?;
                                    out.flush()?;
                                    drawn_hints = matching;
                                }
                            }
                        }
                        Action::None => {}
                    }
                }

                // Dispatch the coalesced hover move once per batch (enables :hover).
                if let Some((x, y)) = pending_move {
                    let _ = browser.move_mouse(x, y).await;
                }
                // Redraw the status line immediately after input so URL editing
                // and mode changes update even on static pages (no frame ticks).
                page_loading = *browser_loading.borrow();
                write_status(&mut out, &ui, mapper.mode, &current_url, &url_buffer, mcp_control_active, page_loading, loading_phase)?;
                out.flush()?;
                if quit { break; }
            }
        }
    }

    guard.restore();
    Ok(())
}

/// Handle a leftover-browser profile conflict before the UI starts.
///
/// If no *live* process holds the default profile, returns `true` immediately
/// (the normal path). If one does, the user is asked whether to kill it and
/// reuse the real profile (history, logins) or open anonymously in a throwaway
/// profile. When stdin isn't a TTY (e.g. piped/automated), we can't ask, so we
/// keep the historical behaviour and open anonymously without prompting.
///
/// Returns `false` only when the user chooses to quit.
fn resolve_profile_conflict(cfg: &Config) -> bool {
    use std::io::{IsTerminal, Write};

    let Some(pid) = crate::browser::profile::detect_conflict(&cfg.profile_dir) else {
        return true;
    };

    if !std::io::stdin().is_terminal() {
        tracing::warn!("profile held by live process {pid}; stdin not a TTY, opening anonymously");
        return true;
    }

    let mut err = std::io::stderr();
    loop {
        let _ = write!(
            err,
            "\nwebcat: a previous browser (pid {pid}) is still running and holds your profile.\n  \
             [k] kill it and reuse your profile (history, logins)\n  \
             [a] open anonymously in a temporary profile\n  \
             [q] quit\nchoice [k/a/q]: ",
        );
        let _ = err.flush();

        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            // EOF (e.g. ^D): treat as quit rather than looping forever.
            let _ = writeln!(err);
            return false;
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "k" | "kill" => {
                if crate::browser::profile::kill_profile_holder(pid) {
                    let _ = writeln!(err, "killed {pid}; reusing your profile.");
                } else {
                    let _ = writeln!(err, "could not kill {pid}; opening anonymously instead.");
                }
                return true;
            }
            "a" | "anon" | "anonymous" => {
                let _ = writeln!(err, "opening anonymously.");
                return true;
            }
            "q" | "quit" => return false,
            _ => {
                let _ = writeln!(err, "please type k, a, or q.");
            }
        }
    }
}

/// Convert hint overlay entries (label, col, row) to frame-pixel rectangles for
/// the renderer to bake in as label backgrounds. Each box spans the label's
/// cells: width = label length × cell width, height = one cell.
fn hint_boxes(
    overlay: &[(String, u16, u16)],
    cell: geometry::CellSize,
) -> Vec<crate::renderer::HintBox> {
    overlay
        .iter()
        .map(|(label, col, row)| crate::renderer::HintBox {
            x: *col as u32 * cell.w as u32,
            y: *row as u32 * cell.h as u32,
            w: label.chars().count() as u32 * cell.w as u32,
            h: cell.h as u32,
        })
        .collect()
}

/// Draw the bottom status line (and the URL prompt when in URL-input mode).
/// Each piece clears its line first, so shrinking text leaves no leftovers.
fn write_status(
    out: &mut impl std::io::Write,
    ui: &Ui,
    mode: Mode,
    current_url: &str,
    url_buffer: &str,
    mcp_control_active: bool,
    loading: bool,
    loading_phase: u16,
) -> std::io::Result<()> {
    let status = match mode {
        Mode::Insert => format!("-- INSERT --  {current_url}"),
        _ => current_url.to_string(),
    };
    let status = if mcp_control_active {
        format!("{status}  MCP control active")
    } else {
        status
    };
    out.write_all(&ui.status_bar_frame(&status, loading, loading_phase))?;
    if mode == Mode::UrlInput {
        out.write_all(&ui.url_prompt(url_buffer))?;
    }
    Ok(())
}

/// Compositor window size for the headless browser. Must be >= the (device)
/// page viewport so the screencast doesn't crop the page; macOS new-headless
/// insets the surface unpredictably, so a generous margin makes the captured
/// surface reach (near) the full device viewport in both dimensions.
fn window_for(dev: geometry::Viewport) -> (u32, u32) {
    (dev.width_px + 600, dev.height_px + 600)
}

fn scroll_target(
    x: f64,
    y: f64,
    last_mouse_pos: Option<(f64, f64)>,
    vp: geometry::Viewport,
) -> (f64, f64) {
    if x != 0.0 || y != 0.0 {
        return (x, y);
    }
    last_mouse_pos.unwrap_or((vp.width_px as f64 / 2.0, vp.height_px as f64 / 2.0))
}

/// Read a JPEG/PNG's pixel dimensions from its header (to detect capture-size
/// changes for the re-sync). Returns None if not parseable.
fn jpeg_dims(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() >= 24 && &b[0..8] == b"\x89PNG\r\n\x1a\n" {
        let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
        let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
        return Some((w, h));
    }
    if b.len() >= 4 && b[0] == 0xFF && b[1] == 0xD8 {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let m = b[i + 1];
            if (0xC0..=0xCF).contains(&m) && m != 0xC4 && m != 0xC8 && m != 0xCC {
                let h = ((b[i + 5] as u32) << 8) | b[i + 6] as u32;
                let w = ((b[i + 7] as u32) << 8) | b[i + 8] as u32;
                return Some((w, h));
            }
            let len = ((b[i + 2] as usize) << 8) | b[i + 3] as usize;
            i += 2 + len;
        }
    }
    None
}

/// Await a CDP call with a short timeout; on timeout or error, log and continue
/// instead of aborting. Chrome 149 + chromiumoxide 0.7.0 can hang a request
/// whose response fails to deserialize, but the command itself still runs, so
/// killing the app over it (a black screen) is worse than proceeding.
async fn best_effort<F>(what: &str, fut: F)
where
    F: std::future::Future<Output = Result<()>>,
{
    match tokio::time::timeout(Duration::from_secs(1), fut).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("{what} failed (continuing): {e}"),
        Err(_) => tracing::warn!("{what} timed out (continuing)"),
    }
}

fn normalize_url(input: &str) -> String {
    let t = input.trim();
    if t.contains("://") || t.starts_with("about:") {
        t.to_string()
    } else if let Some(url) = file_url_if_path(t) {
        url
    } else if t.contains('.') && !t.contains(' ') {
        format!("https://{t}")
    } else {
        format!("https://www.google.com/search?q={}", urlencode(t))
    }
}

fn file_url_if_path(input: &str) -> Option<String> {
    if input.is_empty() {
        return None;
    }
    let path = expand_home(input);
    if !path.exists() {
        return None;
    }
    let abs = path.canonicalize().ok()?;
    Some(format!("file://{}", encode_file_path(&abs)))
}

fn expand_home(input: &str) -> PathBuf {
    if input == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(input))
    } else if let Some(rest) = input.strip_prefix("~/") {
        dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(input))
    } else {
        PathBuf::from(input)
    }
}

fn encode_file_path(path: &Path) -> String {
    path.to_string_lossy()
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{:02X}", b),
        })
        .collect()
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{:02X}", b),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_scroll_uses_last_mouse_position() {
        let vp = geometry::Viewport {
            width_px: 800,
            height_px: 600,
        };
        assert_eq!(
            scroll_target(0.0, 0.0, Some((120.0, 240.0)), vp),
            (120.0, 240.0)
        );
    }

    #[test]
    fn keyboard_scroll_falls_back_to_viewport_center() {
        let vp = geometry::Viewport {
            width_px: 801,
            height_px: 601,
        };
        assert_eq!(scroll_target(0.0, 0.0, None, vp), (400.5, 300.5));
    }

    #[test]
    fn mouse_wheel_keeps_actual_position() {
        let vp = geometry::Viewport {
            width_px: 800,
            height_px: 600,
        };
        assert_eq!(
            scroll_target(10.0, 20.0, Some((120.0, 240.0)), vp),
            (10.0, 20.0)
        );
    }

    #[test]
    fn normalize_url_uses_existing_file_paths() {
        let dir = std::env::temp_dir().join(format!("webcat-url-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hello world.html");
        std::fs::write(&file, "<h1>hello</h1>").unwrap();

        let url = normalize_url(file.to_str().unwrap());
        assert!(url.starts_with("file:///"), "{url}");
        assert!(url.ends_with("/hello%20world.html"), "{url}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalize_url_keeps_web_and_search_inputs() {
        assert_eq!(
            normalize_url("https://example.com/a b"),
            "https://example.com/a b"
        );
        assert_eq!(normalize_url("example.com"), "https://example.com");
        assert_eq!(
            normalize_url("rust async book"),
            "https://www.google.com/search?q=rust+async+book"
        );
    }
}
