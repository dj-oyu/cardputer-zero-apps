//! SKK の画面部品: 候補一覧（[`CandidatePopup`]、モック 8）と、ステータスバーの上に開く
//! 1 行の入力欄（[`MiniBuffer`]、モック 9 の辞書登録）。`zc-skk` の型は知らない
//! （文字列と状態で受け取る）。

use crate::layout::STATUS_PX;
use crate::{RenderCx, Widget};
use zc_core::{palette, Color, Damage, Priority, Rect, RowRange};
use zc_font::TextStyle;

/// 小窓の地の色・枠の色（モック 8 の `.cand`）。
const POPUP_BG: Color = Color::hex(0x252526);
const POPUP_BORDER: Color = Color::hex(0x454545);
const FOOT_LINE: Color = Color::hex(0x3c3c3c);
/// 候補の 1 行の高さ。
const ROW_H: u16 = 11;
/// 下の案内の高さ。
const FOOT_H: u16 = 10;
/// 入力欄の高さ（モック 9 の `.mini`）。
pub const MINI_H: u16 = 13;

/// 候補一覧の 1 行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateRow {
    /// 選ぶキー（`a` `s` `d` …）。
    pub key: char,
    pub text: String,
    /// 注釈（`〔めいめつ〕` の形で薄く出す）。
    pub annotation: Option<String>,
}

/// SKK の候補一覧（カーソルの下に出る小窓）。位置がカーソルで決まるので `Widget` は
/// 実装しない（[`CandidatePopup::place`] と [`CandidatePopup::render_at`]）。
#[derive(Default)]
pub struct CandidatePopup {
    rows: Vec<CandidateRow>,
    selected: usize,
    /// 案内の右端（`1/3` のようなページ）。
    page: String,
    dirty: bool,
    drawn: Option<Rect>,
}

impl CandidatePopup {
    pub fn new() -> CandidatePopup {
        CandidatePopup { dirty: true, ..Default::default() }
    }
    /// 中身。変わったときだけ描き直す。
    pub fn set(&mut self, rows: Vec<CandidateRow>, selected: usize, page: &str) {
        if self.rows != rows || self.selected != selected || self.page != page {
            self.rows = rows;
            self.selected = selected;
            self.page = page.into();
            self.dirty = true;
        }
    }
    /// 前回描いた矩形（閉じたとき・動いたときに、下を描き直してもらう範囲）。
    pub fn drawn(&self) -> Option<Rect> {
        self.drawn
    }
    /// 描いたものを忘れる（下を描き直したあと）。
    pub fn forget(&mut self) {
        self.drawn = None;
        self.dirty = true;
    }
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn footer(&self) -> String {
        format!("Space 次  x 前  Enter 確定  {}", self.page)
    }

    /// 置く矩形。`anchor` は変換中の語（▼）の先頭の画面上の矩形、`bounds` は出してよい範囲。
    /// 下に入らなければ上に出す。
    pub fn place(&self, cx: &mut RenderCx<'_>, anchor: Rect, bounds: Rect) -> Rect {
        let (ts, small) = (TextStyle::sans(STATUS_PX), TextStyle::mono(STATUS_PX - 1.0));
        let key_w = cx.fonts.measure("a", &small) + 4;
        let row_w = self
            .rows
            .iter()
            .map(|r| cx.fonts.measure(&r.text, &ts) + r.annotation.as_ref().map_or(0, |a| cx.fonts.measure(&format!(" 〔{a}〕"), &small)))
            .max()
            .unwrap_or(0);
        let foot_w = cx.fonts.measure(&self.footer(), &TextStyle::sans(STATUS_PX - 1.5)) + 8;
        let w = (2 + key_w + row_w + 8).max(foot_w).min(bounds.w as i32) as u16;
        let h = self.rows.len() as u16 * ROW_H + FOOT_H + 2;
        let x = (anchor.x as i32).clamp(bounds.x as i32, (bounds.right() - w as i32).max(bounds.x as i32));
        let below = anchor.bottom() + 1;
        let y = if below + h as i32 <= bounds.bottom() { below } else { (anchor.y as i32 - h as i32 - 1).max(bounds.y as i32) };
        Rect::new(x as i16, y as i16, w, h.min(bounds.h))
    }

    /// `rect` に描く。`force` なら変わっていなくても描く（下の行が描き直されたとき）。
    pub fn render_at(&mut self, cx: &mut RenderCx<'_>, rect: Rect, force: bool) -> Damage {
        if !force && !self.dirty && self.drawn == Some(rect) {
            return Damage::none();
        }
        self.dirty = false;
        self.drawn = Some(rect);
        let (ts, small) = (TextStyle::sans(STATUS_PX), TextStyle::mono(STATUS_PX - 1.0));
        cx.canvas.fill_rect(rect, POPUP_BG);
        cx.canvas.stroke_rect(rect, POPUP_BORDER);
        let inner = rect.inset(1, 1);
        let key_w = cx.fonts.measure("a", &small) + 4;
        let mut buf = [0u8; 4];
        for (i, r) in self.rows.iter().enumerate() {
            let row = Rect::new(inner.x, inner.y + (i as u16 * ROW_H) as i16, inner.w, ROW_H).intersect(&inner);
            if row.is_empty() {
                break;
            }
            let on = i == self.selected;
            let bg = if on { palette::LIST_FOCUS } else { POPUP_BG };
            cx.canvas.fill_rect(row, bg);
            cx.text_in(Rect::new(row.x + 2, row.y, key_w as u16, row.h), r.key.encode_utf8(&mut buf), &small, palette::STATUS, bg);
            let text_area = Rect::new(row.x + 2 + key_w as i16, row.y, row.w.saturating_sub(2 + key_w as u16), row.h);
            let x = cx.text_in(text_area, &r.text, &ts, if on { Color::WHITE } else { palette::FG }, bg);
            if let Some(a) = &r.annotation {
                let rest = Rect::new(x as i16, row.y, (text_area.right() - x).max(0) as u16, row.h);
                cx.text_in(rest, &format!(" 〔{a}〕"), &small, palette::DIM, bg);
            }
        }
        let foot_y = inner.y as i32 + self.rows.len() as i32 * ROW_H as i32;
        if foot_y < inner.bottom() {
            cx.canvas.hline(inner.x as i32, foot_y, inner.w, FOOT_LINE);
            let foot = Rect::new(inner.x + 4, (foot_y + 1) as i16, inner.w.saturating_sub(4), FOOT_H - 1).intersect(&inner);
            cx.text_in(foot, &self.footer(), &TextStyle::sans(STATUS_PX - 1.5), palette::DIM, POPUP_BG);
        }
        Damage::range(rect.rows(), Priority::Cursor)
    }
}

/// ステータスバーの上に開く 1 行の入力欄（辞書登録。後で検索・行へ移動もここへ寄せる）。
#[derive(Default)]
pub struct MiniBuffer {
    label: String,
    text: String,
    /// 入力欄の中の未確定（▽ / ▼ の記号込み）と、変換中（▼）か。
    preedit: Option<(String, bool)>,
    dirty: bool,
    last_area: Rect,
}

impl MiniBuffer {
    pub fn new() -> MiniBuffer {
        MiniBuffer { dirty: true, ..Default::default() }
    }
    /// 中身。変わったときだけ描き直す。
    pub fn set(&mut self, label: &str, text: &str, preedit: Option<(&str, bool)>) {
        let preedit = preedit.map(|(s, c)| (s.to_string(), c));
        if self.label != label || self.text != text || self.preedit != preedit {
            self.label = label.into();
            self.text = text.into();
            self.preedit = preedit;
            self.dirty = true;
        }
    }
}

impl Widget for MiniBuffer {
    fn render(&mut self, cx: &mut RenderCx<'_>, area: Rect) -> Damage {
        if area.is_empty() || (!self.dirty && area == self.last_area) {
            return Damage::none();
        }
        self.dirty = false;
        self.last_area = area;
        let ts = TextStyle::sans(STATUS_PX);
        cx.canvas.fill_rect(area, POPUP_BG);
        cx.canvas.hline(area.x as i32, area.y as i32, area.w, palette::STATUS);
        let line = Rect::new(area.x + 4, area.y + 1, area.w.saturating_sub(8), area.h.saturating_sub(1));
        let mut x = cx.text_in(line, &self.label, &ts, palette::STATUS, POPUP_BG);
        let rest = |x: i32| Rect::new(x as i16, line.y, (line.right() - x).max(0) as u16, line.h);
        x = cx.text_in(rest(x), &self.text, &ts, palette::FG, POPUP_BG);
        if let Some((p, converting)) = &self.preedit {
            let (fg, bg) = if *converting { (Color::WHITE, palette::SELECTION) } else { (Color::WHITE, POPUP_BG) };
            let w = cx.fonts.measure(p, &ts);
            let r = rest(x);
            let pr = Rect::new(r.x, r.y, (w as u16).min(r.w), r.h);
            if *converting {
                cx.canvas.fill_rect(pr, bg);
            }
            let x1 = cx.text_in(pr, p, &ts, fg, bg);
            cx.canvas.hline(x, line.bottom() - 2, (x1 - x).max(0) as u16, Color::WHITE);
            x = x1;
        }
        // カーソル
        cx.canvas.fill_rect(Rect::new(x as i16, line.y + 1, 1, line.h.saturating_sub(3)).intersect(&line), palette::CURSOR);
        Damage::range(RowRange::new(area.y as u16, area.bottom() as u16), Priority::Cursor)
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

    #[test]
    fn candidate_popup_places_and_redraws_only_on_change() {
        let Ok(mut fonts) = Fonts::load(&FontConfig::default(), RasterMode::Inline) else { return };
        let (mut canvas, theme) = (Canvas::new(), Theme::dark_plus());
        let mut cx = RenderCx::new(&mut canvas, &mut fonts, &theme);
        let mut p = CandidatePopup::new();
        let row = |k, t: &str, a: Option<&str>| CandidateRow { key: k, text: t.into(), annotation: a.map(Into::into) };
        p.set(vec![row('a', "点滅", None), row('s', "明滅", Some("めいめつ")), row('d', "天滅", None)], 0, "1/1");
        let bounds = Rect::new(0, 13, 320, 146);
        let r = p.place(&mut cx, Rect::new(98, 61, 30, 12), bounds);
        assert_eq!(r.y, 74);
        assert_eq!(r.h, 3 * ROW_H + FOOT_H + 2);
        assert!(!p.render_at(&mut cx, r, false).is_empty());
        assert!(p.render_at(&mut cx, r, false).is_empty());
        p.set(vec![row('a', "点滅", None)], 0, "1/1");
        assert!(!p.render_at(&mut cx, r, false).is_empty());
        // 下に入らなければ上に出す
        let r = p.place(&mut cx, Rect::new(98, 150, 30, 12), bounds);
        assert!(r.bottom() <= 150, "{r:?}");
    }

    #[test]
    fn mini_buffer_redraws_only_on_change() {
        let Ok(mut fonts) = Fonts::load(&FontConfig::default(), RasterMode::Inline) else { return };
        let (mut canvas, theme) = (Canvas::new(), Theme::dark_plus());
        let mut cx = RenderCx::new(&mut canvas, &mut fonts, &theme);
        let mut m = MiniBuffer::new();
        let area = Rect::new(0, 146, 320, MINI_H);
        m.set("[辞書登録] かーどぴゅーたー：", "カード", Some(("▽ぴゅーた", false)));
        assert_eq!(m.render(&mut cx, area).bounds(), RowRange::new(146, 159));
        assert!(m.render(&mut cx, area).is_empty());
        m.set("[辞書登録] かーどぴゅーたー：", "カードピューター", None);
        assert!(!m.render(&mut cx, area).is_empty());
    }
}
