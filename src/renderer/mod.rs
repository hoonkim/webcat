pub mod graphics;
pub mod shm;

use crate::geometry::{GridSize, CellSize};

const IMAGE_ID: u32 = 1;

pub struct Renderer {
    gfx: graphics::KittyGraphics,
    shm: shm::Shm,
    #[allow(dead_code)]
    grid: GridSize,
    #[allow(dead_code)]
    cell: CellSize,
}

impl Renderer {
    pub fn new(grid: GridSize, cell: CellSize) -> Renderer {
        Renderer {
            gfx: graphics::KittyGraphics::new(IMAGE_ID),
            shm: shm::Shm::new(),
            grid,
            cell,
        }
    }

    pub fn resize(&mut self, grid: GridSize, cell: CellSize) {
        self.grid = grid;
        self.cell = cell;
    }

    /// Decode a JPEG screencast frame to RGBA, write the pixels to shared memory,
    /// and return the bytes to position the cursor and display the frame from
    /// shared memory (kitty `f=32,t=s`). Returns an empty Vec if the frame can't
    /// be decoded or written (the caller simply skips it — the previous frame
    /// stays on screen).
    pub fn present_jpeg_bytes(&self, jpeg: &[u8]) -> Vec<u8> {
        let img = match image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg) {
            Ok(img) => img.into_rgba8(),
            Err(e) => {
                tracing::warn!("frame JPEG decode failed: {e}");
                return Vec::new();
            }
        };
        let (w, h) = img.dimensions();
        let rgba = img.into_raw();
        if let Err(e) = self.shm.write(&rgba) {
            tracing::warn!("shm write failed: {e}");
            return Vec::new();
        }
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(b"\x1b[1;1H"); // cursor to row 1, col 1
        out.extend_from_slice(&self.gfx.transmit_shm(self.shm.name_base64(), w, h));
        out
    }

    // exercised by the clear_emits_delete unit test
    #[allow(dead_code)]
    pub fn clear(&self) -> Vec<u8> {
        self.gfx.delete_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{GridSize, CellSize};

    /// Encode a tiny solid-color image to JPEG bytes for use as a fake frame.
    fn tiny_jpeg() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(4, 4, image::Rgb([10, 20, 30]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
        buf.into_inner()
    }

    #[test]
    fn present_positions_cursor_then_transmits_from_shm() {
        let r = Renderer::new(GridSize { cols: 80, rows: 24 }, CellSize { w: 8, h: 16 });
        let out = String::from_utf8(r.present_jpeg_bytes(&tiny_jpeg())).unwrap();
        // Cursor home (row1,col1) before the graphics block.
        let home_idx = out.find("\x1b[1;1H").expect("cursor home missing");
        let gfx_idx = out.find("\x1b_G").expect("graphics block missing");
        assert!(home_idx < gfx_idx, "cursor must be positioned before placement");
        assert!(out.contains("t=s"), "frame must be sent via shared memory");
        assert!(out.contains("f=32"), "frame must be RGBA");
    }

    #[test]
    fn present_skips_invalid_jpeg() {
        let r = Renderer::new(GridSize { cols: 80, rows: 24 }, CellSize { w: 8, h: 16 });
        // Not a JPEG: decode fails, frame skipped (empty output, no panic).
        let out = r.present_jpeg_bytes(&[0x00, 0x01, 0x02, 0x03]);
        assert!(out.is_empty());
    }

    #[test]
    fn clear_emits_delete() {
        let r = Renderer::new(GridSize { cols: 80, rows: 24 }, CellSize { w: 8, h: 16 });
        let out = String::from_utf8(r.clear()).unwrap();
        assert!(out.contains("a=d"));
    }
}
