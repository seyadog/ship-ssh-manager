//! Traduce eventos de teclado de crossterm a los bytes que espera un terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn encode(key: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let shift = m.contains(KeyModifiers::SHIFT);
    // Parámetro de modificadores xterm: 1 + shift(1) + alt(2) + ctrl(4)
    let md = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;

    let csi = |final_byte: char| -> Vec<u8> {
        if md > 1 {
            format!("\x1b[1;{md}{final_byte}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if md > 1 { format!("\x1b[{n};{md}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };

    let bytes = match key.code {
        KeyCode::Char(c) => {
            let mut out = Vec::new();
            if ctrl {
                let b = match c {
                    'a'..='z' => c as u8 - b'a' + 1,
                    'A'..='Z' => c as u8 - b'A' + 1,
                    ' ' | '@' | '2' => 0,
                    '[' | '3' => 27,
                    '\\' | '4' => 28,
                    ']' | '5' => 29,
                    '^' | '6' => 30,
                    '_' | '7' | '/' => 31,
                    '8' | '?' => 127,
                    _ => return None,
                };
                out.push(b);
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            if alt {
                out.insert(0, 0x1b);
            }
            out
        }
        KeyCode::Enter => {
            if alt {
                b"\x1b\r".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            if alt {
                b"\x1b\x7f".to_vec()
            } else {
                b"\x7f".to_vec()
            }
        }
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Up => csi('A'),
        KeyCode::Down => csi('B'),
        KeyCode::Right => csi('C'),
        KeyCode::Left => csi('D'),
        KeyCode::Home => csi('H'),
        KeyCode::End => csi('F'),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::F(n) => match n {
            1..=4 => {
                let c = (b'P' + n - 1) as char;
                if md > 1 { format!("\x1b[1;{md}{c}").into_bytes() } else { format!("\x1bO{c}").into_bytes() }
            }
            5 => tilde(15),
            6 => tilde(17),
            7 => tilde(18),
            8 => tilde(19),
            9 => tilde(20),
            10 => tilde(21),
            11 => tilde(23),
            12 => tilde(24),
            _ => return None,
        },
        _ => return None,
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> Option<Vec<u8>> {
        encode(KeyEvent::new(code, m), false)
    }

    #[test]
    fn basics() {
        assert_eq!(k(KeyCode::Char('a'), KeyModifiers::NONE).unwrap(), b"a");
        assert_eq!(k(KeyCode::Char('c'), KeyModifiers::CONTROL).unwrap(), [3]);
        assert_eq!(k(KeyCode::Char('b'), KeyModifiers::ALT).unwrap(), b"\x1bb");
        assert_eq!(k(KeyCode::Up, KeyModifiers::NONE).unwrap(), b"\x1b[A");
        assert_eq!(k(KeyCode::Left, KeyModifiers::CONTROL).unwrap(), b"\x1b[1;5D");
        assert_eq!(k(KeyCode::Char('ñ'), KeyModifiers::NONE).unwrap(), "ñ".as_bytes());
    }

    #[test]
    fn application_cursor_mode() {
        let e = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(encode(e, true).unwrap(), b"\x1bOA");
    }
}
