//! キー配列（`zcode/keymap/*.toml`）。形式はファイルの先頭のコメントを参照。

use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use zc_core::{Key, KeyEvent, Mods};

/// どの表のキーか（表示用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    #[default]
    Normal,
    Sym,
    Fn,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Normal => "normal",
            Layer::Sym => "sym",
            Layer::Fn => "fn",
        }
    }
}

/// 1 つのキーコードの定義。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyDef {
    pub code: u16,
    /// 論理キー。対応が決まっていなければ `Key::Unknown(code)`。
    pub key: Key,
    /// 入力される文字。
    pub text: Option<char>,
    /// Shift を押しているときの文字。
    pub shift: Option<char>,
    pub layer: Layer,
    /// キーボード行列の (行, 列)。
    pub pos: Option<(u8, u8)>,
    /// 未確定（キー確認モードで確かめる）。
    pub provisional: bool,
}

/// キー配列の読み込みエラー。
#[derive(Debug)]
pub struct KeymapError(pub String);

impl fmt::Display for KeymapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "キー配列: {}", self.0)
    }
}

impl std::error::Error for KeymapError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    version: u32,
    #[serde(default)]
    device: Option<String>,
    modifiers: RawMods,
    keys: Vec<RawKey>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawMods {
    #[serde(default)]
    shift: Vec<u16>,
    #[serde(default)]
    ctrl: Vec<u16>,
    #[serde(default)]
    alt: Vec<u16>,
    #[serde(default)]
    sym: Vec<u16>,
    #[serde(default, rename = "fn")]
    fn_: Vec<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKey {
    code: u16,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    shift: Option<String>,
    #[serde(default)]
    layer: Layer,
    #[serde(default)]
    pos: Option<[u8; 2]>,
    #[serde(default)]
    provisional: bool,
}

/// キー配列。
#[derive(Clone, Debug)]
pub struct Keymap {
    device: Option<String>,
    keys: HashMap<u16, KeyDef>,
    mods: HashMap<u16, Mods>,
}

fn one_char(s: &str, what: &str, code: u16) -> Result<char, KeymapError> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(KeymapError(format!("code {code}: {what} は 1 文字で書く（{s:?}）"))),
    }
}

impl Keymap {
    /// 実行ファイルに埋め込んだ CardputerZero のキー配列。
    pub fn builtin() -> Keymap {
        Keymap::parse(include_str!("../../../keymap/cardputer-zero.toml")).expect("埋め込みのキー配列が読めない")
    }

    /// TOML から読む。
    pub fn parse(src: &str) -> Result<Keymap, KeymapError> {
        let raw: RawFile = toml::from_str(src).map_err(|e| KeymapError(e.to_string()))?;
        if raw.version != 1 {
            return Err(KeymapError(format!("version {} は読めない（1 だけ）", raw.version)));
        }
        let mut mods = HashMap::new();
        for (codes, m) in [
            (&raw.modifiers.shift, Mods::SHIFT),
            (&raw.modifiers.ctrl, Mods::CTRL),
            (&raw.modifiers.alt, Mods::ALT),
            (&raw.modifiers.sym, Mods::SYM),
            (&raw.modifiers.fn_, Mods::FN),
        ] {
            for &c in codes {
                if mods.insert(c, m).is_some() {
                    return Err(KeymapError(format!("code {c} が modifiers に 2 回ある")));
                }
            }
        }
        let mut keys = HashMap::new();
        for k in raw.keys {
            let key = match &k.key {
                None => Key::Unknown(k.code),
                Some(name) => Key::from_name(name)
                    .ok_or_else(|| KeymapError(format!("code {}: key {name:?} が分からない", k.code)))?,
            };
            let text = match (&k.text, key) {
                (Some(t), _) => Some(one_char(t, "text", k.code)?),
                (None, Key::Char(c)) => Some(c),
                _ => None,
            };
            let shift = match (&k.shift, text) {
                (Some(s), _) => Some(one_char(s, "shift", k.code)?),
                (None, Some(c)) if c.is_ascii_lowercase() => Some(c.to_ascii_uppercase()),
                (None, t) => t,
            };
            if mods.contains_key(&k.code) {
                return Err(KeymapError(format!("code {} が modifiers と keys の両方にある", k.code)));
            }
            let def = KeyDef {
                code: k.code,
                key,
                text,
                shift,
                layer: k.layer,
                pos: k.pos.map(|p| (p[0], p[1])),
                provisional: k.provisional,
            };
            if keys.insert(k.code, def).is_some() {
                return Err(KeymapError(format!("code {} が 2 回ある", k.code)));
            }
        }
        Ok(Keymap { device: raw.device, keys, mods })
    }

    /// 対象のデバイス名（evdev の名前）。
    pub fn device(&self) -> Option<&str> {
        self.device.as_deref()
    }
    /// キーコードの定義。
    pub fn get(&self, code: u16) -> Option<&KeyDef> {
        self.keys.get(&code)
    }
    /// モディファイアのキーコードなら、そのビット。
    pub fn modifier(&self, code: u16) -> Option<Mods> {
        self.mods.get(&code).copied()
    }
    /// 全部のキーの定義（順不同）。
    pub fn defs(&self) -> impl Iterator<Item = &KeyDef> {
        self.keys.values()
    }
    /// 行列の位置 `pos` にある通常レイヤーのキー（その物理キーの呼び名に使う）。
    pub fn normal_at(&self, pos: (u8, u8)) -> Option<&KeyDef> {
        self.keys.values().find(|d| d.layer == Layer::Normal && d.pos == Some(pos))
    }

    /// キーコード `code` を、モディファイア `mods` のもとで押したときの打鍵。
    /// 表に無いコードは `Key::Unknown`。
    pub fn translate(&self, code: u16, mods: Mods) -> KeyEvent {
        let Some(def) = self.keys.get(&code) else {
            return KeyEvent::new(Key::Unknown(code), mods);
        };
        let text = if mods.ctrl() || mods.alt() {
            None
        } else if mods.shift() {
            def.shift
        } else {
            def.text
        };
        KeyEvent { key: def.key, mods, text, repeat: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::MediaKey;

    #[test]
    fn builtin_parses_and_covers_layers() {
        let km = Keymap::builtin();
        assert_eq!(km.device(), Some("tca8418c"));
        assert_eq!(km.modifier(42), Some(Mods::SHIFT));
        assert_eq!(km.modifier(383), Some(Mods::SYM));
        assert_eq!(km.modifier(389), Some(Mods::FN));
        assert_eq!(km.get(16).unwrap().key, Key::Char('q'));
        assert_eq!(km.get(63).unwrap().key, Key::F(5));
        assert_eq!(km.get(115).unwrap().key, Key::Media(MediaKey::VolumeUp));
        // Sym レイヤーの未確定キー
        let kp7 = km.get(71).unwrap();
        assert!(kp7.provisional);
        assert_eq!(kp7.key, Key::Unknown(71));
        assert_eq!(kp7.layer, Layer::Sym);
        assert_eq!(km.normal_at(kp7.pos.unwrap()).unwrap().key, Key::Char('r'));
        // 通常レイヤーは 41 キー、どれも確定
        let normal: Vec<_> = km.defs().filter(|d| d.layer == Layer::Normal).collect();
        assert_eq!(normal.len(), 41);
        assert!(normal.iter().all(|d| !d.provisional));
    }

    #[test]
    fn translate_applies_shift_ctrl() {
        let km = Keymap::builtin();
        assert_eq!(km.translate(30, Mods::NONE).text, Some('a'));
        let e = km.translate(30, Mods::SHIFT);
        assert_eq!((e.key, e.text), (Key::Char('a'), Some('A')));
        let e = km.translate(31, Mods::CTRL);
        assert_eq!((e.key, e.text), (Key::Char('s'), None));
        assert_eq!(km.translate(57, Mods::NONE).text, Some(' '));
        assert_eq!(km.translate(28, Mods::NONE), KeyEvent::new(Key::Enter, Mods::NONE));
        assert_eq!(km.translate(3, Mods::SHIFT).text, Some('2'));
        assert_eq!(km.translate(600, Mods::NONE).key, Key::Unknown(600));
    }

    #[test]
    fn parse_errors() {
        let base = "version = 1\nmodifiers = { shift = [42] }\n";
        assert!(Keymap::parse(&format!("{base}keys = [{{ code = 1, key = \"Nope\" }}]")).is_err());
        assert!(Keymap::parse(&format!("{base}keys = [{{ code = 1 }}, {{ code = 1 }}]")).is_err());
        assert!(Keymap::parse(&format!("{base}keys = [{{ code = 42, key = \"a\" }}]")).is_err());
        assert!(Keymap::parse(&format!("{base}keys = [{{ code = 1, key = \"a\", shift = \"AB\" }}]")).is_err());
        assert!(Keymap::parse(&format!("{base}keys = [{{ code = 1, kee = \"a\" }}]")).is_err());
        assert!(Keymap::parse("version = 2\nmodifiers = {}\nkeys = []").is_err());
        let km = Keymap::parse(&format!("{base}keys = [{{ code = 71, key = \"&\", shift = \"*\" }}]")).unwrap();
        assert_eq!(km.translate(71, Mods::SHIFT).text, Some('*'));
    }
}
