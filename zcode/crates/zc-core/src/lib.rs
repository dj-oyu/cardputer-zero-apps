//! zcode 共通の型（design.md §3）。
//!
//! ほかの crate はすべてこの crate の型を介してやり取りする。依存の向きは
//! `zc-core` ← 各 crate ← `zc-ui` ← `zcode` で、横同士は依存しない。
//!
//! | モジュール | 中身 |
//! |---|---|
//! | [`geom`] | 画面の大きさ、[`Rect`]、[`RowRange`] |
//! | [`color`] | [`Color`]（RGB565）、16 段階の色の表 [`BlendLut`]、Dark+ の配色 [`palette`] |
//! | [`style`] | 文字の見た目 [`Style`] / [`Attrs`] |
//! | [`canvas`] | 描画先 [`Canvas`]（fb0 も PNG もこれを写すだけ） |
//! | [`damage`] | 部品が返す「描き変えた行範囲」[`Damage`] と優先度 [`Priority`] |
//! | [`key`] | 入力 [`KeyEvent`] / [`Key`] / [`Mods`] / [`KeyChord`] |
//! | [`diag`] | 診断 [`Diagnostic`] / [`DiagnosticSet`]（出どころごとに置き換える） |
//! | [`command`] | 操作 [`Command`]（キー割り当て表は `zcode/src/commands.rs`） |
//! | [`text`] | テキストの位置 [`Pos`]、行の読み口 [`LineSource`]、トークン [`Span`] / [`TokenKind`]、配色 [`Theme`] |
//! | [`version`] | 版番号と取り消し（古い結果を捨てる仕組み） |
//! | [`rt`] | スレッドの受け渡し（eventfd、結果の送り口、ワーカー） |
//! | [`time`] | 単調時計（µs） |
//!
//! API を変えたいときは勝手に変えず、統合側（メインのセッション）に戻すこと。

#[cfg(not(target_endian = "little"))]
compile_error!("zcode はリトルエンディアン専用（fb0 の RGB565 をそのまま u16 で持つ）");

pub mod canvas;
pub mod color;
pub mod command;
pub mod damage;
pub mod diag;
pub mod geom;
pub mod key;
pub mod rt;
pub mod style;
pub mod text;
pub mod time;
pub mod version;

pub use canvas::Canvas;
pub use color::{palette, BlendLut, Color};
pub use command::{Command, Motion};
pub use damage::{Damage, Priority};
pub use diag::{DiagSource, Diagnostic, DiagnosticSet, FileDiagnostics, Severity};
pub use geom::{Rect, RowRange, SCREEN_H, SCREEN_W};
pub use key::{Key, KeyChord, KeyEvent, MediaKey, Mods};
pub use style::{Attrs, Style};
pub use text::{LineChange, LineRange, LineSource, Pos, Span, Theme, TokenKind};
pub use version::{CancelToken, Cancelled, DocId, Version, VersionCounter, Versioned};
