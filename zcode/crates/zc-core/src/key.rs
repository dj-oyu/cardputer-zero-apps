//! キー入力。`zc-input` が evdev のキーコードをこの形に直し、SKK・エディタ・
//! ターミナルはこれだけを受け取る（design.md §6.2）。

use core::fmt;

/// 論理キー。キーボードのレイヤー（通常 / Sym / Fn）はドライバが解決して別の
/// キーコードで届くので、ここには「何のキーか」だけが入る。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// 文字のキー。Shift を押していない状態の文字（英字は小文字、Sym レイヤーの
    /// 記号はその記号）。空白は `Char(' ')`。
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// F1〜F12（Fn レイヤー）。
    F(u8),
    /// 音量・輝度など。
    Media(MediaKey),
    /// 対応表に無いキーコード（evdev のコード）。
    Unknown(u16),
}

/// 音量などのキー（Fn レイヤー）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKey {
    Mute,
    VolumeDown,
    VolumeUp,
    BrightnessDown,
    BrightnessUp,
    PlayPause,
    PrevSong,
    NextSong,
    Help,
    SysRq,
}

const NAMED: &[(&str, Key)] = &[
    ("Enter", Key::Enter),
    ("Esc", Key::Esc),
    ("Tab", Key::Tab),
    ("Backspace", Key::Backspace),
    ("Delete", Key::Delete),
    ("Insert", Key::Insert),
    ("Up", Key::Up),
    ("Down", Key::Down),
    ("Left", Key::Left),
    ("Right", Key::Right),
    ("Home", Key::Home),
    ("End", Key::End),
    ("PageUp", Key::PageUp),
    ("PageDown", Key::PageDown),
    ("Space", Key::Char(' ')),
    ("Mute", Key::Media(MediaKey::Mute)),
    ("VolumeDown", Key::Media(MediaKey::VolumeDown)),
    ("VolumeUp", Key::Media(MediaKey::VolumeUp)),
    ("BrightnessDown", Key::Media(MediaKey::BrightnessDown)),
    ("BrightnessUp", Key::Media(MediaKey::BrightnessUp)),
    ("PlayPause", Key::Media(MediaKey::PlayPause)),
    ("PrevSong", Key::Media(MediaKey::PrevSong)),
    ("NextSong", Key::Media(MediaKey::NextSong)),
    ("Help", Key::Media(MediaKey::Help)),
    ("SysRq", Key::Media(MediaKey::SysRq)),
];

impl Key {
    /// 名前から作る。1 文字ならその文字のキー、`"F5"`、`"Enter"` など。
    /// キー配列のデータファイルとキー割り当て表で使う。
    pub fn from_name(s: &str) -> Option<Key> {
        let mut it = s.chars();
        if let (Some(c), None) = (it.next(), it.next()) {
            return Some(Key::Char(c));
        }
        if let Some(n) = s.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
            if (1..=24).contains(&n) {
                return Some(Key::F(n));
            }
        }
        NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(s)).map(|&(_, k)| k)
    }
    /// 文字を打つキーか。
    pub fn is_char(&self) -> bool {
        matches!(self, Key::Char(_))
    }
}

impl fmt::Display for Key {
    /// [`Key::from_name`] で戻せる名前（`Unknown` は `Key#<コード>`）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Key::Char(' ') => f.write_str("Space"),
            Key::Char(c) => write!(f, "{c}"),
            Key::F(n) => write!(f, "F{n}"),
            Key::Unknown(c) => write!(f, "Key#{c}"),
            k => f.write_str(NAMED.iter().find(|&&(_, x)| x == k).map(|&(n, _)| n).unwrap_or("?")),
        }
    }
}

/// 押されているモディファイア（ビットの集合）。
///
/// キー割り当ての照合に使うのは `SHIFT` / `CTRL` / `ALT` だけ（[`Mods::chord`]）。
/// `SYM` / `FN` はドライバがレイヤー切り替えに使うキーで、参考情報として入る
/// （ステータスバーの表示用。押下が通知されるかはドライバ次第）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods(pub u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const SHIFT: Mods = Mods(1);
    pub const CTRL: Mods = Mods(1 << 1);
    pub const ALT: Mods = Mods(1 << 2);
    pub const SYM: Mods = Mods(1 << 3);
    pub const FN: Mods = Mods(1 << 4);

    pub const fn contains(self, o: Mods) -> bool {
        self.0 & o.0 == o.0
    }
    pub const fn union(self, o: Mods) -> Mods {
        Mods(self.0 | o.0)
    }
    pub const fn without(self, o: Mods) -> Mods {
        Mods(self.0 & !o.0)
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub const fn shift(self) -> bool {
        self.contains(Mods::SHIFT)
    }
    pub const fn ctrl(self) -> bool {
        self.contains(Mods::CTRL)
    }
    pub const fn alt(self) -> bool {
        self.contains(Mods::ALT)
    }
    /// キー割り当ての照合に使う部分（SHIFT / CTRL / ALT）だけを残す。
    pub const fn chord(self) -> Mods {
        Mods(self.0 & 0b111)
    }
}

impl core::ops::BitOr for Mods {
    type Output = Mods;
    fn bitor(self, o: Mods) -> Mods {
        self.union(o)
    }
}

impl core::ops::BitOrAssign for Mods {
    fn bitor_assign(&mut self, o: Mods) {
        self.0 |= o.0;
    }
}

impl fmt::Debug for Mods {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = [(Mods::CTRL, "Ctrl"), (Mods::ALT, "Alt"), (Mods::SHIFT, "Shift"), (Mods::SYM, "Sym"), (Mods::FN, "Fn")];
        let mut first = true;
        for (m, n) in names {
            if self.contains(m) {
                if !first {
                    f.write_str("+")?;
                }
                f.write_str(n)?;
                first = false;
            }
        }
        if first {
            f.write_str("-")?;
        }
        Ok(())
    }
}

/// 1 回の打鍵（押下、または自前のキーリピート）。離上は通知しない。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// 何のキーか。
    pub key: Key,
    /// そのとき押されていたモディファイア。
    pub mods: Mods,
    /// 文字として入力するなら、その文字（Shift を反映済み）。Ctrl / Alt を
    /// 押しているとき、文字でないキーのときは `None`。
    pub text: Option<char>,
    /// 自前のキーリピートで作った打鍵なら `true`（design.md §6.2）。
    pub repeat: bool,
}

impl KeyEvent {
    /// モディファイアつきの打鍵（`text` は付けない）。
    pub const fn new(key: Key, mods: Mods) -> KeyEvent {
        KeyEvent { key, mods, text: None, repeat: false }
    }
    /// 文字 `c` を打った打鍵。英大文字は Shift つきの小文字キーとして作る。
    pub fn char(c: char) -> KeyEvent {
        let (key, mods) = if c.is_ascii_uppercase() {
            (Key::Char(c.to_ascii_lowercase()), Mods::SHIFT)
        } else {
            (Key::Char(c), Mods::NONE)
        };
        KeyEvent { key, mods, text: Some(c), repeat: false }
    }
    /// キー割り当ての照合に使う形。
    pub const fn chord(&self) -> KeyChord {
        KeyChord { key: self.key, mods: self.mods.chord() }
    }
}

/// キー割り当ての 1 つ（例: `Ctrl+Shift+K`）。`mods` は SHIFT / CTRL / ALT だけ。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub key: Key,
    pub mods: Mods,
}

impl KeyChord {
    pub const fn new(key: Key, mods: Mods) -> KeyChord {
        KeyChord { key, mods: mods.chord() }
    }
    /// `"Ctrl+Shift+K"`、`"F5"`、`"Ctrl+/"` のような書き方を読む。英字は大文字・
    /// 小文字どちらでもよい（キーは小文字で持つ）。
    pub fn parse(s: &str) -> Option<KeyChord> {
        let mut mods = Mods::NONE;
        let mut rest = s.trim();
        loop {
            let lower = rest.to_ascii_lowercase();
            let (m, n) = if lower.starts_with("ctrl+") {
                (Mods::CTRL, 5)
            } else if lower.starts_with("shift+") {
                (Mods::SHIFT, 6)
            } else if lower.starts_with("alt+") {
                (Mods::ALT, 4)
            } else {
                break;
            };
            if rest.len() == n {
                break; // "Ctrl++" のように、キーが "+" の場合
            }
            mods |= m;
            rest = &rest[n..];
        }
        let key = match Key::from_name(rest)? {
            Key::Char(c) => Key::Char(c.to_ascii_lowercase()),
            k => k,
        };
        Some(KeyChord { key, mods })
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.ctrl() {
            f.write_str("Ctrl+")?;
        }
        if self.mods.alt() {
            f.write_str("Alt+")?;
        }
        if self.mods.shift() {
            f.write_str("Shift+")?;
        }
        match self.key {
            Key::Char(c) if c.is_ascii_lowercase() => write!(f, "{}", c.to_ascii_uppercase()),
            k => write!(f, "{k}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_roundtrip() {
        for k in [Key::Enter, Key::Char('a'), Key::Char(' '), Key::F(5), Key::PageDown, Key::Media(MediaKey::VolumeUp), Key::Char('/')] {
            assert_eq!(Key::from_name(&k.to_string()), Some(k), "{k}");
        }
        assert_eq!(Key::from_name("enter"), Some(Key::Enter));
        assert_eq!(Key::from_name("F0"), None);
        assert_eq!(Key::from_name("Nope"), None);
        assert_eq!(Key::from_name("あ"), Some(Key::Char('あ')));
    }

    #[test]
    fn chord_parse_and_display() {
        let c = KeyChord::parse("Ctrl+Shift+K").unwrap();
        assert_eq!(c, KeyChord::new(Key::Char('k'), Mods::CTRL | Mods::SHIFT));
        assert_eq!(c.to_string(), "Ctrl+Shift+K");
        assert_eq!(KeyChord::parse("F5").unwrap().to_string(), "F5");
        assert_eq!(KeyChord::parse("ctrl+/").unwrap(), KeyChord::new(Key::Char('/'), Mods::CTRL));
        assert_eq!(KeyChord::parse("Ctrl++").unwrap(), KeyChord::new(Key::Char('+'), Mods::CTRL));
        assert_eq!(KeyChord::parse("Ctrl+Tab").unwrap().to_string(), "Ctrl+Tab");
        assert!(KeyChord::parse("Ctrl+").is_none());
    }

    #[test]
    fn event_helpers() {
        let e = KeyEvent::char('A');
        assert_eq!(e.key, Key::Char('a'));
        assert!(e.mods.shift());
        assert_eq!(e.text, Some('A'));
        let mut e = KeyEvent::new(Key::Char('s'), Mods::CTRL | Mods::FN);
        e.repeat = true;
        assert_eq!(e.chord(), KeyChord::parse("Ctrl+S").unwrap());
        assert_eq!(format!("{:?}", Mods::CTRL | Mods::SHIFT), "Ctrl+Shift");
    }
}
