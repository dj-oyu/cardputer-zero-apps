//! Markdown のライブプレビューの行の組み方（段階 5、モック 10）。
//!
//! `zc-markdown` の [`MdDocument::line_into`] が返す「区間の並び・ブロックの種類」から、
//! 見出しの高さ・本文のプロポーショナル・折り返し・行の飾り（引用の線・箇条書きの点・
//! チェックボックス・コードブロックの背景・見出しの下線）を組む。
//!
//! - 桁は文書の桁のまま: 隠す記号も幅 0 の文字として入れる（[`LineLayout::push_hidden`]）
//! - カーソル行では記号を全部見せ、記号の代わりの飾り（点・チェックボックス）は置かない
//! - カーソル行でなければ、``` の行は 2 画素、空の行は半分の高さにつぶす
//!
//! 文書の解析結果 [`MdDocument`] は、アプリ（`zcode`）と同じものを共有する
//! （アプリが編集のたびに [`MdDocument::edited`] で直し、batch の全体解析の結果を
//! [`MdDocument::replace`] で入れる。ここは読むだけ）。解析結果が変わったら、アプリが
//! [`crate::EditorView::recheck_all`] を呼ぶ（文字列が同じでも見た目が変わった行は、
//! [`LineLayout::set_sig`] の印で見つかる）。

use std::cell::RefCell;
use std::rc::Rc;

use zc_core::{palette, Attrs, Color, Style, Theme};
use zc_font::{Fonts, TextStyle};
use zc_markdown::{MdBlock, MdDocument, MdFont, MdLine};

use crate::textlayout::{Deco, LineInput, LineLayout, LineLayouter};

/// 本文の大きさと行の高さ。
pub const MD_BODY_PX: f32 = 8.5;
pub const MD_BODY_H: u16 = 12;
/// コード・記号の大きさと、コードブロックの行の高さ。モックは 7.5px だが、
/// DejaVu Sans Mono は 8px 以下で `z` の斜めの線が消える（`=` に見える）ので 9px にした。
pub const MD_CODE_PX: f32 = 9.0;
pub const MD_CODE_H: u16 = 10;
/// 左右の余白。
pub const MD_PAD_L: i32 = 10;
pub const MD_PAD_R: i32 = 8;
/// カーソルの無い空の行の高さ。
pub const MD_BLANK_H: u16 = 6;

/// 箇条書きの字下げ（1 段）。
const LIST_INDENT: i32 = 9;
/// 引用の字下げ（線 2 + すき間 5）。
const QUOTE_INDENT: i32 = 7;
/// コードブロックの字下げ（線 2 + すき間 4）。
const CODE_INDENT: i32 = 6;
/// チェックボックスの大きさと、後ろのすき間。
const CHECK_W: i32 = 8;
const CHECK_GAP: i32 = 3;
/// つぶした ``` の行・水平線の行の高さ。
const FENCE_H: u16 = 2;
const RULE_H: u16 = 8;

const CODE_BG: Color = palette::LINE;
const CODE_BAR: Color = palette::STATUS;
const QUOTE_BAR: Color = Color::hex(0x555555);
const RULE: Color = Color::hex(0x3c3c3c);
const CHECK_BORDER: Color = Color::hex(0xaaaaaa);
const CHECK_ON: Color = palette::ACCENT;
const BULLET: Color = palette::STATUS;

/// 行の種類（高さとベースラインを決める）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    Body = 0,
    H1,
    H2,
    H3,
    H4,
    Code,
}

impl Row {
    /// `(書体, 行の高さ)`。
    fn spec(self) -> (TextStyle, u16) {
        match self {
            Row::Body => (TextStyle::sans(MD_BODY_PX), MD_BODY_H),
            Row::H1 => (TextStyle::sans(13.0).bold(), 17),
            Row::H2 => (TextStyle::sans(10.5).bold(), 15),
            Row::H3 => (TextStyle::sans(9.5).bold(), 13),
            Row::H4 => (TextStyle::sans(MD_BODY_PX).bold(), MD_BODY_H),
            Row::Code => (TextStyle::mono(MD_CODE_PX), MD_CODE_H),
        }
    }
}

/// 区間の書体。
fn font_of(font: MdFont, style: &Style) -> TextStyle {
    let mut f = match font {
        MdFont::Body => TextStyle::sans(MD_BODY_PX),
        MdFont::Heading(1) => Row::H1.spec().0,
        MdFont::Heading(2) => Row::H2.spec().0,
        MdFont::Heading(3) => Row::H3.spec().0,
        MdFont::Heading(_) => Row::H4.spec().0,
        MdFont::Code | MdFont::Marker => TextStyle::mono(MD_CODE_PX),
    };
    f.bold |= style.attrs.contains(Attrs::BOLD);
    f.italic = style.attrs.contains(Attrs::ITALIC);
    f
}

/// 組む途中の 1 字。
#[derive(Clone, Copy)]
struct Ch {
    ch: char,
    adv: u16,
    font: TextStyle,
    style: Style,
    hidden: bool,
}

/// Markdown のライブプレビュー。
pub struct MdLayouter {
    doc: Rc<RefCell<MdDocument>>,
    line: MdLine,
    chars: Vec<Ch>,
    breaks: Vec<u32>,
    /// 行の種類ごとのベースライン（最初に使うときにフォントから取る）。
    base: [Option<u16>; 6],
    cell: Option<u16>,
}

impl MdLayouter {
    pub fn new(doc: Rc<RefCell<MdDocument>>) -> MdLayouter {
        MdLayouter { doc, line: MdLine::new(), chars: Vec::new(), breaks: Vec::new(), base: [None; 6], cell: None }
    }
    /// 共有している解析結果。
    pub fn doc(&self) -> &Rc<RefCell<MdDocument>> {
        &self.doc
    }
    fn baseline(&mut self, fonts: &mut Fonts, row: Row) -> u16 {
        if let Some(b) = self.base[row as usize] {
            return b;
        }
        let (font, h) = row.spec();
        let m = fonts.metrics(&font);
        let b = ((h as i32 - m.height() as i32).div_euclid(2) + m.ascent as i32).clamp(0, h as i32) as u16;
        self.base[row as usize] = Some(b);
        b
    }
}

/// 見た目を決めるものの印（FNV-1a）。
fn line_sig(l: &MdLine, cur: bool) -> u32 {
    let mut h = 0x811c_9dc5u32;
    let mut add = |v: u32| {
        for b in v.to_le_bytes() {
            h = (h ^ b as u32).wrapping_mul(0x0100_0193);
        }
    };
    add(match l.block {
        MdBlock::Paragraph => 1,
        MdBlock::Heading(n) => 0x100 | n as u32,
        MdBlock::ListItem { depth, checkbox } => 0x200 | depth as u32 | (checkbox.map_or(0, |c| 1 + c as u32) << 16),
        MdBlock::Quote => 3,
        MdBlock::Code => 4,
        MdBlock::CodeFence => 5,
        MdBlock::Rule => 6,
        MdBlock::Table => 7,
        MdBlock::Blank => 8,
    });
    add(l.indent as u32 | (l.quote as u32) << 8 | (l.bullet as u32) << 16 | (l.collapse as u32) << 17 | (cur as u32) << 18);
    for r in &l.runs {
        add(r.range.end);
        add(match r.font {
            MdFont::Body => 0,
            MdFont::Heading(n) => 0x10 | n as u32,
            MdFont::Code => 1,
            MdFont::Marker => 2,
        } | (r.hidden as u32) << 8
            | (r.style.attrs.0 as u32) << 16);
        add(r.style.fg.0 as u32 | (r.style.bg.map_or(0, |c| c.0 as u32 + 1) << 16));
    }
    h | 1
}

/// 行の途中で折り返してよいか（`prev` と `ch` の間）。英数字の単語の途中では折り返さない。
fn can_break(prev: char, ch: char) -> bool {
    ch != ' ' && (prev == ' ' || !prev.is_ascii() || !ch.is_ascii())
}

impl LineLayouter for MdLayouter {
    fn layout(&mut self, fonts: &mut Fonts, _theme: &Theme, input: &LineInput<'_>, out: &mut LineLayout) {
        out.clear();
        let cur = input.cursor_line;
        let cursor = cur.then_some(input.line);
        match self.doc.try_borrow() {
            Ok(d) => d.line_into(input.line, input.text, cursor, &mut self.line),
            // アプリが書き換えている最中: 1 行だけの解析で組む
            Err(_) => MdDocument::new().line_into(u32::MAX, input.text, cursor.map(|_| u32::MAX), &mut self.line),
        }
        let block = self.line.block;
        let row = match block {
            MdBlock::Heading(1) => Row::H1,
            MdBlock::Heading(2) => Row::H2,
            MdBlock::Heading(3) => Row::H3,
            MdBlock::Heading(_) => Row::H4,
            MdBlock::Code | MdBlock::CodeFence | MdBlock::Table => Row::Code,
            _ => Row::Body,
        };
        let (row_font, row_h) = row.spec();
        let base = self.baseline(fonts, row);
        let code = matches!(block, MdBlock::Code | MdBlock::CodeFence);
        let checkbox = match block {
            MdBlock::ListItem { checkbox, .. } if !cur => checkbox,
            _ => None,
        };

        // 左右の位置
        let quote = self.line.quote.min(4) as i32;
        let base_x = MD_PAD_L + quote * QUOTE_INDENT + self.line.indent.min(6) as i32 * LIST_INDENT;
        let text_x = base_x + if code { CODE_INDENT } else { 0 };
        let first_x = text_x + if checkbox.is_some() { CHECK_W + CHECK_GAP } else { 0 };
        let limit = input.wrap_width - MD_PAD_R;
        let deco_w = (limit - base_x).clamp(0, u16::MAX as i32) as u16;

        // 文字ごとの書体と幅
        self.chars.clear();
        let default = (row_font, Style::new(palette::FG));
        let mut ri = 0;
        for (bi, ch) in input.text.char_indices() {
            let bi = bi as u32;
            while self.line.runs.get(ri).is_some_and(|r| r.range.end <= bi) {
                ri += 1;
            }
            let (font, style, hidden) = match self.line.runs.get(ri) {
                Some(r) if r.range.start <= bi => (font_of(r.font, &r.style), r.style, r.hidden),
                _ => (default.0, default.1, false),
            };
            let adv = if hidden || (ch.is_control() && ch != '\t') {
                0
            } else if ch == '\t' {
                fonts.advance(' ', &font) * 4
            } else {
                fonts.advance(ch, &font)
            };
            self.chars.push(Ch { ch, adv, font, style, hidden });
        }

        // つぶす行: 文字は全部幅 0 にして、低い表示行 1 つに入れる
        let squash = if cur {
            None
        } else if self.line.collapse {
            Some(if block == MdBlock::CodeFence { FENCE_H } else { 1 })
        } else {
            match block {
                MdBlock::Rule => Some(RULE_H),
                MdBlock::Blank => Some(MD_BLANK_H),
                _ => None,
            }
        };
        if let Some(h) = squash {
            out.start_row(h, h, text_x);
            for c in &self.chars {
                out.push_hidden(c.font, c.style);
            }
            match block {
                MdBlock::CodeFence => {
                    out.push_deco(Deco::fill(base_x, 0, deco_w, h, CODE_BG));
                    out.push_deco(Deco::fill(base_x, 0, 2, h, CODE_BAR));
                }
                MdBlock::Rule => out.push_deco(Deco::fill(base_x, h as i32 / 2, deco_w, 1, RULE)),
                _ => {}
            }
            for k in 0..quote {
                out.push_deco(Deco::fill(MD_PAD_L + k * QUOTE_INDENT, 0, 2, h, QUOTE_BAR));
            }
            out.set_sig(line_sig(&self.line, cur));
            return;
        }

        // 折り返す位置
        self.breaks.clear();
        let n = self.chars.len();
        let (mut i, mut x, mut row_start) = (0usize, first_x, 0usize);
        let mut last_brk: Option<usize> = None;
        let mut prev: Option<char> = None;
        while i < n {
            let c = self.chars[i];
            if c.adv > 0 {
                if let Some(p) = prev {
                    if code || can_break(p, c.ch) {
                        last_brk = Some(i);
                    }
                    // 行末の空白 1 つは、右の余白にはみ出させる
                    let fits = x + c.adv as i32 <= limit || (c.ch == ' ' && x + c.adv as i32 <= limit + MD_PAD_R - 2);
                    if !fits {
                        let b = match last_brk {
                            Some(b) if b > row_start => b,
                            _ => i,
                        };
                        self.breaks.push(b as u32);
                        row_start = b;
                        x = text_x;
                        last_brk = None;
                        prev = None;
                        i = b;
                        continue;
                    }
                }
                x += c.adv as i32;
                prev = Some(c.ch);
            }
            i += 1;
        }

        // 表示行に入れる。見出し 2 は上に 1、見出し 1 は下に 3（下線 1 + すき間 2）あける
        let rows = self.breaks.len() + 1;
        let top = if row == Row::H2 { 1 } else { 0 };
        let bottom = if row == Row::H1 { 3 } else { 0 };
        let h_of = |r: usize| row_h + if r == 0 { top } else { 0 } + if r + 1 == rows { bottom } else { 0 };
        out.start_row(h_of(0), base + top, first_x);
        let mut bi = 0;
        for (i, c) in self.chars.iter().enumerate() {
            if self.breaks.get(bi).is_some_and(|b| *b as usize == i) {
                bi += 1;
                out.start_row(h_of(bi), base, text_x);
            }
            if c.hidden {
                out.push_hidden(c.font, c.style);
            } else {
                out.push(c.adv, c.font, c.style);
            }
        }

        // 飾り
        out.set_bg_inset(1);
        let total = out.height();
        let first_base = (base + top) as i32;
        if code {
            out.push_deco(Deco::fill(base_x, 0, deco_w, total, CODE_BG));
            out.push_deco(Deco::fill(base_x, 0, 2, total, CODE_BAR));
        }
        for k in 0..quote {
            out.push_deco(Deco::fill(MD_PAD_L + k * QUOTE_INDENT, 0, 2, total, QUOTE_BAR));
        }
        if row == Row::H1 {
            let w = (limit - MD_PAD_L).clamp(0, u16::MAX as i32) as u16;
            out.push_deco(Deco::fill(MD_PAD_L, total as i32 - 3, w, 1, RULE));
        }
        if !cur {
            if self.line.bullet {
                out.push_deco(Deco::glyph(text_x - LIST_INDENT + 1, first_base, '•', TextStyle::sans(MD_BODY_PX), BULLET));
            }
            if let Some(on) = checkbox {
                let (x, y, w) = (text_x, first_base - CHECK_W + 1, CHECK_W as u16);
                if on {
                    out.push_deco(Deco::fill(x, y, w, w, CHECK_ON));
                } else {
                    out.push_deco(Deco::fill(x, y, w, 1, CHECK_BORDER));
                    out.push_deco(Deco::fill(x, y + CHECK_W - 1, w, 1, CHECK_BORDER));
                    out.push_deco(Deco::fill(x, y, 1, w, CHECK_BORDER));
                    out.push_deco(Deco::fill(x + CHECK_W - 1, y, 1, w, CHECK_BORDER));
                }
            }
        }
        out.set_sig(line_sig(&self.line, cur));
    }
    fn uniform_height(&self) -> Option<u16> {
        None
    }
    fn gutter(&self) -> bool {
        false
    }
    fn wraps(&self) -> bool {
        true
    }
    fn depends_on_cursor(&self) -> bool {
        true
    }
    fn cell_width(&mut self, fonts: &mut Fonts) -> u16 {
        if let Some(c) = self.cell {
            return c;
        }
        let c = fonts.metrics(&TextStyle::sans(MD_BODY_PX)).cell_w.max(1);
        self.cell = Some(c);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EditorView, NoSpans, RenderCx};
    use zc_core::{Canvas, CancelToken, Damage, LineChange, LineRange, LineSource, Pos, Rect};
    use zc_font::{FontConfig, RasterMode};

    struct Doc(Vec<String>);

    impl LineSource for Doc {
        fn len_lines(&self) -> u32 {
            self.0.len().max(1) as u32
        }
        fn line(&self, line: u32, out: &mut String) {
            out.clear();
            if let Some(s) = self.0.get(line as usize) {
                out.push_str(s);
            }
        }
    }

    /// モック 10 の文書（モック 11 がそのソース）。
    const SAMPLE: &str = "# 買い物と作業メモ\n- [x] 基板を発注する\n- [ ] ケースを **3D 印刷** する\n- [ ] `zcode` を実機で試す\n## メモ\n> CMA は **64MB** しかない。*カメラ*と同時に使うときは注意\n詳細は [development.md](../development.md) を参照\n```python\nimport zero_ui as ui\nui.font(12)\n```";

    const AREA: Rect = Rect::new(0, 13, 320, 146);

    struct Fx {
        fonts: Fonts,
        theme: Theme,
        canvas: Canvas,
        view: EditorView,
        doc: Doc,
        md: Rc<RefCell<MdDocument>>,
    }

    impl Fx {
        fn new(text: &str) -> Option<Fx> {
            let cfg = FontConfig::default();
            if !cfg.available() {
                eprintln!("フォントが無いので飛ばす");
                return None;
            }
            let doc = Doc(text.split('\n').map(String::from).collect());
            let md = Rc::new(RefCell::new(MdDocument::parse(&doc, &CancelToken::never()).unwrap().value));
            let mut canvas = Canvas::new();
            canvas.fill_rect(Rect::new(0, 0, 320, 170), palette::BG);
            Some(Fx {
                fonts: Fonts::load(&cfg, RasterMode::Inline).unwrap(),
                theme: Theme::dark_plus(),
                canvas,
                view: EditorView::new(Box::new(MdLayouter::new(md.clone()))),
                doc,
                md,
            })
        }
        fn render(&mut self) -> Damage {
            let mut cx = RenderCx::new(&mut self.canvas, &mut self.fonts, &self.theme);
            self.view.render(&mut cx, AREA, &self.doc, &mut NoSpans)
        }
        fn caret(&mut self, line: u32, col: u32) {
            self.view.set_selection(Pos::new(line, col), Pos::new(line, col));
        }
        fn layout(&mut self, line: u32, cur: bool) -> LineLayout {
            let mut lay = MdLayouter::new(self.md.clone());
            let mut out = LineLayout::new();
            let text = self.doc.0[line as usize].clone();
            lay.layout(&mut self.fonts, &self.theme, &LineInput { line, text: &text, spans: &[], cursor_line: cur, wrap_width: 320 }, &mut out);
            out
        }
        /// 同じ状態を、まっさらなビューで描いたものと比べる。
        fn assert_matches_fresh(&mut self, what: &str) {
            let mut view = EditorView::new(Box::new(MdLayouter::new(self.md.clone())));
            let head = self.view.cursor();
            view.set_selection(head, head);
            view.set_viewport(self.view.viewport());
            let mut canvas = Canvas::new();
            canvas.fill_rect(Rect::new(0, 0, 320, 170), palette::BG);
            let mut cx = RenderCx::new(&mut canvas, &mut self.fonts, &self.theme);
            view.render(&mut cx, AREA, &self.doc, &mut NoSpans);
            if view.viewport() != self.view.viewport() {
                return; // 表示位置が違えば比べられない
            }
            for y in 0..170u16 {
                assert!(self.canvas.row(y) == canvas.row(y), "{what}: 画素行 {y} が、描き直した結果と違う");
            }
        }
    }

    #[test]
    fn layout_keeps_every_char_and_hides_markers() {
        let Some(mut f) = Fx::new(SAMPLE) else { return };
        for line in 0..f.doc.0.len() as u32 {
            let n = f.doc.0[line as usize].chars().count() as u32;
            for cur in [false, true] {
                let l = f.layout(line, cur);
                assert_eq!(l.len_chars(), n, "行 {line}: 桁は文書の桁のまま");
                // 境目の x は表示行の中で単調
                for row in l.rows() {
                    assert!(l.xs(row).windows(2).all(|w| w[0] <= w[1]));
                }
                let hidden = l.rows().iter().flat_map(|r| l.runs(r)).any(|r| r.hidden);
                assert_eq!(hidden, !cur && [0, 1, 2, 3, 4, 5, 6, 7, 10].contains(&line), "行 {line} cur={cur}");
            }
        }
        // 見出し 1: 高さ 17 + 下線とすき間 3、記号 `# ` は幅 0
        let l = f.layout(0, false);
        assert_eq!((l.height(), l.rows().len()), (20, 1));
        assert_eq!(l.caret(0).x, MD_PAD_L);
        assert_eq!(l.caret(2).x, MD_PAD_L);
        assert!(l.caret(3).x > MD_PAD_L);
        // カーソル行では記号に幅がある
        let l = f.layout(0, true);
        assert!(l.caret(2).x > MD_PAD_L);
        // 箇条書き: 字下げ 9、チェックボックスの分だけ右へ。カーソル行は記号から
        let l = f.layout(2, false);
        assert_eq!((l.height(), l.caret(0).x), (12, MD_PAD_L + 9 + 11));
        assert!(l.decos().len() >= 5, "点と、チェックボックスの枠");
        let l = f.layout(2, true);
        assert_eq!(l.caret(0).x, MD_PAD_L + 9);
        assert!(l.decos().is_empty());
        // 見出し 2: 上に 1 あけて 15
        assert_eq!(f.layout(4, false).height(), 16);
        // 引用: 左に線、字は 7 右へ
        let mut l = f.layout(5, false);
        assert_eq!((l.rows().len(), l.height()), (1, 12));
        assert_eq!(l.decos()[0], Deco::fill(MD_PAD_L, 0, 2, 12, QUOTE_BAR));
        assert_eq!(l.caret(0).x, MD_PAD_L + 7);
        // 幅が狭ければ折り返して 2 行、線は全部の高さ
        let mut lay = MdLayouter::new(f.md.clone());
        let t = f.doc.0[5].clone();
        lay.layout(&mut f.fonts, &f.theme, &LineInput { line: 5, text: &t, spans: &[], cursor_line: false, wrap_width: 200 }, &mut l);
        assert_eq!((l.rows().len(), l.height()), (2, 24));
        assert_eq!(l.decos()[0], Deco::fill(MD_PAD_L, 0, 2, 24, QUOTE_BAR));
        let c = l.rows()[1].chars.start;
        assert_eq!((l.row_of(c), l.caret(c).x), (1, MD_PAD_L + 7));
        // 囲みはつぶす。カーソル行なら 1 行ぶん
        assert_eq!(f.layout(7, false).height(), FENCE_H);
        assert_eq!(f.layout(7, true).height(), MD_CODE_H);
        assert_eq!(f.layout(8, false).height(), MD_CODE_H);
        assert_eq!(f.layout(8, false).caret(0).x, MD_PAD_L + 6);
    }

    #[test]
    fn wraps_long_lines_within_width() {
        let text = "これは折り返しの試験です。long English words should not be split in the middle of a word, 日本語はどこでも折り返せる。\n```\nlet very_long_code_line = some_function(argument_one, argument_two, argument_three, argument_four);\n```\n- item with a very long text that needs to wrap around to the next row of the list item";
        let Some(mut f) = Fx::new(text) else { return };
        for (line, x0) in [(0u32, MD_PAD_L), (2, MD_PAD_L + 6), (4, MD_PAD_L + 9)] {
            for width in [320, 236, 120, 40, 8, 1] {
                let mut lay = MdLayouter::new(f.md.clone());
                let mut out = LineLayout::new();
                let t = f.doc.0[line as usize].clone();
                lay.layout(&mut f.fonts, &f.theme, &LineInput { line, text: &t, spans: &[], cursor_line: false, wrap_width: width }, &mut out);
                assert_eq!(out.len_chars(), t.chars().count() as u32);
                if width >= 120 {
                    assert!(out.rows().len() > 1, "行 {line} 幅 {width}");
                    assert!(out.width() <= width, "行 {line} 幅 {width}: 右端 {}", out.width());
                    for row in &out.rows()[1..] {
                        assert_eq!(out.xs(row)[0], x0, "続きの行は字下げの位置から");
                    }
                }
            }
        }
        // 英単語の途中では折り返さない
        let l = f.layout(0, false);
        let t: Vec<char> = f.doc.0[0].chars().collect();
        for row in &l.rows()[1..] {
            let (a, b) = (t[row.chars.start as usize - 1], t[row.chars.start as usize]);
            assert!(!(a.is_ascii_alphabetic() && b.is_ascii_alphabetic()), "{a}{b} の間で折り返した");
        }
    }

    #[test]
    fn cursor_move_and_edit_repaint_matches_fresh() {
        let Some(mut f) = Fx::new(SAMPLE) else { return };
        f.caret(2, 19);
        f.render();
        f.assert_matches_fresh("最初");
        assert!(f.render().is_empty(), "変わらなければ描かない");
        // カーソルを動かす: 前の行は記号が隠れ、新しい行は記号が出る
        for (line, col) in [(3, 2), (7, 0), (8, 3), (10, 1), (5, 4), (0, 0), (6, 30)] {
            f.caret(line, col);
            f.render();
            f.assert_matches_fresh(&format!("カーソル {line}:{col}"));
        }
        // 編集: 3 行目を ``` にすると、下の行がコードになる（文字列は同じでも描き直す）
        f.doc.0[2] = "```".into();
        let c = LineChange { old: LineRange::new(2, 3), new: LineRange::new(2, 3) };
        let need = f.md.borrow_mut().edited(&f.doc, c);
        assert!(need);
        f.view.lines_changed(c);
        f.view.recheck_all();
        f.render();
        f.assert_matches_fresh("編集の直後");
        let p = MdDocument::parse(&f.doc, &CancelToken::never()).unwrap();
        let v = p.version;
        assert!(f.md.borrow_mut().replace(p, v));
        f.view.recheck_all();
        assert!(!f.render().is_empty(), "解析結果が変わった行を描き直す");
        f.assert_matches_fresh("全体の解析の後");
        // 未確定文字列を挟む
        f.caret(5, 6);
        f.view.set_preedit(Some(crate::PreeditView::reading("▽てん")));
        f.render();
        assert!(f.view.preedit_rect().is_some());
        f.view.set_preedit(None);
        f.render();
        f.assert_matches_fresh("未確定文字列を消した後");
    }

    #[test]
    fn hit_testing_roundtrip_on_cursor_line() {
        let Some(mut f) = Fx::new(SAMPLE) else { return };
        f.caret(2, 19);
        f.render();
        // カーソル行は全部の桁に幅があるので、境目の位置からその桁に戻る
        let n = f.doc.0[2].chars().count() as u32;
        for col in 0..=n {
            let r = f.view.point_of(Pos::new(2, col)).unwrap();
            assert_eq!(f.view.pos_at(r.x as i32, r.y as i32 + 2), Some(Pos::new(2, col)), "桁 {col}");
        }
        // 記号を隠した行: 見える文字の境目は、その桁か、同じ位置の（隠れた記号をはさんだ）桁に戻る
        let n = f.doc.0[5].chars().count() as u32;
        for col in 0..=n {
            let r = f.view.point_of(Pos::new(5, col)).unwrap();
            let p = f.view.pos_at(r.x as i32, r.y as i32 + 2).unwrap();
            assert_eq!(p.line, 5);
            assert_eq!(f.view.point_of(p).unwrap().x, r.x, "桁 {col} → {}", p.col);
        }
    }

    /// モック 10 と同じ文書を描いて PNG に出す（目で見て比べる用）。
    #[test]
    fn render_mock10_png() {
        let Some(mut f) = Fx::new(SAMPLE) else { return };
        f.caret(2, 19);
        f.render();
        let dir = std::env::var("ZCODE_MD_PNG_DIR").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../out").to_string());
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{dir}/md-mock10.png");
        zc_display::png::write_png(&f.canvas, &path).unwrap();
        eprintln!("描いた: {path}");
        // 2 枚目: カーソルをコードブロックの囲みに置いたもの
        f.caret(7, 3);
        f.render();
        zc_display::png::write_png(&f.canvas, format!("{dir}/md-mock10-fence.png")).unwrap();
    }
}
