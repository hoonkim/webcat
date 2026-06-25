pub mod graphics;
pub mod shm;

use crate::geometry::{GridSize, CellSize};

const IMAGE_ID: u32 = 1;
/// Number of shared-memory buffers to rotate through. Using a fresh buffer each
/// frame means the object kitty is still reading (from the previous frame) is
/// never the one we're overwriting — which otherwise causes an occasional black
/// flash when kitty reads a half-written/just-unlinked object.
const SHM_POOL: usize = 4;

/// A solid rectangle (frame pixels) to paint onto the frame as a hint-label
/// background. kitty draws the z=-1 frame above *all* cell backgrounds, so a
/// terminal-cell background can never show over it — the only way to give hint
/// labels a visible box is to bake it into the frame pixels themselves. The
/// label letter is still drawn as ordinary terminal text on top of the frame.
#[derive(Clone, Copy)]
pub struct HintBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Hint-box fill colour (RGB), a golden yellow that reads well under black text.
const HINT_RGB: [u8; 3] = [255, 204, 0];

pub struct Renderer {
    gfx: graphics::KittyGraphics,
    shms: Vec<shm::Shm>,
    shm_idx: usize,
    /// Last decoded frame (clean, before any hint boxes) so we can re-transmit
    /// it when hints are shown/hidden without waiting for a fresh screencast
    /// frame (the page is usually static while picking a hint).
    last_frame: Option<(Vec<u8>, u32, u32)>,
    /// Hint boxes currently baked into the displayed frame (empty when not in
    /// hint mode). Re-applied to every incoming frame so they persist.
    hint_boxes: Vec<HintBox>,
    #[allow(dead_code)]
    grid: GridSize,
    #[allow(dead_code)]
    cell: CellSize,
}

impl Renderer {
    pub fn new(grid: GridSize, cell: CellSize) -> Renderer {
        Renderer {
            gfx: graphics::KittyGraphics::new(IMAGE_ID),
            shms: (0..SHM_POOL).map(|_| shm::Shm::new()).collect(),
            shm_idx: 0,
            last_frame: None,
            hint_boxes: Vec::new(),
            grid,
            cell,
        }
    }

    pub fn resize(&mut self, grid: GridSize, cell: CellSize) {
        self.grid = grid;
        self.cell = cell;
    }

    /// Decode a screencast frame (PNG/JPEG) to RGBA, write the pixels to the next
    /// shared-memory buffer in the pool, and return the bytes to position the
    /// cursor and display the frame from shared memory (kitty `f=32,t=s`).
    /// Returns an empty Vec if the frame can't be decoded or written (the caller
    /// simply skips it — the previous frame stays on screen).
    pub fn present_jpeg_bytes(&mut self, jpeg: &[u8]) -> Vec<u8> {
        let img = match image::load_from_memory(jpeg) {
            Ok(img) => img.into_rgba8(),
            Err(e) => {
                tracing::warn!("frame decode failed: {e}");
                return Vec::new();
            }
        };
        let (w, h) = img.dimensions();
        let mut rgba = img.into_raw();
        if self.hint_boxes.is_empty() {
            // Common path: no boxes, so the decoded pixels are both what we
            // display and what we cache — no clone needed.
            let out = self.transmit(&rgba, w, h);
            self.last_frame = Some((rgba, w, h));
            out
        } else {
            // Hint mode: cache the clean frame, then display a boxed copy.
            self.last_frame = Some((rgba.clone(), w, h));
            let (dw, dh) = self.dev_grid();
            bake_boxes(&mut rgba, w, h, dw, dh, &self.hint_boxes);
            self.transmit(&rgba, w, h)
        }
    }

    /// The page area in device pixels (cols×cell, page rows×cell) — the space
    /// hint boxes use. The frame may differ (logical capture / inset), so boxes
    /// are scaled by frame/device when baked to stay aligned with the cell grid.
    fn dev_grid(&self) -> (u32, u32) {
        let page_rows = self.grid.rows.saturating_sub(1) as u32;
        (
            self.grid.cols as u32 * self.cell.w as u32,
            page_rows * self.cell.h as u32,
        )
    }

    /// Show `boxes` as hint-label backgrounds, re-rendering the cached frame so
    /// they appear immediately even on a static page. Returns the escape bytes.
    pub fn set_hint_boxes(&mut self, boxes: Vec<HintBox>) -> Vec<u8> {
        self.hint_boxes = boxes;
        self.rerender_cached()
    }

    /// Remove all hint-label backgrounds and re-render the clean cached frame.
    pub fn clear_hint_boxes(&mut self) -> Vec<u8> {
        if self.hint_boxes.is_empty() {
            return Vec::new();
        }
        self.hint_boxes.clear();
        self.rerender_cached()
    }

    /// Re-bake the current hint boxes onto the last cached frame and transmit.
    fn rerender_cached(&mut self) -> Vec<u8> {
        let Some((clean, w, h)) = self.last_frame.clone() else {
            return Vec::new();
        };
        let mut rgba = clean;
        let (dw, dh) = self.dev_grid();
        bake_boxes(&mut rgba, w, h, dw, dh, &self.hint_boxes);
        self.transmit(&rgba, w, h)
    }

    /// Write `rgba` to the next shm buffer and build the cursor-home + display
    /// escape, scaling the frame to fill the page cell area. Returns empty on shm
    /// failure (caller keeps the previous frame).
    fn transmit(&mut self, rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
        let cols = self.grid.cols;
        let page_rows = self.grid.rows.saturating_sub(1);
        let shm = &self.shms[self.shm_idx];
        self.shm_idx = (self.shm_idx + 1) % self.shms.len();
        if let Err(e) = shm.write(rgba) {
            tracing::warn!("shm write failed: {e}");
            return Vec::new();
        }
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(b"\x1b[1;1H"); // cursor to row 1, col 1
        out.extend_from_slice(&self.gfx.transmit_shm(shm.name_base64(), w, h, cols, page_rows));
        out
    }

    // exercised by the clear_emits_delete unit test
    #[allow(dead_code)]
    pub fn clear(&self) -> Vec<u8> {
        self.gfx.delete_all()
    }
}

/// Fill each box with the hint colour. Boxes are in device-grid pixels
/// (`dev_w`×`dev_h`); the frame (`w`×`h`) may differ (logical capture / inset)
/// and is cell-scaled to the grid on display, so each box is scaled by
/// frame/device to stay aligned with the text labels drawn on top.
fn bake_boxes(rgba: &mut [u8], w: u32, h: u32, dev_w: u32, dev_h: u32, boxes: &[HintBox]) {
    if dev_w == 0 || dev_h == 0 {
        return;
    }
    let sx = w as f64 / dev_w as f64;
    let sy = h as f64 / dev_h as f64;
    for b in boxes {
        let bx = (b.x as f64 * sx) as u32;
        let by = (b.y as f64 * sy) as u32;
        let bw = (b.w as f64 * sx).ceil() as u32;
        let bh = (b.h as f64 * sy).ceil() as u32;
        let x1 = (bx + bw).min(w);
        let y1 = (by + bh).min(h);
        for y in by.min(h)..y1 {
            let row = (y * w) as usize * 4;
            for x in bx.min(w)..x1 {
                let i = row + x as usize * 4;
                rgba[i] = HINT_RGB[0];
                rgba[i + 1] = HINT_RGB[1];
                rgba[i + 2] = HINT_RGB[2];
                rgba[i + 3] = 255;
            }
        }
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
        let mut r = Renderer::new(GridSize { cols: 80, rows: 24 }, CellSize { w: 8, h: 16 });
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
        let mut r = Renderer::new(GridSize { cols: 80, rows: 24 }, CellSize { w: 8, h: 16 });
        // Not a JPEG: decode fails, frame skipped (empty output, no panic).
        let out = r.present_jpeg_bytes(&[0x00, 0x01, 0x02, 0x03]);
        assert!(out.is_empty());
    }

    #[test]
    fn bake_boxes_fills_rect_and_clamps() {
        // 4x4 RGBA, all black/opaque.
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for px in rgba.chunks_mut(4) { px[3] = 255; }
        // device grid == frame (4x4) so boxes map 1:1. A 2x2 box at (1,1) plus
        // one that overflows the right/bottom edge.
        bake_boxes(&mut rgba, 4, 4, 4, 4, &[
            HintBox { x: 1, y: 1, w: 2, h: 2 },
            HintBox { x: 3, y: 3, w: 10, h: 10 },
        ]);
        let at = |x: u32, y: u32| {
            let i = ((y * 4 + x) * 4) as usize;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        };
        assert_eq!(at(1, 1), HINT_RGB, "box interior filled");
        assert_eq!(at(2, 2), HINT_RGB, "box interior filled");
        assert_eq!(at(0, 0), [0, 0, 0], "outside box untouched");
        assert_eq!(at(3, 3), HINT_RGB, "overflowing box clamped, corner filled");
    }

    #[test]
    fn bake_boxes_scales_device_to_frame() {
        // Frame is half the device grid: a box at device (4,4,4x4) -> frame (2,2,2x2).
        let mut rgba = vec![0u8; 8 * 8 * 4];
        for px in rgba.chunks_mut(4) { px[3] = 255; }
        bake_boxes(&mut rgba, 8, 8, 16, 16, &[HintBox { x: 4, y: 4, w: 4, h: 4 }]);
        let at = |x: u32, y: u32| {
            let i = ((y * 8 + x) * 4) as usize;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        };
        assert_eq!(at(2, 2), HINT_RGB, "scaled box interior filled");
        assert_eq!(at(0, 0), [0, 0, 0], "outside scaled box untouched");
    }

    #[test]
    fn clear_hint_boxes_is_noop_when_none() {
        let mut r = Renderer::new(GridSize { cols: 80, rows: 24 }, CellSize { w: 8, h: 16 });
        assert!(r.clear_hint_boxes().is_empty(), "no boxes -> nothing to redraw");
    }

    #[test]
    fn clear_emits_delete() {
        let r = Renderer::new(GridSize { cols: 80, rows: 24 }, CellSize { w: 8, h: 16 });
        let out = String::from_utf8(r.clear()).unwrap();
        assert!(out.contains("a=d"));
    }
}
