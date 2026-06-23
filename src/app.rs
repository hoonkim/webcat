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
    // `vp` is the page area in LOGICAL (CSS) pixels — the layout viewport.
    // `dev` is that area in DEVICE pixels (vp × dpr) — the size the screencast
    // captures and the renderer places 1:1, so it fills the terminal's HiDPI
    // backing store (and renders crisply). This mirrors how awrit renders the
    // browser at device resolution.
    let mut vp = geometry::page_viewport(grid, cell, 1);
    let mut dev = dev_viewport(vp, cfg.dpr);

    let chrome = crate::browser::profile::discover_chrome(cfg.chrome.as_deref())?;
    let (mut browser, mut frames) = Browser::launch(&cfg, chrome, window_for(dev)).await?;
    browser.set_viewport(vp, cfg.dpr).await?;
    browser.navigate(&cfg.start_url).await?;
    browser.start_screencast(cfg.quality, dev).await?;

    let mut renderer = Renderer::new(grid, cell);
    let ui = Ui::new(grid);
    let mut mapper = InputMapper::new(cell);
    let mut url_buffer = String::new();

    // Cached URL for status bar (avoids a per-frame round-trip to Chrome).
    // Also used as last_url for reconnection.
    let mut current_url = cfg.start_url.clone();

    let mut inputs = Box::pin(input_stream());
    let mut out = std::io::stdout();
    let mut hints: Vec<(String, crate::browser::Clickable)> = Vec::new();

    // alive receiver — watch this for Chrome disconnect.
    let mut browser_alive = browser.alive();

    // Frame counter for throttled URL refresh (every 30 frames).
    let mut frame_count: u64 = 0;

    // One-shot re-sync shortly after startup: the first screencast frames can be
    // captured before the headless window/viewport fully settles, leaving the
    // image slightly smaller than the terminal. Re-running the resize path once
    // it has settled makes the page fill exactly (same effect as a manual
    // resize, which the user observed fixes it).
    let startup_resync = tokio::time::sleep(Duration::from_millis(500));
    tokio::pin!(startup_resync);
    let mut resynced = false;

    loop {
        tokio::select! {
            // Frame branch: render the latest frame + status bar.
            maybe = frames.recv() => {
                let Some(f) = maybe else { break; };
                out.write_all(&renderer.present_jpeg_bytes(&f.jpeg))?;
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
                let status = match mapper.mode {
                    Mode::Insert => format!("-- INSERT --  {current_url}"),
                    _ => current_url.clone(),
                };
                out.write_all(&ui.status_bar(&status, false))?;
                if mapper.mode == Mode::UrlInput {
                    out.write_all(&ui.url_prompt(&url_buffer))?;
                }
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
                        match Browser::launch(&cfg, chrome.clone(), window_for(dev)).await {
                            Ok((nb, nf)) => {
                                if nb.set_viewport(vp, cfg.dpr).await.is_ok()
                                    && nb.navigate(&current_url).await.is_ok()
                                    && nb.start_screencast(cfg.quality, dev).await.is_ok()
                                {
                                    browser_alive = nb.alive();
                                    browser = nb;
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
                dev = dev_viewport(vp, cfg.dpr);
                renderer.resize(grid, cell);
                browser.set_viewport(vp, cfg.dpr).await?;
                let _ = browser.start_screencast(cfg.quality, dev).await;
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
                        dev = dev_viewport(vp, cfg.dpr);
                        renderer.resize(grid, cell);
                        browser.set_viewport(vp, cfg.dpr).await?;
                        let _ = browser.start_screencast(cfg.quality, dev).await;
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
                        // Update cached URL on successful submission.
                        current_url = target;
                        url_buffer.clear();
                    }
                    Action::UrlCancel => { url_buffer.clear(); }
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
                            out.write_all(&ui.hint_overlay(&overlay))?;
                            out.flush()?;
                        }
                    }
                    Action::HintKey(c) => {
                        if let Some((_, target)) = hints.iter().find(|(l, _)| l == &c.to_string()) {
                            let _ = browser.click(target.x, target.y, crate::terminal::mouse::MouseButton::Left).await;
                            // Refresh URL after hint-based navigation (best-effort).
                            if let Some(url) = browser.current_url().await {
                                current_url = url;
                            }
                        }
                        mapper.mode = crate::input::Mode::Normal;
                        hints.clear();
                    }
                    Action::None => {}
                }
            }
        }
    }

    guard.restore();
    Ok(())
}

/// The page area in DEVICE pixels: logical viewport × dpr. Chrome renders at
/// this resolution (via deviceScaleFactor) and the screencast captures it, so a
/// native (1:1) placement fills the terminal's HiDPI backing store.
fn dev_viewport(css: geometry::Viewport, dpr: f64) -> geometry::Viewport {
    geometry::Viewport {
        width_px: (css.width_px as f64 * dpr).round() as u32,
        height_px: (css.height_px as f64 * dpr).round() as u32,
    }
}

/// Compositor window size for the headless browser. Must be >= the (device)
/// page viewport so the screencast doesn't crop the page; macOS new-headless
/// insets the surface unpredictably, so a generous margin makes the captured
/// surface reach (near) the full device viewport in both dimensions.
fn window_for(dev: geometry::Viewport) -> (u32, u32) {
    (dev.width_px + 600, dev.height_px + 600)
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
