#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton { Left, Middle, Right }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind { Down(MouseButton), Up(MouseButton), Move, WheelUp, WheelDown }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent { pub kind: MouseKind, pub col: u16, pub row: u16 }

pub fn parse_sgr_mouse(seq: &str) -> Option<MouseEvent> {
    let body = seq.strip_prefix("\x1b[<")?;
    let (is_release, body) = if let Some(b) = body.strip_suffix('M') {
        (false, b)
    } else if let Some(b) = body.strip_suffix('m') {
        (true, b)
    } else {
        return None;
    };

    let mut parts = body.split(';');
    let cb: u32 = parts.next()?.parse().ok()?;
    let cx: u16 = parts.next()?.parse().ok()?;
    let cy: u16 = parts.next()?.parse().ok()?;
    if parts.next().is_some() { return None; }

    let col = cx.checked_sub(1)?;
    let row = cy.checked_sub(1)?;

    let kind = if cb & 64 != 0 {
        // Wheel buttons in SGR mouse mode are encoded in the low two bits:
        // 0/1 are vertical up/down, 2/3 are horizontal left/right. Trackpads can
        // emit horizontal wheel codes during a vertical gesture; do not treat
        // them as vertical scroll or the page jitters up and down.
        match cb & 0b11 {
            0 => MouseKind::WheelUp,
            1 => MouseKind::WheelDown,
            _ => return None,
        }
    } else if cb & 32 != 0 {
        MouseKind::Move
    } else {
        let button = match cb & 0b11 {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            _ => MouseButton::Right,
        };
        if is_release { MouseKind::Up(button) } else { MouseKind::Down(button) }
    };

    Some(MouseEvent { kind, col, row })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_press_and_release() {
        let d = parse_sgr_mouse("\x1b[<0;5;9M").unwrap();
        assert!(matches!(d.kind, MouseKind::Down(MouseButton::Left)));
        assert_eq!((d.col, d.row), (4, 8)); // converted to 0-based
        let u = parse_sgr_mouse("\x1b[<0;5;9m").unwrap();
        assert!(matches!(u.kind, MouseKind::Up(MouseButton::Left)));
    }

    #[test]
    fn wheel_up_and_down() {
        assert!(matches!(parse_sgr_mouse("\x1b[<64;1;1M").unwrap().kind, MouseKind::WheelUp));
        assert!(matches!(parse_sgr_mouse("\x1b[<65;1;1M").unwrap().kind, MouseKind::WheelDown));
    }

    #[test]
    fn horizontal_wheel_is_ignored() {
        assert!(parse_sgr_mouse("\x1b[<66;1;1M").is_none());
        assert!(parse_sgr_mouse("\x1b[<67;1;1M").is_none());
    }

    #[test]
    fn motion_is_move() {
        // 32 (motion) + 0 (left held) = 32
        assert!(matches!(parse_sgr_mouse("\x1b[<32;2;2M").unwrap().kind, MouseKind::Move));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_sgr_mouse("\x1b[<0;5M").is_none());
        assert!(parse_sgr_mouse("nope").is_none());
    }
}
