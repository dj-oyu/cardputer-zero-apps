//! 補完・診断・アウトライン（design.md §10）。**段階 2 で最初の版を実装する。
//! ここは公開 API の雛形。**
//!
//! ほかの crate に見せるのは 3 つの形だけ: 補完候補（[`Completion`]）、診断
//! （[`Diagnostic`]）、アウトライン（[`OutlineItem`]）。最初の版（単語補完・保存時の
//! 構文チェック・トークンからのアウトライン）と、後の版の LSP クライアントは、
//! この形の裏で差し替える。
//!
//! この crate は `zc-buffer` も `zc-highlight` も知らない。テキストは
//! `zc_core::LineSource`、トークンは `zc_core::Span` で受け取る。

use std::path::Path;
use zc_core::{CancelToken, Cancelled, LineChange, LineSource, Pos, Span};

/// 補完候補の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    /// 開いているファイルに出てくる単語。
    Word,
    /// 言語のキーワード。
    Keyword,
}

/// 補完候補。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub kind: CompletionKind,
}

/// 診断の重さ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// 診断（波線とステータスバーに出す）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// 0 始まりの行。
    pub line: u32,
    /// 桁（分かれば）。
    pub col: Option<u32>,
    pub severity: Severity,
    pub message: String,
}

/// アウトラインの項目の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutlineKind {
    Function,
    Class,
    Heading,
}

/// アウトラインの 1 項目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    pub line: u32,
    /// 入れ子の深さ（字下げ・見出しのレベルから）。
    pub depth: u8,
    pub kind: OutlineKind,
    pub name: String,
}

/// 単語の索引（開いている文書 1 つぶん）。メインスレッドが持つ。
#[derive(Default)]
pub struct WordIndex {
    _private: (),
}

impl WordIndex {
    pub fn new() -> WordIndex {
        WordIndex::default()
    }
    /// 全体から作り直す。**batch スレッドで呼べる**（途中で `cancel` を見る）。
    pub fn build(_src: &dyn LineSource, _cancel: &CancelToken) -> Result<WordIndex, Cancelled> {
        todo!("段階 2")
    }
    /// 編集を知らせる（変わった行の単語を入れ替える）。
    pub fn edited(&mut self, _src: &dyn LineSource, _change: LineChange) {
        todo!("段階 2")
    }
}

/// カーソル位置の単語の先頭部分（補完の対象）。単語の途中でなければ `None`。
pub fn prefix_at(_src: &dyn LineSource, _pos: Pos) -> Option<String> {
    todo!("段階 2")
}

/// 補完候補を出す。`keywords` は言語のキーワード。近い行の単語を先に、最大 `limit` 件。
pub fn complete(_index: &WordIndex, _keywords: &[&str], _prefix: &str, _near: Pos, _limit: usize) -> Vec<Completion> {
    todo!("段階 2")
}

/// 構文チェックのエラー。
#[derive(Debug)]
pub enum CheckError {
    /// 編集が入って古くなった。
    Cancelled,
    /// コマンドを起動できなかった。
    Spawn(std::io::Error),
    /// 時間切れ（子プロセスは止めた）。
    Timeout,
}

/// 保存時の構文チェック。**batch スレッドで呼ぶ。** `command`（`language.toml` の
/// `check`。`{file}` をパスに置き換える）を使い捨てで実行し、出力から `pattern`
/// （`check_pattern`）でエラー行を拾う。`cancel` が古くなったら子プロセスを止める。
pub fn run_check(_command: &str, _pattern: &str, _file: &Path, _cancel: &CancelToken) -> Result<Vec<Diagnostic>, CheckError> {
    todo!("段階 2")
}

/// 1 行のトークンから、アウトラインの項目になるか調べる（`def` / `class` の行など）。
/// 呼ぶ側が全行（または変わった行）について呼んで一覧にする。
pub fn outline_item(_line: u32, _text: &str, _spans: &[Span]) -> Option<OutlineItem> {
    todo!("段階 2")
}
