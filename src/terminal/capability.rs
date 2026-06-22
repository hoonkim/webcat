use std::io::{Read, Write};
use std::time::{Duration, Instant};
use crate::geometry::CellSize;

/// Parse a `CSI 6 ; height ; width t` cell-size report.
pub fn parse_cell_size_report(seq: &str) -> Option<CellSize> {
    let body = seq.strip_prefix("\x1b[")?.strip_suffix('t')?;
    let mut parts = body.split(';');
    if parts.next()? != "6" {
        return None;
    }
    let h: u16 = parts.next()?.parse().ok()?;
    let w: u16 = parts.next()?.parse().ok()?;
    Some(CellSize { w, h })
}

/// Read available bytes from stdin until `terminator` byte or `timeout`.
///
/// IMPLEMENTER NOTE: raw mode must set `VMIN=0, VTIME=1` (crossterm raw mode
/// does this by default on Unix) so the per-byte `read` returns quickly rather
/// than blocking forever. If responses are flaky in manual testing, set the
/// terminal to non-blocking explicitly; this is acceptable to adjust during the
/// integration smoke.
fn read_reply(terminator: u8, timeout: Duration) -> String {
    let mut stdin = std::io::stdin();
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match stdin.read(&mut byte) {
            Ok(1) => {
                buf.push(byte[0]);
                if byte[0] == terminator {
                    break;
                }
            }
            _ => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

pub fn query_cell_size(timeout: Duration) -> CellSize {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[16t");
    let _ = out.flush();
    let reply = read_reply(b't', timeout);
    parse_cell_size_report(&reply).unwrap_or_else(|| {
        tracing::warn!("cell-size query failed; falling back to 8x16");
        CellSize { w: 8, h: 16 }
    })
}

pub fn detect_kitty_graphics(timeout: Duration) -> bool {
    let mut out = std::io::stdout();
    // Query action with a 1px base64 RGB pixel; Kitty replies with an APC ...;OK ST.
    let _ = write!(out, "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
    let _ = out.flush();
    let reply = read_reply(b'\\', timeout);
    reply.contains("\x1b_G") && reply.contains("OK")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cell_size_report() {
        let cs = parse_cell_size_report("\x1b[6;16;8t").unwrap();
        assert_eq!(cs.w, 8);
        assert_eq!(cs.h, 16);
    }

    #[test]
    fn rejects_wrong_report() {
        assert!(parse_cell_size_report("\x1b[4;16;8t").is_none()); // wrong leading code
        assert!(parse_cell_size_report("garbage").is_none());
    }
}
