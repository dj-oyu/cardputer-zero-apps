//! serve: UNIX ソケットの相手・ジョブの取り回し・バックエンドの選択
//! （docs/stage3-design.md §7.1・§7.6）。課題 D5。
//!
//! ```text
//!   poll( UNIX ソケット（待ち受け + つないできた相手） / ビルド係への接続 / timerfd / eventfd )
//!     相手の依頼 ─▶ scan（根・目録）─▶ Session（状態機械）─▶ Conn（送る）
//!     Conn（届いた）─▶ Session ─▶ 知らせ ─▶ 相手（IPC）・成果物の置き場
//! ```
//!
//! - スレッドは 1 本。`poll()` の 1 回の起床でやる仕事には上限を付ける（読むのは 64KB まで、
//!   ハッシュは 1 ファイルずつ）。打鍵を受けるのは zcode で、serve は nice +5
//! - 相手は何人でもよい（zcode の専用スレッド、F5 の `zc-remote run`、ssh からの CLI）。
//!   **`Editor` の相手には、ほかの相手が頼んだジョブの進み具合・診断・終了も流す**
//!   （`req` = 0。F5 のビルドの診断を波線にするため）。ログと成果物は頼んだ相手にだけ
//! - 進み具合とログは、相手ごとに最短 33ms に 1 回にまとめる。相手が読まなくて
//!   送り待ちが 1MB を超えたら、その相手を切る（serve は待たない）
//! - 無通信（相手がいない・ジョブが無い）が [`ServeOptions::idle_exit`] 続いたら終わる。
//!   終わる前に SD の控え（[`crate::artifact::LastGood`]）を書く。SIGTERM / SIGINT でも同じ
//! - ビルド係への接続は、最後のジョブが終わってから [`ServeOptions::keep_warm`] だけ保ち、
//!   静かに閉じる（待機中に ping は打たない）
//! - **バックエンドの選択**（設定が `auto` のとき）: PC の設定があれば、まず PC へ 300ms だけ
//!   TCP の接続を試す。届けば PC、届かなければ Cloudflare。結果は 60 秒覚える
//! - ソケットは 0600、フォルダは 0700。相手の uid が自分と同じことを `SO_PEERCRED` で確かめる
//! - 二重起動: ソケットにつなげたら、もう動いているので何もせず終わる。つなげなければ
//!   古いソケットを消して作り直す（`bind` の前に `flock` で 1 つに絞る）

use crate::config::Config;
use std::path::PathBuf;
use std::time::Duration;

/// serve の指定。
#[derive(Clone, Debug)]
pub struct ServeOptions {
    /// ソケットの場所（既定は [`zc_remote_ipc::socket_path`]）。
    pub socket: PathBuf,
    pub config: Config,
    /// 相手もジョブも無いまま、これだけ経ったら終わる。
    pub idle_exit: Duration,
    /// 最後のジョブのあと、ビルド係への接続を保つ時間。
    pub keep_warm: Duration,
    /// 段ごとの所要を標準エラーに出す（`--stats`）。
    pub stats: bool,
}

impl ServeOptions {
    /// 既定（無通信 180 秒で終了、接続は 75 秒保つ）。
    pub fn new(config: Config) -> ServeOptions {
        ServeOptions { socket: zc_remote_ipc::socket_path(), config, idle_exit: Duration::from_secs(180), keep_warm: Duration::from_secs(75), stats: false }
    }
}

/// 終わり方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeExit {
    /// 無通信で終わった。
    Idle,
    /// シグナル、または [`zc_remote_ipc::Request::Shutdown`]。
    Stopped,
    /// もう別の serve が動いていた。
    AlreadyRunning,
}

/// serve を動かす（終わるまで戻らない）。
pub fn run(opts: ServeOptions) -> std::io::Result<ServeExit> {
    let _ = opts;
    todo!("D5")
}

/// serve が動いていなければ起動して、ソケットにつなげるようになるまで待つ（最大 `wait`）。
/// `detach` なら `setsid` で切り離す（CLI から。zcode からは切り離さない = 同じプロセス
/// グループなので、APPLaunch の Esc 長押しで一緒に終わる）。
pub fn ensure_running(program: &std::path::Path, socket: &std::path::Path, detach: bool, wait: Duration) -> std::io::Result<std::os::unix::net::UnixStream> {
    let _ = (program, socket, detach, wait);
    todo!("D5")
}

// 受け入れ試験（課題 D5）は tests/serve.rs に。ビルド係は同じプロセスの中の偽物
// （zc-wire で書いた台本どおりに返す TCP サーバ）と、本物の zc-buildd の両方:
// 依頼 → 知らせの列 → Done、Editor の相手に他人のジョブの診断が届く・ログは届かない、
// 2 回目の保存で前の check が Cancelled、NotApplicable、設定なしで Auth、オフラインで
// 控えからの Artifact{stale} → Offline、auto で PC が閉じていれば Cloudflare 側へ、
// 相手が読まないと切られる、無通信で終わる・終わるとき控えを書く、二重起動、
// 他人の uid を断る（確かめられる範囲で）、SIGTERM。
