use crate::geometry::GridSize;

const REVERSE: &str = "\x1b[7m";
const RESET: &str = "\x1b[0m";
const CLEAR_LINE: &str = "\x1b[2K";
// The hint-label *background* is baked into the frame pixels by the renderer
// (kitty draws the z=-1 frame above every cell background, so a terminal cell
// background can never show over it). The letter itself is plain terminal text
// drawn on top of the frame, so it only needs a bold, dark glyph to read
// against the baked golden-yellow box.
const HINT_STYLE: &str = "\x1b[1;30m";

pub struct Ui {
    grid: GridSize,
}

impl Ui {
    pub fn new(grid: GridSize) -> Self {
        Ui { grid }
    }

    /// Update the grid after a terminal resize so the status bar / prompt are
    /// drawn at the new bottom row and truncated to the new width.
    pub fn resize(&mut self, grid: GridSize) {
        self.grid = grid;
    }

    fn bottom_row(&self) -> u16 {
        self.grid.rows
    }

    pub fn status_bar(&self, url: &str, loading: bool) -> Vec<u8> {
        let prefix = if loading { "⟳ " } else { "  " };
        let width = self.grid.cols as usize;
        let mut text = format!("{prefix}{url}");
        if text.chars().count() > width {
            text = text.chars().take(width).collect();
        }
        format!(
            "\x1b[{};1H{CLEAR_LINE}{REVERSE}{text}{RESET}",
            self.bottom_row()
        )
        .into_bytes()
    }

    pub fn url_prompt(&self, buffer: &str) -> Vec<u8> {
        let width = self.grid.cols as usize;
        let mut text = format!(": {buffer}");
        if text.chars().count() > width {
            text = text.chars().take(width).collect();
        }
        format!("\x1b[{};1H{CLEAR_LINE}{text}", self.bottom_row()).into_bytes()
    }

    pub fn hint_overlay(&self, hints: &[(String, u16, u16)]) -> Vec<u8> {
        let mut out = String::new();
        for (label, col, row) in hints {
            out.push_str(&format!(
                "\x1b[{};{}H{HINT_STYLE}{label}{RESET}",
                row + 1,
                col + 1
            ));
        }
        out.into_bytes()
    }

    /// Erase previously-drawn hint labels by overwriting their cells with
    /// (default-background) spaces — the z=-1 frame shows through again.
    pub fn clear_hints(&self, hints: &[(String, u16, u16)]) -> Vec<u8> {
        let mut out = String::new();
        for (label, col, row) in hints {
            let blanks = " ".repeat(label.chars().count());
            out.push_str(&format!("\x1b[{};{}H{blanks}", row + 1, col + 1));
        }
        out.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::GridSize;

    fn ui() -> Ui {
        Ui::new(GridSize { cols: 80, rows: 24 })
    }

    #[test]
    fn status_bar_targets_bottom_row() {
        let out = String::from_utf8(ui().status_bar("https://example.com", false)).unwrap();
        assert!(out.contains("\x1b[24;1H")); // rows=24 -> bottom row
        assert!(out.contains("example.com"));
    }

    #[test]
    fn status_bar_truncates_long_url() {
        let long = "https://example.com/".to_string() + &"a".repeat(200);
        let out = String::from_utf8(ui().status_bar(&long, false)).unwrap();
        // The visible payload must not exceed the 80-col width (plus escapes).
        let visible: String = out.chars().filter(|c| !c.is_control() && *c != '[').collect();
        assert!(visible.len() <= 80 + 11);
    }

    #[test]
    fn url_prompt_shows_buffer() {
        let out = String::from_utf8(ui().url_prompt("git")).unwrap();
        assert!(out.contains(": git"));
    }

    #[test]
    fn hint_overlay_places_labels() {
        let out = String::from_utf8(ui().hint_overlay(&[("a".into(), 5, 9)])).unwrap();
        assert!(out.contains("\x1b[10;6H")); // row+1, col+1
        assert!(out.contains('a'));
    }
}
