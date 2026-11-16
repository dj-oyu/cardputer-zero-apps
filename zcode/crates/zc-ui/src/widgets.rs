//! 段階 6 で実装する部品の雛形（ほかは実装済み: [`crate::editor`]・[`crate::tabs`]・
//! [`crate::tree`]・[`crate::terminal`]・[`crate::skkui`]・[`crate::picker`]）。[`crate::Widget`] を
//! 実装し、「描き変えた行範囲だけを返す」約束に従う。
//!
//! | 部品 | 段階 | 担当 | モック |
//! |---|---|---|---|
//! | [`ShortcutList`] | 6 | Sonnet | 5・6 |

use crate::{RenderCx, Widget};
use zc_core::{Command, Damage, KeyChord, Rect};

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
