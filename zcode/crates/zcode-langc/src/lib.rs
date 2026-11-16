//! zcode の言語定義の道具（PC 用。実機には入らない）。
//!
//! - [`import`]: Sublime Text の `.sublime-syntax`（YAML）を `language.toml` の下書きにする。
//!   DFA にできない書き方（後方参照・先読み・Unicode の文字クラスなど）は、近い形に直すか
//!   規則ごと外して、一覧（[`Report`]）にする
//! - `check` / `show`（`main.rs`）: `language.toml` を変換して大きさを出す・見本に色を付けて見せる

pub mod import;
pub mod regex;

pub use import::{import_sublime_syntax, Imported, Report};
