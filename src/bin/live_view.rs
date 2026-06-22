// Throwaway smoke binary (removed in Task 16): open a URL and continuously
// render screencast frames to the terminal for ~10 seconds, then restore.
use std::io::Write;
use std::time::Duration;

#[path = "../error.rs"]    mod error;
#[path = "../cli.rs"]      mod cli;
#[path = "../config.rs"]   mod config;
#[path = "../geometry.rs"] mod geometry;
#[path = "../renderer/mod.rs"] mod renderer;
#[path = "../terminal/mod.rs"] mod terminal;
#[path = "../browser/mod.rs"]  mod browser;

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
        quality: 70,
        dpr: 1.0,
        start_url: url.clone(),
    };
    let (b, mut frames) = browser::Browser::launch(&cfg, chrome).await?;
    b.set_viewport(vp, 1.0).await?;
    b.navigate(&url).await?;
    b.start_screencast(70, vp).await?;

    let renderer = renderer::Renderer::new(grid, cell);

    // Render frames for 10 seconds, then restore the terminal.
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
