//! テキストバッファ（design.md §9）。
//!
//! - 中身は rope（`ropey`）。[`Buffer::snapshot`] は中身を共有する写しで、コピーしない
//!   （batch への依頼に付ける。design.md §4.2）
//! - 編集は「変わった行範囲」（[`LineChange`]）を返す。ハイライトと描画はそれを見て
//!   無効化する。範囲は正確（変わっていない行は含まない）だが、undo / redo や
//!   複数行の一括編集では、間の行も含めた 1 つの範囲にまとめる
//! - 編集のたびに版（[`Version`]）が進む。ワーカーへの依頼には [`Buffer::token`] を付ける
//! - undo は操作単位でまとめる（連続した文字入力は 1 単位、カーソルが飛ぶと切れる）
//! - 位置は [`Pos`]（行と、行内の文字数）。表示桁への変換は [`Buffer::display_col`] と
//!   [`width`]（全角 = 2、タブはタブ位置まで）。字形の寸法は `zc-ui` の仕事
//!
//! # 内部の表現
//!
//! 中身は **改行を LF に正規化**して持つ。読み込み時に CRLF が多ければ
//! [`LineEnding::CrLf`] と覚え、保存時に CRLF へ戻す（CRLF と LF の混在は、多い方に
//! そろう）。行の区切りは LF だけ（U+2028 や単独の CR では行が分かれない）。
//! 末尾の改行の有無は、最後の行が空かどうかとしてそのまま残る（`"a\n"` は 2 行）。
//! UTF-8 の BOM は取り除いて覚え、保存時に付け直す。

pub mod width;

mod search;

use ropey::{Rope, RopeBuilder, RopeSlice};
use std::borrow::Cow;
use std::io;
use std::path::Path;
use zc_core::{CancelToken, LineChange, LineRange, LineSource, Motion, Pos, Version, VersionCounter};

/// 選択範囲。`anchor` が選び始めた位置、`head` がカーソルのある位置。
/// 選択していないときは `anchor == head`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
}

impl Selection {
    pub const fn caret(pos: Pos) -> Selection {
        Selection { anchor: pos, head: pos }
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
    /// 先頭側の位置。
    pub fn start(&self) -> Pos {
        self.anchor.min(self.head)
    }
    /// 末尾側の位置。
    pub fn end(&self) -> Pos {
        self.anchor.max(self.head)
    }
}

/// 改行の種類（保存するときに元のまま書く）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
}

const BOM: &str = "\u{feff}";
/// undo の履歴の最大数（操作のまとまりの数）。
const UNDO_LIMIT: usize = 1000;
/// 読み込みで取り消しを見る間隔（バイト）。
const LOAD_CHUNK: usize = 32 * 1024;

// ---------------------------------------------------------------------------
// rope の読み口（Buffer と Snapshot で共有）
// ---------------------------------------------------------------------------

/// `line` 行目の文字数（改行を除く）。範囲外は 0。
pub(crate) fn line_len_of(rope: &Rope, line: usize) -> usize {
    if line >= rope.len_lines() {
        return 0;
    }
    let s = rope.line(line);
    let n = s.len_chars();
    if n > 0 && s.char(n - 1) == '\n' {
        n - 1
    } else {
        n
    }
}

fn last_line(rope: &Rope) -> u32 {
    (rope.len_lines() - 1) as u32
}

/// 位置 → 文字の通し番号（範囲外は丸める）。
pub(crate) fn idx_of(rope: &Rope, p: Pos) -> usize {
    let l = (p.line as usize).min(rope.len_lines() - 1);
    let c = (p.col as usize).min(line_len_of(rope, l));
    rope.line_to_char(l) + c
}

/// 文字の通し番号 → 位置。
pub(crate) fn pos_of(rope: &Rope, idx: usize) -> Pos {
    let idx = idx.min(rope.len_chars());
    let l = rope.char_to_line(idx);
    Pos::new(l as u32, (idx - rope.line_to_char(l)) as u32)
}

fn clamp_pos(rope: &Rope, p: Pos) -> Pos {
    pos_of(rope, idx_of(rope, p))
}

fn line_into(rope: &Rope, line: u32, out: &mut String) {
    out.clear();
    let line = line as usize;
    if line >= rope.len_lines() {
        return;
    }
    let n = line_len_of(rope, line);
    let s: RopeSlice = rope.line(line).slice(..n);
    for chunk in s.chunks() {
        out.push_str(chunk);
    }
}

fn text_in_rope(rope: &Rope, sel: Selection) -> String {
    let a = idx_of(rope, sel.start());
    let b = idx_of(rope, sel.end());
    rope.slice(a..b).to_string()
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// バッファの写し。中身は共有していて、作るのは軽い。`Send` なのでワーカーに渡せる。
#[derive(Clone)]
pub struct Snapshot {
    version: Version,
    rope: Rope,
    ending: LineEnding,
    bom: bool,
}

impl Snapshot {
    /// 写しを取った時点の版。
    pub fn version(&self) -> Version {
        self.version
    }
    /// 読み込んだときの改行の種類。
    pub fn line_ending(&self) -> LineEnding {
        self.ending
    }
    /// 全体を書き出す（保存用。storage スレッドで呼ぶ）。内部の LF を `ending` に
    /// 直して書く。読み込み時に BOM があれば先頭に付け直す。
    pub fn write_to(&self, out: &mut dyn io::Write, ending: LineEnding) -> io::Result<()> {
        if self.bom {
            out.write_all(BOM.as_bytes())?;
        }
        for chunk in self.rope.chunks() {
            match ending {
                LineEnding::Lf => out.write_all(chunk.as_bytes())?,
                LineEnding::CrLf => {
                    let mut rest = chunk;
                    while let Some(i) = rest.find('\n') {
                        out.write_all(&rest.as_bytes()[..i])?;
                        out.write_all(b"\r\n")?;
                        rest = &rest[i + 1..];
                    }
                    out.write_all(rest.as_bytes())?;
                }
            }
        }
        Ok(())
    }
    /// 全体の文字数。
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }
    /// `line` 行目の文字数（改行を除く）。
    pub fn line_len(&self, line: u32) -> u32 {
        line_len_of(&self.rope, line as usize) as u32
    }
    /// 範囲の文字列（LF 区切り）。
    pub fn text_in(&self, sel: Selection) -> String {
        text_in_rope(&self.rope, sel)
    }
    /// 検索（大文字小文字を区別する）。[`Buffer::find`] を参照。
    pub fn find(&self, needle: &str, from: Pos, backward: bool) -> Option<Selection> {
        search::find(&self.rope, needle, from, backward, true)
    }
    /// 検索。`case_sensitive` が偽なら大文字小文字を区別しない。
    pub fn find_with(&self, needle: &str, from: Pos, backward: bool, case_sensitive: bool) -> Option<Selection> {
        search::find(&self.rope, needle, from, backward, case_sensitive)
    }
}

impl LineSource for Snapshot {
    fn len_lines(&self) -> u32 {
        self.rope.len_lines() as u32
    }
    fn line(&self, line: u32, out: &mut String) {
        line_into(&self.rope, line, out)
    }
}

// ---------------------------------------------------------------------------
// undo の履歴
// ---------------------------------------------------------------------------

/// 1 回の置き換え（`at` から `rem_len` 文字を `ins` に）。
#[derive(Clone, Debug)]
struct Edit {
    at: usize,
    removed: String,
    rem_len: usize,
    ins: String,
    ins_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// 連続した文字入力。
    Typing,
    /// 連続した BackSpace。
    Backspace,
    /// 連続した Delete。
    Delete,
    /// そのほか（続けない）。
    Other,
}

/// undo の 1 単位。
#[derive(Clone, Debug)]
struct Group {
    id: u64,
    edits: Vec<Edit>,
    before: Selection,
    after: Selection,
    kind: Kind,
    /// 続けて同じ種類の編集を足してよいか。
    open: bool,
}

/// 行ごとの小さな編集（字下げ・コメント用）。
struct LineEdit {
    line: u32,
    col: u32,
    remove: u32,
    insert: String,
}

/// 連続した 2 つの変更を 1 つにまとめる（間の行は含める。広めに取るだけで、
/// 変わっていない行を落とすことはない）。
fn merge_change(acc: Option<LineChange>, c: LineChange) -> LineChange {
    let Some(a) = acc else { return c };
    let start = a.new.start.min(c.old.start);
    // この編集の直前の（まとめた範囲の）終わり
    let cur_end_before = a.new.end.max(c.old.end);
    let old_end = a.old.end + (cur_end_before - a.new.end);
    let new_end = (cur_end_before as i64 + c.delta()) as u32;
    LineChange { old: LineRange::new(start, old_end), new: LineRange::new(start, new_end) }
}

/// 改行コードを LF にそろえる（CRLF・単独の CR → LF）。
fn normalize_newlines(s: &str) -> Cow<'_, str> {
    if !s.contains('\r') {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.replace("\r\n", "\n").replace('\r', "\n"))
}

// ---------------------------------------------------------------------------
// 文字の種類（単語移動）
// ---------------------------------------------------------------------------

fn char_class(c: char) -> u8 {
    match c {
        ' ' | '\t' => 0,
        'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => 1,
        '\u{3041}'..='\u{309F}' => 2,
        '\u{30A0}'..='\u{30FF}' | '\u{FF66}'..='\u{FF9F}' => 3,
        '\u{3400}'..='\u{9FFF}' | '\u{20000}'..='\u{3FFFD}' => 4,
        c if !c.is_ascii() && c.is_alphanumeric() => 1,
        _ => 5,
    }
}

// ---------------------------------------------------------------------------
// Buffer
// ---------------------------------------------------------------------------

/// 1 つの文書のテキスト・カーソル・undo 履歴。メインスレッドが持つ。
pub struct Buffer {
    rope: Rope,
    versions: VersionCounter,
    selection: Selection,
    /// 上下移動で覚えておく表示桁。
    goal: Option<u32>,
    ending: LineEnding,
    bom: bool,
    tab_width: u32,
    undo: Vec<Group>,
    redo: Vec<Group>,
    next_id: u64,
    /// 履歴の底（古い履歴を捨てたあと）の状態の番号。
    base_id: u64,
    /// 保存した時点の状態の番号。
    saved_id: u64,
}

/// 文字列を rope にする。`cancel` が古くなったら `None`。
fn build_rope(text: &str, cancel: Option<&CancelToken>) -> Option<Rope> {
    if text.len() <= LOAD_CHUNK {
        return Some(Rope::from_str(text));
    }
    let mut b = RopeBuilder::new();
    let mut rest = text;
    while !rest.is_empty() {
        if cancel.is_some_and(|c| c.is_stale()) {
            return None;
        }
        let mut n = LOAD_CHUNK.min(rest.len());
        while !rest.is_char_boundary(n) {
            n += 1;
        }
        b.append(&rest[..n]);
        rest = &rest[n..];
    }
    Some(b.finish())
}

impl Buffer {
    /// 空のバッファ。
    pub fn new() -> Buffer {
        Buffer::with_rope(Rope::new(), LineEnding::Lf, false)
    }

    fn with_rope(rope: Rope, ending: LineEnding, bom: bool) -> Buffer {
        Buffer {
            rope,
            versions: VersionCounter::new(),
            selection: Selection::default(),
            goal: None,
            ending,
            bom,
            tab_width: 4,
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            base_id: 0,
            saved_id: 0,
        }
    }

    /// 文字列から作る。改行は種類を見て LF にそろえる。
    pub fn from_text(text: &str) -> Buffer {
        Buffer::from_text_inner(text, false, None).expect("取り消しなしでは失敗しない")
    }

    fn from_text_inner(text: &str, bom: bool, cancel: Option<&CancelToken>) -> Option<Buffer> {
        let crlf = text.as_bytes().windows(2).filter(|w| w == b"\r\n").count();
        let (ending, text) = if crlf > 0 {
            let lf_total = text.bytes().filter(|&b| b == b'\n').count();
            let lone_lf = lf_total - crlf;
            let e = if crlf >= lone_lf { LineEnding::CrLf } else { LineEnding::Lf };
            (e, Cow::Owned(text.replace("\r\n", "\n")))
        } else {
            (LineEnding::Lf, Cow::Borrowed(text))
        };
        let rope = build_rope(&text, cancel)?;
        Some(Buffer::with_rope(rope, ending, bom))
    }

    /// バイト列から作る。UTF-8 でなければ `InvalidData` のエラー（panic しない）。
    /// 先頭の BOM は取り除いて覚える。
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Buffer> {
        Buffer::from_bytes_inner(bytes, None)
    }

    fn from_bytes_inner(bytes: &[u8], cancel: Option<&CancelToken>) -> io::Result<Buffer> {
        let text = std::str::from_utf8(bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("UTF-8 ではない（{} バイト目）", e.valid_up_to())))?;
        let (bom, text) = match text.strip_prefix(BOM) {
            Some(t) => (true, t),
            None => (false, text),
        };
        Buffer::from_text_inner(text, bom, cancel)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Interrupted, "読み込みを取り消した"))
    }

    /// 不正な UTF-8 を U+FFFD に置き換えて作る。2 つ目は置き換えがあったか
    /// （あれば保存すると元のバイト列と変わるので、呼ぶ側で警告する）。
    pub fn from_bytes_lossy(bytes: &[u8]) -> (Buffer, bool) {
        match std::str::from_utf8(bytes) {
            Ok(_) => (Buffer::from_bytes(bytes).expect("UTF-8"), false),
            Err(_) => {
                let s = String::from_utf8_lossy(bytes);
                (Buffer::from_bytes(s.as_bytes()).expect("UTF-8"), true)
            }
        }
    }

    /// ファイルを読んで作る。**batch スレッドで呼ぶ**（読み込み・rope の構築まで）。
    /// `cancel` が古くなったら途中でやめる（`Interrupted`）。UTF-8 でなければ
    /// `InvalidData`。
    pub fn load(path: &Path, cancel: &CancelToken) -> io::Result<Buffer> {
        let bytes = std::fs::read(path)?;
        if cancel.is_stale() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "読み込みを取り消した"));
        }
        Buffer::from_bytes_inner(&bytes, Some(cancel))
    }

    /// バイト列から作る。[`Buffer::from_bytes`] と同じだが、`cancel` が古くなったら途中でやめる
    /// （`Interrupted`）。すでに読んだバイト列を使うとき用。
    pub fn from_bytes_cancellable(bytes: &[u8], cancel: &CancelToken) -> io::Result<Buffer> {
        Buffer::from_bytes_inner(bytes, Some(cancel))
    }

    // --- 版 ---

    /// 現在の版。
    pub fn version(&self) -> Version {
        self.versions.current()
    }
    /// ワーカーへの依頼に付ける取り消しトークン。
    pub fn token(&self) -> CancelToken {
        self.versions.token()
    }
    /// 版のカウンタ（結果が古いかの判定に使う）。
    pub fn versions(&self) -> &VersionCounter {
        &self.versions
    }
    /// 中身を共有する写し。
    pub fn snapshot(&self) -> Snapshot {
        Snapshot { version: self.version(), rope: self.rope.clone(), ending: self.ending, bom: self.bom }
    }
    /// 全体を書き出す（[`Snapshot::write_to`] と同じ。自分の改行の種類で書く）。
    pub fn write_to(&self, out: &mut dyn io::Write) -> io::Result<()> {
        self.snapshot().write_to(out, self.ending)
    }

    // --- 読む ---

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }
    /// `line` 行目の文字数（改行を除く）。範囲外は 0。
    pub fn line_len(&self, line: u32) -> u32 {
        line_len_of(&self.rope, line as usize) as u32
    }
    /// 全体を文字列にする（LF 区切り）。
    pub fn to_text(&self) -> String {
        self.rope.to_string()
    }
    /// 範囲の文字列（コピー・切り取り用。LF 区切り）。
    pub fn text_in(&self, sel: Selection) -> String {
        text_in_rope(&self.rope, sel)
    }
    pub fn line_ending(&self) -> LineEnding {
        self.ending
    }
    /// 保存する改行の種類を変える。
    pub fn set_line_ending(&mut self, e: LineEnding) {
        self.ending = e;
    }
    /// 保存した時点から変わっているか（タブの ● 表示）。undo で保存時点に戻れば偽に戻る。
    pub fn is_modified(&self) -> bool {
        self.state_id() != self.saved_id
    }
    /// いまの内容を「保存済み」とする。
    pub fn mark_saved(&mut self) {
        self.break_undo_group();
        self.saved_id = self.state_id();
    }
    fn state_id(&self) -> u64 {
        self.undo.last().map_or(self.base_id, |g| g.id)
    }

    // --- 位置と表示桁 ---

    /// タブ幅（既定 4）。表示桁の計算と、上下移動の桁合わせに使う。
    pub fn tab_width(&self) -> u32 {
        self.tab_width
    }
    pub fn set_tab_width(&mut self, w: u32) {
        self.tab_width = w.clamp(1, 16);
    }
    /// 位置を範囲内に丸める。
    pub fn clamp(&self, p: Pos) -> Pos {
        clamp_pos(&self.rope, p)
    }
    /// `line` 行目の、文字位置 `col` の表示桁（全角 = 2、タブはタブ位置まで）。
    pub fn display_col(&self, line: u32, col: u32) -> u32 {
        let l = line as usize;
        if l >= self.rope.len_lines() {
            return 0;
        }
        width::display_col_of(self.rope.line(l).chars().take(self.line_len(line) as usize), col, self.tab_width)
    }
    /// `line` 行目の、表示桁 `dcol` に対応する文字位置（全角やタブの途中なら手前）。
    pub fn col_at_display(&self, line: u32, dcol: u32) -> u32 {
        let l = line as usize;
        if l >= self.rope.len_lines() {
            return 0;
        }
        width::char_col_at_display(self.rope.line(l).chars().take(self.line_len(line) as usize), dcol, self.tab_width)
    }
    /// 位置 → 文字の通し番号。
    pub fn char_index(&self, p: Pos) -> usize {
        idx_of(&self.rope, p)
    }
    /// 文字の通し番号 → 位置。
    pub fn pos_at(&self, idx: usize) -> Pos {
        pos_of(&self.rope, idx)
    }

    // --- カーソルと選択 ---

    pub fn cursor(&self) -> Pos {
        self.selection.head
    }
    pub fn selection(&self) -> Selection {
        self.selection
    }
    /// 選択範囲を置く（範囲外の位置は丸める）。undo のまとまりは切れる。
    pub fn set_selection(&mut self, sel: Selection) {
        self.selection = Selection { anchor: self.clamp(sel.anchor), head: self.clamp(sel.head) };
        self.goal = None;
        self.break_undo_group();
    }
    pub fn select_all(&mut self) {
        let end = pos_of(&self.rope, self.rope.len_chars());
        self.set_selection(Selection { anchor: Pos::default(), head: end });
    }

    fn line_chars(&self, line: u32) -> Vec<char> {
        let l = line as usize;
        if l >= self.rope.len_lines() {
            return Vec::new();
        }
        self.rope.line(l).chars().take(self.line_len(line) as usize).collect()
    }

    /// カーソルを動かす。`select` なら選択を伸ばす。`page_lines` は 1 画面の行数
    /// （PgUp / PgDn 用）。上下移動では、元の表示桁を覚えておく。
    /// 選択があって `select` でない `Left` / `Right` は、選択の端に寄せるだけ。
    pub fn move_cursor(&mut self, motion: Motion, select: bool, page_lines: u32) {
        let sel = self.selection;
        let head = sel.head;
        if !select && !sel.is_empty() && matches!(motion, Motion::Left | Motion::Right) {
            let p = if motion == Motion::Left { sel.start() } else { sel.end() };
            self.selection = Selection::caret(p);
            self.goal = None;
            self.break_undo_group();
            return;
        }
        let last = last_line(&self.rope);
        let mut vertical = false;
        let new_head = match motion {
            Motion::Left => {
                if head.col > 0 {
                    Pos::new(head.line, head.col - 1)
                } else if head.line > 0 {
                    Pos::new(head.line - 1, self.line_len(head.line - 1))
                } else {
                    head
                }
            }
            Motion::Right => {
                if head.col < self.line_len(head.line) {
                    Pos::new(head.line, head.col + 1)
                } else if head.line < last {
                    Pos::new(head.line + 1, 0)
                } else {
                    head
                }
            }
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
                vertical = true;
                let step = match motion {
                    Motion::Up | Motion::Down => 1,
                    _ => page_lines.max(1),
                };
                let up = matches!(motion, Motion::Up | Motion::PageUp);
                if up && head.line == 0 {
                    Pos::new(0, 0)
                } else if !up && head.line == last {
                    Pos::new(last, self.line_len(last))
                } else {
                    let goal = self.goal.unwrap_or_else(|| self.display_col(head.line, head.col));
                    self.goal = Some(goal);
                    let line = if up { head.line.saturating_sub(step) } else { (head.line + step).min(last) };
                    Pos::new(line, self.col_at_display(line, goal))
                }
            }
            Motion::WordLeft => self.word_left(head),
            Motion::WordRight => self.word_right(head),
            Motion::LineStart => {
                let chars = self.line_chars(head.line);
                let fw = chars.iter().take_while(|c| **c == ' ' || **c == '\t').count() as u32;
                Pos::new(head.line, if head.col == fw { 0 } else { fw })
            }
            Motion::LineEnd => Pos::new(head.line, self.line_len(head.line)),
            Motion::DocStart => Pos::new(0, 0),
            Motion::DocEnd => Pos::new(last, self.line_len(last)),
        };
        if !vertical {
            self.goal = None;
        }
        self.selection = Selection { anchor: if select { sel.anchor } else { new_head }, head: new_head };
        self.break_undo_group();
    }

    fn word_right(&self, p: Pos) -> Pos {
        let ch = self.line_chars(p.line);
        let len = ch.len() as u32;
        if p.col >= len {
            return if p.line < last_line(&self.rope) { Pos::new(p.line + 1, 0) } else { p };
        }
        let mut i = p.col as usize;
        while i < ch.len() && char_class(ch[i]) == 0 {
            i += 1;
        }
        if i < ch.len() {
            let cls = char_class(ch[i]);
            while i < ch.len() && char_class(ch[i]) == cls {
                i += 1;
            }
        }
        Pos::new(p.line, i as u32)
    }

    fn word_left(&self, p: Pos) -> Pos {
        if p.col == 0 {
            return if p.line > 0 { Pos::new(p.line - 1, self.line_len(p.line - 1)) } else { p };
        }
        let ch = self.line_chars(p.line);
        let mut i = (p.col as usize).min(ch.len());
        while i > 0 && char_class(ch[i - 1]) == 0 {
            i -= 1;
        }
        if i > 0 {
            let cls = char_class(ch[i - 1]);
            while i > 0 && char_class(ch[i - 1]) == cls {
                i -= 1;
            }
        }
        Pos::new(p.line, i as u32)
    }

    // --- 編集の土台 ---

    /// rope を書き換える（履歴には載せない）。置き換えた文字列と、変わった行範囲を返す。
    fn raw(&mut self, at: usize, rem: usize, text: &str) -> (String, LineChange) {
        let end = at + rem;
        let l0 = self.rope.char_to_line(at);
        let l1 = self.rope.char_to_line(end);
        let removed = if rem > 0 { self.rope.slice(at..end).to_string() } else { String::new() };
        let rem_nl = removed.matches('\n').count();
        let ins_nl = text.matches('\n').count();
        if rem > 0 {
            self.rope.remove(at..end);
        }
        if !text.is_empty() {
            self.rope.insert(at, text);
        }
        let new_len = (l1 - l0 + 1) + ins_nl - rem_nl;
        let ch = LineChange {
            old: LineRange::new(l0 as u32, l1 as u32 + 1),
            new: LineRange::new(l0 as u32, (l0 + new_len) as u32),
        };
        (removed, ch)
    }

    /// 操作の始まり。続けてよい種類なら直前のまとまりに足し、そうでなければ新しく作る。
    fn begin_op(&mut self, kind: Kind) {
        let cont = kind != Kind::Other
            && self.selection.is_empty()
            && self.undo.last().is_some_and(|g| g.open && g.kind == kind && g.after == self.selection);
        if !cont {
            let id = self.next_id;
            self.next_id += 1;
            self.undo.push(Group { id, edits: Vec::new(), before: self.selection, after: self.selection, kind, open: true });
            if self.undo.len() > UNDO_LIMIT {
                let g = self.undo.remove(0);
                self.base_id = g.id;
            }
        }
    }

    /// 履歴に載せて rope を書き換える。
    fn edit(&mut self, acc: &mut Option<LineChange>, at: usize, rem: usize, text: &str) {
        let (removed, ch) = self.raw(at, rem, text);
        *acc = Some(merge_change(*acc, ch));
        self.redo.clear();
        let ins_len = text.chars().count();
        let g = self.undo.last_mut().expect("begin_op の後");
        if let Some(e) = g.edits.last_mut() {
            if rem == 0 && e.rem_len == 0 && e.at + e.ins_len == at {
                e.ins.push_str(text);
                e.ins_len += ins_len;
                return;
            }
            if ins_len == 0 && e.ins_len == 0 && rem > 0 {
                if at + rem == e.at {
                    e.removed.insert_str(0, &removed);
                    e.rem_len += rem;
                    e.at = at;
                    return;
                }
                if at == e.at {
                    e.removed.push_str(&removed);
                    e.rem_len += rem;
                    return;
                }
            }
        }
        g.edits.push(Edit { at, removed, rem_len: rem, ins: text.to_string(), ins_len });
    }

    /// 操作の終わり。変わらなかったときは空のまとまりを捨てて `None`。
    fn end_op(&mut self, acc: Option<LineChange>) -> Option<LineChange> {
        let empty = self.undo.last().is_some_and(|g| g.edits.is_empty());
        if empty {
            self.undo.pop();
        }
        let ch = acc?;
        if let Some(g) = self.undo.last_mut() {
            g.after = self.selection;
        }
        self.goal = None;
        self.versions.bump();
        Some(ch)
    }

    /// 連続入力のまとまりをここで切る（カーソル移動・保存・一定時間の無操作のあと）。
    pub fn break_undo_group(&mut self) {
        if let Some(g) = self.undo.last_mut() {
            g.open = false;
        }
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    // --- 編集（どれも、変わった行範囲を返す。何も変わらなければ `None`） ---

    /// 選択範囲を `text` で置き換える（選択が無ければカーソル位置に入れる）。
    /// 改行は LF にそろえる。自動字下げはしない（貼り付けにも使うので。入力の Enter は
    /// [`Buffer::newline`]）。選択が無く改行を含まない入力は、続けると 1 つの undo 単位。
    pub fn insert(&mut self, text: &str) -> Option<LineChange> {
        let text = normalize_newlines(text);
        if text.is_empty() && self.selection.is_empty() {
            return None;
        }
        let kind = if self.selection.is_empty() && !text.contains('\n') { Kind::Typing } else { Kind::Other };
        self.begin_op(kind);
        let a = idx_of(&self.rope, self.selection.start());
        let b = idx_of(&self.rope, self.selection.end());
        let mut acc = None;
        self.edit(&mut acc, a, b - a, &text);
        self.selection = Selection::caret(pos_of(&self.rope, a + text.chars().count()));
        self.end_op(acc)
    }

    /// `start..end` を `text` に置き換え、カーソルを入れた文字列の末尾に置く（補完の確定）。
    /// 取り消しの 1 単位になり、取り消すと置き換える前の選択（ふつうはカーソルだけ）に戻る。
    pub fn replace_range(&mut self, start: Pos, end: Pos, text: &str) -> Option<LineChange> {
        let text = normalize_newlines(text);
        let (start, end) = (self.clamp(start.min(end)), self.clamp(start.max(end)));
        if text.is_empty() && start == end {
            return None;
        }
        self.begin_op(Kind::Other);
        let a = idx_of(&self.rope, start);
        let b = idx_of(&self.rope, end);
        let mut acc = None;
        self.edit(&mut acc, a, b - a, &text);
        self.selection = Selection::caret(pos_of(&self.rope, a + text.chars().count()));
        self.end_op(acc)
    }

    /// Enter。直前の行の字下げをそのまま写す。カーソルの手前が `indent_after` に含まれる
    /// 文字（例 `":{(["`）で終わっていれば、`indent_unit` を 1 段足す。
    pub fn newline(&mut self, indent_unit: &str, indent_after: &str) -> Option<LineChange> {
        let start = self.selection.start();
        let before: Vec<char> = self.line_chars(start.line).into_iter().take(start.col as usize).collect();
        let mut text = String::from("\n");
        text.extend(before.iter().take_while(|c| **c == ' ' || **c == '\t'));
        let last_non_ws = before.iter().rev().find(|c| **c != ' ' && **c != '\t');
        if last_non_ws.is_some_and(|c| indent_after.contains(*c)) {
            text.push_str(indent_unit);
        }
        self.begin_op(Kind::Other);
        let a = idx_of(&self.rope, start);
        let b = idx_of(&self.rope, self.selection.end());
        let mut acc = None;
        self.edit(&mut acc, a, b - a, &text);
        self.selection = Selection::caret(pos_of(&self.rope, a + text.chars().count()));
        self.end_op(acc)
    }

    fn delete_selection(&mut self) -> Option<LineChange> {
        let a = idx_of(&self.rope, self.selection.start());
        let b = idx_of(&self.rope, self.selection.end());
        self.begin_op(Kind::Other);
        let mut acc = None;
        self.edit(&mut acc, a, b - a, "");
        self.selection = Selection::caret(pos_of(&self.rope, a));
        self.end_op(acc)
    }

    /// BackSpace。
    pub fn delete_backward(&mut self) -> Option<LineChange> {
        if !self.selection.is_empty() {
            return self.delete_selection();
        }
        let i = idx_of(&self.rope, self.selection.head);
        if i == 0 {
            return None;
        }
        self.begin_op(Kind::Backspace);
        let mut acc = None;
        self.edit(&mut acc, i - 1, 1, "");
        self.selection = Selection::caret(pos_of(&self.rope, i - 1));
        self.end_op(acc)
    }

    /// Delete。
    pub fn delete_forward(&mut self) -> Option<LineChange> {
        if !self.selection.is_empty() {
            return self.delete_selection();
        }
        let i = idx_of(&self.rope, self.selection.head);
        if i >= self.rope.len_chars() {
            return None;
        }
        self.begin_op(Kind::Delete);
        let mut acc = None;
        self.edit(&mut acc, i, 1, "");
        self.selection = Selection::caret(pos_of(&self.rope, i));
        self.end_op(acc)
    }

    /// 選択にかかっている行（`a..=b`）。選択の終わりが行頭ちょうどなら、その行は含めない。
    fn selected_lines(&self) -> (u32, u32) {
        let s = self.selection.start();
        let e = self.selection.end();
        let b = if !self.selection.is_empty() && e.col == 0 && e.line > s.line { e.line - 1 } else { e.line };
        (s.line, b)
    }

    /// カーソルのある行（選択があれば、かかっている行すべて）を消す。
    pub fn delete_lines(&mut self) -> Option<LineChange> {
        let (a, b) = self.selected_lines();
        let n = self.rope.len_lines() as u32;
        let head_col = self.selection.head.col;
        let (from, to) = if b + 1 < n {
            (self.rope.line_to_char(a as usize), self.rope.line_to_char(b as usize + 1))
        } else if a > 0 {
            (self.rope.line_to_char(a as usize) - 1, self.rope.len_chars())
        } else {
            (0, self.rope.len_chars())
        };
        if from == to {
            return None;
        }
        self.begin_op(Kind::Other);
        let mut acc = None;
        self.edit(&mut acc, from, to - from, "");
        let line = a.min(last_line(&self.rope));
        let col = head_col.min(self.line_len(line));
        self.selection = Selection::caret(Pos::new(line, col));
        self.end_op(acc)
    }

    /// 行ごとの編集をまとめて 1 つの undo 単位として行い、選択を桁ずれに合わせて直す。
    fn apply_line_edits(&mut self, edits: Vec<LineEdit>) -> Option<LineChange> {
        if edits.is_empty() {
            return None;
        }
        self.begin_op(Kind::Other);
        let mut acc = None;
        for e in &edits {
            let at = self.rope.line_to_char(e.line as usize) + e.col as usize;
            self.edit(&mut acc, at, e.remove as usize, &e.insert);
        }
        let shift = |mut p: Pos| {
            if let Some(e) = edits.iter().find(|e| e.line == p.line) {
                let ins = e.insert.chars().count() as u32;
                if e.remove == 0 {
                    if p.col >= e.col {
                        p.col += ins;
                    }
                } else if p.col >= e.col + e.remove {
                    p.col = p.col - e.remove + ins;
                } else if p.col > e.col {
                    p.col = e.col;
                }
            }
            p
        };
        self.selection = Selection { anchor: shift(self.selection.anchor), head: shift(self.selection.head) };
        self.end_op(acc)
    }

    /// 字下げする / 戻す（`indent` は言語の設定の字下げ文字列）。
    ///
    /// - 字下げで選択が無い: カーソル位置に `indent` を入れる
    /// - それ以外: 選択のかかる行（1 行でも）の行頭を字下げする / 戻す。字下げでは
    ///   空行は飛ばす。戻すときは `indent` 1 つぶん、タブ 1 つ、または空白（`indent` の
    ///   文字数、`indent` がタブなら `tab_width` まで）を取る
    pub fn indent_lines(&mut self, indent: &str, outdent: bool) -> Option<LineChange> {
        if !outdent && self.selection.is_empty() {
            // Tab は独立した操作: 直前の文字入力と同じ undo のまとまりに入れない
            // （入れると、undo で直前に打った文字まで消える）。後ろも切る
            self.break_undo_group();
            let ch = self.insert(indent);
            self.break_undo_group();
            return ch;
        }
        let (a, b) = self.selected_lines();
        let mut edits = Vec::new();
        for line in a..=b {
            if outdent {
                let ch = self.line_chars(line);
                let ind: Vec<char> = indent.chars().collect();
                let remove = if !ind.is_empty() && ch.starts_with(&ind) {
                    ind.len()
                } else if ch.first() == Some(&'\t') {
                    1
                } else {
                    let max = if indent.contains('\t') { self.tab_width as usize } else { ind.len().max(1) };
                    ch.iter().take(max).take_while(|c| **c == ' ').count()
                };
                if remove > 0 {
                    edits.push(LineEdit { line, col: 0, remove: remove as u32, insert: String::new() });
                }
            } else if self.line_len(line) > 0 {
                edits.push(LineEdit { line, col: 0, remove: 0, insert: indent.to_string() });
            }
        }
        self.apply_line_edits(edits)
    }

    /// 選択行の行コメントを切り替える（`marker` は言語の設定の `line_comment`）。
    /// 空でない行がすべてコメント済みなら外し（`marker` の後ろの空白 1 つも）、そうでなければ
    /// 空でない行の最小の字下げの桁に `marker` と空白を入れる。
    pub fn toggle_line_comment(&mut self, marker: &str) -> Option<LineChange> {
        if marker.is_empty() {
            return None;
        }
        let (a, b) = self.selected_lines();
        let m: Vec<char> = marker.chars().collect();
        // (行, 字下げの文字数, 空行か, コメント済みか)
        let mut info = Vec::new();
        for line in a..=b {
            let ch = self.line_chars(line);
            let ws = ch.iter().take_while(|c| **c == ' ' || **c == '\t').count();
            let blank = ws == ch.len();
            let commented = !blank && ch[ws..].starts_with(&m);
            let after = ch.get(ws + m.len()).copied();
            info.push((line, ws, blank, commented, after));
        }
        let any_nonblank = info.iter().any(|i| !i.2);
        let all_commented = any_nonblank && info.iter().all(|i| i.2 || i.3);
        let mut edits = Vec::new();
        if all_commented {
            for &(line, ws, blank, _, after) in &info {
                if !blank {
                    let remove = m.len() + usize::from(after == Some(' '));
                    edits.push(LineEdit { line, col: ws as u32, remove: remove as u32, insert: String::new() });
                }
            }
        } else {
            let col = if any_nonblank { info.iter().filter(|i| !i.2).map(|i| i.1).min().unwrap_or(0) } else { 0 };
            for &(line, _, blank, _, _) in &info {
                if !blank || !any_nonblank {
                    edits.push(LineEdit { line, col: col as u32, remove: 0, insert: format!("{marker} ") });
                }
            }
        }
        self.apply_line_edits(edits)
    }

    fn restore(&mut self, g: Group, forward: bool) -> Option<LineChange> {
        let mut acc = None;
        if forward {
            for e in &g.edits {
                let (_, ch) = self.raw(e.at, e.rem_len, &e.ins);
                acc = Some(merge_change(acc, ch));
            }
            self.selection = g.after;
        } else {
            for e in g.edits.iter().rev() {
                let (_, ch) = self.raw(e.at, e.ins_len, &e.removed);
                acc = Some(merge_change(acc, ch));
            }
            self.selection = g.before;
        }
        self.selection = Selection { anchor: self.clamp(self.selection.anchor), head: self.clamp(self.selection.head) };
        self.goal = None;
        self.versions.bump();
        if forward {
            self.undo.push(Group { open: false, ..g });
        } else {
            self.redo.push(Group { open: false, ..g });
        }
        acc
    }

    pub fn undo(&mut self) -> Option<LineChange> {
        let g = self.undo.pop()?;
        let ch = self.restore(g, false);
        self.break_undo_group();
        ch
    }

    pub fn redo(&mut self) -> Option<LineChange> {
        let g = self.redo.pop()?;
        self.restore(g, true)
    }

    // --- 検索 ---

    /// `from` より後ろ（`backward` なら前）で `needle` が最初に現れる範囲。
    /// 前方は開始が `from` 以降、後方は終了が `from` 以前。見つからなければ先頭（末尾）に
    /// 戻って探す。大文字小文字を区別する。次を探すときは `from` に直前の選択の
    /// 終わり（後方は始まり）を渡す。
    pub fn find(&self, needle: &str, from: Pos, backward: bool) -> Option<Selection> {
        search::find(&self.rope, needle, from, backward, true)
    }
    /// 検索。`case_sensitive` が偽なら大文字小文字を区別しない。
    pub fn find_with(&self, needle: &str, from: Pos, backward: bool, case_sensitive: bool) -> Option<Selection> {
        search::find(&self.rope, needle, from, backward, case_sensitive)
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer::new()
    }
}

impl LineSource for Buffer {
    fn len_lines(&self) -> u32 {
        self.rope.len_lines() as u32
    }
    fn line(&self, line: u32, out: &mut String) {
        line_into(&self.rope, line, out)
    }
}

#[cfg(test)]
mod tests;
