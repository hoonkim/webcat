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
        // kitty's disambiguate mode still sends Enter/Backspace/Tab as their
        // legacy control bytes (not CSI-u), so map those to the special keys;
        // otherwise they'd be treated as typed characters.
        let key = match seq {
            "\r" | "\n" => Some(Key::Enter),
            "\x7f" | "\x08" => Some(Key::Backspace),
            "\t" => Some(Key::Tab),
            _ => None,
        };
        if let Some(key) = key {
            return Some(RawInput::Key(KeyEvent { key, mods: Mods::none(), text: None }));
        }
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
pub fn input_stream() -> tokio::sync::mpsc::UnboundedReceiver<RawInput> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RawInput>();

    // Resize watcher via SIGWINCH. We must NOT use crossterm's EventStream here:
    // it reads the SAME stdin as the raw byte reader below, so the two would
    // race for bytes and crossterm would silently consume (and discard) key and
    // mouse sequences — causing dropped keystrokes and clicks. SIGWINCH does not
    // touch stdin.
    let tx_resize = tx.clone();
    std::thread::spawn(move || {
        use signal_hook::consts::SIGWINCH;
        use signal_hook::iterator::Signals;
        if let Ok(mut signals) = Signals::new([SIGWINCH]) {
            for _ in signals.forever() {
                if tx_resize.send(RawInput::Resize).is_err() {
                    break;
                }
            }
        }
    });

    // Raw byte reader. Bytes are accumulated in a persistent buffer and only
    // COMPLETE tokens are drained each read; an escape sequence (or multibyte
    // UTF-8 char) split across two reads stays buffered until the rest arrives.
    // (A previous stateless splitter dropped sequences cut at a read boundary,
    // which lost mouse clicks during high-volume motion reporting.)
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut chunk = [0u8; 4096];
        let mut buf: Vec<u8> = Vec::with_capacity(8192);
        loop {
            match stdin.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    for seq in drain_tokens(&mut buf) {
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

    rx
}

/// Drain all COMPLETE tokens from `buf`, leaving any trailing incomplete escape
/// sequence or partial UTF-8 character in place for the next read. A token is
/// either a complete CSI sequence (`ESC [` … final byte `0x40..=0x7E`), a short
/// `ESC <x>` escape, or a run of plain UTF-8 text.
fn drain_tokens(buf: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    let n = buf.len();
    let mut i = 0;
    while i < n {
        if buf[i] == 0x1b {
            if i + 1 >= n {
                break; // lone ESC at end — wait for more
            }
            if buf[i + 1] == b'[' {
                // CSI: scan to the final byte in 0x40..=0x7E.
                let mut j = i + 2;
                while j < n && !(0x40..=0x7e).contains(&buf[j]) {
                    j += 1;
                }
                if j < n {
                    out.push(String::from_utf8_lossy(&buf[i..=j]).into_owned());
                    i = j + 1;
                } else {
                    break; // incomplete CSI — keep for next read
                }
            } else {
                // Other ESC-prefixed escape: emit ESC + the following byte.
                out.push(String::from_utf8_lossy(&buf[i..i + 2]).into_owned());
                i += 2;
            }
        } else {
            // Plain text run up to the next ESC.
            let mut j = i;
            while j < n && buf[j] != 0x1b {
                j += 1;
            }
            match std::str::from_utf8(&buf[i..j]) {
                Ok(s) => {
                    out.push(s.to_string());
                    i = j;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        out.push(String::from_utf8_lossy(&buf[i..i + valid]).into_owned());
                    }
                    if j == n {
                        // Trailing bytes are an incomplete UTF-8 char — keep them.
                        i += valid;
                        break;
                    } else {
                        // Invalid bytes mid-buffer (not a split char): skip past them.
                        i = j;
                    }
                }
            }
        }
    }
    buf.drain(..i);
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
    fn classifies_legacy_control_keys() {
        use crate::terminal::keyboard::Key;
        assert!(matches!(classify("\r"), Some(RawInput::Key(KeyEvent { key: Key::Enter, .. }))));
        assert!(matches!(classify("\x7f"), Some(RawInput::Key(KeyEvent { key: Key::Backspace, .. }))));
        assert!(matches!(classify("\t"), Some(RawInput::Key(KeyEvent { key: Key::Tab, .. }))));
    }

    #[test]
    fn classifies_plain_utf8_text() {
        match classify("가").unwrap() {
            RawInput::Key(ev) => assert_eq!(ev.text.as_deref(), Some("가")),
            _ => panic!("expected key with text"),
        }
    }

    #[test]
    fn drain_extracts_complete_sequences() {
        let mut buf = b"\x1b[<0;5;9M\x1b[105u".to_vec();
        let toks = drain_tokens(&mut buf);
        assert_eq!(toks, vec!["\x1b[<0;5;9M".to_string(), "\x1b[105u".to_string()]);
        assert!(buf.is_empty());
    }

    #[test]
    fn drain_keeps_sequence_split_across_reads() {
        // First read ends mid-SGR-mouse sequence (no final byte yet).
        let mut buf = b"\x1b[<0;20;1".to_vec();
        let toks = drain_tokens(&mut buf);
        assert!(toks.is_empty(), "incomplete CSI must not be emitted");
        assert_eq!(buf, b"\x1b[<0;20;1", "incomplete CSI is kept buffered");
        // Second read brings the rest; now the full sequence is emitted.
        buf.extend_from_slice(b"2M");
        let toks = drain_tokens(&mut buf);
        assert_eq!(toks, vec!["\x1b[<0;20;12M".to_string()]);
        assert!(buf.is_empty());
        // And it classifies as a mouse Down (not lost, not a stray key).
        assert!(matches!(classify(&toks[0]), Some(RawInput::Mouse(_))));
    }

    #[test]
    fn drain_keeps_partial_utf8_across_reads() {
        // '가' is 3 bytes (EA B0 80); split after the first byte.
        let full = "가".as_bytes();
        let mut buf = full[..1].to_vec();
        let toks = drain_tokens(&mut buf);
        assert!(toks.is_empty(), "partial UTF-8 must not be emitted");
        buf.extend_from_slice(&full[1..]);
        let toks = drain_tokens(&mut buf);
        assert_eq!(toks, vec!["가".to_string()]);
        assert!(buf.is_empty());
    }

    #[test]
    fn drain_handles_text_then_escape() {
        let mut buf = b"ab\x1b[97u".to_vec();
        let toks = drain_tokens(&mut buf);
        assert_eq!(toks, vec!["ab".to_string(), "\x1b[97u".to_string()]);
    }
}
