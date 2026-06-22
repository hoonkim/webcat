#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize { pub w: u16, pub h: u16 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize { pub cols: u16, pub rows: u16 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport { pub width_px: u32, pub height_px: u32 }

pub fn page_viewport(grid: GridSize, cell: CellSize, status_rows: u16) -> Viewport {
    let page_rows = grid.rows.saturating_sub(status_rows);
    Viewport {
        width_px: grid.cols as u32 * cell.w as u32,
        height_px: page_rows as u32 * cell.h as u32,
    }
}

pub fn cell_to_pixel(col: u16, row: u16, cell: CellSize) -> (f64, f64) {
    let x = col as f64 * cell.w as f64 + cell.w as f64 / 2.0;
    let y = row as f64 * cell.h as f64 + cell.h as f64 / 2.0;
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_excludes_status_rows() {
        let vp = page_viewport(GridSize { cols: 100, rows: 30 },
                               CellSize { w: 8, h: 16 }, 1);
        assert_eq!(vp.width_px, 800);
        assert_eq!(vp.height_px, 29 * 16);
    }

    #[test]
    fn cell_center_maps_to_pixel_center() {
        // col 0,row 0 center -> (4, 8) for an 8x16 cell.
        let (x, y) = cell_to_pixel(0, 0, CellSize { w: 8, h: 16 });
        assert_eq!((x, y), (4.0, 8.0));
        let (x, y) = cell_to_pixel(2, 1, CellSize { w: 8, h: 16 });
        assert_eq!((x, y), (20.0, 24.0));
    }
}
