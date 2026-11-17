//! 画面（格子・スクロールバック・カーソル・モード）と、`vte` から届く動作の解釈。
//!
//! pty には触らない（[`crate::Term`] が読み書きする）。返事が要るシーケンス
//! （DSR・DA）は [`Screen::replies`] に溜め、`Term` が子プロセスへ書く。
//!
//! 全角の字は左のマスに `wide: true` で入れ、右のマスは `' '` の詰め物にする。
//! 片方だけを上書き・消去したときは、もう片方も空白に戻す（字が半分だけ残らないように）。

use crate::{Cell, TermEvent};
use std::collections::VecDeque;
use unicode_width::UnicodeWidthChar;
use vte::{Params, ParamsIter, Perform};
use zc_core::{palette, Attrs, Color, Style};

/// スクロールバックの行数。
pub(crate) const SCROLLBACK: usize = 1000;

/// 溜めておくイベントの上限（取り出されないまま BEL が続いても膨らまないように）。
const MAX_EVENTS: usize = 256;

/// スクロールバックの 1 行。マスの格子（1 マス 16 バイト）のままだと 53 桁 1000 行で
/// 約 0.85MB になるので、字（4 バイト）と見た目の変わり目だけを持つ（1000 行で約 0.3MB）。
///
/// `chars` はマスごとの字で、最上位ビットが全角の左半分の印（右隣の詰め物も 1 マスとして
/// 入る）。行末の空白（既定の見た目）は持たない。`runs` は見た目が変わるマスの位置と見た目。
#[derive(Default)]
pub(crate) struct SbLine {
    chars: Vec<u32>,
    runs: Vec<(u16, Style)>,
}

const WIDE_BIT: u32 = 1 << 31;

impl SbLine {
    /// 画面の行を詰めて入れる（持っている領域は使い回す）。
    fn store(&mut self, row: &[Cell]) {
        let n = row.iter().rposition(|c| *c != BLANK).map_or(0, |i| i + 1);
        // 全角の詰め物が既定の見た目でも、左半分と一緒に残す
        let n = if n > 0 && n < row.len() && row[n - 1].wide { n + 1 } else { n };
        let row = &row[..n];
        self.chars.clear();
        self.chars.extend(row.iter().map(|c| c.ch as u32 | if c.wide { WIDE_BIT } else { 0 }));
        self.runs.clear();
        let mut cur = BLANK.style;
        for (i, c) in row.iter().enumerate() {
            if c.style != cur || i == 0 {
                self.runs.push((i as u16, c.style));
                cur = c.style;
            }
        }
    }

    /// `cols` 桁のマスの行に戻す。
    fn expand(&self, cols: usize, out: &mut Vec<Cell>) {
        out.clear();
        let mut runs = self.runs.iter().peekable();
        let mut style = BLANK.style;
        for (i, &v) in self.chars.iter().enumerate().take(cols) {
            while let Some(&&(at, st)) = runs.peek() {
                if at as usize > i {
                    break;
                }
                style = st;
                runs.next();
            }
            let ch = char::from_u32(v & !WIDE_BIT).unwrap_or(' ');
            out.push(Cell { ch, style, wide: v & WIDE_BIT != 0 });
        }
        out.resize(cols, BLANK);
        fix_last_col(out, BLANK);
    }
}

/// 何も書いていないマス（既定の色）。
const BLANK: Cell = Cell { ch: ' ', style: Style::new(palette::FG), wide: false };

/// VS Code Dark+ の端末の 16 色（`terminal.ansi*` の既定値）。
const ANSI16: [u32; 16] = [
    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, //
    0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xe5e5e5,
];

/// 256 色の表（16 色 + 6×6×6 の立方体 + 灰色 24 段）。
static PALETTE256: [Color; 256] = palette256();

const fn palette256() -> [Color; 256] {
    let mut t = [Color(0); 256];
    let mut i = 0;
    while i < 16 {
        t[i] = Color::hex(ANSI16[i]);
        i += 1;
    }
    const LV: [u8; 6] = [0, 95, 135, 175, 215, 255];
    while i < 232 {
        let n = i - 16;
        t[i] = Color::rgb(LV[n / 36], LV[(n / 6) % 6], LV[n % 6]);
        i += 1;
    }
    while i < 256 {
        let v = (8 + (i - 232) * 10) as u8;
        t[i] = Color::rgb(v, v, v);
        i += 1;
    }
    t
}

/// 256 色の番号から色を引く。
pub(crate) fn indexed_color(i: u8) -> Color {
    PALETTE256[i as usize]
}

/// SGR で指定された色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Ink {
    #[default]
    Default,
    Index(u8),
    Rgb(Color),
}

/// 今の文字の装飾（SGR の状態）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct Pen {
    fg: Ink,
    bg: Ink,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    reverse: bool,
    hidden: bool,
    strike: bool,
}

impl Pen {
    /// マスに入れる [`Style`] に直す。既定の背景は `None`（パネルの地の色）。
    /// 太字は 0〜7 の色を明るい方に替える（VS Code の既定と同じ）。暗い（dim）は
    /// 文字色を背景へ半分寄せ、隠す（hidden）は文字色を背景と同じにする。
    fn style(&self) -> Style {
        let bg = match self.bg {
            Ink::Default => None,
            Ink::Index(i) => Some(indexed_color(i)),
            Ink::Rgb(c) => Some(c),
        };
        let mut fg = match self.fg {
            Ink::Default => palette::FG,
            Ink::Index(i) if self.bold && i < 8 => indexed_color(i + 8),
            Ink::Index(i) => indexed_color(i),
            Ink::Rgb(c) => c,
        };
        let ground = bg.unwrap_or(palette::BG);
        if self.dim {
            fg = fg.mix(ground, 128);
        }
        if self.hidden {
            fg = ground;
        }
        let mut a = Attrs::NONE;
        if self.bold {
            a = a | Attrs::BOLD;
        }
        if self.italic {
            a = a | Attrs::ITALIC;
        }
        if self.underline {
            a = a | Attrs::UNDERLINE;
        }
        if self.strike {
            a = a | Attrs::STRIKE;
        }
        if self.reverse {
            a = a | Attrs::REVERSE;
        }
        Style { fg, bg, attrs: a }
    }
}

/// カーソル。`wrap` は「右端に書いた直後で、次の字で折り返す」状態。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct Cursor {
    x: usize,
    y: usize,
    wrap: bool,
}

/// DECSC で保存するもの。
#[derive(Clone, Copy, Debug)]
struct Saved {
    x: usize,
    y: usize,
    pen: Pen,
    origin: bool,
    charsets: [bool; 2],
    gl: usize,
}

/// 画面 1 枚ぶんの状態（主画面と代替画面、スクロールバック、モード）。
pub(crate) struct Screen {
    cols: usize,
    rows: usize,
    /// 表示している画面の行（主画面か代替画面）。
    grid: Vec<Vec<Cell>>,
    /// 表示していない方の画面。
    other: Vec<Vec<Cell>>,
    /// 代替画面を表示中か。
    alt: bool,
    /// 主画面の上から流れた行（古い順）。
    sb: VecDeque<SbLine>,
    /// さかのぼって表示している行（`view > 0` のときだけ使う。スクロールバックを戻したもの）。
    view_rows: Vec<Vec<Cell>>,
    cur: Cursor,
    pen: Pen,
    /// `pen` から作った、書く字の見た目。
    style: Style,
    /// 消去に使うマス（背景色だけ今の色を引き継ぐ。xterm の bce）。
    erase: Cell,
    /// 保存したカーソル（主画面・代替画面で別々）。
    saved: [Option<Saved>; 2],
    /// スクロール範囲（両端を含む）。
    top: usize,
    bottom: usize,
    autowrap: bool,
    cursor_visible: bool,
    /// DECCKM（矢印キーをアプリケーションモードで送る）。
    pub(crate) app_cursor: bool,
    /// 括弧付き貼り付けモード（`CSI ? 2004 h`）。
    pub(crate) bracketed_paste: bool,
    origin: bool,
    insert: bool,
    tabs: Vec<bool>,
    /// G0・G1 が DEC の罫線文字集合か。
    charsets: [bool; 2],
    /// 今使っている文字集合（0 = G0、1 = G1）。
    gl: usize,
    /// スクロールバックをさかのぼっている行数（0 なら最新）。
    view: usize,
    dirty: u64,
    /// 前回 [`Screen::take_dirty_rows`] のときのカーソル。
    last_cursor: Option<(u16, u16)>,
    /// 画面かカーソルが変わった（[`crate::Term::read`] が見て消す）。
    pub(crate) changed: bool,
    pub(crate) events: Vec<TermEvent>,
    /// 子プロセスへの返事（DSR・DA）。
    pub(crate) replies: Vec<u8>,
}

fn blank_grid(cols: usize, rows: usize) -> Vec<Vec<Cell>> {
    (0..rows).map(|_| vec![BLANK; cols]).collect()
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|x| x % 8 == 0 && x != 0).collect()
}

/// DEC の罫線文字集合（`ESC ( 0`）。
fn dec_graphics(c: char) -> char {
    const T: [char; 32] = [
        ' ', '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', //
        '⎺', '⎻', '─', '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
    ];
    match c {
        '\x5f'..='\x7e' => T[c as usize - 0x5f],
        _ => c,
    }
}

/// 行の `[x0, x1)` を書き換える前に、範囲の端で全角の字が割れるならもう片方を空白にする。
fn split_wide_edges(row: &mut [Cell], x0: usize, x1: usize, blank: Cell) {
    let cols = row.len();
    if x0 > 0 && x0 < cols && row[x0 - 1].wide {
        row[x0 - 1] = blank;
    }
    if x1 > 0 && x1 < cols && row[x1 - 1].wide {
        row[x1] = blank;
    }
}

/// 右端に左半分だけ残った全角の字を消す（幅を変えた・字をずらしたあと）。
fn fix_last_col(row: &mut [Cell], blank: Cell) {
    if let Some(last) = row.last_mut() {
        if last.wide {
            *last = blank;
        }
    }
}

/// 数値の引数（各グループの先頭）を最大 16 個取り出す。
fn nums(params: &Params) -> ([u16; 16], usize) {
    let mut a = [0u16; 16];
    let mut n = 0;
    for g in params.iter() {
        if n == a.len() {
            break;
        }
        a[n] = g.first().copied().unwrap_or(0);
        n += 1;
    }
    (a, n)
}

impl Screen {
    pub(crate) fn new(cols: u16, rows: u16) -> Screen {
        let cols = (cols as usize).max(1);
        let rows = (rows as usize).max(1);
        let pen = Pen::default();
        Screen {
            cols,
            rows,
            grid: blank_grid(cols, rows),
            other: blank_grid(cols, rows),
            alt: false,
            sb: VecDeque::new(),
            view_rows: Vec::new(),
            cur: Cursor::default(),
            pen,
            style: pen.style(),
            erase: BLANK,
            saved: [None, None],
            top: 0,
            bottom: rows - 1,
            autowrap: true,
            cursor_visible: true,
            app_cursor: false,
            bracketed_paste: false,
            origin: false,
            insert: false,
            tabs: default_tabs(cols),
            charsets: [false, false],
            gl: 0,
            view: 0,
            dirty: u64::MAX,
            last_cursor: None,
            changed: true,
            events: Vec::new(),
            replies: Vec::new(),
        }
    }

    // ---- 外から見る ----

    pub(crate) fn size(&self) -> (u16, u16) {
        (self.cols as u16, self.rows as u16)
    }

    /// 表示中の `r` 行目（スクロールバックをさかのぼっていればその位置）。
    pub(crate) fn row(&self, r: u16) -> &[Cell] {
        let r = r as usize;
        if r >= self.rows {
            return &[];
        }
        if self.view == 0 {
            &self.grid[r]
        } else {
            &self.view_rows[r]
        }
    }

    pub(crate) fn cursor(&self) -> Option<(u16, u16)> {
        if !self.cursor_visible || self.view != 0 {
            return None;
        }
        Some((self.cur.x as u16, self.cur.y as u16))
    }

    /// カーソルの位置・表示（`changed` の判定用。スクロール位置は見ない）。
    pub(crate) fn cursor_state(&self) -> (usize, usize, bool) {
        (self.cur.x, self.cur.y, self.cursor_visible)
    }

    pub(crate) fn take_dirty_rows(&mut self) -> u64 {
        let c = self.cursor();
        if c != self.last_cursor {
            for p in [c, self.last_cursor].into_iter().flatten() {
                self.mark(p.1 as usize);
            }
            self.last_cursor = c;
        }
        let mask = if self.rows >= 64 { u64::MAX } else { (1u64 << self.rows) - 1 };
        let d = self.dirty & mask;
        self.dirty = 0;
        d
    }

    pub(crate) fn alt_screen(&self) -> bool {
        self.alt
    }

    pub(crate) fn scrollback_len(&self) -> usize {
        self.sb.len()
    }

    /// スクロールバックをさかのぼる（正で過去へ）。代替画面の表示中は何もしない。
    pub(crate) fn scroll_view(&mut self, delta: i32) {
        if self.alt {
            return;
        }
        let v = (self.view as i64 + delta as i64).clamp(0, self.sb.len() as i64) as usize;
        if v != self.view {
            self.view = v;
            self.fill_view();
            self.mark_all();
        }
    }

    /// さかのぼった位置の行を `view_rows` に戻す（出力・入力で最新に戻るので、
    /// さかのぼっている間は画面もスクロールバックも変わらない）。
    fn fill_view(&mut self) {
        self.view_rows.resize_with(self.rows, Vec::new);
        let top = self.sb.len() - self.view;
        for r in 0..self.rows {
            let i = top + r;
            let dst = &mut self.view_rows[r];
            if i < self.sb.len() {
                self.sb[i].expand(self.cols, dst);
            } else {
                dst.clear();
                dst.extend_from_slice(&self.grid[i - self.sb.len()]);
            }
        }
    }

    /// さかのぼっていたら最新の位置へ戻す。
    pub(crate) fn snap_to_bottom(&mut self) {
        if self.view != 0 {
            self.view = 0;
            self.mark_all();
        }
    }

    pub(crate) fn push_event(&mut self, ev: TermEvent) {
        if ev == TermEvent::Bell && self.events.last() == Some(&TermEvent::Bell) {
            return;
        }
        if self.events.len() >= MAX_EVENTS && !matches!(ev, TermEvent::CommandExit(_)) {
            return;
        }
        self.events.push(ev);
    }

    // ---- 大きさ ----

    /// 大きさを変える。折り返しの付け直しはしない（各行を切り詰めるか空白で伸ばす）。
    /// 行が減るときはカーソルより下の行から捨て、足りなければ上の行をスクロールバックへ
    /// 送る。行が増えるときは、主画面ならスクロールバックから行を戻す（xterm と同じ）。
    pub(crate) fn resize(&mut self, cols: u16, rows: u16) {
        let cols = (cols as usize).max(1);
        let rows = (rows as usize).max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        // 主画面のカーソルの行（代替画面の表示中は保存した位置）
        let mut main_y = if self.alt { self.saved[0].map_or(self.rows - 1, |s| s.y) } else { self.cur.y };
        let mut alt_y = if self.alt { self.cur.y } else { 0 };
        let (main, alt) = if self.alt { (&mut self.other, &mut self.grid) } else { (&mut self.grid, &mut self.other) };
        resize_rows(main, &mut main_y, rows, self.cols, Some(&mut self.sb));
        resize_rows(alt, &mut alt_y, rows, self.cols, None);
        for row in self.grid.iter_mut().chain(self.other.iter_mut()) {
            row.resize(cols, BLANK);
            fix_last_col(row, BLANK);
        }
        self.cols = cols;
        self.rows = rows;
        if self.alt {
            self.cur.y = alt_y;
            if let Some(s) = &mut self.saved[0] {
                s.y = main_y;
            }
        } else {
            self.cur.y = main_y;
        }
        self.cur.x = self.cur.x.min(cols - 1);
        self.cur.y = self.cur.y.min(rows - 1);
        self.cur.wrap = false;
        for s in self.saved.iter_mut().flatten() {
            s.x = s.x.min(cols - 1);
            s.y = s.y.min(rows - 1);
        }
        self.top = 0;
        self.bottom = rows - 1;
        let old = self.tabs.len();
        self.tabs.resize(cols, false);
        for (x, t) in self.tabs.iter_mut().enumerate().skip(old) {
            *t = x % 8 == 0;
        }
        self.view = 0;
        self.mark_all();
    }

    // ---- 印 ----

    #[inline]
    fn mark(&mut self, y: usize) {
        self.dirty |= if y < 64 { 1u64 << y } else { u64::MAX };
        self.changed = true;
    }

    /// `y0..=y1` 行に印を付ける。
    fn mark_range(&mut self, y0: usize, y1: usize) {
        if y1 >= 63 {
            self.mark_all();
            return;
        }
        self.dirty |= ((1u64 << (y1 + 1)) - 1) & !((1u64 << y0) - 1);
        self.changed = true;
    }

    fn mark_all(&mut self) {
        self.dirty = u64::MAX;
        self.changed = true;
    }

    fn update_style(&mut self) {
        self.style = self.pen.style();
        let bg = match self.pen.bg {
            Ink::Default => None,
            Ink::Index(i) => Some(indexed_color(i)),
            Ink::Rgb(c) => Some(c),
        };
        self.erase = Cell { ch: ' ', style: Style { fg: palette::FG, bg, attrs: Attrs::NONE }, wide: false };
    }

    // ---- 字を書く ----

    /// 直前に書いた字（REP 用。カーソルの左のマス、折り返し待ちならカーソルのマス）。
    fn char_before_cursor(&self) -> Option<char> {
        let row = &self.grid[self.cur.y];
        let x = if self.cur.wrap { self.cur.x } else { self.cur.x.checked_sub(1)? };
        let x = if x > 0 && row[x - 1].wide { x - 1 } else { x };
        Some(row[x].ch)
    }

    #[inline]
    fn put_char(&mut self, c: char) {
        // 速い道: ASCII を行の途中に書く（折り返し待ち・挿入モード・罫線文字集合でなく、
        // 全角の片割れにも当たらない）。ほとんどの出力はここを通る
        let (x, y) = (self.cur.x, self.cur.y);
        if (c as u32) < 0x7f && !self.cur.wrap && !self.insert && !self.charsets[self.gl & 1] && x + 1 < self.cols {
            let row = &mut self.grid[y];
            if !row[x].wide && (x == 0 || !row[x - 1].wide) {
                row[x] = Cell { ch: c, style: self.style, wide: false };
                self.cur.x = x + 1;
                self.mark(y);
                return;
            }
        }
        self.put_char_slow(c);
    }

    fn put_char_slow(&mut self, c: char) {
        let c = if self.charsets[self.gl & 1] { dec_graphics(c) } else { c };
        let w = if (c as u32) < 0x7f {
            1
        } else {
            match c.width() {
                Some(w) if w > 0 => w,
                // 結合文字・幅 0 の字は 1 マスに入れられないので捨てる
                _ => return,
            }
        };
        let cols = self.cols;
        if self.cur.wrap {
            self.cur.wrap = false;
            self.cur.x = 0;
            self.index();
        }
        let w = if cols < 2 { 1 } else { w };
        if w == 2 && self.cur.x + 1 >= cols {
            if self.autowrap {
                // 右端に入らない全角は、右端を空けて次の行へ
                let (x, y, e) = (self.cur.x, self.cur.y, self.erase);
                split_wide_edges(&mut self.grid[y], x, x + 1, e);
                self.grid[y][x] = e;
                self.mark(y);
                self.cur.x = 0;
                self.index();
            } else {
                self.cur.x = cols - 2;
            }
        }
        if self.insert {
            self.insert_cells(w);
        }
        let (x, y) = (self.cur.x, self.cur.y);
        let style = self.style;
        let e = self.erase;
        let row = &mut self.grid[y];
        split_wide_edges(row, x, x + w, e);
        row[x] = Cell { ch: c, style, wide: w == 2 };
        if w == 2 {
            row[x + 1] = Cell { ch: ' ', style, wide: false };
        }
        self.mark(y);
        if x + w >= cols {
            self.cur.x = cols - 1;
            self.cur.wrap = self.autowrap;
        } else {
            self.cur.x = x + w;
        }
    }

    // ---- 行送り・スクロール ----

    /// 1 行下へ（スクロール範囲の下端ならスクロール）。
    fn index(&mut self) {
        if self.cur.y == self.bottom {
            self.scroll_up(1);
        } else if self.cur.y + 1 < self.rows {
            self.cur.y += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cur.y == self.top {
            self.scroll_down(1);
        } else if self.cur.y > 0 {
            self.cur.y -= 1;
        }
    }

    /// スクロール範囲を `n` 行上へ送る。範囲の上端が画面の上端で主画面なら、
    /// 流れた行をスクロールバックへ入れる。
    fn scroll_up(&mut self, n: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        let e = self.erase;
        for _ in 0..n {
            if top == 0 && !self.alt {
                push_sb(&mut self.sb, &self.grid[top]);
            }
            self.grid[top].fill(e);
            self.grid[top..=bottom].rotate_left(1);
        }
        self.mark_range(top, bottom);
    }

    /// スクロール範囲を `n` 行下へ送る（上に空行が入る）。
    fn scroll_down(&mut self, n: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        let e = self.erase;
        for _ in 0..n {
            self.grid[bottom].fill(e);
            self.grid[top..=bottom].rotate_right(1);
        }
        self.mark_range(top, bottom);
    }

    // ---- 消去・挿入・削除 ----

    /// `y` 行の `[x0, x1)` を消す。
    fn erase_cells(&mut self, y: usize, x0: usize, x1: usize) {
        let x1 = x1.min(self.cols);
        if x0 >= x1 {
            return;
        }
        let e = self.erase;
        let row = &mut self.grid[y];
        split_wide_edges(row, x0, x1, e);
        row[x0..x1].fill(e);
        self.mark(y);
    }

    fn erase_rows(&mut self, y0: usize, y1: usize) {
        for y in y0..y1.min(self.rows) {
            self.erase_cells(y, 0, self.cols);
        }
    }

    /// カーソルの位置に空白を `n` 個入れ、右をずらす（ICH・挿入モード）。
    fn insert_cells(&mut self, n: usize) {
        let (x, y, cols, e) = (self.cur.x, self.cur.y, self.cols, self.erase);
        let n = n.min(cols - x);
        let row = &mut self.grid[y];
        split_wide_edges(row, x, x, e);
        row[x..].rotate_right(n);
        row[x..x + n].fill(e);
        fix_last_col(row, e);
        self.mark(y);
    }

    /// カーソルの位置から `n` 個消して左へ詰める（DCH）。
    fn delete_cells(&mut self, n: usize) {
        let (x, y, cols, e) = (self.cur.x, self.cur.y, self.cols, self.erase);
        let n = n.min(cols - x);
        let row = &mut self.grid[y];
        split_wide_edges(row, x, x + n, e);
        row[x..].rotate_left(n);
        row[cols - n..].fill(e);
        self.mark(y);
    }

    /// カーソルの行に空行を `n` 行入れる（IL）。スクロール範囲の外では何もしない。
    fn insert_lines(&mut self, n: usize) {
        let (y, bottom) = (self.cur.y, self.bottom);
        if y < self.top || y > bottom {
            return;
        }
        let n = n.min(bottom - y + 1);
        let e = self.erase;
        for _ in 0..n {
            self.grid[bottom].fill(e);
            self.grid[y..=bottom].rotate_right(1);
        }
        self.cur.x = 0;
        self.cur.wrap = false;
        self.mark_range(y, bottom);
    }

    /// カーソルの行から `n` 行消して上へ詰める（DL）。
    fn delete_lines(&mut self, n: usize) {
        let (y, bottom) = (self.cur.y, self.bottom);
        if y < self.top || y > bottom {
            return;
        }
        let n = n.min(bottom - y + 1);
        let e = self.erase;
        for _ in 0..n {
            self.grid[y].fill(e);
            self.grid[y..=bottom].rotate_left(1);
        }
        self.cur.x = 0;
        self.cur.wrap = false;
        self.mark_range(y, bottom);
    }

    // ---- カーソル ----

    fn goto(&mut self, x: usize, y: usize) {
        let (y0, y1) = if self.origin { (self.top, self.bottom) } else { (0, self.rows - 1) };
        self.cur.x = x.min(self.cols - 1);
        self.cur.y = (y0 + y).min(y1);
        self.cur.wrap = false;
    }

    fn set_x(&mut self, x: usize) {
        self.cur.x = x.min(self.cols - 1);
        self.cur.wrap = false;
    }

    fn move_up(&mut self, n: usize) {
        let lim = if self.cur.y >= self.top { self.top } else { 0 };
        self.cur.y = self.cur.y.saturating_sub(n).max(lim);
        self.cur.wrap = false;
    }

    fn move_down(&mut self, n: usize) {
        let lim = if self.cur.y <= self.bottom { self.bottom } else { self.rows - 1 };
        self.cur.y = (self.cur.y + n).min(lim);
        self.cur.wrap = false;
    }

    fn tab_forward(&mut self, n: usize) {
        for _ in 0..n {
            let mut x = self.cur.x + 1;
            while x < self.cols && !self.tabs[x] {
                x += 1;
            }
            self.cur.x = x.min(self.cols - 1);
        }
    }

    fn tab_backward(&mut self, n: usize) {
        for _ in 0..n {
            let mut x = self.cur.x;
            while x > 0 {
                x -= 1;
                if self.tabs[x] {
                    break;
                }
            }
            self.cur.x = x;
        }
        self.cur.wrap = false;
    }

    fn save_cursor(&mut self) {
        let s = Saved { x: self.cur.x, y: self.cur.y, pen: self.pen, origin: self.origin, charsets: self.charsets, gl: self.gl };
        self.saved[self.alt as usize] = Some(s);
    }

    fn restore_cursor(&mut self) {
        match self.saved[self.alt as usize] {
            Some(s) => {
                self.cur = Cursor { x: s.x.min(self.cols - 1), y: s.y.min(self.rows - 1), wrap: false };
                self.pen = s.pen;
                self.origin = s.origin;
                self.charsets = s.charsets;
                self.gl = s.gl;
            }
            None => {
                self.cur = Cursor::default();
                self.pen = Pen::default();
                self.origin = false;
            }
        }
        self.update_style();
    }

    // ---- 代替画面 ----

    fn enter_alt(&mut self, clear: bool) {
        if self.alt {
            return;
        }
        std::mem::swap(&mut self.grid, &mut self.other);
        self.alt = true;
        self.view = 0;
        if clear {
            self.erase_rows(0, self.rows);
        }
        self.mark_all();
    }

    fn leave_alt(&mut self, clear: bool) {
        if !self.alt {
            return;
        }
        if clear {
            self.erase_rows(0, self.rows);
        }
        std::mem::swap(&mut self.grid, &mut self.other);
        self.alt = false;
        self.mark_all();
    }

    // ---- リセット ----

    fn soft_reset(&mut self) {
        self.pen = Pen::default();
        self.update_style();
        self.cursor_visible = true;
        self.origin = false;
        self.autowrap = true;
        self.insert = false;
        self.app_cursor = false;
        self.top = 0;
        self.bottom = self.rows - 1;
        self.saved = [None, None];
        self.charsets = [false, false];
        self.gl = 0;
        self.cur.wrap = false;
    }

    fn full_reset(&mut self) {
        self.leave_alt(false);
        self.soft_reset();
        self.bracketed_paste = false;
        self.tabs = default_tabs(self.cols);
        self.grid = blank_grid(self.cols, self.rows);
        self.other = blank_grid(self.cols, self.rows);
        self.cur = Cursor::default();
        self.view = 0;
        self.mark_all();
    }

    // ---- モード ----

    fn set_private_mode(&mut self, mode: u16, on: bool) {
        match mode {
            1 => self.app_cursor = on,
            6 => {
                self.origin = on;
                self.goto(0, 0);
            }
            7 => {
                self.autowrap = on;
                if !on {
                    self.cur.wrap = false;
                }
            }
            25 => self.cursor_visible = on,
            47 => {
                if on {
                    self.enter_alt(false)
                } else {
                    self.leave_alt(false)
                }
            }
            1047 => {
                if on {
                    self.enter_alt(false)
                } else {
                    self.leave_alt(true)
                }
            }
            1048 => {
                if on {
                    self.save_cursor()
                } else {
                    self.restore_cursor()
                }
            }
            1049 => {
                if on {
                    if !self.alt {
                        self.save_cursor();
                        self.enter_alt(true);
                    }
                } else if self.alt {
                    self.leave_alt(false);
                    self.restore_cursor();
                }
            }
            2004 => self.bracketed_paste = on,
            _ => {}
        }
    }

    // ---- SGR ----

    fn sgr(&mut self, params: &Params) {
        let mut it = params.iter();
        if params.is_empty() {
            self.pen = Pen::default();
        }
        while let Some(g) = it.next() {
            let p = &mut self.pen;
            match g.first().copied().unwrap_or(0) {
                0 => *p = Pen::default(),
                1 => p.bold = true,
                2 => p.dim = true,
                3 => p.italic = true,
                4 => p.underline = g.get(1).is_none_or(|&s| s != 0),
                7 => p.reverse = true,
                8 => p.hidden = true,
                9 => p.strike = true,
                21 => p.underline = true,
                22 => {
                    p.bold = false;
                    p.dim = false;
                }
                23 => p.italic = false,
                24 => p.underline = false,
                27 => p.reverse = false,
                28 => p.hidden = false,
                29 => p.strike = false,
                n @ 30..=37 => p.fg = Ink::Index(n as u8 - 30),
                38 => {
                    if let Some(c) = ext_color(g, &mut it) {
                        self.pen.fg = c;
                    }
                }
                39 => p.fg = Ink::Default,
                n @ 40..=47 => p.bg = Ink::Index(n as u8 - 40),
                48 => {
                    if let Some(c) = ext_color(g, &mut it) {
                        self.pen.bg = c;
                    }
                }
                49 => p.bg = Ink::Default,
                // 下線の色（使わないが、引数は読み飛ばす）
                58 => {
                    ext_color(g, &mut it);
                }
                n @ 90..=97 => p.fg = Ink::Index(n as u8 - 90 + 8),
                n @ 100..=107 => p.bg = Ink::Index(n as u8 - 100 + 8),
                _ => {}
            }
        }
        self.update_style();
    }
}

/// 38 / 48 / 58 の色の指定を読む。`38;5;n`・`38;2;r;g;b`（セミコロン区切り）と
/// `38:5:n`・`38:2:r:g:b`・`38:2:id:r:g:b`（コロン区切り）。
fn ext_color(g: &[u16], it: &mut ParamsIter) -> Option<Ink> {
    let ch = |v: u16| v.min(255) as u8;
    if g.len() > 1 {
        return match g[1] {
            5 => g.get(2).map(|&n| Ink::Index(ch(n))),
            2 if g.len() >= 5 => {
                let c = &g[g.len() - 3..];
                Some(Ink::Rgb(Color::rgb(ch(c[0]), ch(c[1]), ch(c[2]))))
            }
            _ => None,
        };
    }
    match it.next()?.first().copied() {
        Some(5) => it.next().and_then(|g| g.first().copied()).map(|n| Ink::Index(ch(n))),
        Some(2) => {
            let mut c = [0u8; 3];
            for v in &mut c {
                *v = ch(it.next()?.first().copied().unwrap_or(0));
            }
            Some(Ink::Rgb(Color::rgb(c[0], c[1], c[2])))
        }
        _ => None,
    }
}

/// 主画面・代替画面の行数を変える（幅はあとでまとめて直す）。`y` はその画面のカーソル行。
/// 行をスクロールバックへ入れる（一杯なら一番古い行を捨て、その領域を使い回す）。
fn push_sb(sb: &mut VecDeque<SbLine>, row: &[Cell]) {
    let mut line = if sb.len() >= SCROLLBACK { sb.pop_front().unwrap_or_default() } else { SbLine::default() };
    line.store(row);
    sb.push_back(line);
}

fn resize_rows(grid: &mut Vec<Vec<Cell>>, y: &mut usize, rows: usize, cols: usize, sb: Option<&mut VecDeque<SbLine>>) {
    let old = grid.len();
    if rows < old {
        let excess = old - rows;
        let below = old - 1 - (*y).min(old - 1);
        let drop_bottom = excess.min(below);
        grid.truncate(old - drop_bottom);
        let up = excess - drop_bottom;
        let moved: Vec<Vec<Cell>> = grid.drain(..up).collect();
        if let Some(sb) = sb {
            for line in &moved {
                push_sb(sb, line);
            }
        }
        *y = y.saturating_sub(up);
    } else if rows > old {
        let extra = rows - old;
        let mut pulled = 0;
        if let Some(sb) = sb {
            while pulled < extra {
                let Some(line) = sb.pop_back() else { break };
                let mut row = Vec::with_capacity(cols);
                line.expand(cols, &mut row);
                grid.insert(0, row);
                pulled += 1;
            }
        }
        *y += pulled;
        grid.resize_with(rows, || vec![BLANK; cols]);
    }
}

impl Perform for Screen {
    #[inline]
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.push_event(TermEvent::Bell),
            0x08 => {
                if self.cur.x > 0 && !self.cur.wrap {
                    self.cur.x -= 1;
                }
                self.cur.wrap = false;
            }
            0x09 => self.tab_forward(1),
            0x0a..=0x0c | 0x84 => {
                self.cur.wrap = false;
                self.index();
            }
            0x0d => {
                self.cur.x = 0;
                self.cur.wrap = false;
            }
            0x0e => self.gl = 1,
            0x0f => self.gl = 0,
            0x85 => {
                self.cur.x = 0;
                self.cur.wrap = false;
                self.index();
            }
            0x88 => self.tabs[self.cur.x] = true,
            0x8d => {
                self.cur.wrap = false;
                self.reverse_index();
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let (a, n) = nums(params);
        let arg = |i: usize| if i < n { a[i] as usize } else { 0 };
        // 回数・位置の引数（0 と省略は 1）
        let cnt = |i: usize| arg(i).max(1);
        match (intermediates, action) {
            ([], 'm') => self.sgr(params),
            ([], '@') => self.insert_cells(cnt(0)),
            ([], 'A') => self.move_up(cnt(0)),
            ([], 'B') | ([], 'e') => self.move_down(cnt(0)),
            ([], 'C') | ([], 'a') => self.set_x(self.cur.x + cnt(0)),
            ([], 'D') => self.set_x(self.cur.x.saturating_sub(cnt(0))),
            ([], 'E') => {
                self.move_down(cnt(0));
                self.cur.x = 0;
            }
            ([], 'F') => {
                self.move_up(cnt(0));
                self.cur.x = 0;
            }
            ([], 'G') | ([], '`') => self.set_x(cnt(0) - 1),
            ([], 'H') | ([], 'f') => self.goto(cnt(1) - 1, cnt(0) - 1),
            ([], 'I') => self.tab_forward(cnt(0)),
            ([], 'J') | ([b'?'], 'J') => {
                self.cur.wrap = false;
                let (x, y) = (self.cur.x, self.cur.y);
                match arg(0) {
                    0 => {
                        self.erase_cells(y, x, self.cols);
                        self.erase_rows(y + 1, self.rows);
                    }
                    1 => {
                        self.erase_rows(0, y);
                        self.erase_cells(y, 0, x + 1);
                    }
                    2 => self.erase_rows(0, self.rows),
                    3 => {
                        self.sb.clear();
                        self.view = 0;
                        self.mark_all();
                    }
                    _ => {}
                }
            }
            ([], 'K') | ([b'?'], 'K') => {
                self.cur.wrap = false;
                let (x, y) = (self.cur.x, self.cur.y);
                match arg(0) {
                    0 => self.erase_cells(y, x, self.cols),
                    1 => self.erase_cells(y, 0, x + 1),
                    2 => self.erase_cells(y, 0, self.cols),
                    _ => {}
                }
            }
            ([], 'L') => self.insert_lines(cnt(0)),
            ([], 'M') => self.delete_lines(cnt(0)),
            ([], 'P') => self.delete_cells(cnt(0)),
            ([], 'S') => self.scroll_up(cnt(0)),
            ([], 'T') => self.scroll_down(cnt(0)),
            ([], 'X') => {
                let (x, y) = (self.cur.x, self.cur.y);
                self.erase_cells(y, x, x + cnt(0));
            }
            ([], 'Z') => self.tab_backward(cnt(0)),
            ([], 'b') => {
                if let Some(c) = self.char_before_cursor() {
                    for _ in 0..cnt(0).min(self.cols * self.rows) {
                        self.put_char(c);
                    }
                }
            }
            ([], 'c') if arg(0) == 0 => self.replies.extend_from_slice(b"\x1b[?62;22c"),
            ([b'>'], 'c') if arg(0) == 0 => self.replies.extend_from_slice(b"\x1b[>0;0;0c"),
            ([], 'd') => {
                let x = self.cur.x;
                self.goto(x, cnt(0) - 1);
            }
            ([], 'g') => match arg(0) {
                0 => self.tabs[self.cur.x] = false,
                3 => self.tabs.fill(false),
                _ => {}
            },
            ([], 'h') | ([], 'l') => {
                for &m in &a[..n] {
                    if m == 4 {
                        self.insert = action == 'h';
                    }
                }
            }
            ([b'?'], 'h') | ([b'?'], 'l') => {
                for &m in &a[..n] {
                    self.set_private_mode(m, action == 'h');
                }
            }
            ([], 'n') => match arg(0) {
                5 => self.replies.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let y = if self.origin { self.cur.y - self.top } else { self.cur.y };
                    let s = format!("\x1b[{};{}R", y + 1, self.cur.x + 1);
                    self.replies.extend_from_slice(s.as_bytes());
                }
                _ => {}
            },
            ([], 'r') => {
                let top = cnt(0) - 1;
                let bottom = if arg(1) == 0 { self.rows } else { arg(1) }.min(self.rows) - 1;
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto(0, 0);
                }
            }
            ([], 's') => self.save_cursor(),
            ([], 'u') => self.restore_cursor(),
            ([], 't') if arg(0) == 18 => {
                let s = format!("\x1b[8;{};{}t", self.rows, self.cols);
                self.replies.extend_from_slice(s.as_bytes());
            }
            ([b'!'], 'p') => self.soft_reset(),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => {
                self.cur.wrap = false;
                self.index();
            }
            ([], b'E') => {
                self.cur.x = 0;
                self.cur.wrap = false;
                self.index();
            }
            ([], b'M') => {
                self.cur.wrap = false;
                self.reverse_index();
            }
            ([], b'H') => self.tabs[self.cur.x] = true,
            ([], b'c') => self.full_reset(),
            ([b'('], c) => self.charsets[0] = c == b'0',
            ([b')'], c) => self.charsets[1] = c == b'0',
            ([b'#'], b'8') => {
                for y in 0..self.rows {
                    self.grid[y].fill(Cell { ch: 'E', ..BLANK });
                }
                self.mark_all();
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        match params {
            [b"0" | b"2", rest @ ..] if !rest.is_empty() => {
                let t = rest.join(&b';');
                let t: String = String::from_utf8_lossy(&t).chars().filter(|c| !c.is_control()).take(200).collect();
                self.push_event(TermEvent::Title(t));
            }
            [b"777", b"zcode-exit", code, ..] => {
                if let Some(n) = std::str::from_utf8(code).ok().and_then(|s| s.trim().parse::<i32>().ok()) {
                    self.push_event(TermEvent::CommandExit(n));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(s: &mut Screen, bytes: &[u8]) {
        let mut p = vte::Parser::new();
        p.advance(s, bytes);
    }

    fn text(s: &Screen, r: u16) -> String {
        let mut out = String::new();
        let row = s.row(r);
        let mut i = 0;
        while i < row.len() {
            out.push(row[i].ch);
            i += if row[i].wide { 2 } else { 1 };
        }
        out.trim_end().to_string()
    }

    #[test]
    fn palette_entries() {
        assert_eq!(indexed_color(1), Color::hex(0xcd3131));
        assert_eq!(indexed_color(16), Color::rgb(0, 0, 0));
        assert_eq!(indexed_color(196), Color::rgb(255, 0, 0));
        assert_eq!(indexed_color(231), Color::rgb(255, 255, 255));
        assert_eq!(indexed_color(232), Color::rgb(8, 8, 8));
        assert_eq!(indexed_color(255), Color::rgb(238, 238, 238));
    }

    #[test]
    fn pending_wrap_and_cr() {
        let mut s = Screen::new(5, 3);
        feed(&mut s, b"abcde");
        assert_eq!(s.cursor(), Some((4, 0)));
        assert!(s.cur.wrap);
        // 右端で CR すると折り返さない（readline が頼る動き）
        feed(&mut s, b"\rX");
        assert_eq!(text(&s, 0), "Xbcde");
        assert_eq!(text(&s, 1), "");
        feed(&mut s, b"\x1b[1;5Hyz");
        assert_eq!(text(&s, 0), "Xbcdy");
        assert_eq!(text(&s, 1), "z");
    }

    #[test]
    fn wide_char_at_edge_and_overwrite() {
        let mut s = Screen::new(5, 3);
        feed(&mut s, "abcd漢".as_bytes());
        assert_eq!(text(&s, 0), "abcd");
        assert!(s.row(1)[0].wide);
        assert_eq!(s.row(1)[0].ch, '漢');
        // 全角の右半分を上書きすると左半分も消える
        feed(&mut s, b"\x1b[2;2Hx");
        assert!(!s.row(1)[0].wide);
        assert_eq!(text(&s, 1), " x");
    }

    #[test]
    fn insert_delete_chars() {
        let mut s = Screen::new(8, 2);
        feed(&mut s, b"abcdef\x1b[1;3H\x1b[2@");
        assert_eq!(text(&s, 0), "ab  cdef");
        feed(&mut s, b"\x1b[3P");
        assert_eq!(text(&s, 0), "abdef");
        feed(&mut s, b"\x1b[1;2H\x1b[2X");
        assert_eq!(text(&s, 0), "a  ef");
    }

    #[test]
    fn sgr_colon_and_semicolon() {
        let mut s = Screen::new(10, 2);
        feed(&mut s, b"\x1b[38:2::10:20:30;48;5;196ma\x1b[38;2;1;2;3;4mb\x1b[0m");
        let a = s.row(0)[0].style;
        assert_eq!(a.fg, Color::rgb(10, 20, 30));
        assert_eq!(a.bg, Some(indexed_color(196)));
        let b = s.row(0)[1].style;
        assert_eq!(b.fg, Color::rgb(1, 2, 3));
        assert!(b.attrs.contains(Attrs::UNDERLINE));
        assert_eq!(s.style, Style::new(palette::FG));
    }

    #[test]
    fn dec_graphics_charset() {
        let mut s = Screen::new(10, 2);
        feed(&mut s, b"\x1b(0lqk\x1b(Bq");
        assert_eq!(text(&s, 0), "┌─┐q");
    }

    #[test]
    fn rep_and_tabs() {
        let mut s = Screen::new(20, 2);
        feed(&mut s, b"a\x1b[3b\tz");
        assert_eq!(text(&s, 0), "aaaa    z");
        feed(&mut s, b"\x1b[Zy");
        assert_eq!(text(&s, 0), "aaaa    y");
    }

    #[test]
    fn replies() {
        let mut s = Screen::new(20, 5);
        feed(&mut s, b"\x1b[3;4H\x1b[6n\x1b[5n");
        assert_eq!(s.replies, b"\x1b[3;4R\x1b[0n");
    }

    #[test]
    fn scrollback_line_roundtrip() {
        let mut s = Screen::new(8, 2);
        feed(&mut s, "\x1b[31ma漢\x1b[0mb \x1b[44m \x1b[0m".as_bytes());
        let orig = s.row(0).to_vec();
        assert!(orig[1].wide && orig[5].style.bg.is_some());
        let mut l = SbLine::default();
        l.store(&orig);
        let mut out = Vec::new();
        l.expand(8, &mut out);
        assert_eq!(out, orig);
        // 狭くすると、右端で割れる全角は空白になる
        l.expand(2, &mut out);
        assert_eq!(out, [orig[0], BLANK]);
        // 画面から流れた行をさかのぼって見る
        feed(&mut s, b"\r\n\r\n");
        assert_eq!(s.scrollback_len(), 1);
        s.scroll_view(1);
        assert_eq!(s.row(0), &orig[..]);
        assert_eq!(text(&s, 1), "");
        // 全角の右半分が既定の見た目でも、行末で切り落とさない
        let mut s = Screen::new(6, 2);
        feed(&mut s, "ab漢".as_bytes());
        let orig = s.row(0).to_vec();
        l.store(&orig);
        l.expand(6, &mut out);
        assert_eq!(out, orig);
    }

    #[test]
    fn resize_keeps_cursor_line_and_pulls_back() {
        let mut s = Screen::new(10, 4);
        feed(&mut s, b"1\r\n2\r\n3\r\n4\r\n5\r\n6");
        assert_eq!(s.scrollback_len(), 2);
        assert_eq!(text(&s, 3), "6");
        s.resize(10, 2);
        assert_eq!(text(&s, 0), "5");
        assert_eq!(text(&s, 1), "6");
        assert_eq!(s.cursor(), Some((1, 1)));
        assert_eq!(s.scrollback_len(), 4);
        s.resize(6, 5);
        assert_eq!(text(&s, 0), "2");
        assert_eq!(text(&s, 4), "6");
        assert_eq!(s.cursor(), Some((1, 4)));
        assert_eq!(s.row(0).len(), 6);
    }
}
