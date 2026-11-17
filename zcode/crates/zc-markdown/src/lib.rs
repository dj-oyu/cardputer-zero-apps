//! Markdown のライブプレビュー（design.md §9、モック 10・11）。
//! `MdDocument` は batch スレッドで作ってメインへ送るので `Send`。
//!
//! - `pulldown-cmark` で解析し、ソースの行ごとに「装飾つきの区間・ブロックの種類・
//!   記号の位置」（[`MdLine`]）を返す。描くのは `zc-ui` のエディタ（可変の行高・字幅）
//! - 記号（`#`、`**`、`- [ ]` など）は、カーソルのある行だけ薄く見せ、ほかの行では
//!   隠す。その切り替えはここで行う（[`MdDocument::line`] の `cursor_line`）
//! - 編集中のブロックだけメインで解析し直し（[`MdDocument::edited`]）、全体の再解析は
//!   版番号つきで batch に出す（[`MdDocument::parse`] → [`MdDocument::replace`]）
//!
//! この crate は `zc-buffer` も `zc-font` も知らない。大きさは [`MdFont`] で指定し、
//! 実際の画素数は `zc-ui` が決める。
//!
//! # 中の持ち方
//!
//! 行ごとに「文字列の印（ハッシュ）・ブロックの種類・区間の並び」を持つ。
//! [`MdDocument::line`] は、渡された文字列の印が覚えている印と同じなら、覚えている
//! 区間をそのまま返す（解析しない）。違えば（日本語入力の未確定文字列を挟んだ行、
//! `edited` を呼ぶ前の行）、その 1 行だけをその場で解析して返す。
//!
//! # 一部だけの解析が正しい理由（`edited`）
//!
//! いちばん外側のブロックが始まる行では、ブロックの解析の状態は文書の先頭と同じに
//! なる（開いている入れ物が無い）。だから「編集した行より前で、最後に外側のブロックが
//! 始まる行」から解析し直せば、そこから先の結果は全体を解析したときと同じになる
//! （表だけは次の行を見て始まりが決まるので、編集した行の 2 行以上前から選ぶ）。
//! どこまで解析するかは「編集した行より後ろで、最初に外側のブロックが始まっていた行」
//! まで。解析し直した結果でも、その行で外側のブロックが始まっていれば、そこから先は
//! 前の結果のままでよい。始まっていなければ（``` を打って後ろの行を飲み込んだ、など）
//! 全体の再解析が要る。
//!
//! 例外は参照リンクの定義（`[名前]: URL`）で、文書のどこにあっても全体のリンクの
//! 解釈を変える。定義らしい行が文書にあるあいだは、いつも全体の再解析を頼む。

use core::ops::Range;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
use zc_core::{palette, Attrs, CancelToken, Cancelled, LineChange, LineSource, Style, Version, Versioned};

/// 区間の色（モック 10）。
pub mod colors {
    use zc_core::{palette, Color};
    /// カーソル行で見せる記号。
    pub const MARKER: Color = Color::hex(0x6b6b6b);
    /// 見出し。
    pub const HEADING: Color = Color::WHITE;
    /// 太字。
    pub const STRONG: Color = Color::WHITE;
    /// リンク。
    pub const LINK: Color = Color::hex(0x3794ff);
    /// インラインコードの字と背景。
    pub const CODE_FG: Color = palette::STRING;
    pub const CODE_BG: Color = Color::hex(0x333333);
    /// チェック済みの項目の字。
    pub const DONE: Color = Color::hex(0x777777);
    /// 引用の字。
    pub const QUOTE: Color = Color::hex(0xaaaaaa);
    /// 番号つきの箇条書きの番号。
    pub const NUMBER: Color = palette::STATUS;
}

/// 区間に使う書体の区分（実際のフォントと大きさは `zc-ui` が決める）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MdFont {
    /// 本文（プロポーショナル）。
    Body,
    /// 見出し。1〜6。
    Heading(u8),
    /// インラインコード・コードブロック（等幅）。
    Code,
    /// 記号（カーソル行で薄く見せるもの。等幅・小さめ）。
    Marker,
}

/// ブロックの種類（行の左側の飾りを決める）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MdBlock {
    Paragraph,
    Heading(u8),
    /// 箇条書き。`depth` は入れ子の深さ、`checkbox` はチェックボックスの状態。
    ListItem { depth: u8, checkbox: Option<bool> },
    /// 引用。
    Quote,
    /// コードブロックの中の行。
    Code,
    /// コードブロックの囲み（```）の行。
    CodeFence,
    /// 水平線。
    Rule,
    /// 表（最初の版ではソースのまま表示する）。
    Table,
    Blank,
}

/// 1 行の中の、同じ見た目が続く区間。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdRun {
    /// ソースの行の中のバイト範囲。
    pub range: Range<u32>,
    pub font: MdFont,
    pub style: Style,
    /// 記号なので、カーソル行でなければ描かない（幅も取らない）。
    pub hidden: bool,
}

/// ソース 1 行ぶんのレイアウトの元。`runs` は行の全部のバイトを、前から順に
/// すき間なく覆う（空の行では空）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdLine {
    pub block: MdBlock,
    pub runs: Vec<MdRun>,
    /// 箇条書きの入れ子の深さ（字下げの段数）。箇条書きの中の段落・コードにも付く。
    pub indent: u8,
    /// 引用の入れ子の深さ（左の線の本数）。
    pub quote: u8,
    /// 番号なしの箇条書きの項目の最初の行（カーソル行でなければ「•」を描く）。
    pub bullet: bool,
    /// カーソル行でなければ、行をつぶして見せる（``` の行、`===` の下線の行）。
    pub collapse: bool,
}

impl MdLine {
    /// 空の行。[`MdDocument::line_into`] に渡して使い回す。
    pub fn new() -> MdLine {
        MdLine { block: MdBlock::Paragraph, runs: Vec::new(), indent: 0, quote: 0, bullet: false, collapse: false }
    }
}

impl Default for MdLine {
    fn default() -> Self {
        MdLine::new()
    }
}

// 区間の印
const S_MARKER: u8 = 1;
const S_BOLD: u8 = 1 << 1;
const S_ITALIC: u8 = 1 << 2;
const S_STRIKE: u8 = 1 << 3;
const S_CODE: u8 = 1 << 4;
const S_LINK: u8 = 1 << 5;
const S_NUMBER: u8 = 1 << 6;

// 行の印
/// いちばん外側のブロックがこの行で始まる。
const L_TOP: u16 = 1;
/// 字下げなしの外側の ``` の中の行（1 行の中の編集は解析し直さなくてよい）。
const L_CODE_FAST: u16 = 1 << 1;
const L_BULLET: u16 = 1 << 2;
const L_COLLAPSE: u16 = 1 << 3;
/// チェック済みの項目の中。
const L_DONE: u16 = 1 << 4;
/// 参照リンクの定義かもしれない行。
const L_REFDEF: u16 = 1 << 5;
const L_TABLE: u16 = 1 << 6;
/// どれかのブロックに入っている（入っていない行は記号を探さない）。
const L_CLAIMED: u16 = 1 << 7;
/// 中身が分からない（`line` は、その場で 1 行だけ解析する）。
const L_STALE: u16 = 1 << 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seg {
    start: u32,
    end: u32,
    flags: u8,
}

/// 行ごとの解析結果。`segs` が空なら、行全体がブロックの既定の見た目。
#[derive(Clone, Debug, PartialEq, Eq)]
struct LineData {
    hash: u32,
    block: MdBlock,
    quote: u8,
    indent: u8,
    flags: u16,
    segs: Vec<Seg>,
}

impl LineData {
    fn blank() -> LineData {
        LineData { hash: 0, block: MdBlock::Paragraph, quote: 0, indent: 0, flags: 0, segs: Vec::new() }
    }
    fn stale() -> LineData {
        LineData { flags: L_STALE, ..LineData::blank() }
    }
    fn has(&self, f: u16) -> bool {
        self.flags & f != 0
    }
}

/// 文字列の印（FNV-1a、32bit）。
fn hash(s: &str) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for &b in s.as_bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    h ^ s.len() as u32
}

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// 囲みの行に見えるか（``` か ~~~ で始まる。前の空白と `>` は読み飛ばす）。
fn looks_like_fence(t: &str) -> bool {
    let t = t.trim_start_matches([' ', '\t', '>']);
    t.starts_with('`') || t.starts_with('~')
}

/// 開いているもの。
enum Frame {
    Inline(u8),
    Quote,
    List { ordered: bool },
    Item { l0: usize, l1: usize },
    Code { fenced: bool, fast: bool, l0: usize, l1: usize, at: usize },
    Heading { l0: usize, l1: usize },
    Block,
}

struct Builder<'a> {
    text: &'a str,
    /// 行の先頭のバイト位置。
    starts: Vec<u32>,
    lines: Vec<LineData>,
    stack: Vec<Frame>,
}

impl<'a> Builder<'a> {
    fn new(text: &'a str) -> Builder<'a> {
        let mut starts = vec![0u32];
        starts.extend(text.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i as u32 + 1));
        let lines = vec![LineData::blank(); starts.len()];
        Builder { text, starts, lines, stack: Vec::new() }
    }
    fn line_of(&self, off: usize) -> usize {
        self.starts.partition_point(|&s| s as usize <= off).saturating_sub(1)
    }
    fn span(&self, l: usize) -> (usize, usize) {
        let s = self.starts[l] as usize;
        let e = self.starts.get(l + 1).map_or(self.text.len(), |&n| n as usize - 1);
        (s, e)
    }
    fn ltext(&self, l: usize) -> &'a str {
        let (s, e) = self.span(l);
        &self.text[s..e]
    }
    /// バイト範囲にかかる行（両端を含む）。
    fn lines_of(&self, r: &Range<usize>) -> (usize, usize) {
        let l0 = self.line_of(r.start);
        let l1 = self.line_of(r.end.saturating_sub(1).max(r.start));
        (l0, l1.max(l0))
    }
    fn depth(&self) -> (u8, u8) {
        let (mut q, mut d) = (0u8, 0u8);
        for f in &self.stack {
            match f {
                Frame::Quote => q = q.saturating_add(1),
                Frame::List { .. } => d = d.saturating_add(1),
                _ => {}
            }
        }
        (q, d)
    }
    fn inline_flags(&self) -> u8 {
        self.stack.iter().fold(0, |a, f| if let Frame::Inline(x) = f { a | x } else { a })
    }
    /// 見える文字の区間を足す（行ごとに分ける）。
    fn content(&mut self, r: &Range<usize>, flags: u8) {
        if r.start >= r.end {
            return;
        }
        let (l0, l1) = self.lines_of(r);
        for l in l0..=l1 {
            let (s, e) = self.span(l);
            let (a, b) = (r.start.max(s), r.end.min(e));
            let d = &mut self.lines[l];
            let a = a.max(s + d.segs.last().map_or(0, |x| x.end as usize));
            if a < b {
                d.segs.push(Seg { start: (a - s) as u32, end: (b - s) as u32, flags });
            }
        }
    }

    fn run(mut self, cancel: Option<&CancelToken>) -> Result<Vec<LineData>, Cancelled> {
        let text = self.text;
        let len = text.len();
        for (i, (ev, r)) in Parser::new_ext(text, options()).into_offset_iter().enumerate() {
            if i & 255 == 0 {
                if let Some(c) = cancel {
                    c.check()?;
                }
            }
            let r = r.start.min(len)..r.end.min(len).max(r.start.min(len));
            match ev {
                Event::Start(tag) => self.start(tag, &r),
                Event::End(_) => {
                    if let Some(f) = self.stack.pop() {
                        self.end(f);
                    }
                }
                Event::Code(_) => {
                    let n = text.as_bytes()[r.clone()].iter().take_while(|b| **b == b'`').count();
                    let f = self.inline_flags() | S_CODE;
                    if n > 0 && r.start + n <= r.end.saturating_sub(n) {
                        self.content(&(r.start + n..r.end - n), f);
                    } else {
                        self.content(&r, f);
                    }
                }
                Event::Rule => {
                    let (l0, _) = self.lines_of(&r);
                    let (q, d) = self.depth();
                    let top = self.stack.is_empty();
                    let ld = &mut self.lines[l0];
                    ld.block = MdBlock::Rule;
                    ld.quote = q;
                    ld.indent = d;
                    ld.flags |= L_CLAIMED | if top { L_TOP } else { 0 };
                }
                Event::TaskListMarker(on) => {
                    let (l, _) = self.lines_of(&r);
                    let (_, d) = self.depth();
                    self.lines[l].block = MdBlock::ListItem { depth: d.max(1), checkbox: Some(on) };
                    if on {
                        if let Some((a, b)) = self.stack.iter().rev().find_map(|f| if let Frame::Item { l0, l1 } = f { Some((*l0, *l1)) } else { None }) {
                            for ld in &mut self.lines[a..=b] {
                                ld.flags |= L_DONE;
                            }
                        }
                    }
                }
                Event::SoftBreak | Event::HardBreak => {}
                // 文字・HTML・脚注の参照など: そのまま見せる
                _ => {
                    if self.stack.is_empty() {
                        let (l0, l1) = self.lines_of(&r);
                        self.lines[l0].flags |= L_TOP;
                        for ld in &mut self.lines[l0..=l1] {
                            ld.flags |= L_CLAIMED;
                        }
                    }
                    let f = self.inline_flags();
                    self.content(&r, f);
                }
            }
        }
        Ok(self.finish())
    }

    fn start(&mut self, tag: Tag<'_>, r: &Range<usize>) {
        let inline = match tag {
            Tag::Emphasis => Some(S_ITALIC),
            Tag::Strong => Some(S_BOLD),
            Tag::Strikethrough => Some(S_STRIKE),
            Tag::Link { .. } | Tag::Image { .. } => Some(S_LINK),
            Tag::Superscript | Tag::Subscript => Some(0),
            _ => None,
        };
        if let Some(f) = inline {
            self.stack.push(Frame::Inline(f));
            return;
        }
        let top = self.stack.is_empty();
        let (l0, l1) = self.lines_of(r);
        if top {
            self.lines[l0].flags |= L_TOP;
        }
        for ld in &mut self.lines[l0..=l1] {
            ld.flags |= L_CLAIMED;
        }
        let frame = match tag {
            Tag::BlockQuote(_) => {
                self.stack.push(Frame::Quote);
                let (q, d) = self.depth();
                for ld in &mut self.lines[l0..=l1] {
                    ld.block = MdBlock::Quote;
                    ld.quote = q;
                    ld.indent = d;
                    ld.flags &= !L_DONE;
                }
                return;
            }
            Tag::List(first) => Frame::List { ordered: first.is_some() },
            Tag::Item => {
                let (q, d) = self.depth();
                let ordered = matches!(self.stack.last(), Some(Frame::List { ordered: true }));
                for ld in &mut self.lines[l0..=l1] {
                    ld.block = MdBlock::ListItem { depth: d.max(1), checkbox: None };
                    ld.quote = q;
                    ld.indent = d.max(1);
                    ld.flags &= !L_DONE;
                }
                if ordered {
                    // 番号は隠さない
                    let b = &self.text.as_bytes()[r.start..r.end];
                    let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
                    if n > 0 && matches!(b.get(n), Some(b'.' | b')')) {
                        self.content(&(r.start..r.start + n + 1), S_NUMBER);
                    }
                } else {
                    self.lines[l0].flags |= L_BULLET;
                }
                Frame::Item { l0, l1 }
            }
            Tag::Heading { level, .. } => {
                let n = level as u8;
                for ld in &mut self.lines[l0..=l1] {
                    ld.block = MdBlock::Heading(n);
                }
                Frame::Heading { l0, l1 }
            }
            Tag::CodeBlock(kind) => {
                for ld in &mut self.lines[l0..=l1] {
                    ld.block = MdBlock::Code;
                }
                let fenced = matches!(kind, CodeBlockKind::Fenced(_));
                Frame::Code { fenced, fast: top && fenced && r.start == self.starts[l0] as usize, l0, l1, at: r.start }
            }
            Tag::Table(_) => {
                for ld in &mut self.lines[l0..=l1] {
                    ld.flags |= L_TABLE;
                }
                Frame::Block
            }
            _ => Frame::Block,
        };
        self.stack.push(frame);
    }

    fn end(&mut self, f: Frame) {
        match f {
            Frame::Heading { l0, l1 } => {
                // `===` / `---` の下線の行
                for ld in &mut self.lines[l0 + 1..=l1] {
                    if ld.segs.is_empty() {
                        ld.flags |= L_COLLAPSE;
                    }
                }
            }
            Frame::Code { fenced: true, fast, l0, l1, at } => {
                let (s, _) = self.span(l0);
                let open = &self.ltext(l0)[(at - s).min(self.ltext(l0).len())..];
                let ch = open.bytes().next().unwrap_or(b'`');
                let n = open.bytes().take_while(|b| *b == ch).count();
                let mut last = l1;
                if l1 > l0 && self.lines[l1].segs.is_empty() {
                    let t = self.ltext(l1).trim_start_matches([' ', '\t', '>']);
                    let m = t.bytes().take_while(|b| *b == ch).count();
                    if m >= n.max(3) && t[m..].trim().is_empty() {
                        self.fence(l1);
                        last = l1 - 1;
                    }
                }
                self.fence(l0);
                if fast {
                    for ld in &mut self.lines[l0 + 1..=last] {
                        ld.flags |= L_CODE_FAST;
                    }
                }
            }
            _ => {}
        }
    }
    fn fence(&mut self, l: usize) {
        let ld = &mut self.lines[l];
        ld.block = MdBlock::CodeFence;
        ld.flags |= L_COLLAPSE;
        ld.segs.clear();
    }

    fn finish(mut self) -> Vec<LineData> {
        for l in 0..self.lines.len() {
            let t = self.ltext(l);
            let d = &mut self.lines[l];
            d.hash = hash(t);
            let code = matches!(d.block, MdBlock::Code | MdBlock::CodeFence);
            if !code && t.contains("]:") {
                d.flags |= L_REFDEF;
            }
            if !code && t.trim().is_empty() {
                d.block = MdBlock::Blank;
                d.segs.clear();
                d.quote = 0;
                d.indent = 0;
                d.flags &= L_TOP;
            } else if d.has(L_CODE_FAST) {
                // 字下げなしの ``` の中は、行の全部が中身
                d.segs.clear();
            } else if d.has(L_TABLE) {
                d.block = MdBlock::Table;
                d.segs.clear();
            } else if !d.has(L_CLAIMED) {
                d.block = MdBlock::Paragraph;
                d.segs.clear();
            } else {
                // 見える文字のすき間が記号
                let len = t.len() as u32;
                let old = std::mem::take(&mut d.segs);
                let mut pos = 0u32;
                let push = |v: &mut Vec<Seg>, s: Seg| match v.last_mut() {
                    Some(p) if p.flags == s.flags && p.end == s.start => p.end = s.end,
                    _ => v.push(s),
                };
                for s in old {
                    if s.start > pos {
                        push(&mut d.segs, Seg { start: pos, end: s.start, flags: S_MARKER });
                    }
                    push(&mut d.segs, s);
                    pos = s.end;
                }
                if pos < len {
                    push(&mut d.segs, Seg { start: pos, end: len, flags: S_MARKER });
                }
                if d.segs.len() == 1 && d.segs[0].flags == 0 {
                    d.segs.clear();
                }
                // `>` の無い行（前の行の続きとして引用に入った行）は、引用として見せない。
                // 行ごとに、その行の記号どおりに見せる（モック 10 の 7 行目）
                if d.quote > 0 {
                    let marked = d.segs.first().is_some_and(|s| s.flags == S_MARKER && s.start == 0 && t[..s.end as usize].contains('>'));
                    if !marked {
                        d.quote = 0;
                        if d.block == MdBlock::Quote {
                            d.block = MdBlock::Paragraph;
                        }
                    }
                }
            }
        }
        self.lines
    }
}

fn build(text: &str, cancel: Option<&CancelToken>) -> Result<Vec<LineData>, Cancelled> {
    Builder::new(text).run(cancel)
}

/// 1 行だけを解析する。`ctx` は、その行について覚えている結果（あれば、コードブロックの
/// 中かどうかや、箇条書き・引用の深さを引き継ぐ）。
fn parse_one(text: &str, ctx: Option<&LineData>) -> LineData {
    let ctx = ctx.filter(|c| !c.has(L_STALE));
    if let Some(c) = ctx {
        if matches!(c.block, MdBlock::Code | MdBlock::Table) {
            return LineData { hash: hash(text), block: c.block, quote: c.quote, indent: c.indent, flags: 0, segs: Vec::new() };
        }
    }
    let text = text.split('\n').next().unwrap_or("");
    let mut d = match build(text, None) {
        Ok(mut v) if !v.is_empty() => v.swap_remove(0),
        _ => LineData::blank(),
    };
    if let Some(c) = ctx {
        match (d.block, c.block) {
            (MdBlock::Paragraph | MdBlock::Code, MdBlock::ListItem { depth, .. }) => {
                if d.block == MdBlock::Code {
                    d.segs.clear();
                }
                d.block = MdBlock::ListItem { depth, checkbox: None };
                d.indent = c.indent;
                d.quote = c.quote;
                d.flags |= c.flags & L_DONE;
            }
            (MdBlock::Paragraph, MdBlock::Quote) => {
                d.block = MdBlock::Quote;
                d.quote = c.quote.max(1);
                d.indent = c.indent;
            }
            (MdBlock::ListItem { checkbox, .. }, MdBlock::ListItem { depth, .. }) => {
                d.block = MdBlock::ListItem { depth, checkbox };
                d.indent = c.indent;
                d.quote = d.quote.max(c.quote);
            }
            _ => {}
        }
    }
    d
}

/// 覚えている結果から [`MdLine`] を作る。
fn fill(d: &LineData, len: u32, show: bool, out: &mut MdLine) {
    out.block = d.block;
    out.indent = d.indent;
    out.quote = d.quote;
    out.bullet = d.has(L_BULLET);
    out.collapse = d.has(L_COLLAPSE);
    out.runs.clear();
    let heading = matches!(d.block, MdBlock::Heading(_));
    let done = d.has(L_DONE);
    let (font, base) = match d.block {
        MdBlock::Heading(n) => (MdFont::Heading(n), Style::new(colors::HEADING).with(Attrs::BOLD)),
        MdBlock::Code | MdBlock::Table => (MdFont::Code, Style::new(palette::FG)),
        _ if done => (MdFont::Body, Style::new(colors::DONE).with(Attrs::STRIKE)),
        _ if d.quote > 0 => (MdFont::Body, Style::new(colors::QUOTE)),
        _ => (MdFont::Body, Style::new(palette::FG)),
    };
    let mut pos = 0u32;
    for s in &d.segs {
        let (a, b) = (s.start.max(pos).min(len), s.end.min(len));
        if a > pos {
            out.runs.push(MdRun { range: pos..a, font, style: base, hidden: false });
        }
        if b <= a {
            continue;
        }
        let f = s.flags;
        let run = if f & S_MARKER != 0 {
            MdRun { range: a..b, font: MdFont::Marker, style: Style::new(colors::MARKER), hidden: !show }
        } else {
            let mut st = base;
            let mut ft = font;
            if f & S_BOLD != 0 {
                st = st.with(Attrs::BOLD);
                if !done && !heading {
                    st.fg = colors::STRONG;
                }
            }
            if f & S_ITALIC != 0 {
                st = st.with(Attrs::ITALIC);
            }
            if f & S_STRIKE != 0 {
                st = st.with(Attrs::STRIKE);
            }
            if f & S_LINK != 0 {
                st.fg = colors::LINK;
                st = st.with(Attrs::UNDERLINE);
            }
            if f & S_CODE != 0 {
                ft = MdFont::Code;
                st.fg = colors::CODE_FG;
                st.bg = Some(colors::CODE_BG);
            }
            if f & S_NUMBER != 0 {
                st = Style::new(colors::NUMBER);
            }
            MdRun { range: a..b, font: ft, style: st, hidden: false }
        };
        out.runs.push(run);
        pos = b;
    }
    if pos < len {
        out.runs.push(MdRun { range: pos..len, font, style: base, hidden: false });
    }
}

/// `edited` がメインで解析し直す範囲の上限の既定値（バイト）。
pub const REGION_LIMIT: usize = 16 * 1024;

/// 文書全体の解析結果。メインスレッドが持つ。
pub struct MdDocument {
    lines: Vec<LineData>,
    /// 参照リンクの定義かもしれない行の数。
    refdefs: u32,
    /// 全体の再解析を頼んだまま、まだ結果を受け取っていない。
    dirty: bool,
    region_limit: usize,
}

impl MdDocument {
    /// 空の文書。
    pub fn new() -> MdDocument {
        MdDocument::from_lines(vec![LineData::blank()])
    }
    fn from_lines(lines: Vec<LineData>) -> MdDocument {
        let refdefs = lines.iter().filter(|l| l.has(L_REFDEF)).count() as u32;
        MdDocument { lines, refdefs, dirty: false, region_limit: REGION_LIMIT }
    }
    /// **batch スレッドで呼ぶ。** 全体を解析する。途中で `cancel` を見て、古ければやめる。
    pub fn parse(src: &dyn LineSource, cancel: &CancelToken) -> Result<Versioned<MdDocument>, Cancelled> {
        cancel.check()?;
        let n = src.len_lines().max(1);
        let mut text = String::new();
        let mut buf = String::new();
        for i in 0..n {
            if i & 255 == 255 {
                cancel.check()?;
            }
            push_line(src, i, i > 0, &mut buf, &mut text);
        }
        let mut lines = build(&text, Some(cancel))?;
        lines.resize(n as usize, LineData::stale());
        Ok(Versioned::new(cancel.version(), MdDocument::from_lines(lines)))
    }
    /// batch の結果で置き換える。`current` と版が違えば捨てて `false`。
    pub fn replace(&mut self, parsed: Versioned<MdDocument>, current: Version) -> bool {
        if parsed.version != current {
            return false;
        }
        let limit = self.region_limit;
        *self = parsed.value;
        self.region_limit = limit;
        true
    }
    /// 覚えている行数。
    pub fn len_lines(&self) -> u32 {
        self.lines.len() as u32
    }
    /// 全体の再解析を頼んだまま（[`MdDocument::edited`] が `true` を返してから、
    /// [`MdDocument::replace`] が成功するまで）か。
    pub fn needs_reparse(&self) -> bool {
        self.dirty
    }
    /// [`MdDocument::edited`] がその場で解析し直す範囲の上限（バイト）。これより大きい
    /// ブロックの中の編集は、編集した行だけをその場で解析して、全体の再解析を頼む。
    pub fn set_region_limit(&mut self, bytes: usize) {
        self.region_limit = bytes;
    }

    /// 編集を知らせ、その行を含むブロックだけ解析し直す（メインで呼ぶ）。
    /// `src` は編集した後の文書。ブロックの境界が変わって全体の再解析が要るなら
    /// `true` を返す（batch に出す。結果を [`MdDocument::replace`] で受け取るまでは、
    /// 以後の呼び出しも `true` を返す）。
    pub fn edited(&mut self, src: &dyn LineSource, change: LineChange) -> bool {
        let n_new = src.len_lines().max(1) as usize;
        let n_old = self.lines.len();
        let (os, oe) = (change.old.start as usize, change.old.end.max(change.old.start) as usize);
        let (ns, ne) = (change.new.start as usize, change.new.end.max(change.new.start) as usize);
        if os != ns || oe > n_old || ne > n_new || n_old - (oe - os) + (ne - ns) != n_new {
            // 行数が合わない: 全部を「分からない」にして、解析し直してもらう
            self.lines.clear();
            self.lines.resize(n_new, LineData::stale());
            self.refdefs = 0;
            self.dirty = true;
            return true;
        }
        let mut buf = String::new();
        // 外側の ``` の中の 1 行: 囲みにならない限り、何も変わらない
        if oe - os == 1 && ne - ns == 1 && self.lines[os].has(L_CODE_FAST) && !self.lines[os].has(L_STALE) {
            src.line(os as u32, &mut buf);
            // 空白だけの行は、最後の行だとブロックの外になるので、ふつうに解析する
            if !looks_like_fence(&buf) && !buf.contains('\n') &&!buf.trim().is_empty() {
                self.lines[os].hash = hash(&buf);
                self.lines[os].segs.clear();
                return self.dirty;
            }
        }
        // 解析し直す範囲: 前の外側のブロックの先頭 〜 後ろの外側のブロックの先頭の行
        // （表は「次の行が区切りの行か」で始まりが決まるので、編集した行の 2 行以上前から）
        let a = (0..os.saturating_sub(1)).rev().find(|&i| self.lines[i].has(L_TOP)).unwrap_or(0);
        let g_old = (oe..n_old).find(|&i| self.lines[i].has(L_TOP));
        let g_new = g_old.map(|g| g - oe + ne);
        let end_new = g_new.map_or(n_new, |g| g + 1);
        let mut text = String::new();
        let mut too_big = false;
        for i in a..end_new {
            push_line(src, i as u32, i > a, &mut buf, &mut text);
            if text.len() > self.region_limit {
                too_big = true;
                break;
            }
        }
        let had_refdef = self.refdefs > 0;
        let (new_lines, old_end, ok) = if too_big {
            (vec![LineData::stale(); ne - ns], oe, false)
        } else {
            let mut v = build(&text, None).unwrap_or_default();
            v.resize(end_new - a, LineData::stale());
            match g_old {
                Some(g) => {
                    // 後ろのブロックが、解析し直しても同じ行で始まっていれば、そこから先はそのまま
                    let ok = v.pop().is_some_and(|l| l.has(L_TOP));
                    (v, g, ok)
                }
                None => (v, n_old, true),
            }
        };
        let from = if too_big { os } else { a };
        let add = new_lines.iter().filter(|l| l.has(L_REFDEF)).count() as u32;
        let removed = self.lines.splice(from..old_end, new_lines).filter(|l| l.has(L_REFDEF)).count() as u32;
        self.refdefs = self.refdefs + add - removed.min(self.refdefs);
        debug_assert_eq!(self.lines.len(), n_new);
        if !ok || had_refdef || self.refdefs > 0 {
            self.dirty = true;
        }
        self.dirty
    }

    /// `line` 行目のレイアウトの元。`cursor_line` と同じ行なら記号を見せる
    /// （`hidden` が全部 `false` になる）。
    ///
    /// `text` が覚えている行と違えば（未確定文字列を挟んだ行など）、その場で 1 行だけ
    /// 解析する。
    pub fn line(&self, line: u32, text: &str, cursor_line: Option<u32>) -> MdLine {
        let mut out = MdLine::new();
        self.line_into(line, text, cursor_line, &mut out);
        out
    }
    /// [`MdDocument::line`] と同じ。`out` の領域を使い回す。
    pub fn line_into(&self, line: u32, text: &str, cursor_line: Option<u32>, out: &mut MdLine) {
        let show = cursor_line == Some(line);
        let len = text.len() as u32;
        match self.lines.get(line as usize) {
            Some(d) if !d.has(L_STALE) && d.hash == hash(text) => fill(d, len, show, out),
            ctx => fill(&parse_one(text, ctx), len, show, out),
        }
    }
    /// `line` 行目のチェックボックスを切り替えた文字列（Ctrl+Enter）。
    /// チェックボックスの行でなければ `None`。
    pub fn toggle_checkbox(&self, line: u32, text: &str) -> Option<String> {
        let l = self.line(line, text, None);
        let MdBlock::ListItem { checkbox: Some(_), .. } = l.block else { return None };
        let b = text.as_bytes();
        for r in l.runs.iter().filter(|r| r.font == MdFont::Marker) {
            let (s, e) = (r.range.start as usize, (r.range.end as usize).min(b.len()));
            for i in s..e.saturating_sub(2) {
                if b[i] == b'[' && b[i + 2] == b']' && matches!(b[i + 1], b' ' | b'x' | b'X') {
                    let mut out = String::with_capacity(text.len());
                    out.push_str(&text[..i + 1]);
                    out.push(if b[i + 1] == b' ' { 'x' } else { ' ' });
                    out.push_str(&text[i + 2..]);
                    return Some(out);
                }
            }
        }
        None
    }
}

/// `src` の `i` 行目を `text` に足す（`sep` なら前に改行を入れる）。行の中の改行は
/// 空白に置き換える（行数をずらさない）。
fn push_line(src: &dyn LineSource, i: u32, sep: bool, buf: &mut String, text: &mut String) {
    src.line(i, buf);
    if sep {
        text.push('\n');
    }
    if buf.contains('\n') {
        text.extend(buf.chars().map(|c| if c == '\n' { ' ' } else { c }));
    } else {
        text.push_str(buf);
    }
}

const _: () = {
    const fn send<T: Send>() {}
    send::<MdDocument>()
};

impl Default for MdDocument {
    fn default() -> Self {
        MdDocument::new()
    }
}

#[cfg(test)]
mod tests;
