//! `zc-remote`（別プロセス）の相手をする専用スレッド（docs/stage3-design.md §8）。
//!
//! 雛形（段階 3 の設計）。公開の形はここで確定。中身は課題 Z2 が書く。エディタへの
//! つなぎ込み（保存時の check、「試験を実行」、診断の取り込み）は課題 Z3。
//!
//! ```text
//!   メイン ──(RemoteCmd: mpsc)──▶ zc-remote スレッド ──(UNIX ソケット)──▶ zc-remote serve
//!   メイン ◀─(AppEvent::Remote: MainSender)── zc-remote スレッド ◀──────────┘
//! ```
//!
//! - リモートの仕事（数秒〜数分）を batch ワーカーに載せない（batch は直列で、ファイルを
//!   開く・ハイライトの残り・検索も受け持つ）
//! - スレッドは最初の依頼で作る（スタック 256KB）。ソケットが無ければ `zc-remote serve` を
//!   起動して（zcode と同じプロセスグループのまま）、最大 1 秒待ってつなぐ。`zc-remote` が
//!   入っていなければ [`RemoteEvent::Unavailable`] を 1 回だけ送り、以後の依頼は
//!   [`DoneStatus::NotApplicable`] で返す
//! - zcode に TLS も JSON も入れない。ここが扱うのは `zc-remote-ipc` の固定の並びだけ
//! - **メインを起こす回数を抑える**: 進み具合（[`RemoteEvent::Phase`]）とログは、スレッドの
//!   中で上限つきのバッファにまとめ、メインへは最短 [`MIN_WAKE_INTERVAL`] に 1 回だけ送る。
//!   段階の変化・診断・終了はすぐ送る。ログのバッファがあふれたら古い方を捨てる
//!   （[`LOG_BUFFER_MAX`]）
//!
//! F5（ビルドして実行）はここを通らない: ターミナルの pty で `zc-remote run <file>` を
//! 動かす（ログ・間引き・Ctrl+C はターミナルの仕組みのまま）。その診断は serve が
//! [`RemoteEvent::Diagnostics`]（`req` が `None`）としてここへも流す。

use crate::app::AppEvent;
use std::path::PathBuf;
use std::time::Duration;
use zc_core::rt::MainSender;
use zc_remote_ipc::{ArtifactKind, Backend, DoneStatus, FileDiags, JobKind, JobTag, Phase, RemoteLang, SubmitFlags, TestReport};

/// メインを起こす最短の間隔（design.md §4.3 のターミナルの間引きと同じ 30fps）。
pub const MIN_WAKE_INTERVAL: Duration = Duration::from_millis(33);
/// まとめて持つログの上限。
pub const LOG_BUFFER_MAX: usize = 64 * 1024;
/// 専用スレッドのスタック。
pub const THREAD_STACK: usize = 256 * 1024;

/// メインが振る依頼の番号（[`RemoteClient::submit`] が返す）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RemoteReq(pub u32);

/// ジョブの依頼。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteJob {
    pub kind: JobKind,
    pub lang: RemoteLang,
    pub flags: SubmitFlags,
    /// 保存したファイルの絶対パス。
    pub file: PathBuf,
}

/// 専用スレッドからメインへ（[`AppEvent::Remote`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteEvent {
    /// `zc-remote` が無い・起動できない・約束の版が違う（1 回だけ。理由つき）。
    Unavailable(String),
    /// 進み具合。`req` は自分の依頼なら `Some`（F5 など、ほかが頼んだジョブは `None`）。
    /// [`Phase::Scanned`] を受けたら、メインは `root` の下の開いているタブの「保存済みの版」を
    /// `tag` に結び付けて控える（診断の基準の版）。
    Phase { req: Option<RemoteReq>, tag: JobTag, root: PathBuf, phase: Phase, done: u32, total: u32 },
    /// ログ（「試験を実行」の出力。端末にそのまま流せるバイト列）。
    Log { req: RemoteReq, bytes: Vec<u8> },
    /// 診断（1 ジョブぶんをまとめて 1 回）。メインは `tag` で控えた版を付けて
    /// `zc_core::DiagnosticSet`（出どころ `Remote`）にし、`zc_intel::DiagStore` に取り込む。
    Diagnostics { req: Option<RemoteReq>, tag: JobTag, root: PathBuf, complete: bool, files: Vec<FileDiags> },
    /// 試験の結果。
    Tests { req: RemoteReq, root: PathBuf, report: TestReport },
    /// 成果物を tmpfs に置いた（自分が build を頼んだときだけ）。
    Artifact { req: RemoteReq, kind: ArtifactKind, stale: bool, path: PathBuf },
    /// ジョブが終わった。
    Done { req: Option<RemoteReq>, tag: JobTag, status: DoneStatus, exit: i32, backend: Backend, elapsed_ms: u32, message: String },
}

/// 専用スレッドの持ち手。メインが持つ。捨てるとソケットを閉じてスレッドを終わらせる
/// （serve は残る。無通信が続けば自分で終わる）。
pub struct RemoteClient {
    _private: (),
}

impl RemoteClient {
    /// 持ち手を作る。スレッドはまだ作らない（最初の依頼で作る）。`program` は `zc-remote` の
    /// 実行ファイル（`None` なら `PATH` から探す）。
    pub fn new(results: MainSender<AppEvent>, program: Option<PathBuf>) -> RemoteClient {
        let _ = (results, program);
        todo!("Z2")
    }

    /// 先につないでおく（Rust のプロジェクトを開いた・最初の保存）。待たない。
    pub fn warm(&self, file: PathBuf) {
        let _ = file;
        todo!("Z2")
    }

    /// ジョブを頼む。待たない。結果は [`RemoteEvent`] で届き、最後に必ず
    /// [`RemoteEvent::Done`] が 1 回届く（スレッドやソケットが壊れたときも）。
    pub fn submit(&self, job: RemoteJob) -> RemoteReq {
        let _ = job;
        todo!("Z2")
    }

    /// 依頼を取り消す。
    pub fn cancel(&self, req: RemoteReq) {
        let _ = req;
        todo!("Z2")
    }
}
