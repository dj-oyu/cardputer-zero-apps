//! 実機側のクライアント `zc-remote`（docs/stage3-design.md §7）。
//!
//! 実行ファイルは 1 つで 2 通りに動く:
//!
//! - **serve**: `$XDG_RUNTIME_DIR/zcode/remote.sock` で待ち、zcode と CLI の依頼を受けて
//!   ビルド係と話す。無通信が続けば終わる。systemd の常駐サービスにはしない
//! - **CLI**: `zc-remote build|check|test|run <file>`。serve に頼むだけの薄い口
//!   （無ければ serve を起動する）。`run` は F5 がターミナルの pty で動かす
//!
//! スレッドは 1 本（＋ DNS を引く間だけの使い捨て 1 本）。tokio は使わない。
//! 1 つの `poll()` で、UNIX ソケット・ビルド係への接続・timerfd を待つ。
//!
//! | モジュール | 中身 | 課題 |
//! |---|---|---|
//! | [`config`] | 設定とトークンのファイル | D5 |
//! | [`auth`] | 認証の口（[`auth::Credential`]） | D1 |
//! | [`net`] | つなぐ・TLS・WebSocket（ノンブロッキング。`poll()` に載せる） | D1 |
//! | [`session`] | ビルド係との会話の状態機械（I/O なし） | D2 |
//! | [`scan`] | プロジェクトの根・木の走査・ハッシュの覚え | D3 |
//! | [`artifact`] | 成果物の受け取り（tmpfs へ流し書き）・SD の 1 つだけの控え | D4 |
//! | [`serve`] | UNIX ソケットの相手・ジョブの取り回し・バックエンドの選択 | D5 |
//! | [`cli`] | コマンドライン | D5 |
//!
//! **SD に書かない。** 例外は 3 つだけ: 設定とトークン（人が 1 回書く）、最後に成功した
//! 実行ファイル（プロジェクトごとに 1 つ。[`artifact::LastGood`]）、lock ファイルの書き戻し
//! （変わったときだけ）。ログは標準エラーか RAM。

pub mod artifact;
pub mod auth;
pub mod cli;
pub mod config;
pub mod net;
pub mod scan;
pub mod serve;
pub mod session;

/// `hello` の `client` と `--version` に出す名前。
pub const CLIENT_NAME: &str = concat!("zc-remote/", env!("CARGO_PKG_VERSION"));
