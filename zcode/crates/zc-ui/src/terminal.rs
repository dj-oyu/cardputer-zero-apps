//! ターミナルのパネル（モック 3・4）。上に見出しの行（`TERMINAL`・いま動かしているもの・
//! 右に案内）、その下に端末の画面（[`TermScreen`]）を 1 マスずつ描く。
//!
//! 描き直すのは、前回描いたものと中身が違う行だけ（端末の dirty の印には頼らず、ここで
//! 比べる。13 行 × 60 桁ほどなので比べるのは安い）。

use crate::layout::{STATUS_PX, UI_PX};
use crate::{RenderCx, Widget};
use zc_core::{palette, Attrs, Color, Damage, Priority, Rect, RowRange};
use zc_font::TextStyle;
use zc_term::Cell;

/// 見出しの行の高さ（モックの `.ph`）。
pub const TERM_HEAD_H: u16 = 11;
/// 端末の 1 行の高さ（モックの `.tl`。既定の字のとき）。
pub const TERM_ROW_H: u16 = 11;
/// 下に開いたときのパネルの高さ（見出し + 6 行 + 上の境界線。モック 3。既定の字のとき）。
pub const TERM_PANEL_H: u16 = 1 + TERM_HEAD_H + 6 * TERM_ROW_H;

/// パネルの上の境界線。
const BORDER: Color = Color::hex(0x444444);

/// 端末の画面の読み口（`zc_term::Term` が実装する。試験では作り物を渡す）。
pub trait TermScreen {
    /// 画面の大きさ（桁, 行）。
    fn size(&self) -> (u16, u16);
    /// 画面の 1 行。
    fn row(&self, row: u16) -> &[Cell];
    /// カーソル（桁, 行）。隠れていれば `None`。
    fn cursor(&self) -> Option<(u16, u16)>;
}

impl TermScreen for zc_term::Term {
    fn size(&self) -> (u16, u16) {
        zc_term::Term::size(self)
    }
    fn row(&self, row: u16) -> &[Cell] {
        zc_term::Term::row(self, row)
    }
    fn cursor(&self) -> Option<(u16, u16)> {
        zc_term::Term::cursor(self)
    }
}

/// 描いた 1 行の印（中身・カーソルの桁）。
#[derive(Clone, PartialEq, Eq)]
struct PaintedRow {
    cells: Vec<Cell>,
    caret: Option<u16>,
    /// この行のカーソルの位置に重ねた未確定（SKK）。
    preedit: Option<(u16, String, bool)>,
}

/// ターミナルのパネル。
pub struct TerminalPanel {
    /// 見出しの真ん中（実行中のコマンド・「全画面」など）。
    title: String,
    /// 見出しの右（「Ctrl+C 停止」など）。
    hint: String,
    head_dirty: bool,
    painted: Vec<Option<PaintedRow>>,
    last_area: Rect,
    focused: bool,
    cursor_on: bool,
    /// SKK の未確定（▽ / ▼ の記号込み）と、変換中（▼）か。カーソルの位置に重ねて描く。
    preedit: Option<(String, bool)>,
    /// 前回描いた未確定の始まり（候補一覧を置く位置）。
    preedit_rect: Option<Rect>,
    /// 端末の字（既定は `UI_PX` の等幅）と 1 行の高さ。
    font: TextStyle,
    row_h: u16,
}

impl Default for TerminalPanel {
    fn default() -> Self {
        TerminalPanel::new()
    }
}

impl TerminalPanel {
    pub fn new() -> TerminalPanel {
        TerminalPanel {
            title: String::new(),
            hint: String::new(),
            head_dirty: true,
            painted: Vec::new(),
            last_area: Rect::default(),
            focused: true,
            cursor_on: true,
            preedit: None,
            preedit_rect: None,
            font: TextStyle::mono(UI_PX),
            row_h: TERM_ROW_H,
        }
    }

    /// 端末の字の大きさと 1 行の高さを替える（コードと同じ字にするとき）。
    pub fn set_font(&mut self, px: f32, row_h: u16) {
        self.font = TextStyle::mono(px);
        self.row_h = row_h.max(1);
        self.painted.clear();
        self.head_dirty = true;
    }
    /// 見出しの行（と上の境界線）だけ描き直させる（フォーカスの縁取りを消すとき）。
    pub fn invalidate_head(&mut self) {
        self.head_dirty = true;
    }
    /// 下に開いたときのパネルの高さ。端末の部分は既定の 6 行ぶん（66px）に入るだけの行数
    /// （行が 13px なら 5 行）にして、エディタの残りが字によって大きく変わらないようにする。
    pub fn panel_h(&self) -> u16 {
        1 + TERM_HEAD_H + (6 * TERM_ROW_H / self.row_h).max(1) * self.row_h
    }

    /// 見出しの文字。
    pub fn set_header(&mut self, title: &str, hint: &str) {
        if self.title != title || self.hint != hint {
            self.title = title.into();
            self.hint = hint.into();
            self.head_dirty = true;
        }
    }
    /// 入力がパネルに向いているか（向いていなければカーソルを描かない）。
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }
    /// カーソルの点滅の点いている側か。
    pub fn set_cursor_on(&mut self, on: bool) {
        self.cursor_on = on;
    }
    /// SKK の未確定（モック 7・8 と同じ見た目で、端末のカーソルの位置に重ねる。端末には送らない）。
    pub fn set_preedit(&mut self, preedit: Option<(String, bool)>) {
        self.preedit = preedit;
    }
    /// 前回描いた未確定の始まり（幅 1）。候補一覧はここに合わせて置く。
    pub fn preedit_rect(&self) -> Option<Rect> {
        self.preedit_rect
    }

    /// `area`（パネル全体）に入る端末の大きさ（桁, 行）。
    pub fn grid_size(&self, cx: &mut RenderCx<'_>, area: Rect) -> (u16, u16) {
        let cell = cx.metrics(&self.font).cell_w.max(1);
        let body_h = area.h.saturating_sub(1 + TERM_HEAD_H);
        let cols = area.w.saturating_sub(4) / cell;
        ((cols).max(1), (body_h / self.row_h).max(1))
    }

    /// 端末の画面を描く。描き変えた行範囲を返す。
    // 行番号 r で端末の行と描いた印の両方を引く（iter にすると借用が分かれて読みにくい）
    #[allow(clippy::needless_range_loop)]
    pub fn render_term(&mut self, cx: &mut RenderCx<'_>, area: Rect, term: &dyn TermScreen) -> Damage {
        if area.is_empty() {
            return Damage::none();
        }
        if area != self.last_area {
            self.last_area = area;
            self.painted.clear();
            self.head_dirty = true;
        }
        let mut damage = Damage::none();
        let ts = self.font;
        // 上の境界線と見出し
        if std::mem::take(&mut self.head_dirty) {
            cx.canvas.hline(area.x as i32, area.y as i32, area.w, BORDER);
            let head = Rect::new(area.x, area.y + 1, area.w, TERM_HEAD_H);
            cx.canvas.fill_rect(head, palette::BG);
            let small = TextStyle::mono(STATUS_PX - 1.0);
            let label = Rect::new(head.x + 4, head.y, head.w.saturating_sub(8), head.h);
            let x = cx.text_in(label, "TERMINAL", &small, Color::WHITE, palette::BG);
            cx.canvas.hline(label.x as i32, head.bottom() - 2, (x - label.x as i32).max(0) as u16, Color::WHITE);
            let right = cx.text_right(label, &self.hint, &small, palette::DIM, palette::BG);
            let mid = Rect::new((x + 8) as i16, head.y, (right - x - 12).max(0) as u16, head.h);
            cx.text_in(mid, &self.title, &small, palette::DIM, palette::BG);
            damage.add(RowRange::new(area.y as u16, head.bottom() as u16), Priority::Normal);
        }
        // 端末の画面
        let body = Rect::new(area.x, area.y + 1 + TERM_HEAD_H as i16, area.w, area.h.saturating_sub(1 + TERM_HEAD_H));
        let cell_w = cx.metrics(&ts).cell_w.max(1) as i32;
        let (cols, rows) = term.size();
        let at = if self.focused { term.cursor() } else { None };
        let cursor = at.filter(|_| self.cursor_on);
        self.preedit_rect = None;
        let visible = (body.h / self.row_h) as usize;
        if self.painted.len() != visible {
            self.painted = vec![None; visible];
        }
        for r in 0..visible {
            let y = body.y + (r as u16 * self.row_h) as i16;
            let row_rect = Rect::new(body.x, y, body.w, self.row_h);
            let cells: &[Cell] = if (r as u16) < rows { term.row(r as u16) } else { &[] };
            let caret = cursor.filter(|c| c.1 as usize == r).map(|c| c.0);
            let preedit = match (at, &self.preedit) {
                (Some((c, row)), Some((s, conv))) if row as usize == r => Some((c, s.clone(), *conv)),
                _ => None,
            };
            let now = PaintedRow { cells: cells[..cells.len().min(cols as usize)].to_vec(), caret, preedit };
            // 未確定を置く x と幅。右端からはみ出すなら、右端に収まるまで左へずらす
            // （カーソルが行末に近いとき。左端より左へは出さない）
            let pre_at = now.preedit.as_ref().map(|(c, s, _)| {
                let x = body.x as i32 + 4 + *c as i32 * cell_w;
                let w = cx.fonts.measure(s, &ts);
                let right = row_rect.right() - 2;
                let x = if x + w > right { (right - w).max(body.x as i32 + 4) } else { x };
                (x, w)
            });
            if let Some((x, _)) = pre_at {
                self.preedit_rect = Some(Rect::new(x as i16, y, 1, self.row_h));
            }
            if self.painted[r].as_ref() == Some(&now) {
                continue;
            }
            cx.canvas.fill_rect(row_rect, palette::BG);
            let base = y as i32 + cx.baseline_in(&ts, self.row_h);
            let x0 = body.x as i32 + 4;
            let mut buf = [0u8; 4];
            let mut skip = false;
            for (c, cell) in now.cells.iter().enumerate() {
                // 全角の右半分（`zc-term` は同じ書式の空白を置く）は、左半分で描いてある
                if std::mem::take(&mut skip) {
                    continue;
                }
                skip = cell.wide;
                let x = x0 + c as i32 * cell_w;
                let w = if cell.wide { 2 * cell_w } else { cell_w };
                let (mut fg, mut bg) = (cell.style.fg, cell.style.bg_or(palette::BG));
                if cell.style.attrs.contains(Attrs::REVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let cell_rect = Rect::new(x as i16, y, w as u16, self.row_h).intersect(&row_rect);
                if bg != palette::BG {
                    cx.canvas.fill_rect(cell_rect, bg);
                }
                if cell.ch != ' ' && cell.ch != '\0' {
                    let style = if cell.style.attrs.contains(Attrs::BOLD) { ts.bold() } else { ts };
                    cx.text(x, base, cell.ch.encode_utf8(&mut buf), &style, fg, bg, cell_rect);
                }
                if cell.style.attrs.contains(Attrs::UNDERLINE) {
                    cx.canvas.hline(x, y as i32 + self.row_h as i32 - 2, w as u16, fg);
                }
            }
            // SKK の未確定はカーソルの位置に重ね、カーソルはその後ろ（モック 7・8 と同じ見た目）
            let mut caret_x = caret.map(|c| x0 + c as i32 * cell_w);
            if let (Some((_, s, conv)), Some((x, w))) = (&now.preedit, pre_at) {
                let r = Rect::new(x as i16, y, w.max(0) as u16, self.row_h).intersect(&row_rect);
                let bg = if *conv { palette::SELECTION } else { palette::BG };
                cx.canvas.fill_rect(r, bg);
                cx.text(x, base, s, &ts, Color::WHITE, bg, row_rect);
                cx.canvas.hline(x, y as i32 + self.row_h as i32 - 1, r.w, Color::WHITE);
                caret_x = caret_x.map(|_| x + w);
            }
            if let Some(x) = caret_x {
                cx.canvas.fill_rect(Rect::new(x as i16, y + 1, 1, self.row_h - 2).intersect(&row_rect), palette::CURSOR);
            }
            damage.add(row_rect.rows(), Priority::Edit);
            self.painted[r] = Some(now);
        }
        // 行に割り切れない余り
        let rest_y = body.y as i32 + (visible as u16 * self.row_h) as i32;
        if rest_y < body.bottom() && !damage.is_empty() {
            cx.canvas.fill_rect(Rect::new(body.x, rest_y as i16, body.w, (body.bottom() - rest_y) as u16), palette::BG);
        }
        damage
    }

    /// 端末が無いとき（シェルが終わった後など）に、空のパネルを描く。
    pub fn render_empty(&mut self, cx: &mut RenderCx<'_>, area: Rect, text: &str) -> Damage {
        struct Empty;
        impl TermScreen for Empty {
            fn size(&self) -> (u16, u16) {
                (0, 0)
            }
            fn row(&self, _row: u16) -> &[Cell] {
                &[]
            }
            fn cursor(&self) -> Option<(u16, u16)> {
                None
            }
        }
        let mut d = self.render_term(cx, area, &Empty);
        if !d.is_empty() {
            let ts = self.font;
            let r = Rect::new(area.x + 4, area.y + 1 + TERM_HEAD_H as i16, area.w.saturating_sub(8), self.row_h);
            cx.text_in(r, text, &ts, palette::DIM, palette::BG);
            d.add(r.rows(), Priority::Edit);
        }
        d
    }
}

impl Widget for TerminalPanel {
    /// 端末を持たない描き方（見出しだけ）。ふつうは [`TerminalPanel::render_term`] を使う。
    fn render(&mut self, cx: &mut RenderCx<'_>, area: Rect) -> Damage {
        self.render_empty(cx, area, "")
    }
    fn invalidate(&mut self) {
        self.painted.clear();
        self.head_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::{Canvas, Style, Theme};
    use zc_font::{FontConfig, Fonts, RasterMode};

    struct Fake {
        rows: Vec<Vec<Cell>>,
        cursor: Option<(u16, u16)>,
    }
    impl TermScreen for Fake {
        fn size(&self) -> (u16, u16) {
            (60, self.rows.len() as u16)
        }
        fn row(&self, row: u16) -> &[Cell] {
            &self.rows[row as usize]
        }
        fn cursor(&self) -> Option<(u16, u16)> {
            self.cursor
        }
    }
    fn line(s: &str) -> Vec<Cell> {
        s.chars().map(|ch| Cell { ch, style: Style::new(palette::FG), wide: false }).collect()
    }

    #[test]
    fn only_changed_rows_are_repainted() {
        let Ok(mut fonts) = Fonts::load(&FontConfig::default(), RasterMode::Inline) else { return };
        let (mut canvas, theme) = (Canvas::new(), Theme::dark_plus());
        let mut cx = RenderCx::new(&mut canvas, &mut fonts, &theme);
        let area = Rect::new(0, 80, 320, TERM_PANEL_H);
        let (cols, rows) = TerminalPanel::new().grid_size(&mut cx, area);
        assert_eq!(rows, 6);
        assert!(cols >= 50, "{cols}");
        let mut p = TerminalPanel::new();
        p.set_header("python3 main.py", "Ctrl+C 停止");
        let mut t = Fake { rows: (0..6).map(|i| line(&format!("line {i}"))).collect(), cursor: Some((6, 5)) };
        let d = p.render_term(&mut cx, area, &t);
        assert_eq!(d.bounds(), RowRange::new(80, 80 + TERM_PANEL_H));
        assert!(p.render_term(&mut cx, area, &t).is_empty());
        // 2 行目だけ変える
        t.rows[2] = line("changed");
        let d = p.render_term(&mut cx, area, &t);
        let y = 80 + 1 + TERM_HEAD_H + 2 * TERM_ROW_H;
        assert_eq!(d.bounds(), RowRange::new(y, y + TERM_ROW_H));
        // カーソルの移動は、前の行と新しい行
        t.cursor = Some((0, 0));
        let d = p.render_term(&mut cx, area, &t);
        assert_eq!(d.iter().count(), 2);
    }

    #[test]
    fn preedit_near_the_right_edge_shifts_left() {
        let Ok(mut fonts) = Fonts::load(&FontConfig::default(), RasterMode::Inline) else { return };
        let (mut canvas, theme) = (Canvas::new(), Theme::dark_plus());
        let mut cx = RenderCx::new(&mut canvas, &mut fonts, &theme);
        let area = Rect::new(0, 80, 320, TERM_PANEL_H);
        let (cols, _) = TerminalPanel::new().grid_size(&mut cx, area);
        let mut p = TerminalPanel::new();
        // カーソルが行末の 2 桁手前。長い未確定はそのまま置くと右端を越える
        let t = Fake { rows: (0..6).map(|_| line("$")).collect(), cursor: Some((cols - 2, 0)) };
        p.set_preedit(Some(("▼かーどぴゅーたー".into(), true)));
        p.render_term(&mut cx, area, &t);
        let r = p.preedit_rect().unwrap();
        let w = cx.fonts.measure("▼かーどぴゅーたー", &TextStyle::mono(UI_PX));
        assert!(r.x as i32 + w <= area.right() - 2, "{r:?} + {w}");
        // 短い未確定はカーソルの位置のまま
        let t = Fake { rows: (0..6).map(|_| line("$")).collect(), cursor: Some((2, 0)) };
        p.set_preedit(Some(("▽か".into(), false)));
        p.render_term(&mut cx, area, &t);
        let cell = cx.metrics(&TextStyle::mono(UI_PX)).cell_w as i32;
        assert_eq!(p.preedit_rect().unwrap().x as i32, 4 + 2 * cell);
    }
}