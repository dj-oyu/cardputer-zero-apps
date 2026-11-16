//! 待ち受けと、接続ごとの状態機械（docs/stage3-design.md §4.1・§5.5）。課題 B1。
//!
//! HTTP の口（どれも同じポート）:
//!
//! | 依頼 | 返事 |
//! |---|---|
//! | `GET /v1/ws`（Upgrade、`Sec-WebSocket-Protocol: zc-build.v1`） | WebSocket（[`zc_wire::msg`]） |
//! | `GET /healthz` | `200 ok`（コンテナの起動の確認用） |
//! | `GET /v1/status` | `{"running":n,"queued":n,"jobs":[{"job":…,"kind":…,"state":…}],"uptime_s":n}`（DO がコンテナを起こしておくかの判断に使う） |
//!
//! 接続の決まり:
//!
//! - 最初のメッセージは `hello`。版が違えば `error {version}` を返して閉じる
//! - `manifest` → 持っていない中身を `need` で返す → バイナリフレームを順に受ける
//!   （大きさとハッシュを確かめながら一時ファイルへ → 合えば置き場へ rename）→ 全部そろったら
//!   作業の木に写して `ready`。`need` が空ならすぐ `ready`
//! - `job` はその接続で `ready` を受けた木に対してだけ受け付ける（違えば `error {bad-tree}`）。
//!   job id は受付でも計算し直して確かめる
//! - **ジョブは接続から切り離す**: 接続が切れてもジョブは止めない（[`crate::jobs::Registry`]）。
//!   つなぎ直した相手は `attach` で続きを受ける
//! - **遅い相手を待たない**: 送り待ちが 4MB を超えたら、その接続を閉じる（ジョブは続く。
//!   相手はつなぎ直して `attach` する）。ログは台帳に全部残っている
//! - 15 秒何も送っていなければ ping を打つ。60 秒何も届かなければ閉じる
//! - `X-Zc-Device`（Worker が付ける）があれば、ログに出す名前として使う。認証はしない
//!   （Cloudflare では Access と Worker、PC では Tailscale の ACL が門番）

use crate::jobs::Registry;
use crate::Config;
use std::net::TcpStream;
use std::sync::Arc;

/// 待ち受けて、接続ごとにスレッドを作る（終わるまで戻らない）。SIGTERM を受けたら、
/// 新しい接続を止め、走っているジョブをプロセスグループごと止めてから戻る（Cloudflare は
/// 止めるとき SIGTERM を送り、最大 15 分後に SIGKILL する）。
pub fn run(cfg: Config) -> std::io::Result<()> {
    let _ = cfg;
    todo!("B1")
}

/// 待ち受けを始めて、待ち受けているアドレスと止めるための持ち手を返す（試験用。
/// ポート 0 で空いているポートを選べる）。
pub fn spawn(cfg: Config) -> std::io::Result<Handle> {
    let _ = cfg;
    todo!("B1")
}

/// [`spawn`] の持ち手。捨てると止まる。
pub struct Handle {
    _private: (),
}

impl Handle {
    pub fn addr(&self) -> std::net::SocketAddr {
        todo!("B1")
    }
    pub fn registry(&self) -> Arc<Registry> {
        todo!("B1")
    }
}

/// 接続 1 本を最後まで相手する（接続ごとのスレッドの中身）。
pub fn serve_connection(stream: TcpStream, registry: Arc<Registry>) -> std::io::Result<()> {
    let _ = (stream, registry);
    todo!("B1")
}
