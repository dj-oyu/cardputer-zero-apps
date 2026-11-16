//! 表示（design.md §5.1 / §5.2）。
//!
//! ```text
//! メイン:  Canvas ──present()──▶ 行差分 ──▶ 提示用バッファ + 変更行の表 ──▶ flush を起こす
//! flush :  変更行の表から「まとまり」を 1 つ取る ──▶ fb0 の mmap に書く ──▶ fsync(fb0)
//! ```
//!
//! - [`Display`]: メインが持つ。[`Display::present`] は行差分とコピーだけをして
//!   すぐ返る（`fsync` はメインで呼ばない）
//! - [`plan`]: 変更行の表と、離れた範囲を分けて送る計画（純粋なロジック）
//! - [`diff`]: 行差分
//! - [`Backend`]: 書き込み先。実機は [`FbBackend`]（fb0）、テストは [`MemBackend`]
//! - [`png`]: `Canvas` の PNG 出力と、render-test の正解画像との比較
//!
//! render-test は `Display` を通さず、`Canvas` を直接 [`png::write_png`] /
//! [`png::check_golden`] に渡す。

pub mod backend;
pub mod diff;
pub mod fb;
pub mod flush;
pub mod plan;
pub mod png;

pub use backend::{Backend, MemBackend, MemLog, MemOp};
pub use fb::{emergency_restore, FbBackend};
pub use flush::{Display, FlushConfig, FlushRecord, PresentInfo};
pub use plan::{FlushGroup, PendingRows, MERGE_GAP_ROWS};
