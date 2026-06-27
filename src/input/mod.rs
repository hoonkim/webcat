pub mod action;
pub mod hints;

use crate::geometry::{CellSize, cell_to_pixel};
use crate::terminal::keyboard::{Key, KeyEvent};
use crate::terminal::mouse::{MouseEvent, MouseKind, MouseButton};
use action::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode { Normal, Insert, UrlInput, Hint }

pub struct InputMapper {
    pub mode: Mode,
    cell: CellSize,
}

impl InputMapper {
    pub fn new(cell: CellSize) -> Self {
        InputMapper { mode: Mode::Normal, cell }
    }

    pub fn on_key(&mut self, ev: KeyEvent) -> Action {
        match self.mode {
            Mode::Normal => self.on_key_normal(ev),
            Mode::Insert => self.on_key_insert(ev),
            Mode::UrlInput => self.on_key_url(ev),
            Mode::Hint => self.on_key_hint(ev),
        }
    }

    fn on_key_normal(&mut self, ev: KeyEvent) -> Action {
        let line = 3.0 * self.cell.h as f64;
        match ev.key {
            Key::Char('i') => { self.mode = Mode::Insert; Action::EnterInsertMode }
            Key::Char(':') => { self.mode = Mode::UrlInput; Action::EnterUrlMode }
            Key::Char('f') => { self.mode = Mode::Hint; Action::EnterHintMode }
            Key::Char('q') => Action::Quit,
            Key::Char('r') => Action::Reload,
            Key::Char('H') | Key::Backspace => Action::GoBack,
            Key::Char('j') => Action::ScrollPixel { x: 0.0, y: 0.0, dy: line },
            Key::Char('k') => Action::ScrollPixel { x: 0.0, y: 0.0, dy: -line },
            Key::Esc | Key::Up | Key::Down | Key::Left | Key::Right => Action::Key(ev.key, ev.mods),
            // Normal mode is command-only: any other key (incl. printable text) is swallowed.
            _ => Action::None,
        }
    }

    fn on_key_insert(&mut self, ev: KeyEvent) -> Action {
        match ev.key {
            Key::Esc => { self.mode = Mode::Normal; Action::ExitInsertMode }
            Key::Enter | Key::Backspace | Key::Tab | Key::Delete
            | Key::Up | Key::Down | Key::Left | Key::Right => Action::Key(ev.key, ev.mods),
            _ => {
                // Printable text (incl. Korean) goes to the focused page element.
                if let Some(t) = ev.text { Action::InsertText(t) } else { Action::None }
            }
        }
    }

    fn on_key_url(&mut self, ev: KeyEvent) -> Action {
        match ev.key {
            Key::Esc => { self.mode = Mode::Normal; Action::UrlCancel }
            Key::Enter => { self.mode = Mode::Normal; Action::UrlSubmit }
            Key::Backspace => Action::UrlBackspace,
            _ => {
                if let Some(t) = ev.text { Action::UrlInputChar(t) } else { Action::None }
            }
        }
    }

    fn on_key_hint(&mut self, ev: KeyEvent) -> Action {
        match ev.key {
            Key::Esc => { self.mode = Mode::Normal; Action::UrlCancel }
            Key::Char(c) if c.is_ascii_alphabetic() => Action::HintKey(c.to_ascii_lowercase()),
            _ => Action::None,
        }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> Action {
        let wheel = self.cell.h as f64;
        let (x, y) = cell_to_pixel(ev.col, ev.row, self.cell);
        match ev.kind {
            MouseKind::Down(MouseButton::Left) =>
                Action::ClickPixel { x, y, button: MouseButton::Left },
            MouseKind::Down(b) => Action::ClickPixel { x, y, button: b },
            MouseKind::WheelDown => Action::ScrollPixel { x, y, dy: wheel },
            MouseKind::WheelUp => Action::ScrollPixel { x, y, dy: -wheel },
            MouseKind::Move => Action::MoveMouse { x, y },
            MouseKind::Up(_) => Action::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::keyboard::{Key, Mods, KeyEvent};
    use crate::terminal::mouse::{MouseEvent, MouseKind, MouseButton};
    use crate::geometry::CellSize;
    use crate::input::action::Action;

    fn mapper() -> InputMapper { InputMapper::new(CellSize { w: 8, h: 16 }) }
    fn ev(key: Key, text: Option<&str>) -> KeyEvent {
        KeyEvent { key, mods: Mods::none(), text: text.map(|s| s.to_string()) }
    }

    #[test]
    fn colon_enters_url_mode() {
        let mut m = mapper();
        let a = m.on_key(ev(Key::Char(':'), Some(":")));
        assert!(matches!(a, Action::EnterUrlMode));
        assert!(matches!(m.mode, Mode::UrlInput));
    }

    #[test]
    fn korean_text_in_url_mode_is_captured() {
        let mut m = mapper();
        m.on_key(ev(Key::Char(':'), Some(":")));
        let a = m.on_key(ev(Key::Char('가'), Some("안녕")));
        match a {
            Action::UrlInputChar(s) => assert_eq!(s, "안녕"),
            other => panic!("expected UrlInputChar, got {other:?}"),
        }
    }

    #[test]
    fn i_enters_insert_mode_and_korean_inserts_text() {
        let mut m = mapper();
        assert!(matches!(m.on_key(ev(Key::Char('i'), Some("i"))), Action::EnterInsertMode));
        assert!(matches!(m.mode, Mode::Insert));
        let a = m.on_key(ev(Key::Char('하'), Some("하세요")));
        match a {
            Action::InsertText(s) => assert_eq!(s, "하세요"),
            other => panic!("expected InsertText, got {other:?}"),
        }
    }

    #[test]
    fn normal_mode_does_not_leak_text_to_page() {
        let mut m = mapper();
        // A printable letter that is not a command must be swallowed in Normal mode.
        assert!(matches!(m.on_key(ev(Key::Char('z'), Some("z"))), Action::None));
    }

    #[test]
    fn esc_in_normal_mode_is_sent_to_page() {
        let mut m = mapper();
        assert!(matches!(m.on_key(ev(Key::Esc, None)), Action::Key(Key::Esc, _)));
        assert!(matches!(m.mode, Mode::Normal));
    }

    #[test]
    fn esc_exits_insert_mode() {
        let mut m = mapper();
        m.on_key(ev(Key::Char('i'), Some("i")));
        assert!(matches!(m.on_key(ev(Key::Esc, None)), Action::ExitInsertMode));
        assert!(matches!(m.mode, Mode::Normal));
    }

    #[test]
    fn left_click_maps_to_pixel_center() {
        let mut m = mapper();
        let a = m.on_mouse(MouseEvent { kind: MouseKind::Down(MouseButton::Left), col: 2, row: 1 });
        match a {
            Action::ClickPixel { x, y, .. } => assert_eq!((x, y), (20.0, 24.0)),
            other => panic!("expected ClickPixel, got {other:?}"),
        }
    }

    #[test]
    fn wheel_scrolls() {
        let mut m = mapper();
        let a = m.on_mouse(MouseEvent { kind: MouseKind::WheelDown, col: 0, row: 0 });
        assert!(matches!(a, Action::ScrollPixel { dy, .. } if dy == 16.0));
    }

    #[test]
    fn q_quits_in_normal_mode() {
        let mut m = mapper();
        assert!(matches!(m.on_key(ev(Key::Char('q'), Some("q"))), Action::Quit));
    }

    #[test]
    fn backspace_goes_back_in_normal_mode() {
        let mut m = mapper();
        assert!(matches!(m.on_key(ev(Key::Backspace, None)), Action::GoBack));
        assert!(matches!(m.mode, Mode::Normal));
    }
}
