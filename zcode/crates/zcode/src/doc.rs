//! エディタ画面から見た「文書」。
//!
//! エディタ画面（[`crate::editor::EditorScreen`]）は [`TextDoc`] 越しにテキストを
//! 読み書きする。本番の実装は `zc_buffer::Buffer`（このファイルの下の方で、公開 API を
//! そのまま呼ぶだけの実装を付けている）。[`SimpleDoc`] は行の配列で持つ小さな実装で、
//! テスト・render-test・計測と、`zc-buffer` が入るまでの動作確認（`--simple-buffer`）に
//! 使う。大きなファイルや細かい undo には向かない。

use std::io;
use std::path::Path;
use zc_core::{CancelToken, LineChange, LineRange, LineSource, Motion, Pos, Version, VersionCounter};

/// 保存する中身を書き出す関数（storage スレッドで呼ぶ）。
pub type SaveWriter = Box<dyn FnOnce(&mut dyn io::Write) -> io::Result<()> + Send>;

/// エディタ画面が文書に求める操作。`zc_buffer::Buffer` の公開 API と同じ形。
pub trait TextDoc: LineSource {
    /// 行の読み口として借りる。
    fn as_lines(&self) -> &dyn LineSource;
    /// 現在の版。
    fn version(&self) -> Version;
    /// ワーカーへの依頼に付ける取り消しトークン。
    fn token(&self) -> CancelToken;
    /// 中身の写し（batch スレッドでハイライトの残りを計算するのに渡す）。
    fn snapshot_lines(&self) -> Box<dyn LineSource + Send>;
    /// いまの中身を書き出す関数（保存の依頼に付ける）。
    fn save_writer(&self) -> SaveWriter;
    fn is_modified(&self) -> bool;
    fn mark_saved(&mut self);
    /// `line` 行目の文字数。
    fn line_len(&self, line: u32) -> u32;
    /// 選択範囲 `(anchor, head)`。選択していなければ同じ位置。
    fn selection(&self) -> (Pos, Pos);
    fn set_selection(&mut self, anchor: Pos, head: Pos);
    fn move_cursor(&mut self, motion: Motion, select: bool, page_lines: u32);
    fn select_all(&mut self);
    /// 選択範囲の文字列。
    fn selected_text(&self) -> String;
    fn insert(&mut self, text: &str) -> Option<LineChange>;
    fn delete_backward(&mut self) -> Option<LineChange>;
    fn delete_forward(&mut self) -> Option<LineChange>;
    fn delete_lines(&mut self) -> Option<LineChange>;
    fn indent_lines(&mut self, indent: &str, outdent: bool) -> Option<LineChange>;
    fn toggle_line_comment(&mut self, marker: &str) -> Option<LineChange>;
    fn break_undo_group(&mut self);
    fn undo(&mut self) -> Option<LineChange>;
    fn redo(&mut self) -> Option<LineChange>;
    /// `from` より後ろ（`backward` なら前）で `needle` が最初に現れる範囲。
    fn find(&self, needle: &str, from: Pos, backward: bool) -> Option<(Pos, Pos)>;
}

// --- 本番: zc-buffer ---

impl TextDoc for zc_buffer::Buffer {
    fn as_lines(&self) -> &dyn LineSource {
        self
    }
    fn version(&self) -> Version {
        zc_buffer::Buffer::version(self)
    }
    fn token(&self) -> CancelToken {
        zc_buffer::Buffer::token(self)
    }
    fn snapshot_lines(&self) -> Box<dyn LineSource + Send> {
        Box::new(self.snapshot())
    }
    fn save_writer(&self) -> SaveWriter {
        let (snap, ending) = (self.snapshot(), self.line_ending());
        Box::new(move |out| snap.write_to(out, ending))
    }
    fn is_modified(&self) -> bool {
        zc_buffer::Buffer::is_modified(self)
    }
    fn mark_saved(&mut self) {
        zc_buffer::Buffer::mark_saved(self)
    }
    fn line_len(&self, line: u32) -> u32 {
        zc_buffer::Buffer::line_len(self, line)
    }
    fn selection(&self) -> (Pos, Pos) {
        let s = zc_buffer::Buffer::selection(self);
        (s.anchor, s.head)
    }
    fn set_selection(&mut self, anchor: Pos, head: Pos) {
        zc_buffer::Buffer::set_selection(self, zc_buffer::Selection { anchor, head })
    }
    fn move_cursor(&mut self, motion: Motion, select: bool, page_lines: u32) {
        zc_buffer::Buffer::move_cursor(self, motion, select, page_lines)
    }
    fn select_all(&mut self) {
        zc_buffer::Buffer::select_all(self)
    }
    fn selected_text(&self) -> String {
        self.text_in(zc_buffer::Buffer::selection(self))
    }
    fn insert(&mut self, text: &str) -> Option<LineChange> {
        zc_buffer::Buffer::insert(self, text)
    }
    fn delete_backward(&mut self) -> Option<LineChange> {
        zc_buffer::Buffer::delete_backward(self)
    }
    fn delete_forward(&mut self) -> Option<LineChange> {
        zc_buffer::Buffer::delete_forward(self)
    }
    fn delete_lines(&mut self) -> Option<LineChange> {
        zc_buffer::Buffer::delete_lines(self)
    }
    fn indent_lines(&mut self, indent: &str, outdent: bool) -> Option<LineChange> {
        zc_buffer::Buffer::indent_lines(self, indent, outdent)
    }
    fn toggle_line_comment(&mut self, marker: &str) -> Option<LineChange> {
        zc_buffer::Buffer::toggle_line_comment(self, marker)
    }
    fn break_undo_group(&mut self) {
        zc_buffer::Buffer::break_undo_group(self)
    }
    fn undo(&mut self) -> Option<LineChange> {
        zc_buffer::Buffer::undo(self)
    }
    fn redo(&mut self) -> Option<LineChange> {
        zc_buffer::Buffer::redo(self)
    }
    fn find(&self, needle: &str, from: Pos, backward: bool) -> Option<(Pos, Pos)> {
        zc_buffer::Buffer::find(self, needle, from, backward).map(|s| (s.start(), s.end()))
    }
}

/// 文書の実装の選び方。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DocBackend {
    /// `zc_buffer::Buffer`（本番）。
    #[default]
    Buffer,
    /// [`SimpleDoc`]（テスト・計測・`zc-buffer` が入るまでの動作確認）。
    Simple,
}

impl DocBackend {
    /// 空の文書。
    pub fn empty(self) -> Box<dyn TextDoc + Send> {
        match self {
            DocBackend::Buffer => Box::new(zc_buffer::Buffer::new()),
            DocBackend::Simple => Box::new(SimpleDoc::from_text("")),
        }
    }
    /// ファイルを読んで作る。**batch スレッドで呼ぶ**。
    pub fn load(self, path: &Path, cancel: &CancelToken) -> io::Result<Box<dyn TextDoc + Send>> {
        match self {
            DocBackend::Buffer => Ok(Box::new(zc_buffer::Buffer::load(path, cancel)?)),
            DocBackend::Simple => {
                let text = String::from_utf8(std::fs::read(path)?).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "UTF-8 でない"))?;
                Ok(Box::new(SimpleDoc::from_text(&text)))
            }
        }
    }
}

// --- 小さな実装 ---

/// 行の配列で持つ文書。undo は編集ごとに全体の写しを積む（上限つき）。
pub struct SimpleDoc {
    lines: Vec<String>,
    anchor: Pos,
    head: Pos,
    /// 上下移動で保つ桁。
    goal: Option<u32>,
    versions: VersionCounter,
    /// いまの中身の番号（編集のたびに新しい番号。undo で前の番号に戻る）。
    content: u64,
    next_content: u64,
    /// 保存した時点の中身の番号。
    saved: Option<u64>,
    crlf: bool,
    final_newline: bool,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// 直前の編集が 1 文字の入力で、まとまりがまだ切れていない。
    typing: bool,
}

struct Snap {
    lines: Vec<String>,
    anchor: Pos,
    head: Pos,
    content: u64,
}

const UNDO_LIMIT: usize = 200;

fn byte_at(s: &str, col: u32) -> usize {
    s.char_indices().nth(col as usize).map_or(s.len(), |(i, _)| i)
}

fn chars(s: &str) -> u32 {
    s.chars().count() as u32
}

/// 単語の区切りを決める文字の種類。
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

impl SimpleDoc {
    pub fn from_text(text: &str) -> SimpleDoc {
        let crlf = text.contains("\r\n");
        let final_newline = text.ends_with('\n');
        let mut lines: Vec<String> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
        if final_newline && lines.len() > 1 {
            lines.pop();
        }
        SimpleDoc {
            lines,
            anchor: Pos::default(),
            head: Pos::default(),
            goal: None,
            versions: VersionCounter::new(),
            content: 0,
            next_content: 0,
            saved: Some(0),
            crlf,
            final_newline,
            undo: vec![],
            redo: vec![],
            typing: false,
        }
    }
    /// 「未保存」の印を付ける / 外す（render-test 用）。
    pub fn with_modified(mut self, modified: bool) -> SimpleDoc {
        self.saved = if modified { None } else { Some(self.content) };
        self
    }
    /// 全体の文字列（行は `\n` でつなぐ）。
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
    fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() as u32 - 1);
        Pos::new(line, p.col.min(chars(&self.lines[line as usize])))
    }
    fn range(&self) -> (Pos, Pos) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
    fn snap(&self) -> Snap {
        Snap { lines: self.lines.clone(), anchor: self.anchor, head: self.head, content: self.content }
    }
    /// 編集の前に呼ぶ: undo に積み、版を進める。
    fn begin_edit(&mut self, typing: bool) {
        if !(typing && self.typing) {
            self.undo.push(self.snap());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.typing = typing;
        self.redo.clear();
        self.versions.bump();
        self.next_content += 1;
        self.content = self.next_content;
        self.goal = None;
    }
    /// `start..end` を `text` に置き換え、カーソルを入れた文字列の末尾に置く。
    fn replace(&mut self, start: Pos, end: Pos, text: &str) -> LineChange {
        let (sl, el) = (start.line as usize, end.line as usize);
        let prefix = self.lines[sl][..byte_at(&self.lines[sl], start.col)].to_string();
        let suffix = self.lines[el][byte_at(&self.lines[el], end.col)..].to_string();
        let mut new: Vec<String> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
        let last = new.len() - 1;
        let cur = Pos::new(start.line + last as u32, if last == 0 { start.col } else { 0 } + chars(&new[last]));
        new[0].insert_str(0, &prefix);
        new[last].push_str(&suffix);
        let n = new.len() as u32;
        self.lines.splice(sl..=el, new);
        self.anchor = cur;
        self.head = cur;
        LineChange { old: LineRange::new(start.line, end.line + 1), new: LineRange::new(start.line, start.line + n) }
    }
    fn restore(&mut self, s: Snap) -> LineChange {
        let old = LineRange::new(0, self.lines.len() as u32);
        self.lines = s.lines;
        self.anchor = s.anchor;
        self.head = s.head;
        self.versions.bump();
        self.content = s.content;
        self.typing = false;
        self.goal = None;
        LineChange { old, new: LineRange::new(0, self.lines.len() as u32) }
    }
    /// 選択がかかっている行の範囲（末尾が行頭で終わる複数行の選択は、その行を含めない）。
    fn sel_lines(&self) -> (usize, usize) {
        let (s, e) = self.range();
        let end = if e.line > s.line && e.col == 0 { e.line - 1 } else { e.line };
        (s.line as usize, end as usize)
    }
    fn word_left(&self, p: Pos) -> Pos {
        if p.col == 0 {
            return if p.line == 0 { p } else { Pos::new(p.line - 1, chars(&self.lines[p.line as usize - 1])) };
        }
        let cs: Vec<char> = self.lines[p.line as usize].chars().collect();
        let mut i = p.col as usize;
        while i > 0 && class(cs[i - 1]) == 0 {
            i -= 1;
        }
        if i > 0 {
            let k = class(cs[i - 1]);
            while i > 0 && class(cs[i - 1]) == k {
                i -= 1;
            }
        }
        Pos::new(p.line, i as u32)
    }
    fn word_right(&self, p: Pos) -> Pos {
        let cs: Vec<char> = self.lines[p.line as usize].chars().collect();
        let mut i = p.col as usize;
        if i >= cs.len() {
            return if p.line as usize + 1 >= self.lines.len() { p } else { Pos::new(p.line + 1, 0) };
        }
        let k = class(cs[i]);
        while i < cs.len() && class(cs[i]) == k {
            i += 1;
        }
        while i < cs.len() && class(cs[i]) == 0 {
            i += 1;
        }
        Pos::new(p.line, i as u32)
    }
}

impl LineSource for SimpleDoc {
    fn len_lines(&self) -> u32 {
        self.lines.len() as u32
    }
    fn line(&self, line: u32, out: &mut String) {
        out.clear();
        if let Some(s) = self.lines.get(line as usize) {
            out.push_str(s);
        }
    }
}

/// [`SimpleDoc`] の写し（行の配列を丸ごと持つ）。
struct SimpleSnapshot(Vec<String>);

impl LineSource for SimpleSnapshot {
    fn len_lines(&self) -> u32 {
        self.0.len() as u32
    }
    fn line(&self, line: u32, out: &mut String) {
        out.clear();
        if let Some(s) = self.0.get(line as usize) {
            out.push_str(s);
        }
    }
}

impl TextDoc for SimpleDoc {
    fn as_lines(&self) -> &dyn LineSource {
        self
    }
    fn version(&self) -> Version {
        self.versions.current()
    }
    fn token(&self) -> CancelToken {
        self.versions.token()
    }
    fn snapshot_lines(&self) -> Box<dyn LineSource + Send> {
        Box::new(SimpleSnapshot(self.lines.clone()))
    }
    fn save_writer(&self) -> SaveWriter {
        let (lines, crlf, final_newline) = (self.lines.clone(), self.crlf, self.final_newline);
        Box::new(move |out| {
            let nl: &[u8] = if crlf { b"\r\n" } else { b"\n" };
            for (i, l) in lines.iter().enumerate() {
                if i > 0 {
                    out.write_all(nl)?;
                }
                out.write_all(l.as_bytes())?;
            }
            // 末尾の改行: 元のファイルにあったか、新しい文書なら付ける
            if final_newline || lines.last().is_some_and(|l| !l.is_empty()) {
                out.write_all(nl)?;
            }
            Ok(())
        })
    }
    fn is_modified(&self) -> bool {
        self.saved != Some(self.content)
    }
    fn mark_saved(&mut self) {
        self.saved = Some(self.content);
    }
    fn line_len(&self, line: u32) -> u32 {
        self.lines.get(line as usize).map_or(0, |l| chars(l))
    }
    fn selection(&self) -> (Pos, Pos) {
        (self.anchor, self.head)
    }
    fn set_selection(&mut self, anchor: Pos, head: Pos) {
        self.anchor = self.clamp(anchor);
        self.head = self.clamp(head);
        self.goal = None;
        self.typing = false;
    }
    fn move_cursor(&mut self, motion: Motion, select: bool, page_lines: u32) {
        let h = self.head;
        let last = self.lines.len() as u32 - 1;
        let (s, e) = self.range();
        let vertical = |d: i64, goal: &mut Option<u32>| -> Pos {
            let g = *goal.get_or_insert(h.col);
            let line = (h.line as i64 + d).clamp(0, last as i64) as u32;
            if (d < 0 && h.line == 0) || (d > 0 && h.line == last) {
                // 端からさらに動かすと行頭 / 行末へ
                return Pos::new(line, if d < 0 { 0 } else { u32::MAX });
            }
            Pos::new(line, g)
        };
        let mut goal = self.goal;
        let p = match motion {
            // 選択があるときの左右は、選択の端に移るだけ
            Motion::Left if !select && s != e => s,
            Motion::Right if !select && s != e => e,
            Motion::Left if h.col > 0 => Pos::new(h.line, h.col - 1),
            Motion::Left if h.line > 0 => Pos::new(h.line - 1, u32::MAX),
            Motion::Left => h,
            Motion::Right if h.col < self.line_len(h.line) => Pos::new(h.line, h.col + 1),
            Motion::Right if h.line < last => Pos::new(h.line + 1, 0),
            Motion::Right => h,
            Motion::Up => vertical(-1, &mut goal),
            Motion::Down => vertical(1, &mut goal),
            Motion::PageUp => vertical(-(page_lines.max(1) as i64), &mut goal),
            Motion::PageDown => vertical(page_lines.max(1) as i64, &mut goal),
            Motion::WordLeft => self.word_left(h),
            Motion::WordRight => self.word_right(h),
            Motion::LineStart => {
                // 1 回目は字下げの後ろ、そこにいれば行頭
                let indent = self.lines[h.line as usize].chars().take_while(|c| c.is_whitespace()).count() as u32;
                Pos::new(h.line, if h.col == indent { 0 } else { indent })
            }
            Motion::LineEnd => Pos::new(h.line, u32::MAX),
            Motion::DocStart => Pos::new(0, 0),
            Motion::DocEnd => Pos::new(last, u32::MAX),
        };
        self.head = self.clamp(p);
        if !select {
            self.anchor = self.head;
        }
        self.goal = if motion.scrolls() { goal } else { None };
        self.typing = false;
    }
    fn select_all(&mut self) {
        self.anchor = Pos::new(0, 0);
        self.head = self.clamp(Pos::new(u32::MAX, u32::MAX));
        self.typing = false;
    }
    fn selected_text(&self) -> String {
        let (s, e) = self.range();
        if s.line == e.line {
            let l = &self.lines[s.line as usize];
            return l[byte_at(l, s.col)..byte_at(l, e.col)].to_string();
        }
        let first = &self.lines[s.line as usize];
        let mut out = first[byte_at(first, s.col)..].to_string();
        for l in &self.lines[s.line as usize + 1..e.line as usize] {
            out.push('\n');
            out.push_str(l);
        }
        let lastl = &self.lines[e.line as usize];
        out.push('\n');
        out.push_str(&lastl[..byte_at(lastl, e.col)]);
        out
    }
    fn insert(&mut self, text: &str) -> Option<LineChange> {
        let (s, e) = self.range();
        if text.is_empty() && s == e {
            return None;
        }
        let single = s == e && text.chars().count() == 1 && text != "\n";
        self.begin_edit(single);
        Some(self.replace(s, e, text))
    }
    fn delete_backward(&mut self) -> Option<LineChange> {
        let (s, e) = self.range();
        let start = if s != e {
            s
        } else if s.col > 0 {
            Pos::new(s.line, s.col - 1)
        } else if s.line > 0 {
            Pos::new(s.line - 1, self.line_len(s.line - 1))
        } else {
            return None;
        };
        self.begin_edit(false);
        Some(self.replace(start, e, ""))
    }
    fn delete_forward(&mut self) -> Option<LineChange> {
        let (s, e) = self.range();
        let end = if s != e {
            e
        } else if s.col < self.line_len(s.line) {
            Pos::new(s.line, s.col + 1)
        } else if (s.line as usize) + 1 < self.lines.len() {
            Pos::new(s.line + 1, 0)
        } else {
            return None;
        };
        self.begin_edit(false);
        Some(self.replace(s, end, ""))
    }
    fn delete_lines(&mut self) -> Option<LineChange> {
        let (a, b) = self.sel_lines();
        if self.lines.len() == 1 && self.lines[0].is_empty() {
            return None;
        }
        self.begin_edit(false);
        let old = LineRange::new(a as u32, self.lines.len() as u32);
        self.lines.drain(a..=b);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let p = self.clamp(Pos::new(a as u32, 0));
        self.anchor = p;
        self.head = p;
        // 消した行から下は全部ずれる
        Some(LineChange { old, new: LineRange::new(a as u32, self.lines.len() as u32) })
    }
    fn indent_lines(&mut self, indent: &str, outdent: bool) -> Option<LineChange> {
        let (a, b) = self.sel_lines();
        let width = chars(indent);
        let mut deltas = vec![0i64; b - a + 1];
        for (i, l) in self.lines[a..=b].iter().enumerate() {
            if outdent {
                let n = if l.starts_with('\t') { 1 } else { l.chars().take(width as usize).take_while(|&c| c == ' ').count() };
                deltas[i] = -(n as i64);
            } else if !l.is_empty() {
                deltas[i] = width as i64;
            }
        }
        if deltas.iter().all(|&d| d == 0) {
            return None;
        }
        self.begin_edit(false);
        for (i, &d) in deltas.iter().enumerate() {
            let l = &mut self.lines[a + i];
            if d > 0 {
                l.insert_str(0, indent);
            } else {
                l.drain(..(-d) as usize);
            }
        }
        let fix = |p: Pos| -> Pos {
            let i = p.line as usize;
            if i < a || i > b {
                return p;
            }
            Pos::new(p.line, (p.col as i64 + deltas[i - a]).max(0) as u32)
        };
        self.anchor = fix(self.anchor);
        self.head = fix(self.head);
        Some(LineChange { old: LineRange::new(a as u32, b as u32 + 1), new: LineRange::new(a as u32, b as u32 + 1) })
    }
    fn toggle_line_comment(&mut self, marker: &str) -> Option<LineChange> {
        if marker.is_empty() {
            return None;
        }
        let (a, b) = self.sel_lines();
        let indent_of = |l: &str| l.chars().take_while(|c| c.is_whitespace()).count();
        let body: Vec<usize> = (a..=b).filter(|&i| !self.lines[i].trim().is_empty()).collect();
        if body.is_empty() {
            return None;
        }
        let all = body.iter().all(|&i| self.lines[i].trim_start().starts_with(marker));
        self.begin_edit(false);
        // 外すときは記号と、その後ろの空白 1 つ。付けるときは、いちばん浅い字下げの位置に
        let at = body.iter().map(|&i| indent_of(&self.lines[i])).min().unwrap_or(0);
        let mut deltas = vec![(0usize, 0i64); b - a + 1];
        for &i in &body {
            let l = &mut self.lines[i];
            if all {
                let col = indent_of(l);
                let start = byte_at(l, col as u32);
                let mut end = start + marker.len();
                if l[end..].starts_with(' ') {
                    end += 1;
                }
                let n = l[start..end].chars().count() as i64;
                l.replace_range(start..end, "");
                deltas[i - a] = (col, -n);
            } else {
                let pos = byte_at(l, at as u32);
                l.insert_str(pos, marker);
                l.insert(pos + marker.len(), ' ');
                deltas[i - a] = (at, chars(marker) as i64 + 1);
            }
        }
        let fix = |p: Pos| -> Pos {
            let i = p.line as usize;
            if i < a || i > b {
                return p;
            }
            let (col, d) = deltas[i - a];
            if (p.col as usize) < col {
                return p;
            }
            Pos::new(p.line, (p.col as i64 + d).max(col as i64) as u32)
        };
        self.anchor = fix(self.anchor);
        self.head = fix(self.head);
        Some(LineChange { old: LineRange::new(a as u32, b as u32 + 1), new: LineRange::new(a as u32, b as u32 + 1) })
    }
    fn break_undo_group(&mut self) {
        self.typing = false;
    }
    fn undo(&mut self) -> Option<LineChange> {
        let s = self.undo.pop()?;
        self.redo.push(self.snap());
        Some(self.restore(s))
    }
    fn redo(&mut self) -> Option<LineChange> {
        let s = self.redo.pop()?;
        self.undo.push(self.snap());
        Some(self.restore(s))
    }
    fn find(&self, needle: &str, from: Pos, backward: bool) -> Option<(Pos, Pos)> {
        if needle.is_empty() || needle.contains('\n') {
            return None;
        }
        let n = self.lines.len();
        let from = self.clamp(from);
        let len = chars(needle);
        let hit = |line: usize, byte: usize| {
            let col = self.lines[line][..byte].chars().count() as u32;
            (Pos::new(line as u32, col), Pos::new(line as u32, col + len))
        };
        // 行を 1 周する（最初の行は `from` の後ろ / 前だけ、最後にもう 1 回その行の残り）
        for step in 0..=n {
            let line = if backward { (from.line as usize + n - step % n) % n } else { (from.line as usize + step) % n };
            let l = &self.lines[line];
            let cut = byte_at(l, from.col);
            let found = match (backward, step) {
                (false, 0) => l[cut..].find(needle).map(|i| i + cut),
                (false, s) if s == n => l[..cut].find(needle),
                (false, _) => l.find(needle),
                // 後ろ向き: `from` より前で終わるもの
                (true, 0) => l[..cut].rfind(needle),
                (true, s) if s == n => l.rfind(needle).filter(|&i| i >= cut),
                (true, _) => l.rfind(needle),
            };
            if let Some(i) = found {
                return Some(hit(line, i));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> SimpleDoc {
        SimpleDoc::from_text(text)
    }
    fn at(d: &mut SimpleDoc, line: u32, col: u32) {
        d.set_selection(Pos::new(line, col), Pos::new(line, col));
    }

    #[test]
    fn insert_delete_and_line_changes() {
        let mut d = doc("abc\ndef\n");
        assert_eq!((d.len_lines(), d.is_modified()), (2, false));
        at(&mut d, 0, 1);
        let c = d.insert("X").unwrap();
        assert_eq!((c.old, c.new), (LineRange::new(0, 1), LineRange::new(0, 1)));
        assert_eq!((d.text(), d.selection().1, d.is_modified()), ("aXbc\ndef".into(), Pos::new(0, 2), true));
        let c = d.insert("1\n2\n3").unwrap();
        assert_eq!((c.old, c.new, c.delta()), (LineRange::new(0, 1), LineRange::new(0, 3), 2));
        assert_eq!((d.text(), d.selection().1), ("aX1\n2\n3bc\ndef".into(), Pos::new(2, 1)));
        // 行頭の BackSpace は前の行とつなぐ
        at(&mut d, 1, 0);
        let c = d.delete_backward().unwrap();
        assert_eq!((c.old, c.new), (LineRange::new(0, 2), LineRange::new(0, 1)));
        assert_eq!((d.text(), d.selection().1), ("aX12\n3bc\ndef".into(), Pos::new(0, 3)));
        // 行末の Delete は次の行とつなぐ
        at(&mut d, 0, 4);
        d.delete_forward().unwrap();
        assert_eq!(d.text(), "aX123bc\ndef");
        // 選択を置き換える（日本語の桁は文字数）
        let mut d = doc("あいう\nかきく");
        d.set_selection(Pos::new(0, 1), Pos::new(1, 2));
        assert_eq!(d.selected_text(), "いう\nかき");
        d.insert("-").unwrap();
        assert_eq!((d.text(), d.selection().1), ("あ-く".into(), Pos::new(0, 2)));
        // 端では何も起きない
        let mut d = doc("a");
        assert_eq!(d.delete_backward(), None);
        at(&mut d, 0, 1);
        assert_eq!(d.delete_forward(), None);
        assert_eq!(d.insert(""), None);
    }

    #[test]
    fn undo_groups_typing() {
        let mut d = doc("x");
        at(&mut d, 0, 1);
        for c in ["a", "b", "c"] {
            d.insert(c);
        }
        d.insert("\n");
        d.insert("d");
        assert_eq!(d.text(), "xabc\nd");
        d.undo().unwrap();
        assert_eq!(d.text(), "xabc\n");
        d.undo().unwrap();
        assert_eq!(d.text(), "xabc");
        d.undo().unwrap();
        assert_eq!((d.text(), d.selection().1), ("x".into(), Pos::new(0, 1)));
        assert!(!d.is_modified());
        assert_eq!(d.undo(), None);
        d.redo().unwrap();
        assert_eq!(d.text(), "xabc");
        // まとまりを切ると、続きの入力は別の単位
        d.break_undo_group();
        d.insert("Z");
        d.undo().unwrap();
        assert_eq!(d.text(), "xabc");
        // 版は編集のたびに進む（古い依頼を捨てるため）
        let t = d.token();
        d.insert("!");
        assert!(t.is_stale());
    }

    #[test]
    fn motions() {
        let mut d = doc("    foo bar_baz(1)\n\nlast line");
        at(&mut d, 0, 0);
        d.move_cursor(Motion::WordRight, false, 10);
        assert_eq!(d.selection().1, Pos::new(0, 4));
        d.move_cursor(Motion::WordRight, false, 10);
        assert_eq!(d.selection().1, Pos::new(0, 8));
        d.move_cursor(Motion::WordRight, true, 10);
        assert_eq!(d.selection(), (Pos::new(0, 8), Pos::new(0, 15)));
        d.move_cursor(Motion::WordLeft, false, 10);
        assert_eq!(d.selection().1, Pos::new(0, 8));
        d.move_cursor(Motion::LineEnd, false, 10);
        assert_eq!(d.selection().1, Pos::new(0, 18));
        // 上下は桁を覚える
        d.move_cursor(Motion::Down, false, 10);
        assert_eq!(d.selection().1, Pos::new(1, 0));
        d.move_cursor(Motion::Down, false, 10);
        assert_eq!(d.selection().1, Pos::new(2, 9));
        d.move_cursor(Motion::Up, false, 10);
        d.move_cursor(Motion::Up, false, 10);
        assert_eq!(d.selection().1, Pos::new(0, 18));
        // 行頭: 字下げの後ろ ⇔ 行頭
        d.move_cursor(Motion::LineStart, false, 10);
        assert_eq!(d.selection().1, Pos::new(0, 4));
        d.move_cursor(Motion::LineStart, false, 10);
        assert_eq!(d.selection().1, Pos::new(0, 0));
        // 行をまたぐ左右
        d.move_cursor(Motion::Left, false, 10);
        assert_eq!(d.selection().1, Pos::new(0, 0));
        at(&mut d, 1, 0);
        d.move_cursor(Motion::Left, false, 10);
        assert_eq!(d.selection().1, Pos::new(0, 18));
        d.move_cursor(Motion::Right, false, 10);
        assert_eq!(d.selection().1, Pos::new(1, 0));
        d.move_cursor(Motion::PageDown, false, 10);
        assert_eq!(d.selection().1, Pos::new(2, 0));
        d.move_cursor(Motion::PageDown, false, 10);
        assert_eq!(d.selection().1, Pos::new(2, 9));
        d.move_cursor(Motion::DocStart, true, 10);
        assert_eq!(d.selection(), (Pos::new(2, 9), Pos::new(0, 0)));
        d.select_all();
        assert_eq!(d.selection(), (Pos::new(0, 0), Pos::new(2, 9)));
        // 選択があるときの左右は端へ
        d.move_cursor(Motion::Left, false, 10);
        assert_eq!(d.selection(), (Pos::new(0, 0), Pos::new(0, 0)));
    }

    #[test]
    fn line_operations() {
        let mut d = doc("def f():\n    a = 1\n\n    b = 2\nend");
        d.set_selection(Pos::new(1, 2), Pos::new(3, 3));
        d.indent_lines("    ", false).unwrap();
        assert_eq!(d.text(), "def f():\n        a = 1\n\n        b = 2\nend");
        assert_eq!(d.selection(), (Pos::new(1, 6), Pos::new(3, 7)));
        d.indent_lines("    ", true).unwrap();
        d.indent_lines("    ", true).unwrap();
        assert_eq!(d.text(), "def f():\na = 1\n\nb = 2\nend");
        assert_eq!(d.indent_lines("    ", true), None);
        d.undo();
        // コメント: いちばん浅い字下げの位置に付ける。空の行には付けない
        let c = d.toggle_line_comment("#").unwrap();
        assert_eq!((c.old, c.new), (LineRange::new(1, 4), LineRange::new(1, 4)));
        assert_eq!(d.text(), "def f():\n    # a = 1\n\n    # b = 2\nend");
        d.toggle_line_comment("#").unwrap();
        assert_eq!(d.text(), "def f():\n    a = 1\n\n    b = 2\nend");
        // 行を消す
        d.set_selection(Pos::new(1, 0), Pos::new(2, 0));
        let c = d.delete_lines().unwrap();
        assert_eq!(c.delta(), -1);
        assert_eq!(d.text(), "def f():\n\n    b = 2\nend");
        d.select_all();
        d.delete_lines().unwrap();
        assert_eq!((d.text(), d.len_lines()), (String::new(), 1));
        assert_eq!(d.delete_lines(), None);
    }

    #[test]
    fn find_wraps_both_ways() {
        let d = doc("foo bar\nbar foo\nbaz");
        assert_eq!(d.find("foo", Pos::new(0, 0), false), Some((Pos::new(0, 0), Pos::new(0, 3))));
        assert_eq!(d.find("foo", Pos::new(0, 1), false), Some((Pos::new(1, 4), Pos::new(1, 7))));
        assert_eq!(d.find("foo", Pos::new(1, 5), false), Some((Pos::new(0, 0), Pos::new(0, 3))));
        assert_eq!(d.find("foo", Pos::new(1, 4), true), Some((Pos::new(0, 0), Pos::new(0, 3))));
        assert_eq!(d.find("foo", Pos::new(0, 0), true), Some((Pos::new(1, 4), Pos::new(1, 7))));
        assert_eq!(d.find("nope", Pos::new(0, 0), false), None);
        assert_eq!(d.find("", Pos::new(0, 0), false), None);
    }

    #[test]
    fn save_writer_keeps_line_endings() {
        let write = |d: &SimpleDoc| {
            let mut out = Vec::new();
            (d.save_writer())(&mut out).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!(write(&doc("a\nb\n")), "a\nb\n");
        assert_eq!(write(&doc("a\r\nb\r\n")), "a\r\nb\r\n");
        assert_eq!(write(&doc("a\nb")), "a\nb\n"); // 末尾の改行は付ける
        assert_eq!(write(&doc("")), "");
        let mut d = doc("");
        d.insert("x");
        assert_eq!(write(&d), "x\n");
        d.mark_saved();
        assert!(!d.is_modified());
        // DocBackend::Simple は UTF-8 でないファイルを断る
        let p = std::env::temp_dir().join(format!("zcode-doc-{}.bin", std::process::id()));
        std::fs::write(&p, [0xff, 0xfe, 0x00]).unwrap();
        assert!(DocBackend::Simple.load(&p, &CancelToken::never()).is_err());
        std::fs::write(&p, "ok\n").unwrap();
        assert_eq!(DocBackend::Simple.load(&p, &CancelToken::never()).unwrap().len_lines(), 1);
        let _ = std::fs::remove_file(&p);
    }
}
