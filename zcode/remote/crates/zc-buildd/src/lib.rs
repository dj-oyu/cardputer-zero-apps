//! ビルド係の受付 `zc-buildd`（docs/stage3-design.md §4）。
//!
//! Cloudflare のコンテナの中でも、PC の開発コンテナでも、同じ実行ファイルが同じ口
//! （ポート [`zc_wire::DEFAULT_PORT`]、WebSocket [`zc_wire::WS_PATH`]）で待つ。x86_64 で動き、
//! 実機用（aarch64・Cortex-A53）にクロスビルドする。
//!
//! | モジュール | 中身 | 課題 |
//! |---|---|---|
//! | [`server`] | 待ち受け・接続ごとのスレッド・HTTP の口・接続の状態機械 | B1 |
//! | [`jobs`] | ジョブの台帳: 接続から切り離して走らせる・attach・cancel・ログの保持 | B1 |
//! | [`store`] | 中身の置き場と、作業の木への写し（変わったファイルだけ書く） | B2 |
//! | [`pack`] | 成果物の仕上げ（strip・zstd・SHA-256） | B2 |
//! | [`runner`] | ジョブの実行の口（[`runner::Runner`]）と道具の起動 | （雛形で確定） |
//! | [`runner::rust`] | cargo / rustc と、rustc の JSON → 診断 | B3 |
//! | [`runner::c`]・[`runner::node`] | gcc、npm + tsc + esbuild | B4 |
//!
//! **実機（依頼してきた側）を信用しない。** パスは目録の検査（[`zc_wire::tree::Manifest::new`]）
//! を通ったものにだけ書き、作業の木の外へ出ない。上限（[`zc_wire::limits`]）を超えたら断る。
//! `build.rs`・`postinstall`・試験といった任意のコードはこの中で動くので、**受付の環境に
//! 秘密を置かない**（道具には環境変数を絞って渡す。[`runner::tool_env`]）。
//!
//! スレッド: 待ち受け 1 本 + 接続ごとに 1 本 + ジョブごとに 1 本（道具の出力を読む）。
//! tokio は使わない（接続は同時に数本）。

pub mod jobs;
pub mod pack;
pub mod runner;
pub mod server;
pub mod store;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// `hello` の `server` に出す名前。
pub const SERVER_NAME: &str = concat!("zc-buildd/", env!("CARGO_PKG_VERSION"));

/// 受付の設定（コマンドラインと環境変数から。[`Config::from_env`]）。
#[derive(Clone, Debug)]
pub struct Config {
    /// 待ち受けるアドレス（既定 `0.0.0.0:7411`）。
    pub listen: SocketAddr,
    /// 中身の置き場・作業の木・ビルドの出力を置く根（既定 `/var/lib/zc-buildd`）。
    ///
    /// ```text
    ///   blobs/<sha256 の先頭 2 桁>/<sha256>     中身（内容で引く）
    ///   proj/<project id>/src/                  作業の木（目録のとおりに写す）
    ///   proj/<project id>/target/               cargo の出力（aarch64 用とホスト用）
    ///   proj/<project id>/node_modules/ …       npm ci の結果
    ///   out/<job id>/                           成果物（圧縮済み）と結果
    /// ```
    pub data_dir: PathBuf,
    /// 同時に走らせるジョブの数（既定 2: build / test 1 つ + check 1 つ）。超えた分は待たせる。
    pub max_running: usize,
    /// 待たせるジョブの数の上限（超えたら `error {limit}`）。
    pub max_queued: usize,
    /// 1 ジョブの時間の上限（既定 15 分）。
    pub job_timeout: Duration,
    /// 終わったジョブの結果（ログ・診断・成果物）を残す時間（既定 30 分）と数（既定 32）。
    pub keep_done: Duration,
    pub keep_done_max: usize,
    /// `data_dir` の大きさの上限（既定 8GiB。超えたら古いプロジェクトから消す）。
    pub disk_quota: u64,
}

impl Config {
    /// 既定の値。
    pub fn defaults() -> Config {
        Config {
            listen: SocketAddr::from(([0, 0, 0, 0], zc_wire::DEFAULT_PORT)),
            data_dir: PathBuf::from("/var/lib/zc-buildd"),
            max_running: 2,
            max_queued: 8,
            job_timeout: Duration::from_secs(15 * 60),
            keep_done: Duration::from_secs(30 * 60),
            keep_done_max: 32,
            disk_quota: 8 << 30,
        }
    }

    /// 既定の値を、環境変数（`ZC_BUILDD_LISTEN`・`ZC_BUILDD_DATA`・`ZC_BUILDD_MAX_RUNNING`・
    /// `ZC_BUILDD_JOB_TIMEOUT_S`）と引数（`--listen`・`--data`）で上書きする。
    pub fn from_env(args: &[String]) -> Result<Config, String> {
        let _ = args;
        todo!("B1")
    }
}
