//! 段階 3 以降で実装する部品の雛形（エディタ本体・タブ・ツリーは実装済み:
//! [`crate::editor`]・[`crate::tabs`]・[`crate::tree`]）。どれも [`crate::Widget`] を実装し、
//! 「描き変えた行範囲だけを返す」約束に従う。
//!
//! | 部品 | 段階 | 担当 | モック |
//! |---|---|---|---|
//! | [`TerminalPanel`] | 3 | Opus / Sonnet | 3・4 |
//! | [`CandidatePopup`]・[`MiniBuffer`] | 4 | Opus / Sonnet | 8・9 |
//! | [`ShortcutList`] | 6 | Sonnet | 5・6 |

use crate::{RenderCx, Widget};
use zc_core::{Command, Damage, KeyChord, Rect};

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
