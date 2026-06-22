use crate::geometry::GridSize;

const REVERSE: &str = "\x1b[7m";
const RESET: &str = "\x1b[0m";
const CLEAR_LINE: &str = "\x1b[2K";

pub struct Ui {
    grid: GridSize,
}

impl Ui {
    pub fn new(grid: GridSize) -> Self {
        Ui { grid }
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
                "\x1b[{};{}H{REVERSE}{label}{RESET}",
                row + 1,
                col + 1
            ));
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
