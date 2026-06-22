#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char), Enter, Backspace, Tab, Esc,
    Up, Down, Left, Right, Home, End, PageUp, PageDown, Delete, F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods { pub shift: bool, pub ctrl: bool, pub alt: bool, pub meta: bool }

impl Mods {
    pub fn none() -> Self { Mods::default() }
    fn from_encoded(m: u32) -> Self {
        let bits = m.saturating_sub(1);
        Mods {
            shift: bits & 0b0001 != 0,
            alt:   bits & 0b0010 != 0,
            ctrl:  bits & 0b0100 != 0,
            meta:  bits & 0b1000 != 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent { pub key: Key, pub mods: Mods, pub text: Option<String> }

/// Parse one complete Kitty keyboard escape of the CSI-u form: `CSI code [;mods[:type]] [;text] u`.
pub fn parse_kitty_key(seq: &str) -> Option<KeyEvent> {
    let body = seq.strip_prefix("\x1b[")?.strip_suffix('u')?;
    // Split into up to 3 ';'-separated fields: key, mods(:type), text.
    let mut fields = body.split(';');

    let key_field = fields.next()?;
    let key_code: u32 = key_field.parse().ok()?;

    let mods_field = fields.next();
    let mods = match mods_field {
        Some(f) => {
            // f may be "mods" or "mods:event-type"; we ignore event-type here.
            let m = f.split(':').next()?.parse::<u32>().ok()?;
            Mods::from_encoded(m)
        }
        None => Mods::none(),
    };

    let text = fields.next().and_then(|f| {
        let s: String = f
            .split(':')
            .filter_map(|cp| cp.parse::<u32>().ok())
            .filter_map(char::from_u32)
            .collect();
        if s.is_empty() { None } else { Some(s) }
    });

    let key = match key_code {
        13 => Key::Enter,
        9 => Key::Tab,
        127 => Key::Backspace,
        27 => Key::Esc,
        c => Key::Char(char::from_u32(c)?),
    };

    Some(KeyEvent { key, mods, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_letter() {
        let ev = parse_kitty_key("\x1b[97u").unwrap(); // 'a'
        assert!(matches!(ev.key, Key::Char('a')));
        assert!(!ev.mods.ctrl && !ev.mods.alt);
    }

    #[test]
    fn ctrl_modifier() {
        // 'c' with ctrl: modifiers = 1 + 4 = 5
        let ev = parse_kitty_key("\x1b[99;5u").unwrap();
        assert!(matches!(ev.key, Key::Char('c')));
        assert!(ev.mods.ctrl);
        assert!(!ev.mods.shift);
    }

    #[test]
    fn release_event_is_parsed() {
        // 'a', no mods (1), event-type 3 (release)
        let ev = parse_kitty_key("\x1b[97;1:3u").unwrap();
        assert!(matches!(ev.key, Key::Char('a')));
    }

    #[test]
    fn enter_and_backspace_special_codes() {
        assert!(matches!(parse_kitty_key("\x1b[13u").unwrap().key, Key::Enter));
        assert!(matches!(parse_kitty_key("\x1b[127u").unwrap().key, Key::Backspace));
        assert!(matches!(parse_kitty_key("\x1b[27u").unwrap().key, Key::Esc));
    }

    #[test]
    fn text_field_is_captured() {
        // key code 97, text codepoint 97 ('a') after second ';'
        let ev = parse_kitty_key("\x1b[97;1;97u").unwrap();
        assert_eq!(ev.text.as_deref(), Some("a"));
    }

    #[test]
    fn rejects_incomplete() {
        assert!(parse_kitty_key("\x1b[97").is_none());
        assert!(parse_kitty_key("not an escape").is_none());
    }
}
