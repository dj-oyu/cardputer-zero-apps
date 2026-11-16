//! ジョブの台帳（docs/stage3-design.md §4.2）。課題 B1。
//!
//! ジョブは接続のものではなく、台帳のもの。接続は「見ている人」（[`Watcher`]）として
//! 付いたり離れたりする。
//!
//! ```text
//!   submit ─▶ Queued ─▶ Running ─▶ Done（結果を keep_done のあいだ残す）
//!                │          └─ cancel / 時間切れ: プロセスグループに SIGTERM → 2 秒後 SIGKILL
//!                └─ cancel: すぐ Done{cancelled}
//! ```
//!
//! - **同じ job id の依頼は同じジョブに当たる**: 走っていれば見る人として付くだけ。終わって
//!   結果が残っていれば、実行せずに結果を流す（`accepted {cached:true}`）。ただし
//!   `Status::Failed` 以外の失敗（cancelled・timeout・error）は残さず、やり直す
//! - 同時に走らせる数は [`crate::Config::max_running`]。**同じプロジェクトのジョブは同時に
//!   走らせない**（作業の木と `target/` を取り合うため）。待ちの順は check を先にする
//!   （短い・保存のたびに来る）
//! - ログは全部台帳に残す（上限 [`zc_wire::limits::MAX_LOG_BYTES`]。超えたら先頭を捨てて
//!   「…（省略）…」の 1 行を入れ、`offset` は捨てた分も数える）。見る人へは 50〜100ms か
//!   4KB ごとにまとめて渡す
//! - 診断・試験の結果・書き戻すファイル・成果物の説明は、終わるときに 1 回ずつ
//! - 道具の出力を読むスレッドは、見る人が遅くても止まらない（見る人への受け渡しは
//!   上限つきの列。あふれたら、その見る人を外す）

use crate::runner::Runner;
use crate::Config;
use std::path::PathBuf;
use std::sync::Arc;
use zc_wire::msg::{JobOpts, Kind, Lang, ServerMsg};

/// ジョブの依頼（`job` メッセージと、そろった木）。
#[derive(Clone, Debug)]
pub struct JobSpec {
    pub job: String,
    pub project: String,
    pub tree: String,
    pub kind: Kind,
    pub lang: Lang,
    pub opts: JobOpts,
    /// 依頼してきた相手の名前（`X-Zc-Device`。ログ用）。
    pub device: Option<String>,
}

/// 見る人へ渡すもの。
#[derive(Clone, Debug)]
pub enum Feed {
    /// そのまま送る制御メッセージ（accepted・started・log・diagnostics・tests・file・
    /// artifact・done・error）。
    Msg(ServerMsg),
    /// 成果物の中身を、このファイルの `offset` から最後まで送れ（接続のスレッドが読んで
    /// バイナリフレームにする。`chan` は直前の `artifact` メッセージと同じ）。
    ArtifactFile { chan: u32, path: PathBuf, offset: u64, zbytes: u64 },
}

/// 見る人（接続）の受け口。[`Registry::submit`] / [`Registry::attach`] が返す。捨てると外れる。
pub struct Watcher {
    _private: (),
}

impl Watcher {
    /// 次に渡すものを待つ。`timeout` までに無ければ `Ok(None)`。ジョブが終わって渡すものが
    /// 尽きたら `Err(Finished)`。受け渡しが遅くて外されたら `Err(Lagged)`。
    pub fn recv(&self, timeout: std::time::Duration) -> Result<Option<Feed>, WatchEnd> {
        let _ = timeout;
        todo!("B1")
    }
}

/// 見るのが終わった理由。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchEnd {
    Finished,
    Lagged,
}

/// 依頼を受けられない理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// 待ちの列がいっぱい。
    QueueFull,
    /// job id が、木・種類・オプションから計算したものと違う。
    BadJobId,
    /// 知らないジョブ（attach / cancel）。
    Unknown,
}

/// 状態（`/v1/status` 用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobInfo {
    pub job: String,
    pub project: String,
    pub kind: Kind,
    /// `queued` / `running` / `done`。
    pub state: &'static str,
}

/// 台帳。受付に 1 つ。
pub struct Registry {
    _private: (),
}

impl Registry {
    /// 台帳を作る。`runner` はジョブの実行の口（試験では偽物を渡す）。
    pub fn new(cfg: &Config, runner: Arc<dyn Runner>) -> Arc<Registry> {
        let _ = (cfg, runner);
        todo!("B1")
    }

    /// ジョブを頼む（同じ id のジョブがあればそれに付く）。`want_log` が偽なら、見る人に
    /// ログを渡さない（台帳には残す）。
    pub fn submit(&self, spec: JobSpec, want_log: bool) -> Result<Watcher, SubmitError> {
        let _ = (spec, want_log);
        todo!("B1")
    }

    /// 続きを受け取る。`log` / `artifact` は受け取り済みの位置。
    pub fn attach(&self, job: &str, log: u64, artifact: u64) -> Result<Watcher, SubmitError> {
        let _ = (job, log, artifact);
        todo!("B1")
    }

    /// 取り消す。
    pub fn cancel(&self, job: &str) -> Result<(), SubmitError> {
        let _ = job;
        todo!("B1")
    }

    /// 走っている・待っているジョブ。
    pub fn status(&self) -> Vec<JobInfo> {
        todo!("B1")
    }

    /// 走っているジョブを全部止める（終了時）。
    pub fn shutdown(&self) {
        todo!("B1")
    }
}

// 受け入れ試験（課題 B1）は tests/jobs.rs と tests/protocol.rs に。実行の口は偽物
// （台本どおりにログ・診断・成果物を出し、合図まで止まる Runner）:
// 同じ id の 2 人が同じ結果を受ける、終わった後の依頼は cached、失敗の結果も cached・
// 取り消しはやり直し、接続を切ってもジョブが続く → attach で続きだけ（ログの位置・成果物の
// 位置）、cancel で道具のプロセスグループが消える（本物の `sleep` で）、時間切れ、
// 同じプロジェクトは直列・別のプロジェクトは並列、check が先、列がいっぱい、ログの上限、
// 遅い見る人が外される、protocol: hello の版・順番の間違い・bad-tree・bad-blob・上限。
