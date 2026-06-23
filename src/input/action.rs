use crate::terminal::keyboard::{Key, Mods};
use crate::terminal::mouse::MouseButton;

#[derive(Debug)]
pub enum Action {
    None,
    Quit,
    InsertText(String),
    Key(Key, Mods),
    ClickPixel { x: f64, y: f64, button: MouseButton },
    MoveMouse { x: f64, y: f64 },
    ScrollPixel { x: f64, y: f64, dy: f64 },
    EnterUrlMode,
    EnterInsertMode,
    ExitInsertMode,
    EnterHintMode,
    UrlInputChar(String),
    UrlBackspace,
    UrlSubmit,
    UrlCancel,
    HintKey(char),
    GoBack,
    Reload,
}
