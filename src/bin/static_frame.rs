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
