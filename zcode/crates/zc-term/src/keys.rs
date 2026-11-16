//! 打鍵を端末に送るバイト列に直す（xterm と同じ形）。

use zc_core::{Key, KeyEvent};

/// `ev` を `out` の後ろに足す。送るものが無いキー（音量など）は何も足さない。
/// `app_cursor` は DECCKM（矢印・Home・End をアプリケーションモードの `ESC O x` で送る）。
pub(crate) fn encode_key(ev: &KeyEvent, app_cursor: bool, out: &mut Vec<u8>) {
    let m = ev.mods;
    // xterm のモディファイアの番号（1 + Shift 1 + Alt 2 + Ctrl 4）。1 なら付けない。
    let code = 1 + m.shift() as u8 + 2 * m.alt() as u8 + 4 * m.ctrl() as u8;
    let alt_prefix = |out: &mut Vec<u8>| {
        if m.alt() {
            out.push(0x1b);
        }
    };
    match ev.key {
        Key::Char(c) => {
            if m.ctrl() {
                alt_prefix(out);
                match ctrl_code(c) {
                    Some(b) => out.push(b),
                    None => push_char(out, ev.text.unwrap_or(c)),
                }
                return;
            }
            let ch = ev.text.unwrap_or(if m.shift() { c.to_ascii_uppercase() } else { c });
            alt_prefix(out);
            push_char(out, ch);
        }
        Key::Enter => {
            alt_prefix(out);
            out.push(b'\r');
        }
        Key::Backspace => {
            alt_prefix(out);
            // Ctrl+Backspace は ^H（xterm と同じ）
            out.push(if m.ctrl() { 0x08 } else { 0x7f });
        }
        Key::Tab => {
            if m.shift() {
                out.extend_from_slice(b"\x1b[Z");
            } else {
                alt_prefix(out);
                out.push(b'\t');
            }
        }
        Key::Esc => {
            alt_prefix(out);
            out.push(0x1b);
        }
        Key::Up => cursor_key(out, b'A', code, app_cursor),
        Key::Down => cursor_key(out, b'B', code, app_cursor),
        Key::Right => cursor_key(out, b'C', code, app_cursor),
        Key::Left => cursor_key(out, b'D', code, app_cursor),
        Key::Home => cursor_key(out, b'H', code, app_cursor),
        Key::End => cursor_key(out, b'F', code, app_cursor),
        Key::Insert => tilde_key(out, 2, code),
        Key::Delete => tilde_key(out, 3, code),
        Key::PageUp => tilde_key(out, 5, code),
        Key::PageDown => tilde_key(out, 6, code),
        Key::F(n @ 1..=4) => {
            let c = b"PQRS"[n as usize - 1];
            if code == 1 {
                out.extend_from_slice(&[0x1b, b'O', c]);
            } else {
                out.extend_from_slice(format!("\x1b[1;{code}").as_bytes());
                out.push(c);
            }
        }
        Key::F(n @ 5..=12) => {
            const NUM: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            tilde_key(out, NUM[n as usize - 5], code);
        }
        Key::F(_) | Key::Media(_) | Key::Unknown(_) => {}
    }
}

fn push_char(out: &mut Vec<u8>, c: char) {
    let mut b = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
}

/// Ctrl+文字の C0 制御文字（xterm の割り当て）。
fn ctrl_code(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0x00,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' | '~' | '`' => 0x1e,
        '_' | '7' | '/' | '-' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// 矢印・Home・End。モディファイアがあれば `CSI 1 ; m x`、無ければ
/// 通常モードは `CSI x`、アプリケーションモードは `SS3 x`。
fn cursor_key(out: &mut Vec<u8>, c: u8, code: u8, app: bool) {
    if code != 1 {
        out.extend_from_slice(format!("\x1b[1;{code}").as_bytes());
        out.push(c);
    } else if app {
        out.extend_from_slice(&[0x1b, b'O', c]);
    } else {
        out.extend_from_slice(&[0x1b, b'[', c]);
    }
}

/// `CSI n ~`（モディファイアがあれば `CSI n ; m ~`）。
fn tilde_key(out: &mut Vec<u8>, n: u8, code: u8) {
    let s = if code == 1 { format!("\x1b[{n}~") } else { format!("\x1b[{n};{code}~") };
    out.extend_from_slice(s.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::Mods;

    fn enc(ev: KeyEvent, app: bool) -> Vec<u8> {
        let mut v = Vec::new();
        encode_key(&ev, app, &mut v);
        v
    }
    fn k(key: Key, mods: Mods) -> Vec<u8> {
        enc(KeyEvent::new(key, mods), false)
    }

    #[test]
    fn text_and_utf8() {
        assert_eq!(enc(KeyEvent::char('a'), false), b"a");
        assert_eq!(enc(KeyEvent::char('A'), false), b"A");
        assert_eq!(enc(KeyEvent::char('あ'), false), "あ".as_bytes());
        assert_eq!(enc(KeyEvent::char(' '), false), b" ");
        // text が無ければキーの字（Shift なら英大文字）
        assert_eq!(k(Key::Char('q'), Mods::SHIFT), b"Q");
    }

    #[test]
    fn ctrl_and_alt() {
        assert_eq!(k(Key::Char('c'), Mods::CTRL), b"\x03");
        assert_eq!(k(Key::Char('a'), Mods::CTRL), b"\x01");
        assert_eq!(k(Key::Char('z'), Mods::CTRL | Mods::SHIFT), b"\x1a");
        assert_eq!(k(Key::Char('@'), Mods::CTRL), b"\x00");
        assert_eq!(k(Key::Char(' '), Mods::CTRL), b"\x00");
        assert_eq!(k(Key::Char('['), Mods::CTRL), b"\x1b");
        assert_eq!(k(Key::Char('\\'), Mods::CTRL), b"\x1c");
        assert_eq!(k(Key::Char(']'), Mods::CTRL), b"\x1d");
        assert_eq!(k(Key::Char('^'), Mods::CTRL), b"\x1e");
        assert_eq!(k(Key::Char('_'), Mods::CTRL), b"\x1f");
        assert_eq!(k(Key::Char('?'), Mods::CTRL), b"\x7f");
        assert_eq!(k(Key::Char('b'), Mods::ALT), b"\x1bb");
        assert_eq!(k(Key::Char('b'), Mods::ALT | Mods::SHIFT), b"\x1bB");
        assert_eq!(k(Key::Char('x'), Mods::ALT | Mods::CTRL), b"\x1b\x18");
        // Sym / Fn は無視する
        assert_eq!(k(Key::Char('c'), Mods::CTRL | Mods::SYM | Mods::FN), b"\x03");
    }

    #[test]
    fn editing_keys() {
        assert_eq!(k(Key::Enter, Mods::NONE), b"\r");
        assert_eq!(k(Key::Enter, Mods::ALT), b"\x1b\r");
        assert_eq!(k(Key::Backspace, Mods::NONE), b"\x7f");
        assert_eq!(k(Key::Backspace, Mods::CTRL), b"\x08");
        assert_eq!(k(Key::Backspace, Mods::ALT), b"\x1b\x7f");
        assert_eq!(k(Key::Tab, Mods::NONE), b"\t");
        assert_eq!(k(Key::Tab, Mods::SHIFT), b"\x1b[Z");
        assert_eq!(k(Key::Esc, Mods::NONE), b"\x1b");
    }

    #[test]
    fn cursor_keys_modes() {
        assert_eq!(k(Key::Up, Mods::NONE), b"\x1b[A");
        assert_eq!(k(Key::Down, Mods::NONE), b"\x1b[B");
        assert_eq!(k(Key::Right, Mods::NONE), b"\x1b[C");
        assert_eq!(k(Key::Left, Mods::NONE), b"\x1b[D");
        assert_eq!(k(Key::Home, Mods::NONE), b"\x1b[H");
        assert_eq!(k(Key::End, Mods::NONE), b"\x1b[F");
        let app = |key| enc(KeyEvent::new(key, Mods::NONE), true);
        assert_eq!(app(Key::Up), b"\x1bOA");
        assert_eq!(app(Key::Left), b"\x1bOD");
        assert_eq!(app(Key::Home), b"\x1bOH");
        assert_eq!(app(Key::End), b"\x1bOF");
        // モディファイアつきはモードに関係なく CSI 1;m
        assert_eq!(k(Key::Right, Mods::CTRL), b"\x1b[1;5C");
        assert_eq!(enc(KeyEvent::new(Key::Up, Mods::SHIFT), true), b"\x1b[1;2A");
        assert_eq!(k(Key::Left, Mods::ALT), b"\x1b[1;3D");
        assert_eq!(k(Key::Home, Mods::CTRL | Mods::SHIFT), b"\x1b[1;6H");
    }

    #[test]
    fn tilde_and_function_keys() {
        assert_eq!(k(Key::Insert, Mods::NONE), b"\x1b[2~");
        assert_eq!(k(Key::Delete, Mods::NONE), b"\x1b[3~");
        assert_eq!(k(Key::PageUp, Mods::NONE), b"\x1b[5~");
        assert_eq!(k(Key::PageDown, Mods::NONE), b"\x1b[6~");
        assert_eq!(k(Key::Delete, Mods::CTRL), b"\x1b[3;5~");
        assert_eq!(k(Key::F(1), Mods::NONE), b"\x1bOP");
        assert_eq!(k(Key::F(4), Mods::NONE), b"\x1bOS");
        assert_eq!(k(Key::F(1), Mods::SHIFT), b"\x1b[1;2P");
        assert_eq!(k(Key::F(5), Mods::NONE), b"\x1b[15~");
        assert_eq!(k(Key::F(6), Mods::NONE), b"\x1b[17~");
        assert_eq!(k(Key::F(10), Mods::NONE), b"\x1b[21~");
        assert_eq!(k(Key::F(11), Mods::NONE), b"\x1b[23~");
        assert_eq!(k(Key::F(12), Mods::NONE), b"\x1b[24~");
        assert_eq!(k(Key::F(5), Mods::CTRL), b"\x1b[15;5~");
        assert_eq!(k(Key::F(13), Mods::NONE), b"");
        assert_eq!(k(Key::Media(zc_core::MediaKey::Mute), Mods::NONE), b"");
    }
}
