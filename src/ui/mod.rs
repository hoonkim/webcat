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
        self.status_bar_frame(url, loading, 0)
    }

    pub fn status_bar_frame(&self, url: &str, loading: bool, phase: u16) -> Vec<u8> {
        let prefix = if loading { "⟳ " } else { "  " };
        let width = self.grid.cols as usize;
        let mut text = format!("{prefix}{url}");
        if text.chars().count() > width {
            text = text.chars().take(width).collect();
        }
        if loading {
            return self.animated_status_bar(&text, phase);
        }
        format!(
            "\x1b[{};1H{CLEAR_LINE}{REVERSE}{text}{RESET}",
            self.bottom_row()
        )
        .into_bytes()
    }

    fn animated_status_bar(&self, text: &str, phase: u16) -> Vec<u8> {
        let width = self.grid.cols as usize;
        let mut chars: Vec<char> = text.chars().take(width).collect();
        chars.resize(width, ' ');

        let mut out = format!("\x1b[{};1H{CLEAR_LINE}", self.bottom_row());
        for (idx, ch) in chars.into_iter().enumerate() {
            let [r, g, b] = loading_bg(idx as u16, width as u16, phase);
            out.push_str(&format!("\x1b[38;2;255;255;255m\x1b[48;2;{r};{g};{b}m{ch}"));
        }
        out.push_str(RESET);
        out.into_bytes()
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

fn loading_bg(col: u16, width: u16, phase: u16) -> [u8; 3] {
    let width = width.max(1);
    let pos = (col + width - (phase % width)) % width;
    let band = width.max(12) / 3;
    let dist = pos.min(width - pos) as f32;
    let t = (1.0 - (dist / band as f32)).clamp(0.0, 1.0);
    [
        (28.0 + 44.0 * t) as u8,
        (102.0 + 98.0 * t) as u8,
        (142.0 + 85.0 * t) as u8,
    ]
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
    fn loading_status_bar_paints_gradient_background() {
        let out = String::from_utf8(ui().status_bar_frame("https://example.com", true, 3)).unwrap();
        assert!(out.contains("\x1b[24;1H"));
        assert!(out.contains("\x1b[48;2;"));
        for ch in "example.com".chars() {
            assert!(out.contains(ch));
        }
    }

    #[test]
    fn hint_overlay_places_labels() {
        let out = String::from_utf8(ui().hint_overlay(&[("a".into(), 5, 9)])).unwrap();
        assert!(out.contains("\x1b[10;6H")); // row+1, col+1
        assert!(out.contains('a'));
    }
}
