//! Markdown のライブプレビュー（design.md §9、モック 10・11）。
//! **段階 5 で実装する。ここは公開 API の雛形。**
//!
//! - `pulldown-cmark` で解析し、ソースの行ごとに「装飾つきの区間・行の高さ・記号の
//!   位置」（[`MdLine`]）を返す。描くのは `zc-ui` のエディタ（可変の行高・字幅）
//! - 記号（`#`、`**`、`- [ ]` など）は、カーソルのある行だけ薄く見せ、ほかの行では
//!   隠す。その切り替えはここで行う（[`MdDocument::line`] の `cursor_line`）
//! - 編集中のブロックだけメインで解析し直し、全体の再解析は版番号つきで batch に出す
//!
//! この crate は `zc-buffer` も `zc-font` も知らない。大きさは [`MdFont`] で指定し、
//! 実際の画素数は `zc-ui` が決める。

use core::ops::Range;
use zc_core::{CancelToken, Cancelled, LineChange, LineSource, Style, Version, Versioned};

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

/// ソース 1 行ぶんのレイアウトの元。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdLine {
    pub block: MdBlock,
    pub runs: Vec<MdRun>,
}

/// 文書全体の解析結果。メインスレッドが持つ。
pub struct MdDocument {
    // 段階 5: 行ごとのブロックの種類・インラインの区間
    _private: (),
}

impl MdDocument {
    /// 空の文書。
    pub fn new() -> MdDocument {
        MdDocument { _private: () }
    }
    /// **batch スレッドで呼ぶ。** 全体を解析する。途中で `cancel` を見て、古ければやめる。
    pub fn parse(_src: &dyn LineSource, _cancel: &CancelToken) -> Result<Versioned<MdDocument>, Cancelled> {
        todo!("段階 5")
    }
    /// batch の結果で置き換える。`current` と版が違えば捨てて `false`。
    pub fn replace(&mut self, _parsed: Versioned<MdDocument>, _current: Version) -> bool {
        todo!("段階 5")
    }
    /// 編集を知らせ、その行を含むブロックだけ解析し直す（メインで呼ぶ）。
    /// ブロックの境界が変わって全体の再解析が要るなら `true` を返す（batch に出す）。
    pub fn edited(&mut self, _src: &dyn LineSource, _change: LineChange) -> bool {
        todo!("段階 5")
    }
    /// `line` 行目のレイアウトの元。`cursor_line` と同じ行なら記号を見せる
    /// （`hidden` が全部 `false` になる）。
    pub fn line(&self, _line: u32, _text: &str, _cursor_line: Option<u32>) -> MdLine {
        todo!("段階 5")
    }
    /// `line` 行目のチェックボックスを切り替えた文字列（Ctrl+Enter）。
    /// チェックボックスの行でなければ `None`。
    pub fn toggle_checkbox(&self, _line: u32, _text: &str) -> Option<String> {
        todo!("段階 5")
    }
}

impl Default for MdDocument {
    fn default() -> Self {
        MdDocument::new()
    }
}
