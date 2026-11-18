//! 選ぶための部品: 単語補完の小窓（[`CompletionPopup`]、モック 12）と、絞り込みつきの
//! 一覧（[`PickerList`]、モック 14 のアウトライン。ショートカット一覧と同じ見た目）。

use crate::layout::{CODE_PX, STATUS_PX, UI_PX};
use crate::{RenderCx, Widget};
use zc_core::{palette, Color, Damage, Priority, Rect};
use zc_font::TextStyle;

/// 小窓の地の色・枠の色（SKK の候補一覧と同じ。モック 8・12）。
const POPUP_BG: Color = Color::hex(0x252526);
const POPUP_BORDER: Color = Color::hex(0x454545);
/// 小窓の下の案内の区切り線。
const FOOT_LINE: Color = Color::hex(0x3c3c3c);
/// 一致した部分の色（補完）。
const HIT_BLUE: Color = Color::hex(0x18a3ff);
/// 一致した部分の色（一覧の絞り込み。モック 6・14）。
const HIT_YELLOW: Color = Color::hex(0xffd479);
/// 検索欄の地の色。
const SEARCH_BG: Color = Color::hex(0x3c3c3c);

/// 小窓の 1 行目の札の代わりに描く印（型の情報の問い合わせ中。2026-11-18）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PopupMark {
    /// 印なし（札の文字を描く）。
    #[default]
    None,
    /// 点の輪（7×7、8 個の点）。数は先頭の点の位置（0〜7。コマごとに 1 つ進める）。
    Spin(u8),
    /// 失敗（赤い ×）。
    Failed,
}

/// 点の輪の点の位置（7×7 の中。12 時から時計回り）。
const RING: [(i32, i32); 8] = [(3, 0), (5, 1), (6, 3), (5, 5), (3, 6), (1, 5), (0, 3), (1, 1)];
/// 点の輪の色（明るい青。モックの #4fc1ff）。
const SPIN_BLUE: Color = Color::hex(0x4fc1ff);

/// 補完の候補 1 つ。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PopupItem {
    /// 左の小さな札（`ab` = 文書の単語、`kw` = キーワード）と、その色。
    pub tag: &'static str,
    pub tag_color: Color,
    pub label: String,
    /// 先頭から何文字が、打った文字と一致しているか（青で示す）。
    pub hit_chars: usize,
}

/// 単語補完の小窓（モック 12）。カーソルの下（入らなければ上）に出す。
///
/// `Widget` は実装しない: 位置がカーソルで決まるので、[`CompletionPopup::render_at`] で描く。
#[derive(Default)]
pub struct CompletionPopup {
    items: Vec<PopupItem>,
    sel: usize,
    /// 下の案内の行（空なら案内の行を出さない）。
    footer: &'static str,
    /// 1 行目の札の代わりの印。
    mark: PopupMark,
    dirty: bool,
    /// 前回描いた矩形。
    drawn: Option<Rect>,
}

/// 候補 1 行の高さ。
const ITEM_H: u16 = 12;
/// 案内の行の高さ。
const FOOT_H: u16 = 10;

impl CompletionPopup {
    pub fn new(footer: &'static str) -> CompletionPopup {
        CompletionPopup { footer, dirty: true, ..Default::default() }
    }
    /// 候補と選択中の位置。変わったときだけ描き直す。
    pub fn set(&mut self, items: Vec<PopupItem>, sel: usize) {
        if self.items != items || self.sel != sel {
            self.items = items;
            self.sel = sel;
            self.dirty = true;
        }
    }
    pub fn items(&self) -> &[PopupItem] {
        &self.items
    }
    /// 1 行目の札の代わりの印。変わったときだけ描き直す。
    pub fn set_mark(&mut self, mark: PopupMark) {
        if self.mark != mark {
            self.mark = mark;
            self.dirty = true;
        }
    }
    /// 案内の行の高さ（案内が空なら 0）。
    fn foot_h(&self) -> u16 {
        if self.footer.is_empty() {
            0
        } else {
            FOOT_H
        }
    }
    pub fn selected(&self) -> usize {
        self.sel
    }
    /// 前回描いた矩形（閉じたとき・動いたときに、下の行を描き直してもらう範囲）。
    pub fn drawn(&self) -> Option<Rect> {
        self.drawn
    }
    /// 描いたものを忘れる（下の行を描き直したあと）。
    pub fn forget(&mut self) {
        self.drawn = None;
        self.dirty = true;
    }
    /// 次の描画で必ず描き直す。
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// 置く矩形を求める。`caret` はカーソルの画面上の矩形、`word_x` は打っている単語の
    /// 先頭の x（候補の文字をそこにそろえる）、`bounds` は出してよい範囲（エディタの領域）。
    pub fn place(&self, cx: &mut RenderCx<'_>, caret: Rect, word_x: i32, bounds: Rect) -> Rect {
        let (code, small) = (TextStyle::mono(CODE_PX), TextStyle::mono(STATUS_PX));
        let tag_w = cx.fonts.measure("ab", &small) + 3;
        let label_w = self.items.iter().map(|i| cx.fonts.measure(&i.label, &code)).max().unwrap_or(0);
        let foot_w = cx.fonts.measure(self.footer, &small) + 8;
        let w = (2 + tag_w + label_w + 6).max(foot_w).min(bounds.w as i32) as u16;
        let h = self.items.len() as u16 * ITEM_H + self.foot_h() + 2;
        let x = (word_x - 2 - tag_w).clamp(bounds.x as i32, (bounds.right() - w as i32).max(bounds.x as i32));
        let below = caret.bottom() + 1;
        let y = if below + h as i32 <= bounds.bottom() { below } else { (caret.y as i32 - h as i32 - 1).max(bounds.y as i32) };
        Rect::new(x as i16, y as i16, w, h.min(bounds.h))
    }

    /// `rect` に描く。`force` なら変わっていなくても描く（下の行が描き直されたとき）。
    pub fn render_at(&mut self, cx: &mut RenderCx<'_>, rect: Rect, force: bool) -> Damage {
        if !force && !self.dirty && self.drawn == Some(rect) {
            return Damage::none();
        }
        self.dirty = false;
        self.drawn = Some(rect);
        let (code, small) = (TextStyle::mono(CODE_PX), TextStyle::mono(STATUS_PX));
        cx.canvas.fill_rect(rect, POPUP_BG);
        cx.canvas.stroke_rect(rect, POPUP_BORDER);
        let inner = rect.inset(1, 1);
        let tag_w = cx.fonts.measure("ab", &small) + 3;
        for (i, it) in self.items.iter().enumerate() {
            let row = Rect::new(inner.x, inner.y + (i as u16 * ITEM_H) as i16, inner.w, ITEM_H).intersect(&inner);
            if row.is_empty() {
                break;
            }
            let bg = if i == self.sel { palette::LIST_FOCUS } else { POPUP_BG };
            cx.canvas.fill_rect(row, bg);
            let tag_area = Rect::new(row.x + 2, row.y, tag_w as u16, row.h);
            match (i, self.mark) {
                (0, PopupMark::Spin(head)) => draw_ring(cx, tag_area, head, bg),
                (0, PopupMark::Failed) => draw_cross(cx, tag_area),
                _ => {
                    cx.text_in(tag_area, it.tag, &small, it.tag_color, bg);
                }
            }
            let base = row.y as i32 + cx.baseline_in(&code, row.h);
            let split = it.label.char_indices().nth(it.hit_chars).map_or(it.label.len(), |(b, _)| b);
            let (hit, rest) = it.label.split_at(split);
            let x = row.x as i32 + 2 + tag_w;
            let fg = if i == self.sel { Color::WHITE } else { palette::FG };
            let x = cx.text(x, base, hit, &code.bold(), HIT_BLUE, bg, row);
            cx.text(x, base, rest, &code, fg, bg, row);
        }
        let foot_y = inner.y as i32 + self.items.len() as i32 * ITEM_H as i32;
        if !self.footer.is_empty() && foot_y < inner.bottom() {
            cx.canvas.hline(inner.x as i32, foot_y, inner.w, FOOT_LINE);
            let foot = Rect::new(inner.x + 4, (foot_y + 1) as i16, inner.w.saturating_sub(4), FOOT_H - 1).intersect(&inner);
            cx.text_in(foot, self.footer, &small, palette::DIM, POPUP_BG);
        }
        Damage::range(rect.rows(), Priority::Cursor)
    }
}

/// 点の輪を `area` の左寄せ・上下の中ほどに描く。先頭（`head`）がいちばん明るく、後ろへ薄くなる。
fn draw_ring(cx: &mut RenderCx<'_>, area: Rect, head: u8, bg: Color) {
    let (x0, y0) = (area.x as i32 + 1, area.y as i32 + (area.h as i32 - 7) / 2);
    for (k, (dx, dy)) in RING.iter().enumerate() {
        let age = (head as usize + RING.len() - k) % RING.len();
        // 先頭から 0 / 1 / 2 / それより後ろ: 明るさ 100% / 70% / 45% / 20%
        let t = [255u8, 180, 115, 50][age.min(3)];
        cx.canvas.put(x0 + dx, y0 + dy, bg.mix(SPIN_BLUE, t));
    }
}

/// 赤い × を `area` の左寄せ・上下の中ほどに描く（7×7）。
fn draw_cross(cx: &mut RenderCx<'_>, area: Rect) {
    let (x0, y0) = (area.x as i32 + 1, area.y as i32 + (area.h as i32 - 7) / 2);
    for d in 0..7 {
        cx.canvas.put(x0 + d, y0 + d, palette::ERROR);
        cx.canvas.put(x0 + 6 - d, y0 + d, palette::ERROR);
    }
}

/// 一覧の 1 行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickRow {
    /// 入れ子の深さ（字下げ 1 段 = 空白 2 文字ぶん）。
    pub depth: u8,
    /// 名前の前の印（`ƒ` = 関数、`C` = クラス、`#` = 見出し）と、その色。
    pub icon: &'static str,
    pub icon_color: Color,
    pub name: String,
    /// 名前の中で、絞り込みの文字と一致した区間（バイト位置）。
    pub hits: Vec<core::ops::Range<usize>>,
    /// 右端に出す文字（行番号など）。
    pub right: String,
    /// 一致した項目の親として残しているだけの行（薄く出す。選べない）。
    pub context: bool,
}

/// 絞り込みつきの一覧（モック 14）。上に検索欄、その下に見出し、残りに行。
#[derive(Default)]
pub struct PickerList {
    query: String,
    title: String,
    rows: Vec<PickRow>,
    sel: usize,
    scroll: usize,
    empty_text: String,
    dirty: bool,
}

/// 一覧の 1 行の高さ（モックと同じ）。
const ROW_H: u16 = 11;
/// 検索欄の高さ（上下の余白 1 を含む）。
const SEARCH_H: u16 = 15;

impl PickerList {
    pub fn new(title: &str) -> PickerList {
        PickerList { title: title.into(), dirty: true, ..Default::default() }
    }
    /// 中身を置き換える。`sel` は選んでおく行（範囲外なら丸める）。
    pub fn set(&mut self, query: &str, rows: Vec<PickRow>, sel: usize) {
        let sel = sel.min(rows.len().saturating_sub(1));
        if self.query != query || self.rows != rows || self.sel != sel {
            self.query = query.into();
            self.rows = rows;
            self.sel = sel;
            self.dirty = true;
        }
    }
    /// 見出しを変える。
    pub fn set_title(&mut self, title: &str) {
        if self.title != title {
            self.title = title.into();
            self.dirty = true;
        }
    }
    /// 行が 1 つも無いときに出す文。
    pub fn set_empty_text(&mut self, s: &str) {
        if self.empty_text != s {
            self.empty_text = s.into();
            self.dirty = true;
        }
    }
    pub fn rows(&self) -> &[PickRow] {
        &self.rows
    }
    pub fn selected(&self) -> usize {
        self.sel
    }
    pub fn query(&self) -> &str {
        &self.query
    }
}

impl Widget for PickerList {
    fn render(&mut self, cx: &mut RenderCx<'_>, area: Rect) -> Damage {
        if !self.dirty || area.is_empty() {
            return Damage::none();
        }
        self.dirty = false;
        let ts = TextStyle::mono(UI_PX);
        let small = TextStyle::mono(STATUS_PX - 1.0);
        cx.canvas.fill_rect(area, palette::BG);
        // 検索欄（入力中の枠は青）
        let (search_area, rest) = area.split_top(SEARCH_H);
        let sr = search_area.inset(4, 1);
        cx.canvas.fill_rect(sr, SEARCH_BG);
        cx.canvas.stroke_rect(sr, palette::STATUS);
        let x = cx.text_in(sr.inset(3, 0), &self.query, &ts, palette::FG, SEARCH_BG);
        cx.canvas.vline(x, sr.y as i32 + 2, sr.h.saturating_sub(4), palette::CURSOR);
        // 見出し
        let (head, list) = rest.split_top(ROW_H);
        cx.text_in(Rect::new(head.x + 4, head.y, head.w.saturating_sub(4), head.h), &self.title, &small, palette::STATUS, palette::BG);
        if self.rows.is_empty() {
            let r = Rect::new(list.x + 8, list.y, list.w.saturating_sub(8), ROW_H);
            cx.text_in(r, &self.empty_text, &ts, palette::DIM, palette::BG);
            return Damage::rect(area);
        }
        // 選んだ行が見えるようにずらす
        let visible = (list.h / ROW_H).max(1) as usize;
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + visible {
            self.scroll = self.sel + 1 - visible;
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(visible));
        let indent_w = cx.fonts.measure("  ", &ts);
        for (i, row) in self.rows.iter().enumerate().skip(self.scroll).take(visible) {
            let r = Rect::new(list.x, list.y + ((i - self.scroll) as u16 * ROW_H) as i16, list.w, ROW_H);
            let bg = if i == self.sel { palette::LIST_FOCUS } else { palette::BG };
            cx.canvas.fill_rect(r, bg);
            let dim = |c: Color| if row.context { c.mix(palette::BG, 112) } else { c };
            // 右端の文字
            let right_x = cx.text_right(Rect::new(r.x, r.y, r.w.saturating_sub(6), r.h), &row.right, &small, palette::DIM, bg);
            let clip = Rect::new(r.x, r.y, (right_x - r.x as i32 - 4).max(0) as u16, r.h);
            let base = r.y as i32 + cx.baseline_in(&ts, r.h);
            let mut x = r.x as i32 + 8 + row.depth as i32 * indent_w;
            x = cx.text(x, base, row.icon, &ts, dim(row.icon_color), bg, clip);
            x = cx.text(x, base, " ", &ts, palette::FG, bg, clip);
            // 名前（一致した区間は黄色）
            let mut at = 0;
            for h in &row.hits {
                let (s, e) = (h.start.min(row.name.len()), h.end.min(row.name.len()));
                if s < at || s >= e {
                    continue;
                }
                x = cx.text(x, base, &row.name[at..s], &ts, dim(palette::FG), bg, clip);
                x = cx.text(x, base, &row.name[s..e], &ts, dim(HIT_YELLOW), bg, clip);
                at = e;
            }
            cx.text(x, base, &row.name[at..], &ts, dim(palette::FG), bg, clip);
        }
        // 下に続きがあれば、右端に細いつまみ
        if self.rows.len() > visible {
            let h = (list.h as usize * visible / self.rows.len()).max(4) as u16;
            let y = list.y as i32 + (list.h - h) as i32 * self.scroll as i32 / (self.rows.len() - visible) as i32;
            cx.canvas.fill_rect(Rect::new((list.right() - 3) as i16, y as i16, 3, h), Color::hex(0x4f4f4f));
        }
        Damage::rect(area)
    }
    fn invalidate(&mut self) {
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::{Canvas, Theme};
    use zc_font::{FontConfig, Fonts, RasterMode};

    fn fonts() -> Option<Fonts> {
        Fonts::load(&FontConfig::default(), RasterMode::Inline).ok()
    }

    #[test]
    fn popup_goes_below_caret_or_above_near_bottom() {
        let Some(mut fonts) = fonts() else { return };
        let (mut canvas, theme) = (Canvas::new(), Theme::dark_plus());
        let mut cx = RenderCx::new(&mut canvas, &mut fonts, &theme);
        let mut p = CompletionPopup::new("Tab 確定");
        let item = |s: &str| PopupItem { tag: "ab", tag_color: palette::STATUS, label: s.into(), hit_chars: 2 };
        p.set(vec![item("led"), item("led_blink")], 0);
        let bounds = Rect::new(0, 13, 320, 146);
        let r = p.place(&mut cx, Rect::new(60, 73, 1, 12), 50, bounds);
        assert_eq!(r.y, 86);
        assert!(r.x >= 0 && r.right() <= 320);
        let r = p.place(&mut cx, Rect::new(60, 145, 1, 12), 50, bounds);
        assert!(r.bottom() <= 144, "{r:?}");
        let d = p.render_at(&mut cx, r, false);
        assert!(!d.is_empty());
        // 変わっていなければ描かない
        assert!(p.render_at(&mut cx, r, false).is_empty());
        assert!(!p.render_at(&mut cx, r, true).is_empty());
    }

    #[test]
    fn picker_scrolls_to_selection() {
        let Some(mut fonts) = fonts() else { return };
        let (mut canvas, theme) = (Canvas::new(), Theme::dark_plus());
        let mut cx = RenderCx::new(&mut canvas, &mut fonts, &theme);
        let mut l = PickerList::new("アウトライン");
        let rows: Vec<PickRow> = (0..40)
            .map(|i| PickRow { depth: 0, icon: "ƒ", icon_color: palette::FUNCTION, name: format!("f{i}"), hits: vec![], right: i.to_string(), context: false })
            .collect();
        l.set("", rows, 30);
        let area = Rect::new(0, 13, 320, 146);
        assert!(!l.render(&mut cx, area).is_empty());
        assert!(l.scroll > 0 && l.scroll <= 30);
        assert!(l.render(&mut cx, area).is_empty());
    }
}
