use crate::terminal::keyboard::{parse_kitty_key, Key, KeyEvent, Mods};
use crate::terminal::mouse::parse_sgr_mouse;

// All variants are produced and consumed in app.rs; integration test compiles
// this file standalone so some appear unused in that compilation unit.
#[allow(dead_code)]
#[derive(Debug)]
pub enum RawInput {
    Key(KeyEvent),
    Mouse(crate::terminal::mouse::MouseEvent),
    Resize,
}

/// Route one decoded sequence (a full escape or a plain UTF-8 grapheme/text) to a RawInput.
pub fn classify(seq: &str) -> Option<RawInput> {
    if seq.starts_with("\x1b[<") {
        return parse_sgr_mouse(seq).map(RawInput::Mouse);
    }
    if seq.starts_with("\x1b[") && seq.ends_with('u') {
        return parse_kitty_key(seq).map(RawInput::Key);
    }
    // Plain text (the OS IME already composed Korean into final UTF-8).
    if !seq.is_empty() && !seq.starts_with('\x1b') {
        let first = seq.chars().next()?;
        return Some(RawInput::Key(KeyEvent {
            key: Key::Char(first),
            mods: Mods::none(),
            text: Some(seq.to_string()),
        }));
    }
    None
}

// Called from app.rs; appears unused to the integration test's standalone compile.
#[allow(dead_code)]
pub fn input_stream() -> impl futures::Stream<Item = RawInput> {
    use futures::stream::StreamExt;
    // Resize signals via crossterm's EventStream; key/mouse via raw byte reader on a blocking task.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RawInput>();

    // Resize watcher.
    let tx_resize = tx.clone();
    tokio::spawn(async move {
        let mut ev = crossterm::event::EventStream::new();
        while let Some(Ok(e)) = ev.next().await {
            if let crossterm::event::Event::Resize(_, _) = e {
                let _ = tx_resize.send(RawInput::Resize);
            }
        }
    });

    // Raw byte reader.
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Split the chunk into escape-delimited sequences and plain text.
                    for seq in split_sequences(&buf[..n]) {
                        if let Some(ri) = classify(&seq) {
                            if tx.send(ri).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }
    });

    tokio_stream::wrappers::UnboundedReceiverStream::new(rx)
}

/// Split a raw byte chunk into individual sequences: each ESC-introduced control
/// sequence (up to its final byte) and each run of plain UTF-8 text.
#[allow(dead_code)]
fn split_sequences(bytes: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    let mut text = String::new();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if !text.is_empty() {
                out.push(std::mem::take(&mut text));
            }
            let mut esc = String::from(c);
            // Consume until a plausible terminator (u, M, m, t, letter, or ST).
            while let Some(&n) = chars.peek() {
                esc.push(n);
                chars.next();
                if matches!(n, 'u' | 'M' | 'm' | 't' | '\\') || n.is_ascii_alphabetic() {
                    break;
                }
            }
            out.push(esc);
        } else {
            text.push(c);
        }
    }
    if !text.is_empty() {
        out.push(text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::keyboard::Key;

    #[test]
    fn classifies_kitty_key() {
        match classify("\x1b[97u").unwrap() {
            RawInput::Key(ev) => assert!(matches!(ev.key, Key::Char('a'))),
            _ => panic!("expected key"),
        }
    }

    #[test]
    fn classifies_sgr_mouse() {
        assert!(matches!(classify("\x1b[<0;5;9M"), Some(RawInput::Mouse(_))));
    }

    #[test]
    fn classifies_plain_utf8_text() {
        match classify("가").unwrap() {
            RawInput::Key(ev) => assert_eq!(ev.text.as_deref(), Some("가")),
            _ => panic!("expected key with text"),
        }
    }
}
