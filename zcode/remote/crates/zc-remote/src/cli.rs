//! コマンドライン（docs/stage3-design.md §7.8）。課題 D5。
//!
//! ```text
//! zc-remote build <file> [--release]   ビルドして、置いた実行ファイルのパスを標準出力に出す
//! zc-remote check <file>               型検査。診断を「パス:行:桁: error[E0308]: …」で出す
//! zc-remote test  <file> [filter]      試験をビルド係で実行して、結果を出す
//! zc-remote run   <file> [-- 引数…]    ビルドして、その場で実行する（F5 がターミナルで動かす）
//! zc-remote applies <file>              ビルド係の対象か（0 = 対象、1 = 対象でない。通信しない）
//! zc-remote serve [--stats]            UNIX ソケットで待つ（ふつうは自動で起動される）
//! zc-remote status                     つながっているビルド係・走っているジョブ
//! zc-remote stop                       serve を終わらせる
//! 共通: --lang rust|c|node（省略すると拡張子から）  --stats  --version  --help
//! ```
//!
//! - どれも serve に頼むだけ（無ければ切り離して起動する）。ログは標準エラーへそのまま流す
//! - `run`: 成功したら、置かれた実行ファイルに **`exec` で入れ替わる**（JS は `llrt <パス>`）。
//!   だから終了コードも Ctrl+C も、F5 の今の仕組み（OSC 777）のまま動く。ビルド中の
//!   SIGINT は「取り消して 130 で終わる」
//! - `run` でネットが無く、前に成功した版があるとき: 「オフライン。前の版（<日時>）を
//!   実行する？ [y/N]」と端末で聞く（端末でなければ実行しない）
//! - 終了コード: 0 成功 / 1 ビルド・型検査・試験の失敗 / 2 使い方の間違い /
//!   3 オフライン / 4 認証・設定 / 5 上限 / 6 そのほかの失敗 / 130 取り消し

use std::ffi::OsString;

/// 終了コード。
pub mod exit {
    pub const OK: u8 = 0;
    pub const FAILED: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const OFFLINE: u8 = 3;
    pub const AUTH: u8 = 4;
    pub const LIMIT: u8 = 5;
    pub const ERROR: u8 = 6;
    pub const CANCELLED: u8 = 130;
}

/// コマンドラインを実行して、終了コードを返す（`run` が成功したときは戻らない）。
pub fn main(args: Vec<OsString>) -> u8 {
    if args.iter().skip(1).any(|a| a == "--version") {
        println!("{}", crate::CLIENT_NAME);
        return exit::OK;
    }
    eprintln!("zc-remote: まだ実装していない（docs/stage3-design.md の課題 D5）");
    exit::ERROR
}
