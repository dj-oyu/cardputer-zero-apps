//! 入力（design.md §6.2）。
//!
//! ```text
//! evdev ──▶ RawEvent ──▶ Translator（押下状態・モディファイア・キー配列・リピート）──▶ KeyEvent
//! ```
//!
//! - [`Keyboard`]: 実機のデバイス。evdev を直接読み、起動中は `EVIOCGRAB` で独占する。
//!   `poll()` に渡す fd は 2 本（evdev と、キーリピート用の timerfd）
//! - [`Translator`]: デバイスに依存しないロジック（テストできる部分はすべてここ）
//! - [`Keymap`]: `zcode/keymap/*.toml` のキー配列
//! - [`Repeater`]: 自前のキーリピート（実機のキーボードにはリピートが無い）
//!
//! 外に出す打鍵は `zc_core::KeyEvent` だけ。SKK・エディタ・ターミナルはそれを受け取る。

pub mod evdev;
pub mod keycodes;
pub mod keymap;
pub mod repeat;
pub mod translate;

pub use evdev::{emergency_ungrab, Keyboard, RawRecord};
pub use keymap::{KeyDef, Keymap, KeymapError, Layer};
pub use repeat::{RepeatClass, RepeatConfig, Repeater};
pub use translate::{RawEvent, Translator};
