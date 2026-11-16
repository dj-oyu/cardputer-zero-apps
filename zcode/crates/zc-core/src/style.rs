//! 文字の見た目。ハイライト・Markdown・端末が「区間 → Style」の形で返し、
//! `zc-ui` がフォントと合わせて描く。

use crate::color::{palette, Color};

/// 文字の装飾（ビットの集合）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Attrs(pub u8);

impl Attrs {
    pub const NONE: Attrs = Attrs(0);
    pub const BOLD: Attrs = Attrs(1);
    /// 斜体。CJK に斜体の字形が無いので、字形を傾けて合成する（design.md §5.4）。
    pub const ITALIC: Attrs = Attrs(1 << 1);
    pub const UNDERLINE: Attrs = Attrs(1 << 2);
    pub const STRIKE: Attrs = Attrs(1 << 3);
    /// 波線の下線（構文エラーの表示）。
    pub const SQUIGGLE: Attrs = Attrs(1 << 4);
    /// 前景と背景を入れ替える（端末の反転表示）。
    pub const REVERSE: Attrs = Attrs(1 << 5);

    pub const fn contains(self, o: Attrs) -> bool {
        self.0 & o.0 == o.0
    }
    pub const fn union(self, o: Attrs) -> Attrs {
        Attrs(self.0 | o.0)
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl core::ops::BitOr for Attrs {
    type Output = Attrs;
    fn bitor(self, o: Attrs) -> Attrs {
        self.union(o)
    }
}

/// 文字色・背景色・装飾。
///
/// `bg` が `None` のときは「その部品の地の色」を使う（[`Style::bg_or`]）。
/// 文字の合成は色の表（[`crate::BlendLut`]）を引く方式なので、描く時点では
/// 背景色が必ず 1 色に決まっている必要がある。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Color,
    pub bg: Option<Color>,
    pub attrs: Attrs,
}

impl Style {
    pub const fn new(fg: Color) -> Style {
        Style { fg, bg: None, attrs: Attrs::NONE }
    }
    pub const fn on(mut self, bg: Color) -> Style {
        self.bg = Some(bg);
        self
    }
    pub const fn with(mut self, attrs: Attrs) -> Style {
        self.attrs = self.attrs.union(attrs);
        self
    }
    /// 背景色。指定が無ければ `default`。
    pub const fn bg_or(&self, default: Color) -> Color {
        match self.bg {
            Some(c) => c,
            None => default,
        }
    }
}

impl Default for Style {
    fn default() -> Self {
        Style::new(palette::FG)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attrs_and_style() {
        let a = Attrs::BOLD | Attrs::UNDERLINE;
        assert!(a.contains(Attrs::BOLD));
        assert!(!a.contains(Attrs::ITALIC));
        let s = Style::new(palette::KEYWORD).with(Attrs::ITALIC);
        assert_eq!(s.bg_or(palette::BG), palette::BG);
        assert_eq!(s.on(palette::LINE).bg_or(palette::BG), palette::LINE);
    }
}
