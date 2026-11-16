//! テキストまわりの共通の型。`zc-buffer`・`zc-highlight`・`zc-intel`・`zc-markdown`
//! は互いに依存しないので、やり取りに使う型はここに置く。

use crate::color::palette;
use crate::style::{Attrs, Style};

/// テキスト上の位置。`line` は 0 始まりの行、`col` はその行の中の **文字数**
/// （Unicode スカラー値の数。バイト数でも表示桁でもない）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl Pos {
    pub const fn new(line: u32, col: u32) -> Pos {
        Pos { line, col }
    }
}

/// 行の半開区間 `start..end`（0 始まり）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    pub const fn new(start: u32, end: u32) -> LineRange {
        LineRange { start, end }
    }
    pub const fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }
    pub const fn is_empty(&self) -> bool {
        self.end <= self.start
    }
    pub const fn contains(&self, line: u32) -> bool {
        self.start <= line && line < self.end
    }
}

/// 編集で変わった行の通知（design.md §9 zc-buffer）。ハイライトと描画はこれを見て
/// 無効化する。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineChange {
    /// 編集前の行範囲。
    pub old: LineRange,
    /// 編集後に、その範囲が占める行範囲（`new.start == old.start`）。
    pub new: LineRange,
}

impl LineChange {
    /// 行数の増減。
    pub const fn delta(&self) -> i64 {
        self.new.len() as i64 - self.old.len() as i64
    }
}

/// 行単位で読めるテキスト。`zc-buffer` のバッファとその写し（スナップショット）が
/// 実装し、ハイライト・補完・Markdown はこれ越しに読む。
pub trait LineSource {
    /// 行数（空のテキストでも 1）。
    fn len_lines(&self) -> u32;
    /// `line` 行目を `out` に入れる（先に `out` を空にする。行末の改行は含めない）。
    fn line(&self, line: u32, out: &mut String);
}

impl LineSource for [&str] {
    fn len_lines(&self) -> u32 {
        self.len().max(1) as u32
    }
    fn line(&self, line: u32, out: &mut String) {
        out.clear();
        if let Some(s) = self.get(line as usize) {
            out.push_str(s);
        }
    }
}

/// トークンの種類。ハイライトの規則（`language.toml` の `token = "..."`）と、
/// 配色（[`crate::Theme`]）の両方がこの名前を使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum TokenKind {
    /// 色を付けない地の文。
    #[default]
    Text = 0,
    Comment,
    String,
    /// 文字列の中のエスケープ。
    Escape,
    Number,
    /// `def`・`class`・`let` などの宣言のキーワード。
    Keyword,
    /// `if`・`for`・`return` などの制御のキーワード。
    Control,
    Function,
    /// 型・クラス名。
    Type,
    Variable,
    /// `True`・`None`・定数。
    Constant,
    Operator,
    Punctuation,
    /// デコレータ・属性・マクロ。
    Attribute,
    /// Markdown の見出し、TOML のテーブル名。
    Heading,
    /// キー（JSON / TOML のキー）。
    Key,
    Link,
    /// 構文エラー。
    Invalid,
}

impl TokenKind {
    /// 種類の数。
    pub const COUNT: usize = TokenKind::Invalid as usize + 1;

    const NAMES: [&'static str; TokenKind::COUNT] = [
        "text", "comment", "string", "escape", "number", "keyword", "control", "function", "type", "variable",
        "constant", "operator", "punctuation", "attribute", "heading", "key", "link", "invalid",
    ];

    /// `language.toml` に書く名前。
    pub const fn name(self) -> &'static str {
        TokenKind::NAMES[self as usize]
    }
    /// 名前から。
    pub fn from_name(s: &str) -> Option<TokenKind> {
        TokenKind::NAMES.iter().position(|n| *n == s).map(|i| TokenKind::from_u8(i as u8))
    }
    /// 番号から（範囲外は `Text`）。
    pub const fn from_u8(v: u8) -> TokenKind {
        if (v as usize) < TokenKind::COUNT {
            // SAFETY: repr(u8) で、0..COUNT は全部有効な値。
            unsafe { core::mem::transmute::<u8, TokenKind>(v) }
        } else {
            TokenKind::Text
        }
    }
}

/// 1 行の中の、同じ種類のトークンが続く区間。`start..end` はその行の **バイト位置**。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub kind: TokenKind,
}

/// 配色。トークンの種類 → [`Style`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    tokens: [Style; TokenKind::COUNT],
}

impl Theme {
    /// VS Code の Dark+。
    pub fn dark_plus() -> Theme {
        use TokenKind as T;
        let mut tokens = [Style::new(palette::FG); TokenKind::COUNT];
        let mut set = |k: T, s: Style| tokens[k as usize] = s;
        set(T::Comment, Style::new(palette::COMMENT));
        set(T::String, Style::new(palette::STRING));
        set(T::Escape, Style::new(crate::Color::hex(0xd7ba7d)));
        set(T::Number, Style::new(palette::NUMBER));
        set(T::Keyword, Style::new(palette::KEYWORD));
        set(T::Control, Style::new(palette::CONTROL));
        set(T::Function, Style::new(palette::FUNCTION));
        set(T::Type, Style::new(palette::CLASS));
        set(T::Variable, Style::new(palette::VARIABLE));
        set(T::Constant, Style::new(palette::KEYWORD));
        set(T::Attribute, Style::new(palette::FUNCTION));
        set(T::Heading, Style::new(palette::KEYWORD).with(Attrs::BOLD));
        set(T::Key, Style::new(palette::VARIABLE));
        set(T::Link, Style::new(crate::Color::hex(0x3794ff)).with(Attrs::UNDERLINE));
        set(T::Invalid, Style::new(palette::ERROR));
        Theme { tokens }
    }
    /// トークンの種類に対する見た目。
    pub fn token(&self, kind: TokenKind) -> Style {
        self.tokens[kind as usize]
    }
    /// 見た目を差し替える（`languages/theme.toml` から作るとき用）。
    pub fn set_token(&mut self, kind: TokenKind, style: Style) {
        self.tokens[kind as usize] = style;
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::dark_plus()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_names_roundtrip() {
        for i in 0..TokenKind::COUNT as u8 {
            let k = TokenKind::from_u8(i);
            assert_eq!(TokenKind::from_name(k.name()), Some(k));
        }
        assert_eq!(TokenKind::from_u8(200), TokenKind::Text);
        assert_eq!(TokenKind::from_name("nope"), None);
        assert_eq!(TokenKind::Invalid.name(), "invalid");
    }

    #[test]
    fn theme_and_line_source() {
        let t = Theme::dark_plus();
        assert_eq!(t.token(TokenKind::Keyword).fg, palette::KEYWORD);
        assert_eq!(t.token(TokenKind::Text).fg, palette::FG);
        let src: &[&str] = &["a", "b"];
        let mut s = String::from("x");
        src.line(1, &mut s);
        assert_eq!(s, "b");
        assert_eq!(src.len_lines(), 2);
        let c = LineChange { old: LineRange::new(3, 4), new: LineRange::new(3, 6) };
        assert_eq!(c.delta(), 2);
    }
}
