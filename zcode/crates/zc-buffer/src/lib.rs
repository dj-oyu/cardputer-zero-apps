//! テキストバッファ（design.md §9）。**段階 2 で実装する。ここは公開 API の雛形。**
//!
//! - 中身は rope（`ropey` を使う想定）。[`Buffer::snapshot`] は中身を共有する写しで、
//!   コピーしない（batch への依頼に付ける。design.md §4.2）
//! - 編集は「変わった行範囲」（[`LineChange`]）を返す。ハイライトと描画はそれを見て
//!   無効化する
//! - 編集のたびに版（[`Version`]）が進む。ワーカーへの依頼には [`Buffer::token`] を付ける
//! - undo は操作単位でまとめる（連続した文字入力は 1 単位）
//! - 位置は [`Pos`]（行と、行内の文字数）。表示桁（全角 = 2）への変換は `zc-ui` の仕事
//!
//! 実装するときは、関数の形（名前・引数・戻り値）を変えないこと。足りないものが
//! あれば統合側に戻す。

use std::io;
use std::path::Path;
use zc_core::{CancelToken, LineChange, LineSource, Motion, Pos, Version, VersionCounter};

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

/// バッファの写し。中身は共有していて、作るのは軽い。`Send` なのでワーカーに渡せる。
#[derive(Clone)]
pub struct Snapshot {
    version: Version,
    // 段階 2: ropey::Rope（clone は中身を共有する）
    _rope: (),
}

impl Snapshot {
    /// 写しを取った時点の版。
    pub fn version(&self) -> Version {
        self.version
    }
    /// 全体を書き出す（保存用。storage スレッドで呼ぶ）。
    pub fn write_to(&self, _out: &mut dyn io::Write, _ending: LineEnding) -> io::Result<()> {
        todo!("段階 2")
    }
    /// 全体の文字数。
    pub fn len_chars(&self) -> usize {
        todo!("段階 2")
    }
}

impl LineSource for Snapshot {
    fn len_lines(&self) -> u32 {
        todo!("段階 2")
    }
    fn line(&self, _line: u32, _out: &mut String) {
        todo!("段階 2")
    }
}

/// 1 つの文書のテキスト・カーソル・undo 履歴。メインスレッドが持つ。
pub struct Buffer {
    versions: VersionCounter,
    selection: Selection,
    // 段階 2: rope、undo / redo の履歴、保存時点の印、改行の種類
}

impl Buffer {
    /// 空のバッファ。
    pub fn new() -> Buffer {
        Buffer { versions: VersionCounter::new(), selection: Selection::default() }
    }
    /// 文字列から作る。
    pub fn from_text(_text: &str) -> Buffer {
        todo!("段階 2")
    }
    /// ファイルを読んで作る。**batch スレッドで呼ぶ**（読み込み・rope の構築まで）。
    /// `cancel` が古くなったら途中でやめる。UTF-8 でなければエラー。
    pub fn load(_path: &Path, _cancel: &CancelToken) -> io::Result<Buffer> {
        todo!("段階 2")
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
        todo!("段階 2")
    }

    // --- 読む ---

    pub fn len_chars(&self) -> usize {
        todo!("段階 2")
    }
    /// `line` 行目の文字数（改行を除く）。
    pub fn line_len(&self, _line: u32) -> u32 {
        todo!("段階 2")
    }
    /// 範囲の文字列（コピー・切り取り用）。
    pub fn text_in(&self, _sel: Selection) -> String {
        todo!("段階 2")
    }
    pub fn line_ending(&self) -> LineEnding {
        todo!("段階 2")
    }
    /// 保存した時点から変わっているか（タブの ● 表示）。
    pub fn is_modified(&self) -> bool {
        todo!("段階 2")
    }
    /// いまの内容を「保存済み」とする。
    pub fn mark_saved(&mut self) {
        todo!("段階 2")
    }

    // --- カーソルと選択 ---

    pub fn cursor(&self) -> Pos {
        self.selection.head
    }
    pub fn selection(&self) -> Selection {
        self.selection
    }
    /// 選択範囲を置く（範囲外の位置は丸める）。
    pub fn set_selection(&mut self, _sel: Selection) {
        todo!("段階 2")
    }
    /// カーソルを動かす。`select` なら選択を伸ばす。`page_lines` は 1 画面の行数
    /// （PgUp / PgDn 用）。上下移動では、元の桁を覚えておく。
    pub fn move_cursor(&mut self, _motion: Motion, _select: bool, _page_lines: u32) {
        todo!("段階 2")
    }
    pub fn select_all(&mut self) {
        todo!("段階 2")
    }

    // --- 編集（どれも、変わった行範囲を返す。何も変わらなければ `None`） ---

    /// 選択範囲を `text` で置き換える（選択が無ければカーソル位置に入れる）。
    pub fn insert(&mut self, _text: &str) -> Option<LineChange> {
        todo!("段階 2")
    }
    /// BackSpace。
    pub fn delete_backward(&mut self) -> Option<LineChange> {
        todo!("段階 2")
    }
    /// Delete。
    pub fn delete_forward(&mut self) -> Option<LineChange> {
        todo!("段階 2")
    }
    /// カーソルのある行（選択があれば、かかっている行すべて）を消す。
    pub fn delete_lines(&mut self) -> Option<LineChange> {
        todo!("段階 2")
    }
    /// 選択行を字下げする / 戻す（`indent` は言語の設定の字下げ文字列）。
    pub fn indent_lines(&mut self, _indent: &str, _outdent: bool) -> Option<LineChange> {
        todo!("段階 2")
    }
    /// 選択行の行コメントを切り替える（`marker` は言語の設定の `line_comment`）。
    pub fn toggle_line_comment(&mut self, _marker: &str) -> Option<LineChange> {
        todo!("段階 2")
    }
    /// 連続入力のまとまりをここで切る（カーソル移動・保存・一定時間の無操作のあと）。
    pub fn break_undo_group(&mut self) {
        todo!("段階 2")
    }
    pub fn undo(&mut self) -> Option<LineChange> {
        todo!("段階 2")
    }
    pub fn redo(&mut self) -> Option<LineChange> {
        todo!("段階 2")
    }

    // --- 検索 ---

    /// `from` より後ろ（`backward` なら前）で `needle` が最初に現れる範囲。
    /// 見つからなければ先頭（末尾）に戻って探す。
    pub fn find(&self, _needle: &str, _from: Pos, _backward: bool) -> Option<Selection> {
        todo!("段階 2")
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer::new()
    }
}

impl LineSource for Buffer {
    fn len_lines(&self) -> u32 {
        todo!("段階 2")
    }
    fn line(&self, _line: u32, _out: &mut String) {
        todo!("段階 2")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_helpers() {
        let s = Selection { anchor: Pos::new(3, 4), head: Pos::new(1, 9) };
        assert_eq!(s.start(), Pos::new(1, 9));
        assert_eq!(s.end(), Pos::new(3, 4));
        assert!(Selection::caret(Pos::new(2, 2)).is_empty());
        let b = Buffer::new();
        assert_eq!(b.cursor(), Pos::default());
        assert!(!b.token().is_stale());
    }
}
