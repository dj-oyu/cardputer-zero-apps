//! キー配列（`zcode/keymap/*.toml`）。形式はファイルの先頭のコメントを参照。

use std::collections::HashMap;
use std::fmt;
use zc_core::{Key, KeyEvent, Mods};

/// どの表のキーか（表示用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
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

/// キー配列ファイルの 1 キーぶん（形だけ。意味の検査の前）。
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    pub code: u16,
    pub key: Option<&'a str>,
    pub text: Option<&'a str>,
    pub shift: Option<&'a str>,
    pub layer: Option<&'a str>,
    pub pos: Option<(u8, u8)>,
    pub provisional: bool,
}

/// キー配列ファイルの中身（形だけ）。埋め込みの表は `build.rs` がこの形で作る。
#[derive(Clone, Copy, Debug)]
pub struct Table<'a> {
    pub version: u32,
    pub device: Option<&'a str>,
    pub shift: &'a [u16],
    pub ctrl: &'a [u16],
    pub alt: &'a [u16],
    pub sym: &'a [u16],
    pub fn_: &'a [u16],
    pub keys: &'a [Entry<'a>],
}

include!(concat!(env!("OUT_DIR"), "/keymap_builtin.rs"));

#[cfg(feature = "parse")]
mod schema {
    include!("schema.rs");
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
    /// 実行ファイルに埋め込んだ CardputerZero のキー配列
    /// （`zcode/keymap/cardputer-zero.toml` をビルド時に表にしたもの）。
    pub fn builtin() -> Keymap {
        Keymap::from_table(&BUILTIN).expect("埋め込みのキー配列が正しくない")
    }

    /// TOML から読む（テスト・道具用。`parse` 機能が要る）。実機の実行ファイルには
    /// TOML のパーサを入れないので、既定では使えない。
    #[cfg(feature = "parse")]
    pub fn parse(src: &str) -> Result<Keymap, KeymapError> {
        let raw: schema::RawFile = toml::from_str(src).map_err(|e| KeymapError(e.to_string()))?;
        let keys: Vec<Entry<'_>> = raw
            .keys
            .iter()
            .map(|k| Entry {
                code: k.code,
                key: k.key.as_deref(),
                text: k.text.as_deref(),
                shift: k.shift.as_deref(),
                layer: k.layer.as_deref(),
                pos: k.pos.map(|p| (p[0], p[1])),
                provisional: k.provisional,
            })
            .collect();
        Keymap::from_table(&Table {
            version: raw.version,
            device: raw.device.as_deref(),
            shift: &raw.modifiers.shift,
            ctrl: &raw.modifiers.ctrl,
            alt: &raw.modifiers.alt,
            sym: &raw.modifiers.sym,
            fn_: &raw.modifiers.fn_,
            keys: &keys,
        })
    }

    /// 表から作る（意味の検査: キーの名前、1 文字かどうか、重複）。
    pub fn from_table(t: &Table<'_>) -> Result<Keymap, KeymapError> {
        if t.version != 1 {
            return Err(KeymapError(format!("version {} は読めない（1 だけ）", t.version)));
        }
        let mut mods = HashMap::new();
        for (codes, m) in [(t.shift, Mods::SHIFT), (t.ctrl, Mods::CTRL), (t.alt, Mods::ALT), (t.sym, Mods::SYM), (t.fn_, Mods::FN)] {
            for &c in codes {
                if mods.insert(c, m).is_some() {
                    return Err(KeymapError(format!("code {c} が modifiers に 2 回ある")));
                }
            }
        }
        let mut keys = HashMap::new();
        for k in t.keys {
            let key = match k.key {
                None => Key::Unknown(k.code),
                Some(name) => Key::from_name(name)
                    .ok_or_else(|| KeymapError(format!("code {}: key {name:?} が分からない", k.code)))?,
            };
            let text = match (k.text, key) {
                (Some(t), _) => Some(one_char(t, "text", k.code)?),
                (None, Key::Char(c)) => Some(c),
                _ => None,
            };
            let shift = match (k.shift, text) {
                (Some(s), _) => Some(one_char(s, "shift", k.code)?),
                (None, Some(c)) if c.is_ascii_lowercase() => Some(c.to_ascii_uppercase()),
                (None, t) => t,
            };
            let layer = match k.layer {
                None | Some("normal") => Layer::Normal,
                Some("sym") => Layer::Sym,
                Some("fn") => Layer::Fn,
                Some(other) => return Err(KeymapError(format!("code {}: layer {other:?} が分からない", k.code))),
            };
            if mods.contains_key(&k.code) {
                return Err(KeymapError(format!("code {} が modifiers と keys の両方にある", k.code)));
            }
            let def = KeyDef { code: k.code, key, text, shift, layer, pos: k.pos, provisional: k.provisional };
            if keys.insert(k.code, def).is_some() {
                return Err(KeymapError(format!("code {} が 2 回ある", k.code)));
            }
        }
        Ok(Keymap { device: t.device.map(str::to_owned), keys, mods })
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

    /// ドライバの付属ファイル（[`DRIVER_MAP_PATH`]）で Sym レイヤーの記号の対応を
    /// 上書きする。本体のランチャーと同じ表を使うため（ランチャーも、このファイルが
    /// あれば読み、無ければ埋め込みの表を使う）。読めなければ何もしない。
    /// 上書きした件数を返す。
    pub fn load_driver_map(&mut self) -> usize {
        match std::fs::read_to_string(DRIVER_MAP_PATH) {
            Ok(text) => self.apply_driver_map(&text),
            Err(_) => 0,
        }
    }

    /// `keycode 26 = exclam  # コメント` の形の行を読み、そのキーコードの文字を
    /// 上書きする（キーシンボル名は [`keysym_char`] が分かるものだけ）。
    /// 上書きした件数を返す。
    pub fn apply_driver_map(&mut self, text: &str) -> usize {
        let mut n = 0;
        for line in text.lines() {
            let body = line.split('#').next().unwrap_or("").trim();
            let Some(rest) = body.strip_prefix("keycode") else { continue };
            let mut it = rest.splitn(2, '=');
            let (Some(code), Some(name)) = (it.next(), it.next()) else { continue };
            let (Ok(code), Some(c)) = (code.trim().parse::<u16>(), keysym_char(name.trim())) else { continue };
            if self.mods.contains_key(&code) {
                continue;
            }
            let def = self.keys.entry(code).or_insert(KeyDef {
                code,
                key: Key::Unknown(code),
                text: None,
                shift: None,
                layer: Layer::Sym,
                pos: None,
                provisional: false,
            });
            def.key = Key::Char(c);
            def.text = Some(c);
            def.shift = Some(c);
            def.provisional = false;
            n += 1;
        }
        n
    }
}

/// ドライバ（tca8418_keypad_m5stack）の付属ファイル。Sym レイヤーの「記号ごとの枠」の
/// キーコードと記号の対応（CardputerZero/launcher の `cp0_keyboard_keymap.c` と同じ）。
pub const DRIVER_MAP_PATH: &str = "/usr/share/keymaps/tca8418_keypad_m5stack_keymap.map";

/// X11 のキーシンボル名 → 文字（ドライバの付属ファイルに出てくるもの）。
pub fn keysym_char(name: &str) -> Option<char> {
    Some(match name {
        "exclam" => '!',
        "at" => '@',
        "numbersign" => '#',
        "dollar" => '$',
        "percent" => '%',
        "asciicircum" => '^',
        "ampersand" => '&',
        "asterisk" => '*',
        "parenleft" => '(',
        "parenright" => ')',
        "asciitilde" => '~',
        "grave" => '`',
        "underscore" => '_',
        "minus" => '-',
        "plus" => '+',
        "equal" => '=',
        "bracketleft" => '[',
        "bracketright" => ']',
        "braceleft" => '{',
        "braceright" => '}',
        "semicolon" => ';',
        "colon" => ':',
        "apostrophe" => '\'',
        "quotedbl" => '"',
        "less" => '<',
        "greater" => '>',
        "backslash" => '\\',
        "bar" => '|',
        "comma" => ',',
        "period" => '.',
        "slash" => '/',
        "question" => '?',
        "space" => ' ',
        _ => {
            let mut it = name.chars();
            match (it.next(), it.next()) {
                (Some(c), None) if c.is_ascii_alphanumeric() => c,
                _ => return None,
            }
        }
    })
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
        // Sym レイヤー: 記号ごとの枠のキーコード（71 は KEY_KP7 だが意味は `-`、R の刻印）
        let minus = km.get(71).unwrap();
        assert!(!minus.provisional);
        assert_eq!(minus.key, Key::Char('-'));
        assert_eq!(minus.layer, Layer::Sym);
        assert_eq!(km.normal_at(minus.pos.unwrap()).unwrap().key, Key::Char('r'));
        assert_eq!(km.translate(26, Mods::SYM).text, Some('!')); // Sym+1
        assert_eq!(km.translate(93, Mods::SYM).text, Some('?')); // Sym+M
        // Sym レイヤーで確定していないのは、刻印の無い 78 だけ
        let open: Vec<u16> = km.defs().filter(|d| d.layer == Layer::Sym && d.key == Key::Unknown(d.code)).map(|d| d.code).collect();
        assert_eq!(open, vec![78]);
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
    fn driver_map_overrides_and_matches_builtin() {
        // 実機の /usr/share/keymaps/tca8418_keypad_m5stack_keymap.map の抜粋（2026-11-16）
        let file = "# -- Row 4 --\nkeycode 26 = exclam            # 0xb7 -> !\nkeycode 27 = at\n\
                    keycode 82 = quotedbl  # \"\nkeycode 86 = backslash\nbroken line\nkeycode 42 = at\n";
        let mut km = Keymap::builtin();
        let before = km.clone();
        // 埋め込みの表と同じ内容なので、上書きしても変わらない
        assert_eq!(km.apply_driver_map(file), 4); // 42 は Shift（モディファイア）なので触らない
        assert_eq!(km.keys, before.keys);
        assert_eq!(km.modifier(42), Some(Mods::SHIFT));
        // 違う対応が来たら従う
        assert_eq!(km.apply_driver_map("keycode 26 = question"), 1);
        assert_eq!(km.translate(26, Mods::SYM).text, Some('?'));
        assert_eq!(keysym_char("nosuchsym"), None);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn parse_errors() {
        let base = "version = 1\nmodifiers = { shift = [42] }\n";
        assert!(Keymap::parse(&format!("{base}keys = [{{ code = 1, key = \"Nope\" }}]")).is_err());
        assert!(Keymap::parse(&format!("{base}keys = [{{ code = 1 }}, {{ code = 1 }}]")).is_err());
        assert!(Keymap::parse(&format!("{base}keys = [{{ code = 42, key = \"a\" }}]")).is_err());
        assert!(Keymap::parse(&format!("{base}keys = [{{ code = 1, key = \"a\", shift = \"AB\" }}]")).is_err());
        assert!(Keymap::parse(&format!("{base}keys = [{{ code = 1, kee = \"a\" }}]")).is_err());
        assert!(Keymap::parse("version = 2\nmodifiers = {}\nkeys = []").is_err());
        assert!(Keymap::parse(&format!("{base}keys = [{{ code = 1, layer = \"top\" }}]")).is_err());
        let km = Keymap::parse(&format!("{base}keys = [{{ code = 71, key = \"&\", shift = \"*\" }}]")).unwrap();
        assert_eq!(km.translate(71, Mods::SHIFT).text, Some('*'));
    }

    #[cfg(feature = "parse")]
    #[test]
    fn builtin_table_matches_the_toml_file() {
        // build.rs の変換が、実行時に TOML を読んだ結果と同じになること
        let parsed = Keymap::parse(include_str!("../../../keymap/cardputer-zero.toml")).unwrap();
        let builtin = Keymap::builtin();
        assert_eq!(parsed.device(), builtin.device());
        assert_eq!(parsed.keys, builtin.keys);
        assert_eq!(parsed.mods, builtin.mods);
    }
}
