//! 論理行 → 表示行のレイアウト（design.md §5.5）。
//!
//! エディタの表示は等幅のマス目を前提にしない。論理行 1 つを [`LineLayout`]
//! （表示行の並び。表示行ごとに高さとベースライン、文字ごとの x 座標）に直してから
//! 描く。コード表示（[`CodeLayouter`]）は「全部の行が同じ高さ・同じ字幅・折り返し
//! なし」という特別な場合で、同じ形で扱う。
//!
//! 段階 5 の Markdown ライブプレビューは、[`LineLayouter`] をもう 1 つ実装するだけで
//! 入る（見出しは高い表示行、本文はプロポーショナルの x 座標、折り返しは表示行を
//! 増やす）。[`crate::EditorView`] の縦スクロール・カーソル位置の計算・差分描画は
//! どれも `LineLayout` だけを見ている。
//!
//! 位置の単位: 桁（`col`）は論理行の中の **文字数**（`zc_core::Pos::col` と同じ）。
//! x は行の本文の左端からの画素。y は論理行の上端からの画素。

use core::ops::Range;
use zc_core::{Attrs, Span, Style, Theme, TokenKind};
use zc_font::{Fonts, TextStyle};

/// 表示行の中の、同じ書体・同じ見た目が続く区間。
#[derive(Clone, Debug, PartialEq)]
pub struct VisualRun {
    /// 論理行の中の文字位置の範囲。
    pub chars: Range<u32>,
    pub font: TextStyle,
    pub style: Style,
}

/// 表示行 1 つ。
#[derive(Clone, Debug, PartialEq)]
pub struct VisualRow {
    /// この表示行に載る文字（論理行の中の文字位置）。
    pub chars: Range<u32>,
    /// 論理行の上端から、この表示行の上端まで。
    pub y: u16,
    pub height: u16,
    /// 表示行の上端からベースラインまで。
    pub baseline: u16,
    /// `LineLayout::xs` の中の、この行の分（`chars.len() + 1` 個）。
    xs: Range<u32>,
    /// `LineLayout::runs` の中の、この行の分。
    runs: Range<u32>,
}

/// カーソル（文字の境目）の表示位置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caret {
    /// 何番目の表示行か。
    pub row: u32,
    /// 本文の左端からの x。
    pub x: i32,
    /// 論理行の上端から、表示行の上端まで。
    pub y: u16,
    /// 表示行の高さ。
    pub height: u16,
}

/// 論理行 1 つぶんのレイアウト結果。
///
/// 作るときは [`LineLayout::clear`] → （[`LineLayout::start_row`] →
/// [`LineLayout::push`] を文字の数だけ）を表示行の数だけ → [`LineLayout::finish`]。
/// 空の論理行も表示行を 1 つ持つ（カーソルを置く場所が要る）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LineLayout {
    rows: Vec<VisualRow>,
    runs: Vec<VisualRun>,
    /// 文字の境目の x。表示行ごとに `文字数 + 1` 個（折り返しの境目は、前の行の
    /// 右端と次の行の左端の 2 つの x を持つ）。
    xs: Vec<i32>,
    height: u16,
    width: i32,
    chars: u32,
}

impl LineLayout {
    pub fn new() -> LineLayout {
        LineLayout::default()
    }
    /// 空にする（確保した領域は使い回す）。
    pub fn clear(&mut self) {
        self.rows.clear();
        self.runs.clear();
        self.xs.clear();
        self.height = 0;
        self.width = 0;
        self.chars = 0;
    }
    /// 次の文字から新しい表示行を始める。`x0` は最初の文字の左端。
    pub fn start_row(&mut self, height: u16, baseline: u16, x0: i32) {
        let xi = self.xs.len() as u32;
        let ri = self.runs.len() as u32;
        self.xs.push(x0);
        self.rows.push(VisualRow {
            chars: self.chars..self.chars,
            y: self.height,
            height,
            baseline,
            xs: xi..xi + 1,
            runs: ri..ri,
        });
        self.height += height;
        self.width = self.width.max(x0);
    }
    /// いまの表示行に 1 文字足す。書体と見た目が直前と同じなら、同じ区間にまとめる。
    pub fn push(&mut self, advance: u16, font: TextStyle, style: Style) {
        debug_assert!(!self.rows.is_empty(), "start_row を先に呼ぶ");
        let row = self.rows.last_mut().expect("start_row を先に呼ぶ");
        let x = *self.xs.last().expect("xs は空でない") + advance as i32;
        self.xs.push(x);
        row.xs.end += 1;
        let c = self.chars;
        row.chars.end = c + 1;
        match self.runs.last_mut() {
            Some(r) if row.runs.end > row.runs.start && r.font == font && r.style == style => r.chars.end = c + 1,
            _ => {
                self.runs.push(VisualRun { chars: c..c + 1, font, style });
                row.runs.end += 1;
            }
        }
        self.chars = c + 1;
        self.width = self.width.max(x);
    }
    /// 作り終える。表示行が 1 つも無ければ、高さ `height` の空の行を 1 つ作る。
    pub fn finish(&mut self, height: u16, baseline: u16) {
        if self.rows.is_empty() {
            self.start_row(height, baseline, 0);
        }
    }

    /// 論理行全体の高さ。
    pub fn height(&self) -> u16 {
        self.height
    }
    /// いちばん右の x。
    pub fn width(&self) -> i32 {
        self.width
    }
    /// 文字数。
    pub fn len_chars(&self) -> u32 {
        self.chars
    }
    pub fn rows(&self) -> &[VisualRow] {
        &self.rows
    }
    /// 表示行 `row` の区間。
    pub fn runs(&self, row: &VisualRow) -> &[VisualRun] {
        &self.runs[row.runs.start as usize..row.runs.end as usize]
    }
    /// 表示行 `row` の、文字の境目の x（`文字数 + 1` 個）。
    pub fn xs(&self, row: &VisualRow) -> &[i32] {
        &self.xs[row.xs.start as usize..row.xs.end as usize]
    }

    /// 桁 `col` の境目が載る表示行。折り返しの境目は次の行の先頭に置く。
    /// 行末より後ろの桁は行末に丸める。
    pub fn row_of(&self, col: u32) -> u32 {
        let col = col.min(self.chars);
        match self.rows.iter().rposition(|r| r.chars.start <= col) {
            Some(i) => i as u32,
            None => 0,
        }
    }
    /// 桁 `col` のカーソルの位置。
    pub fn caret(&self, col: u32) -> Caret {
        let col = col.min(self.chars);
        let i = self.row_of(col);
        let Some(row) = self.rows.get(i as usize) else {
            return Caret { row: 0, x: 0, y: 0, height: self.height };
        };
        let x = self.xs(row)[(col - row.chars.start) as usize];
        Caret { row: i, x, y: row.y, height: row.height }
    }
    /// 表示行 `row` の中で、x にいちばん近い境目の桁（文字の中央より右なら次の桁）。
    pub fn col_at(&self, row: u32, x: i32) -> u32 {
        let Some(r) = self.rows.get(row as usize) else { return 0 };
        let xs = self.xs(r);
        // xs は単調に増える: x を超えない最後の境目
        let i = xs.partition_point(|&b| b <= x);
        if i == 0 {
            return r.chars.start;
        }
        if i >= xs.len() {
            return r.chars.end;
        }
        let (a, b) = (xs[i - 1], xs[i]);
        r.chars.start + if x - a >= (b - a + 1) / 2 { i as u32 } else { i as u32 - 1 }
    }
    /// 論理行の上端からの y が載る表示行（範囲外は端の行）。
    pub fn row_at_y(&self, y: i32) -> u32 {
        match self.rows.iter().position(|r| y < r.y as i32 + r.height as i32) {
            Some(i) => i as u32,
            None => self.rows.len().saturating_sub(1) as u32,
        }
    }
}

/// レイアウトに渡す、論理行 1 つぶんの材料。
pub struct LineInput<'a> {
    /// 0 始まりの行番号。
    pub line: u32,
    /// 行の文字列（改行は含まない）。
    pub text: &'a str,
    /// トークン（`text` のバイト位置。無ければ空 = 色なし）。
    pub spans: &'a [Span],
    /// カーソルのある行か（Markdown は、この行だけ記号を見せる）。
    pub cursor_line: bool,
    /// 本文の幅（折り返すレイアウトが使う）。
    pub wrap_width: i32,
}

/// 論理行を表示行に直すもの。コード表示は [`CodeLayouter`]、Markdown のライブ
/// プレビュー（段階 5）は `zc-markdown` の結果から作る別の実装。
pub trait LineLayouter {
    /// `input` の行を `out` に組む（`out` は空にしてから使う）。
    fn layout(&mut self, fonts: &mut Fonts, theme: &Theme, input: &LineInput<'_>, out: &mut LineLayout);
    /// 全部の論理行が同じ高さなら、その高さ。`None` なら行ごとに違う
    /// （ビューは、画面の下端にかかる行を途中まで描く）。
    fn uniform_height(&self) -> Option<u16>;
    /// 行番号の欄を出すか。
    fn gutter(&self) -> bool {
        true
    }
    /// 折り返すか（折り返すなら横スクロールしない）。
    fn wraps(&self) -> bool {
        false
    }
    /// カーソルのある行かどうかでレイアウトが変わるか（変わるなら、カーソルが
    /// 動いたときに前後の行を組み直す）。
    fn depends_on_cursor(&self) -> bool {
        false
    }
    /// 横スクロールの余白と、選択範囲が改行にかかるときに行末に足す幅の目安
    /// （半角 1 字ぶん）。
    fn cell_width(&mut self, fonts: &mut Fonts) -> u16;
}

/// コード表示: 等幅・折り返しなし・全行同じ高さ。全角は半角 2 つぶん、タブは
/// 次のタブ位置まで。
#[derive(Clone, Debug)]
pub struct CodeLayouter {
    font: TextStyle,
    line_h: u16,
    tab_width: u32,
    /// `(字幅, ベースライン)`。最初に使うときにフォントから取る。
    metrics: Option<(u16, u16)>,
}

impl CodeLayouter {
    /// `px` 画素の等幅、行の高さ `line_h`。
    pub fn new(px: f32, line_h: u16) -> CodeLayouter {
        CodeLayouter { font: TextStyle::mono(px), line_h, tab_width: 4, metrics: None }
    }
    /// 既定の大きさ（[`crate::layout::CODE_PX`] / [`crate::layout::CODE_LINE_H`]）。
    pub fn standard() -> CodeLayouter {
        CodeLayouter::new(crate::layout::CODE_PX, crate::layout::CODE_LINE_H)
    }
    /// タブの幅（半角の数）。
    pub fn with_tab_width(mut self, n: u32) -> CodeLayouter {
        self.tab_width = n.max(1);
        self
    }
    pub fn font(&self) -> TextStyle {
        self.font
    }
    fn metrics(&mut self, fonts: &mut Fonts) -> (u16, u16) {
        if let Some(m) = self.metrics {
            return m;
        }
        let m = fonts.metrics(&self.font);
        let base = ((self.line_h as i32 - m.height() as i32).div_euclid(2) + m.ascent as i32).max(0) as u16;
        let v = (m.cell_w.max(1), base);
        self.metrics = Some(v);
        v
    }
}

/// トークンの見た目から、字形を選ぶ条件を作る（太字・斜体）。
fn font_for(base: TextStyle, style: &Style) -> TextStyle {
    let mut f = base;
    f.bold = style.attrs.contains(Attrs::BOLD);
    f.italic = style.attrs.contains(Attrs::ITALIC);
    f
}

impl LineLayouter for CodeLayouter {
    fn layout(&mut self, fonts: &mut Fonts, theme: &Theme, input: &LineInput<'_>, out: &mut LineLayout) {
        let (cell, base) = self.metrics(fonts);
        out.clear();
        out.start_row(self.line_h, base, 0);
        let plain = theme.token(TokenKind::Text);
        let plain_font = self.font;
        let tab = self.tab_width * cell as u32;
        let mut spans = input.spans.iter().peekable();
        // いまの区間（バイト位置の終わり・見た目・書体）
        let mut cur_end = 0u32;
        let mut cur = (plain, plain_font);
        let mut x = 0u32;
        for (bi, ch) in input.text.char_indices() {
            let bi = bi as u32;
            if bi >= cur_end {
                // 終わった区間を捨てて、この位置の区間を探す（区間は位置の順に並ぶ前提。
                // 並んでいなくても、色が付かないだけで壊れない）
                while spans.peek().is_some_and(|s| s.end <= bi) {
                    spans.next();
                }
                match spans.peek() {
                    Some(s) if s.start <= bi => {
                        let st = theme.token(s.kind);
                        cur = (st, font_for(plain_font, &st));
                        cur_end = s.end;
                    }
                    Some(s) => {
                        cur = (plain, plain_font);
                        cur_end = s.start;
                    }
                    None => {
                        cur = (plain, plain_font);
                        cur_end = u32::MAX;
                    }
                }
            }
            let adv = if ch == '\t' {
                (tab - x % tab) as u16
            } else if ch.is_ascii() {
                cell
            } else {
                fonts.advance(ch, &cur.1)
            };
            out.push(adv, cur.1, cur.0);
            x += adv as u32;
        }
        out.finish(self.line_h, base);
    }
    fn uniform_height(&self) -> Option<u16> {
        Some(self.line_h)
    }
    fn cell_width(&mut self, fonts: &mut Fonts) -> u16 {
        self.metrics(fonts).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::palette;

    fn st() -> (TextStyle, Style) {
        (TextStyle::mono(10.0), Style::new(palette::FG))
    }

    /// 等幅 6px の 1 行。
    fn mono(n: u32) -> LineLayout {
        let (f, s) = st();
        let mut l = LineLayout::new();
        l.start_row(12, 9, 0);
        for _ in 0..n {
            l.push(6, f, s);
        }
        l.finish(12, 9);
        l
    }

    #[test]
    fn empty_line_has_one_row() {
        let mut l = LineLayout::new();
        l.finish(12, 9);
        assert_eq!(l.rows().len(), 1);
        assert_eq!(l.height(), 12);
        assert_eq!(l.caret(0), Caret { row: 0, x: 0, y: 0, height: 12 });
        assert_eq!(l.caret(5), l.caret(0)); // 行末より後ろは丸める
        assert_eq!(l.col_at(0, 100), 0);
    }

    #[test]
    fn mono_caret_and_hit_roundtrip() {
        let l = mono(10);
        assert_eq!((l.height(), l.width(), l.len_chars()), (12, 60, 10));
        for col in 0..=10 {
            let c = l.caret(col);
            assert_eq!((c.row, c.x, c.y), (0, col as i32 * 6, 0));
            assert_eq!(l.col_at(0, c.x), col, "境目の x は、その桁に戻る");
        }
        // 文字の中央より左は手前、右は次の桁
        assert_eq!(l.col_at(0, 2), 0);
        assert_eq!(l.col_at(0, 3), 1);
        assert_eq!(l.col_at(0, 5), 1);
        assert_eq!(l.col_at(0, -10), 0);
        assert_eq!(l.col_at(0, 1000), 10);
    }

    #[test]
    fn runs_merge_by_style() {
        let (f, s) = st();
        let kw = Style::new(palette::KEYWORD);
        let mut l = LineLayout::new();
        l.start_row(12, 9, 0);
        for st in [kw, kw, kw, s, s, kw] {
            l.push(6, f, st);
        }
        l.finish(12, 9);
        let row = &l.rows()[0];
        let runs: Vec<_> = l.runs(row).iter().map(|r| (r.chars.clone(), r.style.fg)).collect();
        assert_eq!(runs, vec![(0..3, palette::KEYWORD), (3..5, palette::FG), (5..6, palette::KEYWORD)]);
    }

    #[test]
    fn variable_rows_and_wrap_boundary() {
        // 見出し（高さ 17、字幅 9）のあとに、折り返した本文 2 行（高さ 12、字幅まちまち）
        let (f, s) = st();
        let big = TextStyle::sans(13.0);
        let mut l = LineLayout::new();
        l.start_row(17, 13, 10);
        for _ in 0..3 {
            l.push(9, big, s);
        }
        l.start_row(12, 9, 10);
        for w in [4, 7, 5] {
            l.push(w, f, s);
        }
        l.start_row(12, 9, 10);
        l.push(6, f, s);
        l.finish(12, 9);
        assert_eq!(l.height(), 41);
        assert_eq!(l.rows().iter().map(|r| (r.y, r.height, r.chars.clone())).collect::<Vec<_>>(), vec![
            (0, 17, 0..3),
            (17, 12, 3..6),
            (29, 12, 6..7)
        ]);
        // 桁 3 は折り返しの境目: 次の行の先頭に置く
        assert_eq!(l.caret(2), Caret { row: 0, x: 28, y: 0, height: 17 });
        assert_eq!(l.caret(3), Caret { row: 1, x: 10, y: 17, height: 12 });
        assert_eq!(l.caret(5), Caret { row: 1, x: 21, y: 17, height: 12 });
        assert_eq!(l.caret(7), Caret { row: 2, x: 16, y: 29, height: 12 });
        // 表示行の右端をたたくと、その行の最後の境目（= 次の行の先頭と同じ桁）
        assert_eq!(l.col_at(0, 500), 3);
        assert_eq!(l.col_at(1, 0), 3);
        assert_eq!(l.col_at(1, 15), 4); // 10..14 が 3 文字目、14..21 が 4 文字目
        assert_eq!(l.col_at(1, 18), 5);
        assert_eq!(l.row_at_y(0), 0);
        assert_eq!(l.row_at_y(16), 0);
        assert_eq!(l.row_at_y(17), 1);
        assert_eq!(l.row_at_y(40), 2);
        assert_eq!(l.row_at_y(400), 2);
        assert_eq!(l.row_at_y(-5), 0);
        // 区間は表示行ごとに分かれる（書体が同じでも行をまたがない）
        assert_eq!(l.runs(&l.rows()[1]).len(), 1);
        assert_eq!(l.runs(&l.rows()[2]).len(), 1);
    }

    fn fonts() -> Option<Fonts> {
        let cfg = zc_font::FontConfig::default();
        if !cfg.available() {
            eprintln!("フォントが無いので飛ばす");
            return None;
        }
        Some(Fonts::load(&cfg, zc_font::RasterMode::Inline).unwrap())
    }

    #[test]
    fn code_layouter_cells_tabs_wide_and_spans() {
        let Some(mut f) = fonts() else { return };
        let theme = Theme::dark_plus();
        let mut lay = CodeLayouter::new(10.0, 12);
        assert_eq!(lay.cell_width(&mut f), 6);
        let mut out = LineLayout::new();
        // "def" はキーワード、"日本" は全角、タブは 4 桁ごと
        let text = "def\tf(日本)";
        let spans = [Span { start: 0, end: 3, kind: TokenKind::Keyword }];
        lay.layout(&mut f, &theme, &LineInput { line: 0, text, spans: &spans, cursor_line: false, wrap_width: 300 }, &mut out);
        assert_eq!(out.rows().len(), 1);
        assert_eq!(out.height(), 12);
        let row = &out.rows()[0];
        assert_eq!(out.xs(row), &[0, 6, 12, 18, 24, 30, 36, 48, 60, 66]);
        let runs: Vec<_> = out.runs(row).iter().map(|r| (r.chars.clone(), r.style.fg)).collect();
        assert_eq!(runs, vec![(0..3, palette::KEYWORD), (3..9, palette::FG)]);
        // 桁は文字数: 「本」の左は桁 7
        assert_eq!(out.caret(7).x, 48);
        assert_eq!(out.col_at(0, 50), 7);
        assert_eq!(out.col_at(0, 55), 8);
        // 行頭のタブ 2 つ
        lay.layout(&mut f, &theme, &LineInput { line: 0, text: "\t\tx", spans: &[], cursor_line: false, wrap_width: 300 }, &mut out);
        assert_eq!(out.xs(&out.rows()[0]), &[0, 24, 48, 54]);
        // 空の行
        lay.layout(&mut f, &theme, &LineInput { line: 0, text: "", spans: &[], cursor_line: false, wrap_width: 300 }, &mut out);
        assert_eq!((out.rows().len(), out.len_chars(), out.height()), (1, 0, 12));
    }

    #[test]
    fn code_layouter_tolerates_bad_spans() {
        let Some(mut f) = fonts() else { return };
        let theme = Theme::dark_plus();
        let mut lay = CodeLayouter::standard();
        let mut out = LineLayout::new();
        // 範囲外・重なり・逆順でも落ちない
        let spans = [
            Span { start: 2, end: 100, kind: TokenKind::String },
            Span { start: 1, end: 3, kind: TokenKind::Number },
            Span { start: 0, end: 0, kind: TokenKind::Comment },
        ];
        lay.layout(&mut f, &theme, &LineInput { line: 0, text: "abcd", spans: &spans, cursor_line: false, wrap_width: 0 }, &mut out);
        assert_eq!(out.len_chars(), 4);
        let row = &out.rows()[0];
        assert_eq!(out.runs(row)[0].style.fg, palette::FG);
        assert_eq!(out.runs(row).last().unwrap().style.fg, palette::STRING);
    }
}
