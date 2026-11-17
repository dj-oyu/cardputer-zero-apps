//! ステータスバーとタイトルバー（段階 1 のキー確認・フォント比較画面でも使う）。

use crate::layout::STATUS_PX;
use crate::{RenderCx, Widget};
use zc_core::{palette, Color, Damage, Mods, Priority, Rect};
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

/// 日本語入力のモード（ステータスバー右端の札。中身は段階 4 の `zc-skk`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImeMode {
    /// 英数（コードを書く通常の状態）。
    Ascii,
    Kana,
    Kata,
}

impl Chip {
    /// 入力モードの札（薄い「A」/ 緑「かな」/ 黄「カナ」）。
    pub fn ime(mode: ImeMode) -> Chip {
        match mode {
            ImeMode::Ascii => Chip { text: "A".into(), fg: Color::WHITE, bg: palette::STATUS.mix(Color::WHITE, 64) },
            ImeMode::Kana => Chip { text: "かな".into(), fg: Color::hex(0x0b3d1e), bg: palette::ACCENT },
            ImeMode::Kata => Chip { text: "カナ".into(), fg: Color::hex(0x4a3500), bg: palette::WARN },
        }
    }
}

/// エディタのステータスバーの中身（モック 1・2）。[`EditorStatus::apply`] で
/// [`StatusBar`] に入れる。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorStatus<'a> {
    /// ファイル名。
    pub file: &'a str,
    /// 未保存（● を付ける）。
    pub modified: bool,
    /// カーソルの行と桁（0 始まり。表示は 1 始まり）。
    pub line: u32,
    pub col: u32,
    /// 言語の表示名。
    pub language: &'a str,
    /// 言語の右に出すもの（"UTF-8" など）。
    pub extra: &'a str,
    /// 押している / ロック中のモディファイア（SHIFT / SYM / FN だけ札にする）。
    pub mods: Mods,
    /// 入力モード（`None` なら札を出さない）。
    pub ime: Option<ImeMode>,
}

impl EditorStatus<'_> {
    pub fn apply(&self, bar: &mut StatusBar) {
        use core::fmt::Write;
        let mut left = String::with_capacity(48);
        let _ = write!(left, "{}{}  Ln {}, Col {}", self.file, if self.modified { " ●" } else { "" }, self.line + 1, self.col + 1);
        bar.set_left(&left);
        let mut right = String::with_capacity(24);
        right.push_str(self.language);
        if !self.extra.is_empty() {
            right.push_str("  ");
            right.push_str(self.extra);
        }
        bar.set_right(&right);
        let mut chips = Vec::new();
        for (m, name) in [(Mods::SHIFT, "SHIFT"), (Mods::SYM, "SYM"), (Mods::FN, "FN")] {
            if self.mods.contains(m) {
                chips.push(Chip::modifier(name));
            }
        }
        if let Some(mode) = self.ime {
            chips.push(Chip::ime(mode));
        }
        bar.set_chips(chips);
    }
}

/// ステータスバーの左端の現在地の表示（区画ごとの札。ツリー・エディタ・ターミナルの順で、
/// 番号は移るキーの Ctrl+1/2/3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneMap {
    /// 打鍵が向いている区画（0 ツリー・1 エディタ・2 ターミナル）。
    pub focus: usize,
    /// 区画が開いているか（エディタはいつも開いている）。
    pub open: [bool; 3],
}

/// 現在地の表示の幅（札 14 画素を 15 画素おきに 3 つ + 余白）。
pub const PANE_MAP_W: u16 = 48;

/// 札の印（7×6 以内。`#` が点）。ツリー・エディタ・ターミナル。
const PANE_ICONS: [&[&str]; 3] = [
    &["##.....", "#.###..", "#......", "#.###..", "#......", "#.###.."],
    &["####...", "#..##..", "#.##.#.", "#....#.", "#.##.#.", "######."],
    &["#......", ".#.....", "..#....", ".#.....", "#..###."],
];
/// 札の番号（3×5）。
const PANE_DIGITS: [&[&str]; 3] = [&[".#.", "##.", ".#.", ".#.", "###"], &["##.", "..#", ".#.", "#..", "###"], &["##.", "..#", ".#.", "..#", "##."]];

fn stamp(cx: &mut RenderCx<'_>, rows: &[&str], x: i16, y: i16, c: Color) {
    for (j, row) in rows.iter().enumerate() {
        for (i, ch) in row.bytes().enumerate() {
            if ch == b'#' {
                cx.canvas.fill_rect(Rect::new(x + i as i16, y + j as i16, 1, 1), c);
            }
        }
    }
}

/// 現在地の表示を `area`（ステータスバー）の左端に描く。地は描いてある前提。
fn draw_pane_map(cx: &mut RenderCx<'_>, area: Rect, map: PaneMap, bg: Color) {
    for i in 0..3 {
        let x = area.x + 2 + i as i16 * 15;
        let c = if i == map.focus {
            cx.canvas.fill_rect(Rect::new(x, area.y + 1, 14, area.h.saturating_sub(2)), Color::WHITE);
            bg
        } else if map.open[i] {
            Color::WHITE.mix(bg, 77)
        } else {
            Color::WHITE.mix(bg, 173)
        };
        stamp(cx, PANE_ICONS[i], x + 2, area.y + 2, c);
        stamp(cx, PANE_DIGITS[i], x + 10, area.y + 3, c);
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
    /// 左端の現在地の表示（`None` なら出さない）。
    panes: Option<PaneMap>,
    dirty: bool,
}

impl Default for StatusBar {
    fn default() -> Self {
        StatusBar::new()
    }
}

impl StatusBar {
    pub fn new() -> StatusBar {
        StatusBar { left: String::new(), right: String::new(), chips: vec![], bg: palette::STATUS, panes: None, dirty: true }
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
    pub fn left(&self) -> &str {
        &self.left
    }
    pub fn right(&self) -> &str {
        &self.right
    }
    pub fn chips(&self) -> &[Chip] {
        &self.chips
    }
    /// 左端の現在地の表示（[`PaneMap`]）。出すと左の文字列は [`PANE_MAP_W`] だけ右から始まる。
    pub fn set_panes(&mut self, panes: Option<PaneMap>) {
        if self.panes != panes {
            self.panes = panes;
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
        let left_x = inner.x + if self.panes.is_some() { PANE_MAP_W as i16 } else { 0 };
        let left_area = Rect::new(left_x, area.y, (rx - 6 - left_x as i32).max(0) as u16, area.h);
        cx.text_in(left_area, &self.left, &ts, Color::WHITE, self.bg);
        if let Some(map) = self.panes {
            draw_pane_map(cx, area, map, self.bg);
        }
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
