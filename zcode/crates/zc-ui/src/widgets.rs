//! 段階 2 以降で実装する部品の雛形。どれも [`crate::Widget`] を実装し、
//! 「描き変えた行範囲だけを返す」約束に従う。
//!
//! | 部品 | 段階 | 担当 | モック |
//! |---|---|---|---|
//! | [`EditorView`] | 2（Markdown は 5） | Fable | 1・2・7〜11 |
//! | [`TabBar`]・[`FileTree`] | 2 | Sonnet | 1 |
//! | [`TerminalPanel`] | 3 | Opus / Sonnet | 3・4 |
//! | [`CandidatePopup`]・[`MiniBuffer`] | 4 | Opus / Sonnet | 8・9 |
//! | [`ShortcutList`] | 6 | Sonnet | 5・6 |

use crate::{RenderCx, Widget};
use zc_core::{Command, Damage, DocId, KeyChord, Rect, Style};
use zc_font::TextStyle;

/// 表示行の中の、同じ見た目が続く区間（design.md §5.5）。
#[derive(Clone, Debug, PartialEq)]
pub struct VisualRun {
    /// 論理行の中の文字位置の範囲。
    pub chars: core::ops::Range<u32>,
    /// 表示行の左端からの x。
    pub x: i16,
    pub width: u16,
    pub font: TextStyle,
    pub style: Style,
}

/// 論理行 1 つぶんのレイアウト結果。コード表示は「全部の行が同じ高さ・同じ字幅」の
/// 特別な場合として、同じ形で扱う。
#[derive(Clone, Debug, PartialEq)]
pub struct VisualLine {
    /// 行の高さ（画素）。
    pub height: u16,
    /// 行の上端からベースラインまで。
    pub baseline: u16,
    pub runs: Vec<VisualRun>,
}

/// エディタ本体（行番号・本文・カーソル・選択・未確定文字列・波線）。
/// 論理行 → 表示行（高さ・各文字の x 座標）のレイアウトを持ち、等幅のマス目を
/// 前提にしない。
pub struct EditorView {
    _doc: DocId,
}

impl EditorView {
    pub fn new(doc: DocId) -> EditorView {
        EditorView { _doc: doc }
    }
}

impl Widget for EditorView {
    fn render(&mut self, _cx: &mut RenderCx<'_>, _area: Rect) -> Damage {
        todo!("段階 2")
    }
    fn invalidate(&mut self) {
        todo!("段階 2")
    }
}

/// タブ 1 つ。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub doc: DocId,
    pub title: String,
    /// 未保存（● を付ける）。
    pub modified: bool,
}

/// タブバー（最上段、高さ 13）。
#[derive(Default)]
pub struct TabBar {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

impl Widget for TabBar {
    fn render(&mut self, _cx: &mut RenderCx<'_>, _area: Rect) -> Damage {
        todo!("段階 2")
    }
    fn invalidate(&mut self) {
        todo!("段階 2")
    }
}

/// ファイルツリー（左 84px）。
#[derive(Default)]
pub struct FileTree {
    _private: (),
}

impl Widget for FileTree {
    fn render(&mut self, _cx: &mut RenderCx<'_>, _area: Rect) -> Damage {
        todo!("段階 2")
    }
    fn invalidate(&mut self) {
        todo!("段階 2")
    }
}

/// ターミナルのパネル（下半分、または全画面）。`zc_term::Term` の画面を描く。
#[derive(Default)]
pub struct TerminalPanel {
    _private: (),
}

impl Widget for TerminalPanel {
    fn render(&mut self, _cx: &mut RenderCx<'_>, _area: Rect) -> Damage {
        todo!("段階 3")
    }
    fn invalidate(&mut self) {
        todo!("段階 3")
    }
}

/// SKK の候補一覧（カーソルの下に出る小窓）。
#[derive(Default)]
pub struct CandidatePopup {
    _private: (),
}

impl Widget for CandidatePopup {
    fn render(&mut self, _cx: &mut RenderCx<'_>, _area: Rect) -> Damage {
        todo!("段階 4")
    }
    fn invalidate(&mut self) {
        todo!("段階 4")
    }
}

/// ステータスバーの上に開く 1 行の入力欄（辞書登録・検索・行へ移動）。
#[derive(Default)]
pub struct MiniBuffer {
    _private: (),
}

impl Widget for MiniBuffer {
    fn render(&mut self, _cx: &mut RenderCx<'_>, _area: Rect) -> Damage {
        todo!("段階 4")
    }
    fn invalidate(&mut self) {
        todo!("段階 4")
    }
}

/// ショートカット一覧の 1 行。`zcode/src/commands.rs` の表から作る。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutRow {
    pub category: &'static str,
    pub title: &'static str,
    pub chord: Option<KeyChord>,
    pub command: Command,
}

/// ショートカット一覧（F1。絞り込みつき。コマンドパレットを兼ねる）。
#[derive(Default)]
pub struct ShortcutList {
    pub rows: Vec<ShortcutRow>,
}

impl Widget for ShortcutList {
    fn render(&mut self, _cx: &mut RenderCx<'_>, _area: Rect) -> Damage {
        todo!("段階 6")
    }
    fn invalidate(&mut self) {
        todo!("段階 6")
    }
}
