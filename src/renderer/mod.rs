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

    // Used in unit tests and reserved for explicit screen-clear on quit (v2).
    #[allow(dead_code)]
    pub fn clear(&self) -> Vec<u8> {
        self.gfx.delete_all()
    }
}

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
