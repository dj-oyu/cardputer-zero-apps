//! ステータスバーとタイトルバー（段階 1 のキー確認・フォント比較画面でも使う）。

use crate::layout::STATUS_PX;
use crate::{RenderCx, Widget};
use zc_core::{palette, Color, Damage, Priority, Rect};
use zc_font::TextStyle;

/// ステータスバー右側の小さな札（ロック中のモディファイア、入力モード）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub text: String,
    pub fg: Color,
    pub bg: Color,
}

impl Chip {
    /// 白地に青字（SHIFT / SYM / FN）。
    pub fn modifier(text: &str) -> Chip {
        Chip { text: text.into(), fg: palette::STATUS, bg: Color::WHITE }
    }
}

/// ステータスバー（最下段、高さ 11）。左右の文字列と札。
///
/// 返す `Damage` の優先度は [`Priority::Status`]（打鍵の遅延に乗せない。§5.2）。
#[derive(Clone, Debug)]
pub struct StatusBar {
    left: String,
    right: String,
    chips: Vec<Chip>,
    bg: Color,
    dirty: bool,
}

impl Default for StatusBar {
    fn default() -> Self {
        StatusBar::new()
    }
}

impl StatusBar {
    pub fn new() -> StatusBar {
        StatusBar { left: String::new(), right: String::new(), chips: vec![], bg: palette::STATUS, dirty: true }
    }
    /// 左側の文字列。変わったときだけ描き直す。
    pub fn set_left(&mut self, s: &str) {
        if self.left != s {
            self.left = s.into();
            self.dirty = true;
        }
    }
    /// 右側の文字列。
    pub fn set_right(&mut self, s: &str) {
        if self.right != s {
            self.right = s.into();
            self.dirty = true;
        }
    }
    /// 右端の札（左から順）。
    pub fn set_chips(&mut self, chips: Vec<Chip>) {
        if self.chips != chips {
            self.chips = chips;
            self.dirty = true;
        }
    }
    /// 背景色（実行中は橙、など）。
    pub fn set_bg(&mut self, bg: Color) {
        if self.bg != bg {
            self.bg = bg;
            self.dirty = true;
        }
    }
}

impl Widget for StatusBar {
    fn render(&mut self, cx: &mut RenderCx<'_>, area: Rect) -> Damage {
        if !self.dirty {
            return Damage::none();
        }
        self.dirty = false;
        let ts = TextStyle::mono(STATUS_PX);
        cx.canvas.fill_rect(area, self.bg);
        let inner = area.inset(4, 0);
        // 右から: 札 → 右の文字列
        let mut right = inner.right();
        for chip in self.chips.iter().rev() {
            let w = cx.fonts.measure(&chip.text, &ts) + 4;
            let r = Rect::new((right - w) as i16, area.y + 1, w as u16, area.h - 2);
            cx.canvas.fill_rect(r, chip.bg);
            cx.text_in(Rect::new(r.x + 2, r.y, r.w - 2, r.h), &chip.text, &ts, chip.fg, chip.bg);
            right -= w + 3;
        }
        let right_area = Rect::new(inner.x, area.y, (right - inner.x as i32).max(0) as u16, area.h);
        let rx = cx.text_right(right_area, &self.right, &ts, Color::WHITE, self.bg);
        let left_area = Rect::new(inner.x, area.y, (rx - 6 - inner.x as i32).max(0) as u16, area.h);
        cx.text_in(left_area, &self.left, &ts, Color::WHITE, self.bg);
        Damage::rect(area).with_priority(Priority::Status)
    }
    fn invalidate(&mut self) {
        self.dirty = true;
    }
}

/// 最上段の 1 行の帯（タブバーと同じ高さ・色）。段階 1 の画面の見出し用。
#[derive(Clone, Debug)]
pub struct TitleBar {
    title: String,
    hint: String,
    dirty: bool,
}

impl TitleBar {
    pub fn new(title: &str, hint: &str) -> TitleBar {
        TitleBar { title: title.into(), hint: hint.into(), dirty: true }
    }
    pub fn set_hint(&mut self, s: &str) {
        if self.hint != s {
            self.hint = s.into();
            self.dirty = true;
        }
    }
}

impl Widget for TitleBar {
    fn render(&mut self, cx: &mut RenderCx<'_>, area: Rect) -> Damage {
        if !self.dirty {
            return Damage::none();
        }
        self.dirty = false;
        let ts = TextStyle::mono(9.0);
        cx.canvas.fill_rect(area, palette::TABBAR);
        // 選択中のタブと同じ見た目の見出し
        let w = cx.fonts.measure(&self.title, &ts) + 10;
        let tab = Rect::new(area.x, area.y, w as u16, area.h);
        cx.canvas.fill_rect(tab, palette::TAB_ACTIVE);
        cx.canvas.hline(tab.x as i32, tab.y as i32, tab.w, palette::STATUS);
        cx.text_in(Rect::new(tab.x + 5, tab.y + 1, tab.w - 5, tab.h - 1), &self.title, &ts, Color::WHITE, palette::TAB_ACTIVE);
        let rest = Rect::new(tab.x + tab.w as i16 + 4, area.y, area.w - tab.w - 8, area.h);
        cx.text_right(rest, &self.hint, &TextStyle::mono(8.0), palette::DIM, palette::TABBAR);
        Damage::rect(area)
    }
    fn invalidate(&mut self) {
        self.dirty = true;
    }
}
