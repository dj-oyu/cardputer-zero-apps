//! エディタ本体（行番号・本文・カーソル・選択・波線）。design.md §5。
//!
//! - テキストは `zc_core::LineSource`、色は行ごとの `Span`（[`SpanSource`]）で受け取る。
//!   `zc-buffer` / `zc-highlight` の中身は知らない。`Span` が無ければ色なしで描く
//! - 行の組み方は [`LineLayouter`] に任せる（コードは [`crate::CodeLayouter`]）。
//!   ビューは [`LineLayout`]（表示行の高さ・文字ごとの x）だけを見るので、行ごとに
//!   高さや字幅が違ってもそのまま動く（§5.5）
//! - 描画は差分: 画面に出ている論理行ごとに「前回描いた内容の印（ハッシュ）」を
//!   覚えておき、変わった行だけを描き直す。印を計算し直すのは、変更の通知
//!   （[`EditorView::lines_changed`]・[`EditorView::set_selection`] など）があった
//!   行だけ（打鍵 1 回で 1 行ぶんの読み出しとレイアウトで済む）
//! - 縦スクロールは `Canvas::scroll_rows`（memmove）でずらし、新しく見えた行だけ描く
//!   （§5.3）。返す `Damage` は、カーソル行が [`Priority::Cursor`]、ほかの描き直した行が
//!   [`Priority::Edit`]、スクロールしたときは領域全体（§5.2）
//! - 日本語入力の未確定文字列（[`PreeditView`]。SKK の ▽ / ▼）は、文書には入れずに、
//!   カーソル行の **カーソルの桁に挟んだ文字列** を [`LineLayouter`] に渡して組む
//!   （全角の字幅・横スクロール・折り返しがそのまま効く）。色と下線は描くときに重ねる。
//!   外から見える桁（[`EditorView::point_of`]・[`EditorView::pos_at`]・選択・波線）は
//!   いつも文書の桁で、表示上の桁との行き来はビューの中で済ませる

use crate::textlayout::{DecoKind, LineInput, LineLayout, LineLayouter};
use crate::RenderCx;
use zc_core::{palette, Attrs, BlendLut, Color, Damage, LineChange, LineSource, Pos, Priority, Rect, RowRange, Span};
use zc_font::TextStyle;

/// 行ごとのトークン（色付きの区間）の出どころ。`zc_highlight::Highlighter::spans` を
/// 包んで渡す。
pub trait SpanSource {
    /// `line` 行目（中身は `text`）のトークンを `out` に入れる（先に空にする）。
    fn spans(&mut self, line: u32, text: &str, out: &mut Vec<Span>);
}

/// 色を付けない。
pub struct NoSpans;

impl SpanSource for NoSpans {
    fn spans(&mut self, _line: u32, _text: &str, out: &mut Vec<Span>) {
        out.clear();
    }
}

impl<F: FnMut(u32, &str, &mut Vec<Span>)> SpanSource for F {
    fn spans(&mut self, line: u32, text: &str, out: &mut Vec<Span>) {
        out.clear();
        self(line, text, out)
    }
}

/// 行に付ける波線（構文エラーなど）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineMark {
    pub line: u32,
    /// 桁の範囲。`None` なら行の文字全体。
    pub cols: Option<core::ops::Range<u32>>,
    pub color: Color,
}

/// 未確定文字列の区間の見た目（モック 7・8）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PreeditStyle {
    /// 読みを入力中（▽）: 白い字に白い下線。背景は行のまま。
    Reading,
    /// 変換中（▼）: 選択と同じ青の背景、白い字に白い下線。
    Converting,
    /// 打ちかけのローマ字など、まだ決まっていない部分: 薄い字に薄い下線。
    Pending,
}

impl PreeditStyle {
    /// `(字の色, 背景, 下線の色)`。背景が `None` なら行の背景のまま。
    fn colors(self) -> (Color, Option<Color>, Color) {
        match self {
            PreeditStyle::Reading => (Color::WHITE, None, Color::WHITE),
            PreeditStyle::Converting => (Color::WHITE, Some(palette::SELECTION), Color::WHITE),
            PreeditStyle::Pending => (palette::DIM, None, palette::DIM),
        }
    }
}

/// 未確定文字列の、同じ見た目が続く区間。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreeditSeg {
    pub text: String,
    pub style: PreeditStyle,
}

/// カーソルの位置に挟んで見せる未確定文字列（日本語入力の ▽ 読み / ▼ 候補）。
///
/// 文書には入らない。`▽` / `▼` の印も含めて、**見せる文字列そのもの**を渡す
/// （ビューは印を足さない）。区間ごとに見た目を変えられる
/// （例: `▽てんめ` は [`PreeditStyle::Reading`]、打ちかけの `t` は [`PreeditStyle::Pending`]）。
/// 改行・タブなどの制御文字は入れない前提（入っていたら空白に置き換える）。
///
/// ```
/// use zc_ui::{PreeditStyle, PreeditView};
/// let reading = PreeditView::reading("▽てんめ").with(PreeditStyle::Pending, "t");
/// let converting = PreeditView::converting("▼点滅");
/// assert_eq!(reading.len_chars(), 5);
/// assert_eq!(converting.text(), "▼点滅");
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreeditView {
    segs: Vec<PreeditSeg>,
}

impl PreeditView {
    /// 空（何も見せない）。[`PreeditView::with`] で区間を足す。
    pub fn new() -> PreeditView {
        PreeditView::default()
    }
    /// 読みを入力中（▽）の 1 区間。
    pub fn reading(text: impl Into<String>) -> PreeditView {
        PreeditView::new().with(PreeditStyle::Reading, text)
    }
    /// 変換中（▼）の 1 区間。
    pub fn converting(text: impl Into<String>) -> PreeditView {
        PreeditView::new().with(PreeditStyle::Converting, text)
    }
    /// 区間を後ろに足す（空の文字列は足さない）。
    pub fn with(mut self, style: PreeditStyle, text: impl Into<String>) -> PreeditView {
        let mut text: String = text.into();
        if text.chars().any(char::is_control) {
            text = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        }
        if !text.is_empty() {
            self.segs.push(PreeditSeg { text, style });
        }
        self
    }
    /// 区間の並び。
    pub fn segs(&self) -> &[PreeditSeg] {
        &self.segs
    }
    /// 見せる文字が無いか。
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }
    /// 文字数。
    pub fn len_chars(&self) -> u32 {
        self.segs.iter().map(|s| s.text.chars().count() as u32).sum()
    }
    /// 全部の区間をつないだ文字列。
    pub fn text(&self) -> String {
        self.segs.iter().map(|s| s.text.as_str()).collect()
    }
}

/// いま挟んでいる未確定文字列（描くときの形）。
#[derive(Default)]
struct Pre {
    view: PreeditView,
    /// 全部の区間をつないだ文字列。
    text: String,
    /// 文字数。0 なら未確定文字列なし。
    n: u32,
    /// 区間ごとの、未確定文字列の先頭からの文字位置と見た目。
    segs: Vec<PreSeg>,
}

/// 未確定文字列の区間: 先頭からの文字位置と見た目。
type PreSeg = (core::ops::Range<u32>, PreeditStyle);

/// 行の文字列 `text` の桁 `col`（行末より後ろなら行末）に `pre` を挟み、トークンの
/// 位置をそれに合わせてずらす（挟んだ所にまたがるトークンは 2 つに分ける。挟んだ
/// 文字列にはトークンを付けない）。挟んだ桁を返す。
fn splice_preedit(text: &mut String, spans: &mut Vec<Span>, col: u32, pre: &str) -> u32 {
    let (mut at, mut b) = (0u32, text.len());
    for (i, (bi, _)) in text.char_indices().enumerate() {
        if i as u32 == col {
            b = bi;
            break;
        }
        at = i as u32 + 1;
    }
    text.insert_str(b, pre);
    let (b, len) = (b as u32, pre.len() as u32);
    let mut i = 0;
    while i < spans.len() {
        let s = spans[i];
        if s.start >= b {
            spans[i].start = s.start.saturating_add(len);
            spans[i].end = s.end.saturating_add(len);
        } else if s.end > b {
            spans[i].end = b;
            i += 1;
            spans.insert(i, Span { start: b + len, end: s.end.saturating_add(len), kind: s.kind });
        }
        i += 1;
    }
    at
}

/// 未確定文字列を挟んだ行の、文書の桁 → 表示上の桁。`at` は挟んだ桁、`n` は挟んだ
/// 文字数。`at` ちょうどの桁は、`after` なら未確定文字列の後ろ、そうでなければ前。
fn to_display(col: u32, at: u32, n: u32, after: bool) -> u32 {
    if col > at || (after && col == at) {
        col.saturating_add(n)
    } else {
        col
    }
}

/// 表示位置。いちばん上に見えている論理行と、その行の中で上に隠れている画素数。
/// 行の高さが変わっても位置がずれにくいように、画素の通し番号では持たない。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Viewport {
    /// いちばん上の論理行。
    pub top: u32,
    /// `top` の行のうち、上に隠れている画素数（全行同じ高さのレイアウトでは常に 0）。
    pub top_off: u16,
    /// 横スクロール（画素）。
    pub scroll_x: i32,
}

impl Viewport {
    /// 論理行 `line` の中の表示行（行の上端から `row_y`、高さ `row_h`）が、高さ
    /// `view_h` の表示域に全部入るように動かす。動かしたら `true`。
    ///
    /// 上にはみ出していれば表示域の上端に、下にはみ出していれば下端に合わせる
    /// （必要なぶんだけ動かす）。`partial` が偽なら、行の途中からは表示しない
    /// （`top_off` は 0 のまま）。`height_of` は論理行の高さ。
    pub fn reveal_row(&mut self, line: u32, row_y: u16, row_h: u16, view_h: u16, partial: bool, height_of: &mut dyn FnMut(u32) -> u16) -> bool {
        let before = *self;
        let (ry, rh, vh) = (row_y as i32, row_h as i32, view_h as i32);
        if line < self.top || (line == self.top && ry < self.top_off as i32) {
            self.top = line;
            self.top_off = if partial { row_y } else { 0 };
            return *self != before;
        }
        // 表示域の上端から、目当ての論理行の上端まで
        let mut y = -(self.top_off as i32);
        let mut l = self.top;
        let mut below = false;
        while l < line {
            y += height_of(l) as i32;
            l += 1;
            if y >= vh {
                below = true;
                break;
            }
        }
        if !below && y + ry + rh <= vh {
            return false;
        }
        // 下端に合わせる: 目当ての表示行の下に何も置かず、上へ入るだけ行を積む
        let over = ry + rh - vh;
        if over >= 0 {
            self.top = line;
            self.top_off = if !partial {
                0
            } else if rh > vh {
                row_y
            } else {
                over as u16
            };
            return *self != before;
        }
        let mut remaining = -over;
        let (mut t, mut off) = (line, 0u16);
        while t > 0 && remaining > 0 {
            let hp = height_of(t - 1) as i32;
            if hp <= remaining {
                t -= 1;
                remaining -= hp;
            } else {
                if partial {
                    t -= 1;
                    off = (hp - remaining) as u16;
                }
                break;
            }
        }
        self.top = t;
        self.top_off = off;
        *self != before
    }

    /// 本文の x（スクロール前）が、幅 `text_w` の表示域に入るように横へ動かす。
    /// 端から `margin` 以内に来ないようにする。動かしたら `true`。
    pub fn reveal_x(&mut self, x: i32, text_w: i32, margin: i32) -> bool {
        let before = self.scroll_x;
        let margin = margin.min((text_w - 1).max(0) / 2);
        if x < self.scroll_x + margin {
            self.scroll_x = (x - margin).max(0);
        } else if x >= self.scroll_x + text_w - margin {
            self.scroll_x = x - text_w + margin + 1;
        }
        self.scroll_x != before
    }
}

/// 直前の `render` で何をしたか（テストと計測用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// 見えている論理行の数。
    pub lines_visible: u32,
    /// 読み出してレイアウトし直した論理行の数。
    pub lines_checked: u32,
    /// 描き直した論理行の数。
    pub lines_painted: u32,
    /// memmove でずらした画素数（0 ならずらしていない）。
    pub scrolled: i32,
}

/// 画面に描いてある論理行 1 つ。
struct Painted {
    line: u32,
    /// 論理行の上端の画面上の y（表示域の上にはみ出していれば、領域の上端より小さい）。
    y: i32,
    height: u16,
    /// 描いた内容の印。
    sig: u64,
    layout: LineLayout,
}

enum Plan {
    /// 前回のまま使える（前回の y）。
    Reuse(i32),
    Paint,
}

/// 行番号の欄の幅: 桁数 × 字幅 + 左 2 + 右 3。3 桁ぶんより狭くはしない。
pub fn gutter_width(lines: u32, cell: u16) -> u16 {
    let mut digits = 3u16;
    let mut lim = 1000u32;
    while lines >= lim && digits < 10 {
        digits += 1;
        lim = lim.saturating_mul(10);
    }
    digits * cell + 5
}

/// エディタ本体の表示。文書 1 つにつき 1 つ持つ（スクロール位置を覚えるため）。
///
/// `Widget` は実装しない: 文書（`LineSource`）とトークンは `render` のたびに借りる。
pub struct EditorView {
    layouter: Box<dyn LineLayouter>,
    vp: Viewport,
    anchor: Pos,
    head: Pos,
    cursor_on: bool,
    focused: bool,
    /// 次の `render` で、カーソルが見える位置までスクロールする。
    reveal: bool,
    /// 見えている行を全部確かめ直す。
    all_dirty: bool,
    /// 確かめ直す行の範囲（両端を含む）。
    check: Option<(u32, u32)>,
    painted: Vec<Painted>,
    /// この y から領域の下端までは、地の色で塗ってある。
    tail_y: i32,
    last_area: Rect,
    gutter_w: u16,
    gutter_font: TextStyle,
    pool: Vec<LineLayout>,
    marks: Vec<LineMark>,
    pre: Pre,
    /// 直前の `render` で未確定文字列を挟んだ所 `(論理行, 桁, 文字数)`。
    pre_shown: Option<(u32, u32, u32)>,
    text: String,
    spans: Vec<Span>,
    plans: Vec<(Painted, Plan)>,
    page: u32,
    stats: RenderStats,
}

impl EditorView {
    /// `layouter` で行を組むビュー。
    pub fn new(layouter: Box<dyn LineLayouter>) -> EditorView {
        EditorView {
            layouter,
            vp: Viewport::default(),
            anchor: Pos::default(),
            head: Pos::default(),
            cursor_on: true,
            focused: true,
            reveal: true,
            all_dirty: true,
            check: None,
            painted: Vec::new(),
            tail_y: i32::MAX,
            last_area: Rect::default(),
            gutter_w: 0,
            gutter_font: TextStyle::mono(crate::layout::CODE_PX),
            pool: Vec::new(),
            marks: Vec::new(),
            pre: Pre::default(),
            pre_shown: None,
            text: String::new(),
            spans: Vec::new(),
            plans: Vec::new(),
            page: 1,
            stats: RenderStats::default(),
        }
    }
    /// コード表示（等幅・既定の大きさ）のビュー。
    pub fn code() -> EditorView {
        EditorView::new(Box::new(crate::CodeLayouter::standard()))
    }
    /// 行の組み方を差し替える（Markdown のプレビュー ⇔ ソース表示）。
    pub fn set_layouter(&mut self, layouter: Box<dyn LineLayouter>) {
        self.layouter = layouter;
        self.vp.top_off = 0;
        self.vp.scroll_x = 0;
        self.reveal = true;
        self.invalidate();
    }

    fn mark(&mut self, lo: u32, hi: u32) {
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        self.check = Some(match self.check {
            Some((a, b)) => (a.min(lo), b.max(hi)),
            None => (lo, hi),
        });
    }

    /// 選択範囲とカーソル（`head`）を置く。選択が無ければ `anchor == head`。
    /// 動いたら、次の `render` でカーソルが見える位置までスクロールする。
    pub fn set_selection(&mut self, anchor: Pos, head: Pos) {
        if (anchor, head) == (self.anchor, self.head) {
            return;
        }
        let lines = [self.anchor.line, self.head.line, anchor.line, head.line];
        let had_sel = self.anchor != self.head || anchor != head;
        if had_sel {
            self.mark(*lines.iter().min().unwrap(), *lines.iter().max().unwrap());
        } else {
            // 選択なし → 選択なし: 前のカーソル行と新しいカーソル行だけ
            self.mark(self.head.line, self.head.line);
            self.mark(head.line, head.line);
        }
        self.anchor = anchor;
        self.head = head;
        self.reveal = true;
    }
    /// カーソルの位置。
    pub fn cursor(&self) -> Pos {
        self.head
    }
    /// カーソルを描くか（点滅の点いている側か）。
    pub fn set_cursor_on(&mut self, on: bool) {
        if self.cursor_on != on {
            self.cursor_on = on;
            self.mark(self.head.line, self.head.line);
        }
    }
    /// 入力がこのビューに向いているか（向いていなければカーソルを描かない）。
    pub fn set_focused(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.mark(self.head.line, self.head.line);
        }
    }
    /// 編集で行が変わった。その行から下を確かめ直す（下の行は、行番号がずれたり、
    /// ハイライトの文脈が変わったりしうる。中身が同じ行は描き直さない）。
    pub fn lines_changed(&mut self, change: LineChange) {
        self.mark(change.old.start, u32::MAX);
    }
    /// 見えている行を全部確かめ直す（ハイライトの結果が届いた、など。中身が同じ行は
    /// 描き直さない）。
    pub fn recheck_all(&mut self) {
        self.all_dirty = true;
    }
    /// 次の `render` で必ず全部を描き直す（別の画面から戻った、字形が届いた）。
    pub fn invalidate(&mut self) {
        for p in self.painted.drain(..) {
            self.pool.push(p.layout);
        }
        self.tail_y = i32::MAX;
        self.all_dirty = true;
    }
    /// 画面の行 `y0..y1` に重なっている行を、次の `render` で必ず描き直す（上に重ねた
    /// 小窓を閉じた・動かしたとき。ほかの行はそのまま）。
    pub fn invalidate_rows(&mut self, y0: i32, y1: i32) {
        let mut i = 0;
        while i < self.painted.len() {
            let p = &self.painted[i];
            if p.y < y1 && p.y + p.height as i32 > y0 {
                let p = self.painted.remove(i);
                self.pool.push(p.layout);
            } else {
                i += 1;
            }
        }
        // 最後の行より下（地の色で塗ってある所）に重なっていれば、そこも塗り直す
        if y1 > self.tail_y {
            self.tail_y = y1;
        }
    }
    /// 波線。
    pub fn marks(&self) -> &[LineMark] {
        &self.marks
    }
    /// 波線を置き換える。
    pub fn set_marks(&mut self, marks: Vec<LineMark>) {
        if self.marks != marks {
            self.marks = marks;
            self.all_dirty = true;
        }
    }
    /// カーソルの位置に挟んで見せる未確定文字列を置く（`None` か空なら消す）。
    ///
    /// 文書は変えない。カーソル行の、カーソルの桁に挟んで組み直すので、行の残りは
    /// 右へずれ、カーソルは未確定文字列の **後ろ** に出る。描き直すのはカーソル行だけ
    /// （中身が前と同じなら何もしない）。次の `render` で、カーソルが見える位置まで
    /// スクロールする。カーソルが動けば、未確定文字列も新しいカーソルの位置に付いていく。
    pub fn set_preedit(&mut self, preedit: Option<PreeditView>) {
        let view = preedit.unwrap_or_default();
        if view == self.pre.view {
            return;
        }
        let pre = &mut self.pre;
        pre.text.clear();
        pre.segs.clear();
        pre.n = 0;
        for s in view.segs() {
            let n = s.text.chars().count() as u32;
            pre.text.push_str(&s.text);
            pre.segs.push((pre.n..pre.n + n, s.style));
            pre.n += n;
        }
        pre.view = view;
        self.mark(self.head.line, self.head.line);
        self.reveal = true;
    }
    /// いま置いてある未確定文字列（無ければ `None`）。
    pub fn preedit(&self) -> Option<&PreeditView> {
        (self.pre.n > 0).then_some(&self.pre.view)
    }
    /// 未確定文字列の先頭（`▽` / `▼` の左端）の画面上の矩形（幅 1）。直前の `render` で
    /// 未確定文字列を描いていなければ `None`。折り返して 2 行にまたがるときも、先頭の
    /// ある表示行を返す（後ろの端は [`EditorView::caret_rect`]）。
    pub fn preedit_rect(&self) -> Option<Rect> {
        let (line, at, _) = self.pre_shown?;
        let p = self.painted.iter().find(|p| p.line == line)?;
        Some(self.rect_at(p, at))
    }

    /// 表示位置。
    pub fn viewport(&self) -> Viewport {
        self.vp
    }
    /// 表示位置を置く（カーソルは追わない）。
    pub fn set_viewport(&mut self, vp: Viewport) {
        if vp.scroll_x != self.vp.scroll_x {
            self.all_dirty = true;
        }
        self.vp = vp;
    }
    /// 論理行 `delta` 行ぶん縦にスクロールする（正で下へ）。カーソルは動かさない
    /// （カーソルが `set_selection` で動いていれば、そのあとカーソルが見える位置に寄る）。
    pub fn scroll_lines(&mut self, delta: i32) {
        self.vp.top = (self.vp.top as i64 + delta as i64).clamp(0, u32::MAX as i64) as u32;
        self.vp.top_off = 0;
    }
    /// 次の `render` で、カーソルが見える位置までスクロールする。
    pub fn reveal_cursor(&mut self) {
        self.reveal = true;
    }
    /// 1 画面に全部入る論理行の数（直前の `render` の結果。PgUp / PgDn の移動量）。
    pub fn page_lines(&self) -> u32 {
        self.page.max(1)
    }
    /// 直前の `render` の内訳。
    pub fn stats(&self) -> RenderStats {
        self.stats
    }
    /// 行番号の欄の幅（直前の `render` の結果）。
    pub fn gutter_w(&self) -> u16 {
        self.gutter_w
    }

    /// 本文の左端（スクロール込み）の画面上の x。
    fn origin_x(&self) -> i32 {
        self.last_area.x as i32 + self.gutter_w as i32 - self.vp.scroll_x
    }
    /// 描いてある行 `p` の、表示上の桁 `col` の境目の矩形。
    fn rect_at(&self, p: &Painted, col: u32) -> Rect {
        let c = p.layout.caret(col);
        Rect::new((self.origin_x() + c.x) as i16, (p.y + c.y as i32) as i16, 1, c.height)
    }
    /// 位置 `pos` のカーソルの画面上の矩形（幅 1）。その行が画面に出ていなければ `None`。
    /// 候補一覧などの小窓を置く位置に使う。直前の `render` の結果から求める。
    ///
    /// 未確定文字列（[`EditorView::set_preedit`]）を挟んだ行では、カーソルの桁と
    /// それより右の桁は、未確定文字列の **後ろ** の位置を返す（未確定文字列の幅だけ
    /// 右へずれる）。未確定文字列の先頭は [`EditorView::preedit_rect`]。
    pub fn point_of(&self, pos: Pos) -> Option<Rect> {
        let p = self.painted.iter().find(|p| p.line == pos.line)?;
        let col = match self.pre_shown {
            Some((line, at, n)) if line == pos.line => to_display(pos.col.min(p.layout.len_chars().saturating_sub(n)), at, n, true),
            _ => pos.col,
        };
        Some(self.rect_at(p, col))
    }
    /// カーソルの画面上の矩形（未確定文字列があれば、その後ろ）。
    pub fn caret_rect(&self) -> Option<Rect> {
        self.point_of(self.head)
    }
    /// 画面上の点にいちばん近い文字の境目。領域より上・下の点は、見えている最初・最後の
    /// 行に丸める。何も描いていなければ `None`。未確定文字列の上の点は、それを挟んだ桁
    /// （カーソルの桁）になる。
    pub fn pos_at(&self, x: i32, y: i32) -> Option<Pos> {
        let p = match self.painted.iter().find(|p| y < p.y + p.height as i32) {
            Some(p) => p,
            None => self.painted.last()?,
        };
        let row = p.layout.row_at_y(y - p.y);
        let mut col = p.layout.col_at(row, x - self.origin_x());
        if let Some((line, at, n)) = self.pre_shown {
            if line == p.line && col > at {
                col = col.saturating_sub(n).max(at);
            }
        }
        Some(Pos::new(p.line, col))
    }

    /// `area` に描き、描き変えた行範囲を返す。
    pub fn render(&mut self, cx: &mut RenderCx<'_>, area: Rect, src: &dyn LineSource, spans: &mut dyn SpanSource) -> Damage {
        let mut stats = RenderStats::default();
        if area.is_empty() {
            return Damage::none();
        }
        let n = src.len_lines().max(1);
        let cell = self.layouter.cell_width(cx.fonts);
        let gutter_w = if self.layouter.gutter() { gutter_width(n, cx.fonts.metrics(&self.gutter_font).cell_w) } else { 0 };
        if area != self.last_area || gutter_w != self.gutter_w {
            self.invalidate();
            self.last_area = area;
            self.gutter_w = gutter_w;
        }
        let uniform = self.layouter.uniform_height();
        let partial = uniform.is_none();
        let view_h = match uniform {
            Some(h) if h > 0 && area.h >= h => area.h / h * h,
            _ => area.h,
        };
        let text_w = (area.w as i32 - gutter_w as i32).max(1);
        let (top_y, bottom_y) = (area.y as i32, area.y as i32 + view_h as i32);

        // --- 表示位置を決める ---
        if self.vp.top >= n {
            self.vp.top = n - 1;
            self.vp.top_off = 0;
        }
        if !partial {
            self.vp.top_off = 0;
        }
        let head = Pos::new(self.head.line.min(n - 1), self.head.col);
        let sel = (self.anchor.min(self.head), self.anchor.max(self.head));
        let has_sel = sel.0 != sel.1;
        let cursor_dep = self.layouter.depends_on_cursor();
        let pre_n = self.pre.n;
        if std::mem::take(&mut self.reveal) {
            let mut tmp = self.pool.pop().unwrap_or_default();
            src.line(head.line, &mut self.text);
            spans.spans(head.line, &self.text, &mut self.spans);
            let mut caret_col = head.col;
            if pre_n > 0 {
                caret_col = splice_preedit(&mut self.text, &mut self.spans, head.col, &self.pre.text) + pre_n;
            }
            let input = LineInput { line: head.line, text: &self.text, spans: &self.spans, cursor_line: true, wrap_width: text_w };
            self.layouter.layout(cx.fonts, cx.theme, &input, &mut tmp);
            let caret = tmp.caret(caret_col);
            let (layouter, text, sp, pre_text) = (&mut self.layouter, &mut self.text, &mut self.spans, &self.pre.text);
            let mut scratch = self.pool.pop().unwrap_or_default();
            let mut height_of = |l: u32| -> u16 {
                if let Some(h) = uniform {
                    return h;
                }
                src.line(l, text);
                spans.spans(l, text, sp);
                if pre_n > 0 && l == head.line {
                    splice_preedit(text, sp, head.col, pre_text);
                }
                let input = LineInput { line: l, text, spans: sp, cursor_line: l == head.line, wrap_width: text_w };
                layouter.layout(cx.fonts, cx.theme, &input, &mut scratch);
                scratch.height()
            };
            self.vp.reveal_row(head.line, caret.y, caret.height, view_h, partial, &mut height_of);
            self.pool.push(scratch);
            if self.layouter.wraps() {
                if self.vp.scroll_x != 0 {
                    self.vp.scroll_x = 0;
                    self.all_dirty = true;
                }
            } else if self.vp.reveal_x(caret.x, text_w, cell as i32 * 4) {
                self.all_dirty = true;
            }
            self.pool.push(tmp);
        }

        // --- 見えている行を並べ、前回のまま使えるか確かめる ---
        let mut old: Vec<Option<Painted>> = self.painted.drain(..).map(Some).collect();
        let mut plans = std::mem::take(&mut self.plans);
        plans.clear();
        let mut y = top_y - self.vp.top_off as i32;
        let mut line = self.vp.top;
        while line < n && y < bottom_y {
            let prev = old.iter_mut().find(|p| p.as_ref().is_some_and(|p| p.line == line)).and_then(Option::take);
            let need = self.all_dirty || prev.is_none() || self.check.is_some_and(|(a, b)| a <= line && line <= b);
            let entry = match (need, prev) {
                (false, Some(p)) => {
                    let oy = p.y;
                    (Painted { y, ..p }, Plan::Reuse(oy))
                }
                (_, prev) => {
                    stats.lines_checked += 1;
                    let mut lay = self.pool.pop().unwrap_or_default();
                    src.line(line, &mut self.text);
                    spans.spans(line, &self.text, &mut self.spans);
                    let is_cur = line == head.line;
                    if is_cur && pre_n > 0 {
                        splice_preedit(&mut self.text, &mut self.spans, head.col, &self.pre.text);
                    }
                    let input = LineInput { line, text: &self.text, spans: &self.spans, cursor_line: is_cur && cursor_dep, wrap_width: text_w };
                    self.layouter.layout(cx.fonts, cx.theme, &input, &mut lay);
                    let sig = self.line_sig(line, is_cur, head.col, sel, has_sel, &lay);
                    let height = lay.height();
                    match prev {
                        Some(p) if p.sig == sig && p.height == height => {
                            let oy = p.y;
                            self.pool.push(p.layout);
                            (Painted { line, y, height, sig, layout: lay }, Plan::Reuse(oy))
                        }
                        prev => {
                            if let Some(p) = prev {
                                self.pool.push(p.layout);
                            }
                            (Painted { line, y, height, sig, layout: lay }, Plan::Paint)
                        }
                    }
                }
            };
            y += entry.0.height.max(1) as i32;
            plans.push(entry);
            line += 1;
        }
        for p in old.into_iter().flatten() {
            self.pool.push(p.layout);
        }
        let content_bottom = y.min(area.bottom());

        // --- スクロール: 使い回せる行が全部同じだけ動いていれば、memmove でずらす ---
        let mut dy: Option<i32> = None;
        let mut mixed = false;
        for (p, plan) in &plans {
            if let Plan::Reuse(oy) = plan {
                match dy {
                    None => dy = Some(p.y - oy),
                    Some(d) if d != p.y - oy => mixed = true,
                    _ => {}
                }
            }
        }
        let dy = if mixed { 0 } else { dy.unwrap_or(0) };
        if dy != 0 {
            cx.canvas.scroll_rows(area, dy, palette::BG);
            stats.scrolled = dy;
            self.tail_y = self.tail_y.saturating_add(dy).clamp(top_y, area.bottom());
        }
        let inside = |y: i32, h: u16| y >= top_y && y + h as i32 <= area.bottom();
        for (p, plan) in &mut plans {
            if let Plan::Reuse(oy) = *plan {
                // ずらした行は、ずらす前も後も全部が領域の中にあるときだけ画素が正しい
                let ok = if dy != 0 { p.y - oy == dy && inside(oy, p.height) && inside(p.y, p.height) } else { p.y == oy };
                if !ok {
                    *plan = Plan::Paint;
                }
            }
        }

        // --- 描く ---
        let mut damage = Damage::none();
        let hl_line = !has_sel;
        let mut lut = LutCache::default();
        for (p, plan) in &plans {
            if !matches!(plan, Plan::Paint) {
                continue;
            }
            stats.lines_painted += 1;
            src.line(p.line, &mut self.text);
            let is_cur = p.line == head.line;
            // 未確定文字列を挟んだ桁（挟んでいなければ None）
            let pre_at = (is_cur && pre_n > 0).then(|| {
                self.spans.clear();
                splice_preedit(&mut self.text, &mut self.spans, head.col, &self.pre.text)
            });
            let doc_len = p.layout.len_chars().saturating_sub(if pre_at.is_some() { pre_n } else { 0 });
            let params = PaintLine {
                area,
                clip_bottom: if partial { area.bottom() } else { bottom_y },
                gutter_w,
                gutter_font: self.gutter_font,
                origin_x: area.x as i32 + gutter_w as i32 - self.vp.scroll_x,
                line: p.line,
                y: p.y,
                cur: is_cur,
                bg: if is_cur && hl_line { palette::LINE } else { palette::BG },
                sel: line_selection(p.line, sel, has_sel, doc_len),
                caret: (is_cur && self.focused && self.cursor_on).then_some(head.col.min(doc_len)),
                newline_w: cell,
                marks: &self.marks,
                preedit: pre_at.map(|at| (at, pre_n, &self.pre.segs[..])),
            };
            paint_line(cx, &p.layout, &self.text, &params, &mut lut);
            let rows = RowRange::new(p.y.max(top_y) as u16, (p.y + p.height as i32).clamp(top_y, area.bottom()) as u16);
            damage.add(rows, if is_cur { Priority::Cursor } else { Priority::Edit });
        }
        // 最後の行より下に、前の内容が残っていれば消す
        if content_bottom < self.tail_y {
            let end = self.tail_y.min(area.bottom());
            let r = Rect::new(area.x, content_bottom as i16, area.w, (end - content_bottom).max(0) as u16);
            cx.canvas.fill_rect(r, palette::BG);
            damage.add(r.rows(), Priority::Edit);
        }
        self.tail_y = content_bottom;
        if dy != 0 {
            // ずらしたときは領域の全部の行が変わっている
            damage = Damage::rect(area);
        }

        stats.lines_visible = plans.len() as u32;
        self.page = plans.iter().filter(|(p, _)| inside(p.y, p.height) && p.y + p.height as i32 <= bottom_y).count().max(1) as u32;
        if let Some(h) = uniform {
            self.page = (view_h / h.max(1)).max(1) as u32;
        }
        self.painted.extend(plans.drain(..).map(|(p, _)| p));
        self.plans = plans;
        self.pre_shown = match self.painted.iter().find(|p| p.line == head.line) {
            Some(p) if pre_n > 0 => Some((head.line, head.col.min(p.layout.len_chars().saturating_sub(pre_n)), pre_n)),
            _ => None,
        };
        self.all_dirty = false;
        self.check = None;
        self.stats = stats;
        damage
    }

    /// 論理行 1 つの「描く内容の印」。これが同じなら、描いた結果も同じ。
    #[allow(clippy::too_many_arguments)]
    fn line_sig(&self, line: u32, is_cur: bool, cur_col: u32, sel: (Pos, Pos), has_sel: bool, lay: &LineLayout) -> u64 {
        let mut h = Fnv::new();
        h.bytes(self.text.as_bytes());
        h.u32(0xffff_ffff);
        for s in &self.spans {
            h.u32(s.start);
            h.u32(s.end);
            h.u32(s.kind as u32);
        }
        h.u32(line);
        h.u32(self.vp.scroll_x as u32);
        h.u32(lay.height() as u32 | (lay.rows().len() as u32) << 16);
        if lay.sig() != 0 {
            h.u32(0x4c41_5900);
            h.u32(lay.sig());
        }
        let caret = is_cur && self.focused && self.cursor_on;
        h.u32(is_cur as u32 | (caret as u32) << 1 | (has_sel as u32) << 2);
        if caret {
            h.u32(cur_col);
        }
        if is_cur && self.pre.n > 0 {
            // 文字そのものは `self.text` に挟んである。ここでは挟んだ桁と区間の見た目
            h.u32(0x5052_4500);
            h.u32(cur_col);
            for (r, st) in &self.pre.segs {
                h.u32(r.end);
                h.u32(*st as u32);
            }
        }
        match line_selection(line, sel, has_sel, u32::MAX) {
            Some((a, b, nl)) => {
                h.u32(a);
                h.u32(b);
                h.u32(nl as u32 + 1);
            }
            None => h.u32(0),
        }
        for m in self.marks.iter().filter(|m| m.line == line) {
            h.u32(m.color.0 as u32 | 0x1_0000);
            match &m.cols {
                Some(r) => {
                    h.u32(r.start);
                    h.u32(r.end);
                }
                None => h.u32(u32::MAX),
            }
        }
        h.0
    }
}

/// 論理行 `line` の中で選ばれている桁の範囲と、改行まで選ばれているか。
fn line_selection(line: u32, sel: (Pos, Pos), has_sel: bool, len: u32) -> Option<(u32, u32, bool)> {
    if !has_sel || line < sel.0.line || line > sel.1.line {
        return None;
    }
    let a = if line == sel.0.line { sel.0.col.min(len) } else { 0 };
    let b = if line == sel.1.line { sel.1.col.min(len) } else { len };
    Some((a, b.max(a), line < sel.1.line))
}

/// FNV-1a（64bit）。行の内容の印に使う。
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    #[inline]
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    #[inline]
    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }
}

/// 直前に使った色の表を覚えておく（同じ色の文字が続くことが多い）。
#[derive(Default)]
struct LutCache(Option<(Color, Color, BlendLut)>);

impl LutCache {
    fn get(&mut self, fg: Color, bg: Color) -> BlendLut {
        match self.0 {
            Some((f, b, l)) if f == fg && b == bg => l,
            _ => {
                let l = BlendLut::new(fg, bg);
                self.0 = Some((fg, bg, l));
                l
            }
        }
    }
}

struct PaintLine<'a> {
    area: Rect,
    /// これより下には描かない。
    clip_bottom: i32,
    gutter_w: u16,
    gutter_font: TextStyle,
    /// 本文の x = 0 の画面上の位置（横スクロール込み）。
    origin_x: i32,
    line: u32,
    /// 論理行の上端の画面上の y。
    y: i32,
    cur: bool,
    bg: Color,
    sel: Option<(u32, u32, bool)>,
    caret: Option<u32>,
    newline_w: u16,
    marks: &'a [LineMark],
    /// 未確定文字列: `(挟んだ桁, 文字数, 区間ごとの文字位置と見た目)`。あれば、`sel`・
    /// `caret`・`marks` の桁（文書の桁）を表示上の桁に直してから使う。
    preedit: Option<(u32, u32, &'a [PreSeg])>,
}

/// 論理行 1 つを描く（背景 → 行番号 → 選択 → 文字 → 下線・波線 → カーソル）。
fn paint_line(cx: &mut RenderCx<'_>, lay: &LineLayout, text: &str, p: &PaintLine<'_>, lut: &mut LutCache) {
    let area = p.area;
    let mut chars = text.chars();
    let nrows = lay.rows().len();
    // 文書の桁 → 表示上の桁（未確定文字列を挟んだ行だけずれる）
    let disp = |col: u32, after: bool| match p.preedit {
        Some((at, n, _)) => to_display(col, at, n, after),
        None => col,
    };
    // 表示上の桁 `c` の文字が未確定文字列なら、その見た目
    let pre_style = |c: u32| {
        let (at, n, segs) = p.preedit?;
        if c < at || c >= at + n {
            return None;
        }
        segs.iter().find(|(r, _)| r.contains(&(c - at))).map(|(_, st)| *st)
    };
    let caret = p.caret.map(|c| lay.caret(disp(c, true)));
    for (ri, row) in lay.rows().iter().enumerate() {
        let ry = p.y + row.y as i32;
        let top = ry.max(area.y as i32);
        let bot = (ry + row.height as i32).min(p.clip_bottom);
        let xs = lay.xs(row);
        if bot <= top {
            // 見えない表示行: 文字だけ読み進める
            for _ in row.chars.clone() {
                chars.next();
            }
            continue;
        }
        let h = (bot - top) as u16;
        cx.canvas.fill_rect(Rect::new(area.x, top as i16, area.w, h), p.bg);
        let base = ry + row.baseline as i32;
        // 行番号（最初の表示行だけ）
        if p.gutter_w > 0 && ri == 0 {
            let mut buf = [0u8; 10];
            let s = fmt_u32(p.line + 1, &mut buf);
            let g = Rect::new(area.x, top as i16, p.gutter_w.saturating_sub(3), h);
            let w = cx.fonts.measure(s, &p.gutter_font);
            let fg = if p.cur { palette::GUTTER_CUR } else { palette::GUTTER };
            cx.text((g.right() - w).max(g.x as i32), base, s, &p.gutter_font, fg, p.bg, g);
        }
        let clip = Rect::new(area.x + p.gutter_w as i16, top as i16, area.w.saturating_sub(p.gutter_w), h);
        let (cl, cr) = (clip.x as i32, clip.right());
        // 行の飾り（この表示行にかかる分だけ）
        for d in lay.decos() {
            let (dx, dy) = (p.origin_x + d.x, p.y + d.y);
            match d.kind {
                DecoKind::Fill { w, h } => {
                    let (x0, x1) = (dx.clamp(cl, cr), (dx + w as i32).clamp(cl, cr));
                    let (y0, y1) = (dy.clamp(top, bot), (dy + h as i32).clamp(top, bot));
                    if x1 > x0 && y1 > y0 {
                        cx.canvas.fill_rect(Rect::new(x0 as i16, y0 as i16, (x1 - x0) as u16, (y1 - y0) as u16), d.color);
                    }
                }
                DecoKind::Glyph { ch, font } => {
                    if dy >= ry && dy < ry + row.height as i32 {
                        let l = lut.get(d.color, p.bg);
                        cx.fonts.draw_char(cx.canvas, dx, dy, ch, &font, &l, clip);
                    }
                }
            }
        }
        let x_of = |col: u32| p.origin_x + xs[(col.clamp(row.chars.start, row.chars.end) - row.chars.start) as usize];
        // 選択範囲の背景
        let sel = p.sel.map(|(a, b, nl)| {
            let (a, b) = (disp(a, true), disp(b, false).max(disp(a, true)));
            let (a, b) = (a.clamp(row.chars.start, row.chars.end), b.clamp(row.chars.start, row.chars.end));
            (a, b, nl && ri + 1 == nrows)
        });
        if let Some((a, b, nl)) = sel {
            let (x0, x1) = (x_of(a), x_of(b) + if nl { p.newline_w as i32 } else { 0 });
            if x1 > x0 {
                let r = Rect::new(x0.clamp(cl, cr) as i16, top as i16, (x1.clamp(cl, cr) - x0.clamp(cl, cr)) as u16, h);
                cx.canvas.fill_rect(r, palette::SELECTION);
            }
        }
        let selected = |c: u32| sel.is_some_and(|(a, b, _)| a <= c && c < b);
        // 未確定文字列の背景（変換中）。選択の上に重ねる
        if let Some((at, _, segs)) = p.preedit {
            for (r, st) in segs {
                let (Some(bg), x0, x1) = (st.colors().1, x_of(at + r.start), x_of(at + r.end)) else { continue };
                if x1 > cl && x0 < cr {
                    cx.canvas.fill_rect(Rect::new(x0.clamp(cl, cr) as i16, top as i16, (x1.clamp(cl, cr) - x0.clamp(cl, cr)) as u16, h), bg);
                }
            }
        }
        // 文字
        for run in lay.runs(row) {
            if run.hidden {
                // 隠した文字: 読み進めるだけ
                for _ in run.chars.clone() {
                    chars.next();
                }
                continue;
            }
            let (rx0, rx1) = (x_of(run.chars.start), x_of(run.chars.end));
            if let Some(bg) = run.style.bg {
                let inset = lay.bg_inset() as i32;
                let (y0, y1) = ((ry + inset).max(top), (ry + row.height as i32 - inset).min(bot));
                if rx1 > cl && rx0 < cr && y1 > y0 {
                    let r = Rect::new(rx0.clamp(cl, cr) as i16, y0 as i16, (rx1.clamp(cl, cr) - rx0.clamp(cl, cr)) as u16, (y1 - y0) as u16);
                    cx.canvas.fill_rect(r, bg);
                }
            }
            let run_bg = run.style.bg.unwrap_or(p.bg);
            for c in run.chars.clone() {
                let Some(ch) = chars.next() else { break };
                let i = (c - row.chars.start) as usize;
                let (x0, x1) = (p.origin_x + xs[i], p.origin_x + xs[i + 1]);
                if x1 <= cl || x0 >= cr || ch == ' ' || ch.is_control() {
                    continue;
                }
                let (fg, bg) = match pre_style(c) {
                    Some(st) => (st.colors().0, st.colors().1.unwrap_or(p.bg)),
                    None => (run.style.fg, if selected(c) { palette::SELECTION } else { run_bg }),
                };
                let l = lut.get(fg, bg);
                cx.fonts.draw_char(cx.canvas, x0, base, ch, &run.font, &l, clip);
            }
            let attrs = run.style.attrs;
            if attrs.contains(Attrs::UNDERLINE) || attrs.contains(Attrs::STRIKE) {
                let (a, b) = (rx0.clamp(cl, cr), rx1.clamp(cl, cr));
                if b > a {
                    if attrs.contains(Attrs::UNDERLINE) {
                        cx.canvas.fill_rect(Rect::new(a as i16, (base + 1) as i16, (b - a) as u16, 1).intersect(&clip), run.style.fg);
                    }
                    if attrs.contains(Attrs::STRIKE) {
                        cx.canvas.fill_rect(Rect::new(a as i16, (base - 3) as i16, (b - a) as u16, 1).intersect(&clip), run.style.fg);
                    }
                }
            }
            if attrs.contains(Attrs::SQUIGGLE) {
                squiggle(cx, rx0, rx1, base + 1, palette::ERROR, clip);
            }
        }
        // 未確定文字列の下線（表示行のいちばん下の画素行）
        if let Some((at, _, segs)) = p.preedit {
            let uy = ry + row.height as i32 - 1;
            for (r, st) in segs {
                let (a, b) = (x_of(at + r.start).clamp(cl, cr), x_of(at + r.end).clamp(cl, cr));
                if b > a {
                    cx.canvas.fill_rect(Rect::new(a as i16, uy as i16, (b - a) as u16, 1).intersect(&clip), st.colors().2);
                }
            }
        }
        // 波線
        for m in p.marks.iter().filter(|m| m.line == p.line) {
            let (a, b) = match &m.cols {
                Some(r) => (disp(r.start, true), disp(r.end.max(r.start + 1), false)),
                None => (0, u32::MAX),
            };
            let (a, b) = (a.clamp(row.chars.start, row.chars.end), b.clamp(row.chars.start, row.chars.end));
            let (x0, mut x1) = (x_of(a), x_of(b));
            if x1 <= x0 && ri + 1 == nrows && m.cols.as_ref().is_none_or(|r| disp(r.start, true) >= row.chars.end) {
                x1 = x0 + p.newline_w as i32; // 空の行・行末: 1 字ぶんだけ引く
            }
            squiggle(cx, x0, x1, base + 1, m.color, clip);
        }
        // カーソル（幅 1。上下 1 画素あける）
        if let Some(c) = caret {
            if c.row as usize == ri {
                let x = p.origin_x + c.x;
                let r = Rect::new(x as i16, (ry + 1) as i16, 1, row.height.saturating_sub(2).max(1));
                if x >= cl && x < cr {
                    cx.canvas.fill_rect(r.intersect(&clip), palette::CURSOR);
                }
            }
        }
    }
}

/// 高さ 2 の波線。
fn squiggle(cx: &mut RenderCx<'_>, x0: i32, x1: i32, y: i32, color: Color, clip: Rect) {
    for x in x0.max(clip.x as i32)..x1.min(clip.right()) {
        let yy = y + ((x >> 1) & 1);
        if clip.contains(x, yy) {
            cx.canvas.put(x, yy, color);
        }
    }
}

/// 10 進の文字列にする（確保なし）。
fn fmt_u32(mut v: u32, buf: &mut [u8; 10]) -> &str {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[i..]).expect("数字だけ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::CODE_LINE_H;
    use zc_core::{Canvas, LineRange, Theme, TokenKind};
    use zc_font::{FontConfig, Fonts, RasterMode};

    // --- 表示位置の計算（フォント不要） ---

    #[test]
    fn reveal_row_uniform() {
        let mut h = |_: u32| 12u16;
        let mut vp = Viewport::default();
        // 12 行ぶんの表示域（144px）。行 0〜11 は動かない
        for l in 0..12 {
            assert!(!vp.reveal_row(l, 0, 12, 144, false, &mut h), "{l}");
        }
        // 行 12 は 1 行ぶん下へ
        assert!(vp.reveal_row(12, 0, 12, 144, false, &mut h));
        assert_eq!((vp.top, vp.top_off), (1, 0));
        // 遠くへ飛ぶ: その行が最下段になる
        assert!(vp.reveal_row(500, 0, 12, 144, false, &mut h));
        assert_eq!(vp.top, 489);
        // 上へ戻る: その行が最上段になる
        assert!(vp.reveal_row(100, 0, 12, 144, false, &mut h));
        assert_eq!(vp.top, 100);
        assert!(!vp.reveal_row(111, 0, 12, 144, false, &mut h));
        // 表示域が 1 行より低い
        let mut vp = Viewport { top: 3, ..Default::default() };
        assert!(vp.reveal_row(9, 0, 12, 8, false, &mut h));
        assert_eq!((vp.top, vp.top_off), (9, 0));
    }

    #[test]
    fn reveal_row_variable_heights() {
        // 行 0 は見出し（17）、偶数行は 12、奇数行は 24（折り返して 2 行）
        let mut h = |l: u32| if l == 0 { 17 } else if l % 2 == 0 { 12 } else { 24 };
        let mut vp = Viewport::default();
        // 行 0..=3 で 17+24+12+24 = 77。表示域 80: 行 3 までは入る
        assert!(!vp.reveal_row(3, 12, 12, 80, true, &mut h));
        // 行 4（上端 77、高さ 12）は 9px はみ出す → 下端に合わせる: 行 0 の 9px が隠れる
        assert!(vp.reveal_row(4, 0, 12, 80, true, &mut h));
        assert_eq!((vp.top, vp.top_off), (0, 9));
        // 途中から表示しない設定なら、入るだけの行を上に積む（行 1 から）
        let mut vp2 = Viewport::default();
        assert!(vp2.reveal_row(4, 0, 12, 80, false, &mut h));
        assert_eq!((vp2.top, vp2.top_off), (1, 0));
        // 上に隠れた表示行へ: その表示行を上端に
        assert!(vp.reveal_row(0, 0, 17, 80, true, &mut h));
        assert_eq!((vp.top, vp.top_off), (0, 0));
        let mut vp = Viewport { top: 5, top_off: 12, scroll_x: 0 };
        assert!(vp.reveal_row(5, 0, 12, 80, true, &mut h));
        assert_eq!((vp.top, vp.top_off), (5, 0));
        // 行 5 の 2 番目の表示行（y=12）だけが入る高さ
        let mut vp = Viewport { top: 0, ..Default::default() };
        assert!(vp.reveal_row(5, 12, 12, 20, true, &mut h));
        assert_eq!((vp.top, vp.top_off), (5, 4));
    }

    #[test]
    fn reveal_x_keeps_margin() {
        let mut vp = Viewport::default();
        assert!(!vp.reveal_x(0, 200, 24));
        assert!(!vp.reveal_x(175, 200, 24));
        assert!(vp.reveal_x(176, 200, 24));
        assert_eq!(vp.scroll_x, 1);
        assert!(vp.reveal_x(600, 200, 24));
        assert_eq!(vp.scroll_x, 425);
        assert!(!vp.reveal_x(500, 200, 24));
        assert!(vp.reveal_x(430, 200, 24));
        assert_eq!(vp.scroll_x, 406);
        assert!(vp.reveal_x(0, 200, 24));
        assert_eq!(vp.scroll_x, 0);
        // 幅が狭いときは余白を縮める
        let mut vp = Viewport::default();
        assert!(vp.reveal_x(30, 10, 24));
        assert_eq!(vp.scroll_x, 25);
    }

    #[test]
    fn gutter_width_grows_with_digits() {
        assert_eq!(gutter_width(1, 6), 23);
        assert_eq!(gutter_width(999, 6), 23);
        assert_eq!(gutter_width(1000, 6), 29);
        assert_eq!(gutter_width(123_456, 6), 41);
        assert_eq!(crate::layout::GUTTER_W, gutter_width(1, 6));
    }

    #[test]
    fn line_selection_ranges() {
        let sel = (Pos::new(2, 3), Pos::new(4, 1));
        assert_eq!(line_selection(1, sel, true, 10), None);
        assert_eq!(line_selection(2, sel, true, 10), Some((3, 10, true)));
        assert_eq!(line_selection(3, sel, true, 0), Some((0, 0, true)));
        assert_eq!(line_selection(4, sel, true, 10), Some((0, 1, false)));
        assert_eq!(line_selection(3, sel, false, 10), None);
    }

    // --- 描画（フォントが要る） ---

    /// テスト用の文書（行の配列）。
    struct Doc<'a>(&'a [String]);

    impl LineSource for Doc<'_> {
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

    struct Fx {
        fonts: Fonts,
        theme: Theme,
        canvas: Canvas,
        view: EditorView,
        lines: Vec<String>,
    }

    /// 上に 13、下に 11 の余白がある全幅の領域（12 行ぶん + 2px）。
    const AREA: Rect = Rect::new(0, 13, 320, 146);

    impl Fx {
        fn new(n: usize) -> Option<Fx> {
            let cfg = FontConfig::default();
            if !cfg.available() {
                eprintln!("フォントが無いので飛ばす");
                return None;
            }
            Some(Fx {
                fonts: Fonts::load(&cfg, RasterMode::Inline).unwrap(),
                theme: Theme::dark_plus(),
                canvas: Canvas::new(),
                view: EditorView::code(),
                lines: (0..n).map(|i| format!("line {i} = value_{i}")).collect(),
            })
        }
        fn render_in(&mut self, area: Rect) -> Damage {
            let mut cx = RenderCx::new(&mut self.canvas, &mut self.fonts, &self.theme);
            self.view.render(&mut cx, area, &Doc(&self.lines), &mut NoSpans)
        }
        fn render(&mut self) -> Damage {
            self.render_in(AREA)
        }
        /// 同じ状態を、まっさらな Canvas とビューで描いたもの（差分描画の答え合わせ）。
        fn fresh(&mut self) -> Canvas {
            let mut view = EditorView::code();
            view.set_selection(self.view.anchor, self.view.head);
            view.reveal = false;
            view.vp = self.view.vp;
            view.cursor_on = self.view.cursor_on;
            view.focused = self.view.focused;
            view.marks = self.view.marks.clone();
            view.set_preedit(self.view.preedit().cloned());
            view.reveal = false;
            let mut canvas = Canvas::new();
            let mut cx = RenderCx::new(&mut canvas, &mut self.fonts, &self.theme);
            view.render(&mut cx, AREA, &Doc(&self.lines), &mut NoSpans);
            canvas
        }
        fn assert_matches_fresh(&mut self, what: &str) {
            let fresh = self.fresh();
            for y in 0..170u16 {
                assert!(self.canvas.row(y) == fresh.row(y), "{what}: 画素行 {y} が、描き直した結果と違う");
            }
        }
        fn caret(&mut self, line: u32, col: u32) {
            self.view.set_selection(Pos::new(line, col), Pos::new(line, col));
        }
    }

    fn ranges(d: &Damage) -> Vec<(u16, u16, Priority)> {
        let mut v: Vec<_> = d.iter().map(|(r, p)| (r.start, r.end, p)).collect();
        v.sort();
        v
    }

    #[test]
    fn first_render_then_idle() {
        let Some(mut f) = Fx::new(100) else { return };
        let d = f.render();
        assert_eq!(d.bounds(), RowRange::new(13, 159));
        assert_eq!(f.view.stats(), RenderStats { lines_visible: 12, lines_checked: 12, lines_painted: 12, scrolled: 0 });
        assert_eq!(f.view.page_lines(), 12);
        assert_eq!(f.view.gutter_w(), 23);
        // 何も変わらなければ、何も読まず何も描かない
        let d = f.render();
        assert!(d.is_empty());
        assert_eq!(f.view.stats(), RenderStats { lines_visible: 12, lines_checked: 0, lines_painted: 0, scrolled: 0 });
        // 領域の外は触らない
        assert!(f.canvas.row(12).iter().all(|&p| p == 0));
        assert!(f.canvas.row(159).iter().all(|&p| p == 0));
        // 12 行ぶんより下の 2px は地の色
        assert!(f.canvas.row(158).iter().all(|&p| p == palette::BG.0));
    }

    #[test]
    fn typing_repaints_only_cursor_line() {
        let Some(mut f) = Fx::new(100) else { return };
        f.caret(4, 3);
        f.render();
        // 行 4 に 1 字入れる
        f.lines[4].insert(3, 'X');
        f.view.lines_changed(LineChange { old: LineRange::new(4, 5), new: LineRange::new(4, 5) });
        f.caret(4, 4);
        let d = f.render();
        let y = 13 + 4 * CODE_LINE_H;
        assert_eq!(ranges(&d), vec![(y, y + CODE_LINE_H, Priority::Cursor)]);
        // 行 4 から下の 8 行を確かめ、描いたのは 1 行
        assert_eq!(f.view.stats(), RenderStats { lines_visible: 12, lines_checked: 8, lines_painted: 1, scrolled: 0 });
        f.assert_matches_fresh("1 字入力");
    }

    #[test]
    fn cursor_move_repaints_old_and_new_line() {
        let Some(mut f) = Fx::new(100) else { return };
        f.caret(2, 0);
        f.render();
        f.caret(3, 0);
        let d = f.render();
        let y2 = 13 + 2 * CODE_LINE_H;
        assert_eq!(ranges(&d), vec![(y2, y2 + CODE_LINE_H, Priority::Edit), (y2 + CODE_LINE_H, y2 + 2 * CODE_LINE_H, Priority::Cursor)]);
        assert_eq!(f.view.stats().lines_checked, 2);
        f.assert_matches_fresh("カーソル移動");
        // 点滅: カーソル行だけ
        f.view.set_cursor_on(false);
        let d = f.render();
        assert_eq!(ranges(&d), vec![(y2 + CODE_LINE_H, y2 + 2 * CODE_LINE_H, Priority::Cursor)]);
        f.assert_matches_fresh("点滅");
        let lit = |c: &Canvas| (13..159).any(|y| c.row(y).contains(&palette::CURSOR.0));
        assert!(!lit(&f.canvas));
        f.view.set_cursor_on(true);
        f.render();
        assert!(lit(&f.canvas));
    }

    #[test]
    fn scroll_uses_memmove_and_paints_only_exposed_lines() {
        let Some(mut f) = Fx::new(100) else { return };
        f.caret(11, 0);
        f.render();
        // 1 行下へ: 行 12 が最下段に出る
        f.caret(12, 0);
        let d = f.render();
        assert_eq!(f.view.viewport().top, 1);
        let s = f.view.stats();
        assert_eq!(s.scrolled, -(CODE_LINE_H as i32));
        // 描くのは、新しく見えた行 12 と、カーソルが抜けた行 11
        assert_eq!((s.lines_checked, s.lines_painted), (2, 2));
        assert_eq!(ranges(&d), vec![(13, 159, Priority::Normal)]);
        f.assert_matches_fresh("1 行下へスクロール");
        // 1 画面下へ（カーソルも 12 行動く）: 重なりが無いので全部描く
        f.view.scroll_lines(12);
        f.caret(24, 0);
        f.render();
        assert_eq!(f.view.viewport().top, 13);
        assert_eq!(f.view.stats().lines_painted, 12);
        f.assert_matches_fresh("1 画面下へ");
        // 3 行上へ
        f.view.scroll_lines(-3);
        f.caret(21, 0);
        f.render();
        let s = f.view.stats();
        assert_eq!((f.view.viewport().top, s.scrolled), (10, 3 * CODE_LINE_H as i32));
        assert_eq!(s.lines_painted, 4); // 新しく見えた 3 行 + カーソルが抜けた行（行 21 は見えたまま）
        f.assert_matches_fresh("3 行上へ");
    }

    #[test]
    fn end_of_file_and_shrinking_document() {
        let Some(mut f) = Fx::new(30) else { return };
        f.caret(29, 0);
        f.render();
        assert_eq!(f.view.viewport().top, 18);
        // 末尾を越えてスクロールすると、下が空く
        f.view.scroll_lines(8);
        f.render();
        assert_eq!(f.view.viewport().top, 26);
        assert_eq!(f.view.stats().lines_visible, 4);
        f.assert_matches_fresh("末尾の先へ");
        let blank = 13 + 4 * CODE_LINE_H;
        assert!((blank..159).all(|y| f.canvas.row(y).iter().all(|&p| p == palette::BG.0)));
        // 行を消す: 消えた行のあとが地の色に戻る
        f.lines.truncate(28);
        f.view.lines_changed(LineChange { old: LineRange::new(28, 30), new: LineRange::new(28, 28) });
        f.caret(27, 0);
        let d = f.render();
        f.assert_matches_fresh("行を消した");
        assert!(d.bounds().end >= blank);
        // 表示位置より上まで縮んだ
        f.lines.truncate(5);
        f.view.lines_changed(LineChange { old: LineRange::new(5, 28), new: LineRange::new(5, 5) });
        f.caret(4, 0);
        f.render();
        assert_eq!(f.view.viewport().top, 4);
        f.assert_matches_fresh("大きく縮んだ");
    }

    #[test]
    fn insert_line_shifts_rows_below() {
        let Some(mut f) = Fx::new(50) else { return };
        f.caret(3, 4);
        f.render();
        f.lines.insert(4, "    new".into());
        f.view.lines_changed(LineChange { old: LineRange::new(3, 4), new: LineRange::new(3, 5) });
        f.caret(4, 4);
        let d = f.render();
        f.assert_matches_fresh("改行");
        // 行 3 から下が変わる。上の 3 行は触らない
        assert_eq!(d.bounds(), RowRange::new(13 + 3 * CODE_LINE_H, 13 + 12 * CODE_LINE_H));
        assert_eq!(f.view.stats().lines_painted, 9);
    }

    #[test]
    fn selection_and_horizontal_scroll() {
        let Some(mut f) = Fx::new(40) else { return };
        f.lines[2] = "x".repeat(120);
        f.lines[3].clear();
        f.caret(1, 0);
        f.render();
        // 行 1 の途中から行 4 の途中まで選ぶ
        f.view.set_selection(Pos::new(1, 2), Pos::new(4, 3));
        let d = f.render();
        assert_eq!(d.bounds(), RowRange::new(13 + CODE_LINE_H, 13 + 5 * CODE_LINE_H));
        f.assert_matches_fresh("選択");
        let sel_px = |c: &Canvas, line: u16| c.row(13 + line * CODE_LINE_H + 11).iter().filter(|&&p| p == palette::SELECTION.0).count();
        assert_eq!(sel_px(&f.canvas, 0), 0);
        assert!(sel_px(&f.canvas, 1) > 20);
        assert!(sel_px(&f.canvas, 2) > 200);
        assert_eq!(sel_px(&f.canvas, 3), 6); // 空の行は改行ぶんの 1 字
        assert!(sel_px(&f.canvas, 4) > 0 && sel_px(&f.canvas, 4) <= 18);
        assert_eq!(sel_px(&f.canvas, 5), 0);
        // 選択を外すと元に戻る
        f.caret(4, 3);
        f.render();
        f.assert_matches_fresh("選択解除");
        // 長い行の右端へ: 横にスクロールして、カーソルが領域に入る
        f.caret(2, 120);
        f.render();
        let vp = f.view.viewport();
        assert!(vp.scroll_x > 0);
        let r = f.view.caret_rect().unwrap();
        assert!(r.x as i32 >= 23 && (r.x as i32) < 320, "{r:?}");
        assert_eq!(r.x as i32, 23 + 120 * 6 - vp.scroll_x);
        f.assert_matches_fresh("横スクロール");
        // 行番号の欄には本文がはみ出さない（行番号の色と地の色だけ）
        let y = 13 + 2 * CODE_LINE_H + 6;
        assert!((0..20).all(|x| f.canvas.get(x, y as i32) != Some(palette::FG)));
        // 行頭へ戻ると横スクロールも戻る
        f.caret(2, 0);
        f.render();
        assert_eq!(f.view.viewport().scroll_x, 0);
        f.assert_matches_fresh("横スクロールを戻す");
    }

    #[test]
    fn hit_testing_roundtrip() {
        let Some(mut f) = Fx::new(100) else { return };
        f.lines[5] = "ab日本語cd\tz".into();
        f.caret(40, 0);
        f.render();
        let top = f.view.viewport().top;
        assert_eq!(top, 29);
        // 位置 → 点 → 位置
        for (line, col) in [(29, 0), (30, 5), (40, 0), (40, 7), (35, 13)] {
            let r = f.view.point_of(Pos::new(line, col)).unwrap();
            assert_eq!(r.y as u32, 13 + (line - top) * CODE_LINE_H as u32);
            assert_eq!(r.x as u32, 23 + col * 6);
            assert_eq!(r.h, CODE_LINE_H);
            assert_eq!(f.view.pos_at(r.x as i32, r.y as i32 + 5), Some(Pos::new(line, col)));
        }
        // 見えていない行
        assert_eq!(f.view.point_of(Pos::new(5, 0)), None);
        assert_eq!(f.view.point_of(Pos::new(41, 0)), None);
        // 行末より右は行末、行番号の欄は行頭、領域の上下は端の行
        let len = f.lines[30].chars().count() as u32;
        assert_eq!(f.view.pos_at(319, 13 + CODE_LINE_H as i32), Some(Pos::new(30, len)));
        assert_eq!(f.view.pos_at(3, 13 + CODE_LINE_H as i32), Some(Pos::new(30, 0)));
        assert_eq!(f.view.pos_at(23, 0).unwrap().line, 29);
        assert_eq!(f.view.pos_at(23, 169).unwrap().line, 40);
        // 全角とタブ
        f.caret(5, 0);
        f.render();
        assert_eq!(f.view.viewport().top, 5);
        // a b 日 本 語 c d \t z → 境目の x: 0 6 12 24 36 48 54 60 72 78
        for (col, x) in [(2, 12), (3, 24), (5, 48), (7, 60), (8, 72), (9, 78)] {
            assert_eq!(f.view.point_of(Pos::new(5, col)).unwrap().x, 23 + x, "桁 {col}");
            assert_eq!(f.view.pos_at(23 + x as i32, 14), Some(Pos::new(5, col)));
        }
        assert_eq!(f.view.pos_at(23 + 17, 14), Some(Pos::new(5, 2))); // 「日」の左半分
        assert_eq!(f.view.pos_at(23 + 18, 14), Some(Pos::new(5, 3))); // 右半分
    }

    #[test]
    fn area_change_and_invalidate_repaint_everything() {
        let Some(mut f) = Fx::new(100) else { return };
        f.render();
        // ツリーを開いた幅に変える
        let side = Rect::new(84, 13, 236, 146);
        let d = f.render_in(side);
        assert_eq!(d.bounds(), RowRange::new(13, 159));
        assert_eq!(f.view.stats().lines_painted, 12);
        assert_eq!(f.view.point_of(Pos::new(0, 0)).unwrap().x, 84 + 23);
        // 元に戻して、描き直した結果と比べる
        f.render();
        f.assert_matches_fresh("領域を戻した");
        // 別の画面が上書きしたあと
        f.canvas.clear(Color::WHITE);
        f.view.invalidate();
        let d = f.render();
        assert_eq!(d.bounds(), RowRange::new(13, 159));
        for y in 13..159 {
            assert!(!f.canvas.row(y).contains(&0xFFFF), "画素行 {y} に前の画面が残っている");
        }
        assert!(f.canvas.row(158).iter().all(|&p| p == palette::BG.0));
    }

    #[test]
    fn spans_color_text_and_marks_draw_squiggle() {
        let Some(mut f) = Fx::new(20) else { return };
        f.lines[1] = "def main():".into();
        let lines: Vec<String> = f.lines.clone();
        let mut hl = |line: u32, _t: &str, out: &mut Vec<Span>| {
            if line == 1 {
                out.push(Span { start: 0, end: 3, kind: TokenKind::Keyword });
                out.push(Span { start: 4, end: 8, kind: TokenKind::Function });
            }
        };
        f.caret(10, 0);
        let mut cx = RenderCx::new(&mut f.canvas, &mut f.fonts, &f.theme);
        f.view.render(&mut cx, AREA, &Doc(&lines), &mut hl);
        let has = |c: &Canvas, line: u16, col: Color| (0..CODE_LINE_H).any(|dy| c.row(13 + line * CODE_LINE_H + dy).contains(&col.0));
        assert!(has(&f.canvas, 1, palette::KEYWORD));
        assert!(has(&f.canvas, 1, palette::FUNCTION));
        assert!(!has(&f.canvas, 0, palette::KEYWORD));
        // 波線
        f.view.set_marks(vec![LineMark { line: 2, cols: None, color: palette::ERROR }]);
        let mut cx = RenderCx::new(&mut f.canvas, &mut f.fonts, &f.theme);
        let d = f.view.render(&mut cx, AREA, &Doc(&lines), &mut hl);
        assert_eq!(ranges(&d), vec![(13 + 2 * CODE_LINE_H, 13 + 3 * CODE_LINE_H, Priority::Edit)]);
        assert!(has(&f.canvas, 2, palette::ERROR));
        // トークンだけ変わった（文字は同じ）ときも描き直す
        let mut hl2 = |line: u32, _t: &str, out: &mut Vec<Span>| {
            if line == 1 {
                out.push(Span { start: 0, end: 11, kind: TokenKind::Comment });
            }
        };
        f.view.recheck_all();
        let mut cx = RenderCx::new(&mut f.canvas, &mut f.fonts, &f.theme);
        let d = f.view.render(&mut cx, AREA, &Doc(&lines), &mut hl2);
        assert_eq!(ranges(&d), vec![(13 + CODE_LINE_H, 13 + 2 * CODE_LINE_H, Priority::Edit)]);
        assert!(has(&f.canvas, 1, palette::COMMENT) && !has(&f.canvas, 1, palette::KEYWORD));
    }

    /// 行ごとに高さが違い、折り返すレイアウト（段階 5 の Markdown の代わり）。
    /// `#` で始まる行は高さ 17、ほかは 12 で、10 文字ごとに折り返す。
    struct Varied;

    impl LineLayouter for Varied {
        fn layout(&mut self, _fonts: &mut Fonts, theme: &Theme, input: &LineInput<'_>, out: &mut LineLayout) {
            let heading = input.text.starts_with('#');
            let (h, base, adv, font) = if heading { (17, 13, 8, TextStyle::mono(13.0)) } else { (12, 9, 6, TextStyle::mono(10.0)) };
            let st = theme.token(if heading { TokenKind::Heading } else { TokenKind::Text });
            out.clear();
            for (i, _) in input.text.chars().enumerate() {
                if i % 10 == 0 {
                    out.start_row(h, base, 4);
                }
                out.push(adv, font, st);
            }
            out.finish(h, base);
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
        fn cell_width(&mut self, _fonts: &mut Fonts) -> u16 {
            6
        }
    }

    #[test]
    fn variable_height_layout_scrolls_by_pixels() {
        let Some(mut f) = Fx::new(0) else { return };
        f.view = EditorView::new(Box::new(Varied));
        // 高さ: 17, 12, 36（25 文字 = 3 表示行）, 12, 17, 12 … を繰り返す
        f.lines = (0..40)
            .map(|i| match i % 4 {
                0 => format!("# head {i}"),
                2 => "abcdefghijklmnopqrstuvwxy".to_string(),
                _ => format!("body {i}"),
            })
            .collect();
        let area = Rect::new(0, 13, 320, 100);
        let fresh = |f: &mut Fx| {
            let mut view = EditorView::new(Box::new(Varied));
            view.set_selection(f.view.anchor, f.view.head);
            view.reveal = false;
            view.vp = f.view.vp;
            let mut canvas = Canvas::new();
            let mut cx = RenderCx::new(&mut canvas, &mut f.fonts, &f.theme);
            view.render(&mut cx, area, &Doc(&f.lines), &mut NoSpans);
            canvas
        };
        let same = |f: &mut Fx, what: &str| {
            let c = fresh(f);
            for y in 0..170u16 {
                assert!(f.canvas.row(y) == c.row(y), "{what}: 画素行 {y}");
            }
        };
        f.render_in(area);
        // 17+12+36+12+17 = 94 → 行 5（12）は 6px だけ見える
        assert_eq!(f.view.stats().lines_visible, 6);
        assert_eq!(f.view.gutter_w(), 0);
        // 行 2 の 3 番目の表示行（桁 22）: y = 13 + 29 + 24、x = 4 + 2*6
        let r = f.view.point_of(Pos::new(2, 22)).unwrap();
        assert_eq!((r.x, r.y, r.h), (16, 13 + 53, 12));
        assert_eq!(f.view.pos_at(16, 13 + 53 + 3), Some(Pos::new(2, 22)));
        // 折り返しの境目（桁 10）は次の表示行の先頭
        assert_eq!(f.view.point_of(Pos::new(2, 10)).unwrap().y, 13 + 29 + 12);
        // 見出しは高い
        assert_eq!(f.view.point_of(Pos::new(4, 0)).unwrap().h, 17);
        // 行 5 へ: 6px 足りないので、6px だけ上へずらす（行 0 の途中から表示）
        f.caret(5, 0);
        f.render_in(area);
        let vp = f.view.viewport();
        assert_eq!((vp.top, vp.top_off), (0, 6));
        assert_eq!(f.view.stats().scrolled, -6);
        same(&mut f, "6px 上へ");
        // さらに下へ、そのあと先頭へ戻る
        f.caret(9, 3);
        f.render_in(area);
        same(&mut f, "行 9 へ");
        let r = f.view.caret_rect().unwrap();
        assert_eq!(r.y as i32 + r.h as i32, 113, "カーソルの表示行が下端に付く");
        f.caret(0, 0);
        f.render_in(area);
        assert_eq!(f.view.viewport(), Viewport::default());
        same(&mut f, "先頭へ");
        // 行の高さが変わる編集: 行 1 を 3 表示行に
        f.lines[1] = "0123456789012345678901234".into();
        f.view.lines_changed(LineChange { old: LineRange::new(1, 2), new: LineRange::new(1, 2) });
        f.render_in(area);
        same(&mut f, "行の高さが変わった");
        assert_eq!(f.view.point_of(Pos::new(2, 0)).unwrap().y, 13 + 17 + 36);
    }

    // --- 未確定文字列 ---

    #[test]
    fn splice_preedit_inserts_and_shifts_spans() {
        let sp = |s: u32, e: u32| Span { start: s, end: e, kind: TokenKind::Comment };
        let run = |text: &str, spans: &[Span], col: u32| {
            let (mut t, mut s) = (text.to_string(), spans.to_vec());
            let at = splice_preedit(&mut t, &mut s, col, "▽か");
            (t, s.iter().map(|s| (s.start, s.end)).collect::<Vec<_>>(), at)
        };
        // 「▽か」は 6 バイト
        assert_eq!(run("abcd", &[], 0), ("▽かabcd".into(), vec![], 0));
        assert_eq!(run("abcd", &[], 2), ("ab▽かcd".into(), vec![], 2));
        assert_eq!(run("abcd", &[], 4), ("abcd▽か".into(), vec![], 4));
        // 行末より後ろの桁は行末に丸める
        assert_eq!(run("abcd", &[], 99), ("abcd▽か".into(), vec![], 4));
        assert_eq!(run("", &[], 3), ("▽か".into(), vec![], 0));
        // 桁は文字数（全角は 3 バイト）
        assert_eq!(run("日本語", &[], 1), ("日▽か本語".into(), vec![], 1));
        // 前のトークンはそのまま、後ろはずれ、またがるものは 2 つに分かれる
        let spans = [sp(0, 2), sp(2, 6), sp(6, 8)];
        assert_eq!(run("abcdefgh", &spans, 4).1, vec![(0, 2), (2, 4), (10, 12), (12, 14)]);
        // 境目ちょうど: 分けない
        assert_eq!(run("abcdefgh", &spans, 2).1, vec![(0, 2), (8, 12), (12, 14)]);
        assert_eq!(run("abcdefgh", &spans, 8).1, vec![(0, 2), (2, 6), (6, 8)]);
        assert_eq!(run("abcdefgh", &spans, 0).1, vec![(6, 8), (8, 12), (12, 14)]);
    }

    #[test]
    fn display_columns_shift_after_preedit() {
        // 桁 3 に 4 文字挟んだ行
        assert_eq!(to_display(2, 3, 4, true), 2);
        assert_eq!(to_display(3, 3, 4, false), 3);
        assert_eq!(to_display(3, 3, 4, true), 7);
        assert_eq!(to_display(4, 3, 4, false), 8);
        assert_eq!(to_display(u32::MAX, 3, 4, false), u32::MAX);
    }

    #[test]
    fn preedit_view_builder() {
        let v = PreeditView::reading("▽てんめ").with(PreeditStyle::Pending, "t").with(PreeditStyle::Pending, "");
        assert_eq!((v.segs().len(), v.len_chars(), v.text().as_str()), (2, 5, "▽てんめt"));
        assert!(PreeditView::reading("").is_empty());
        assert_eq!(PreeditView::converting("a\tb\n").text(), "a b ");
    }

    /// 未確定文字列の幅（コード表示の字幅で）。
    fn pre_w(f: &mut Fx, s: &str) -> i32 {
        let font = TextStyle::mono(crate::layout::CODE_PX);
        s.chars().map(|c| if c.is_ascii() { 6 } else { f.fonts.advance(c, &font) as i32 }).sum()
    }

    /// 画面の行 `y` の、色 `c` の画素の x の範囲と数。
    fn px(canvas: &Canvas, y: u16, c: Color) -> Option<(i32, i32, usize)> {
        let row = canvas.row(y);
        let first = row.iter().position(|&p| p == c.0)?;
        let last = row.iter().rposition(|&p| p == c.0)?;
        Some((first as i32, last as i32 + 1, row.iter().filter(|&&p| p == c.0).count()))
    }

    fn snapshot(c: &Canvas) -> Vec<Vec<u16>> {
        (0..170).map(|y| c.row(y).to_vec()).collect()
    }

    #[test]
    fn preedit_splices_at_start_middle_end() {
        let Some(mut f) = Fx::new(30) else { return };
        // 「てん」は全角（半角 2 つぶん）
        assert_eq!(pre_w(&mut f, "てん"), 24);
        let w = pre_w(&mut f, "▽てん");
        let len = f.lines[4].chars().count() as u32; // "line 4 = value_4"
        let y = 13 + 4 * CODE_LINE_H as i16;
        for col in [0, 5, len, len + 7] {
            let at = col.min(len);
            f.caret(4, col);
            f.view.set_preedit(Some(PreeditView::reading("▽てん")));
            f.render();
            let x0 = 23 + at as i16 * 6;
            // 先頭は挟んだ桁、カーソルは後ろ
            assert_eq!(f.view.preedit_rect(), Some(Rect::new(x0, y, 1, CODE_LINE_H)), "桁 {col}");
            assert_eq!(f.view.caret_rect(), Some(Rect::new(x0 + w as i16, y, 1, CODE_LINE_H)), "桁 {col}");
            assert_eq!(f.view.point_of(Pos::new(4, col)), f.view.caret_rect());
            // 左の桁は動かず、右の桁は幅だけずれる
            if at > 0 {
                assert_eq!(f.view.point_of(Pos::new(4, at - 1)).unwrap().x, x0 - 6);
                assert_eq!(f.view.pos_at(x0 as i32 - 6, y as i32 + 5), Some(Pos::new(4, at - 1)));
            }
            if at < len {
                assert_eq!(f.view.point_of(Pos::new(4, at + 1)).unwrap().x, x0 + w as i16 + 6);
                assert_eq!(f.view.pos_at(x0 as i32 + w + 6, y as i32 + 5), Some(Pos::new(4, at + 1)));
            }
            // 未確定文字列の上の点は、挟んだ桁
            for dx in [1, w / 2, w - 1, w] {
                assert_eq!(f.view.pos_at(x0 as i32 + dx, y as i32 + 5), Some(Pos::new(4, at)), "桁 {col} +{dx}");
            }
            // ほかの行はそのまま
            assert_eq!(f.view.point_of(Pos::new(5, 5)).unwrap().x, 23 + 30);
            // 下線は未確定文字列の幅ちょうど、カーソルはその右端
            assert_eq!(px(&f.canvas, y as u16 + 11, Color::WHITE), Some((x0 as i32, x0 as i32 + w, w as usize)), "桁 {col}");
            assert_eq!(px(&f.canvas, y as u16 + 5, palette::CURSOR).map(|p| p.0), Some(x0 as i32 + w));
            f.assert_matches_fresh("未確定文字列");
            // 文書は変わっていない
            assert_eq!(f.lines[4], "line 4 = value_4");
        }
    }

    #[test]
    fn preedit_styles_and_colors() {
        let Some(mut f) = Fx::new(30) else { return };
        f.caret(2, 4);
        let y = 13 + 2 * CODE_LINE_H;
        let x0 = 23 + 4 * 6;
        // 読み: 白い字と白い下線。青い背景は無い
        f.view.set_preedit(Some(PreeditView::reading("▽てんめつ")));
        f.render();
        let w = pre_w(&mut f, "▽てんめつ");
        assert_eq!(px(&f.canvas, y + 11, Color::WHITE), Some((x0, x0 + w, w as usize)));
        assert!((y..y + 11).any(|r| px(&f.canvas, r, Color::WHITE).is_some()), "字が白い");
        assert!((y..y + 12).all(|r| px(&f.canvas, r, palette::SELECTION).is_none()));
        // 背景はカーソル行の色のまま
        assert_eq!(f.canvas.get(x0 + 1, y as i32), Some(palette::LINE));
        // 変換中: 青い背景が幅ちょうど（行の色は中に残らず、外にははみ出さない）、白い下線
        f.view.set_preedit(Some(PreeditView::converting("▼点滅")));
        f.render();
        let w = pre_w(&mut f, "▼点滅");
        assert_eq!(px(&f.canvas, y + 11, Color::WHITE), Some((x0, x0 + w, w as usize)));
        let mut blue = 0;
        for r in y..y + 11 {
            let (a, b, n) = px(&f.canvas, r, palette::SELECTION).unwrap();
            assert!(a >= x0 && b <= x0 + w, "画素行 {r}: 青が外にはみ出している");
            assert!((x0..x0 + w).all(|x| f.canvas.get(x, r as i32) != Some(palette::LINE)), "画素行 {r}: 行の色が残っている");
            // 右隣（カーソルの次の画素。文書では空白）は行の色
            assert_eq!(f.canvas.get(x0 + w + 1, r as i32), Some(palette::LINE));
            blue += n;
        }
        // 字（縁のぼかしも含む）に隠れない所は青
        assert!(blue > 11 * w as usize / 3, "青い背景が少なすぎる: {blue}");
        f.assert_matches_fresh("変換中");
        // 読み + 打ちかけ: 下線が白と薄い色に分かれる
        f.view.set_preedit(Some(PreeditView::reading("▽てんめ").with(PreeditStyle::Pending, "ts")));
        f.render();
        let w = pre_w(&mut f, "▽てんめ");
        assert_eq!(px(&f.canvas, y + 11, Color::WHITE), Some((x0, x0 + w, w as usize)));
        assert_eq!(px(&f.canvas, y + 11, palette::DIM), Some((x0 + w, x0 + w + 12, 12)));
        assert_eq!(f.view.caret_rect().unwrap().x as i32, x0 + w + 12);
        f.assert_matches_fresh("打ちかけ");
    }

    #[test]
    fn preedit_repaints_only_cursor_line_and_restores() {
        let Some(mut f) = Fx::new(100) else { return };
        f.caret(4, 3);
        f.render();
        let before = snapshot(&f.canvas);
        let y = 13 + 4 * CODE_LINE_H;
        let only_cursor_line = vec![(y, y + CODE_LINE_H, Priority::Cursor)];
        let one = RenderStats { lines_visible: 12, lines_checked: 1, lines_painted: 1, scrolled: 0 };
        // 置く
        f.view.set_preedit(Some(PreeditView::reading("▽て")));
        let d = f.render();
        assert_eq!(ranges(&d), only_cursor_line);
        assert_eq!(f.view.stats(), one);
        // 変える（字が増える・見た目だけ変わる）
        f.view.set_preedit(Some(PreeditView::reading("▽てん")));
        let d = f.render();
        assert_eq!(ranges(&d), only_cursor_line);
        assert_eq!(f.view.stats(), one);
        f.view.set_preedit(Some(PreeditView::converting("▽てん")));
        let d = f.render();
        assert_eq!(ranges(&d), only_cursor_line);
        f.assert_matches_fresh("見た目だけ変えた");
        // 同じものを置き直しても何も描かない
        f.view.set_preedit(Some(PreeditView::converting("▽てん")));
        let d = f.render();
        assert!(d.is_empty());
        assert_eq!(f.view.stats().lines_checked, 0);
        // 点滅しても未確定文字列は残る
        f.view.set_cursor_on(false);
        let d = f.render();
        assert_eq!(ranges(&d), only_cursor_line);
        assert!(px(&f.canvas, y, palette::SELECTION).is_some());
        f.assert_matches_fresh("点滅");
        f.view.set_cursor_on(true);
        f.render();
        // 消す: カーソル行だけ描き直して、置く前と 1 画素も違わない
        f.view.set_preedit(None);
        assert_eq!(f.view.preedit(), None);
        let d = f.render();
        assert_eq!(ranges(&d), only_cursor_line);
        assert_eq!(f.view.stats(), one);
        assert_eq!(f.view.preedit_rect(), None);
        assert!(snapshot(&f.canvas) == before, "消したあとが、置く前と違う");
        // 全部を描き直した結果とも同じ
        let partial = snapshot(&f.canvas);
        f.view.invalidate();
        f.render();
        assert!(snapshot(&f.canvas) == partial, "消したあとが、全部を描き直した結果と違う");
        f.assert_matches_fresh("消した");
        // 空の未確定文字列は「無し」と同じ
        f.view.set_preedit(Some(PreeditView::new()));
        assert!(f.render().is_empty());
    }

    #[test]
    fn preedit_follows_cursor_and_edits() {
        let Some(mut f) = Fx::new(100) else { return };
        f.caret(2, 5);
        f.view.set_preedit(Some(PreeditView::reading("▽かな")));
        f.render();
        let w = pre_w(&mut f, "▽かな") as i16;
        // カーソルが別の行へ: 前の行は元に戻り、新しい行に付く
        f.caret(6, 2);
        let d = f.render();
        let y = |l: u16| 13 + l * CODE_LINE_H;
        assert_eq!(ranges(&d), vec![(y(2), y(3), Priority::Edit), (y(6), y(7), Priority::Cursor)]);
        assert_eq!(f.view.preedit_rect().unwrap(), Rect::new(23 + 12, y(6) as i16, 1, CODE_LINE_H));
        assert_eq!(f.view.caret_rect().unwrap().x, 23 + 12 + w);
        assert_eq!(f.view.point_of(Pos::new(2, 5)).unwrap().x, 23 + 30);
        f.assert_matches_fresh("カーソルを動かした");
        // 確定前の文字が文書に入る（1 字入力）: 未確定文字列は新しい桁に付く
        f.lines[6].insert(2, 'X');
        f.view.lines_changed(LineChange { old: LineRange::new(6, 7), new: LineRange::new(6, 7) });
        f.caret(6, 3);
        let d = f.render();
        assert_eq!(ranges(&d), vec![(y(6), y(7), Priority::Cursor)]);
        assert_eq!(f.view.preedit_rect().unwrap().x, 23 + 18);
        f.assert_matches_fresh("1 字入力");
        // 選択と波線は文書の桁のまま（表示は未確定文字列の幅だけ右へ）
        f.view.set_marks(vec![LineMark { line: 6, cols: Some(4..6), color: palette::ERROR }]);
        f.render();
        let (a, b, _) = (y(6)..y(7)).find_map(|r| px(&f.canvas, r, palette::ERROR)).unwrap();
        assert_eq!((a, b), (23 + 24 + w as i32, 23 + 36 + w as i32));
        f.assert_matches_fresh("波線");
        f.view.set_marks(Vec::new());
        f.view.set_selection(Pos::new(6, 1), Pos::new(6, 3));
        f.render();
        // 選択（桁 1〜3）はカーソルの手前まで。未確定文字列には掛からない
        assert_eq!(px(&f.canvas, y(6), palette::SELECTION), Some((23 + 6, 23 + 18, 12)));
        f.assert_matches_fresh("選択");
    }

    #[test]
    fn long_preedit_scrolls_horizontally_to_keep_caret_visible() {
        let Some(mut f) = Fx::new(30) else { return };
        f.lines[3] = "abcdefghij".repeat(6);
        f.caret(3, 42);
        f.render();
        assert_eq!(f.view.viewport().scroll_x, 0);
        let before = snapshot(&f.canvas);
        // 桁 42（x = 252）に長い読みを挟むと、カーソルが右へはみ出すので横にずれる
        let s = "▽ながいながいよみがな";
        f.view.set_preedit(Some(PreeditView::reading(s)));
        f.render();
        let w = pre_w(&mut f, s);
        let vp = f.view.viewport();
        assert!(vp.scroll_x > 0);
        let r = f.view.caret_rect().unwrap();
        assert_eq!(r.x as i32, 23 + 252 + w - vp.scroll_x);
        assert!(r.x as i32 >= 23 && (r.x as i32) < 320 - 23, "カーソルが右端の余白より内側にある: {r:?}");
        assert_eq!(f.view.preedit_rect().unwrap().x as i32, 23 + 252 - vp.scroll_x);
        f.assert_matches_fresh("横スクロール");
        // 消して行頭へ戻ると、置く前と同じ
        f.view.set_preedit(None);
        f.caret(3, 0);
        f.render();
        f.caret(3, 42);
        f.render();
        assert_eq!(f.view.viewport().scroll_x, 0);
        assert!(snapshot(&f.canvas) == before);
    }

    #[test]
    fn preedit_wraps_with_the_layouter() {
        let Some(mut f) = Fx::new(0) else { return };
        f.view = EditorView::new(Box::new(Varied));
        f.lines = vec!["abcdefgh".into(), "next".into(), "last".into()];
        let area = Rect::new(0, 13, 320, 100);
        f.caret(0, 6);
        f.render_in(area);
        let before = snapshot(&f.canvas);
        assert_eq!(f.view.point_of(Pos::new(1, 0)).unwrap().y, 13 + 12);
        // 8 文字 + 5 文字 = 13 文字 → 10 文字で折り返して 2 表示行になる
        f.view.set_preedit(Some(PreeditView::converting("▼かきくけ")));
        let d = f.render_in(area);
        // 先頭は 1 行目の桁 6、カーソルは 2 行目（表示上の桁 11）
        assert_eq!(f.view.preedit_rect(), Some(Rect::new(4 + 36, 13, 1, 12)));
        assert_eq!(f.view.caret_rect(), Some(Rect::new(4 + 6, 13 + 12, 1, 12)));
        // 残りの「gh」はその右、下の行は 1 行ぶん下がる
        assert_eq!(f.view.point_of(Pos::new(0, 8)).unwrap(), Rect::new(4 + 18, 13 + 12, 1, 12));
        assert_eq!(f.view.point_of(Pos::new(1, 0)).unwrap().y, 13 + 24);
        assert_eq!(f.view.pos_at(4 + 13, 13 + 15), Some(Pos::new(0, 7)));
        // 下の行がずれるので、カーソル行から下が全部変わる
        assert!(d.bounds().start == 13 && d.bounds().end >= 13 + 48, "{:?}", d.bounds());
        // 青い背景は 2 つの表示行にまたがる
        assert_eq!(px(&f.canvas, 13, palette::SELECTION).map(|p| (p.0, p.1)), Some((4 + 36, 4 + 60)));
        assert_eq!(px(&f.canvas, 13 + 12, palette::SELECTION).map(|p| (p.0, p.1)), Some((4, 4 + 6)));
        // 消すと元の高さに戻る
        f.view.set_preedit(None);
        f.render_in(area);
        assert!(snapshot(&f.canvas) == before);
    }
}
