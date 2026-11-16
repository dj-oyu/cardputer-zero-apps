//! ビルド係との会話の状態機械（docs/stage3-design.md §5.5・§7.4）。課題 D2。
//!
//! **I/O を持たない。** 入力は「つながった・切れた・メッセージが届いた・時間が来た・
//! 依頼が来た」、出力は「これを送れ・この中身を送れ・これを知らせろ・つなぎ直せ」。
//! ソケットもファイルも時計も触らないので、試験は入力の列を流して出力を確かめるだけで書ける
//! （切断・再接続・順番の入れ替わりを全部ここで試す）。
//!
//! ```text
//!   依頼 ─▶ [目録を送る] ─▶ need ─▶ [中身を送る] ─▶ ready ─▶ [job を送る]
//!        ─▶ accepted / started / log / diagnostics / tests / file / artifact… ─▶ done
//!   切れた ─▶ つなぎ直し ─▶ hello ─▶ 走っていたジョブごとに attach（受け取り済みの位置つき）
//!                         └▶ 木がまだ ready でなければ目録からやり直す
//! ```
//!
//! 決まり:
//!
//! - 1 つの接続で、同時に走らせるのはプロジェクトごとに build 1 つ + check 1 つ + test 1 つ。
//!   同じ種類の新しい依頼が来たら、前のものに `cancel` を送ってから新しい `job` を送る
//!   （前のものの `done` は待たない。届いた `done {cancelled}` は依頼した側へ流す）
//! - 同じ job id の依頼が重なったら 1 つにまとめる（依頼した側の両方へ知らせる）
//! - やり直し: build / test は 0.5 秒から倍々で 3 回まで。check はやり直さない
//!   （次の保存が代わり）。`error {limit}` はやり直さない
//! - つなぎ直しても受付が `unknown-job` と言ったら、木の確認（目録）からやり直す
//!   （受付が再起動した・コンテナが冷えた）。`done {lost}` も同じ。これも「やり直し」に数える
//! - ログは位置（`offset`）で重複を捨てる。成果物は受け取り済みの位置から続きをもらう

use std::sync::Arc;
use zc_wire::msg::{ArtifactKind, FileDiags, JobOpts, Kind, Lang, Status, TestCase};
use zc_wire::tree::Manifest;

/// 状態機械が振る、依頼の番号（[`crate::serve`] が IPC の依頼と結び付ける）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket(pub u32);

/// 依頼。
#[derive(Clone, Debug)]
pub struct JobRequest {
    /// project id（[`zc_wire::tree::project_id`]）。
    pub project: String,
    pub manifest: Arc<Manifest>,
    pub kind: Kind,
    pub lang: Lang,
    pub opts: JobOpts,
    /// ログも受け取るか。
    pub want_log: bool,
}

/// 依頼の終わり方（[`Event::Done`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 受付が `done` を返した。
    Finished { status: Status, exit: i32, queue_ms: u32, run_ms: u32 },
    /// 受付・Worker に断られた（上限・大きさ・約束）。やり直さない。
    Rejected { code: zc_wire::msg::ErrorCode, message: String },
    /// やり直しの回数を使い切った（通信の失敗）。理由は [`crate::net::NetError`] を
    /// serve が付ける。
    GaveUp,
}

/// 知らせ。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// 中身を送っている（バイト数）。
    Uploading { ticket: Ticket, done: u64, total: u64 },
    Queued { ticket: Ticket, ahead: u32 },
    Running { ticket: Ticket },
    Log { ticket: Ticket, text: String },
    Diagnostics { ticket: Ticket, complete: bool, files: Vec<FileDiags> },
    Tests { ticket: Ticket, passed: u32, failed: u32, ignored: u32, cases: Vec<TestCase> },
    /// プロジェクトに書き戻すファイル（lock）。書くのは serve。
    WriteBack { ticket: Ticket, path: String, sha256: String, text: String },
    /// 成果物が始まる（または続きから再開する。`offset` は圧縮後の位置）。
    ArtifactBegin { ticket: Ticket, kind: ArtifactKind, name: String, sha256: String, bytes: u64, zbytes: u64, offset: u64 },
    /// 成果物の中身（圧縮されたまま）。`data` は [`Session::on_binary`] に渡したフレームの
    /// 中の範囲（コピーしない）。`last` で終わり。
    ArtifactData { ticket: Ticket, offset: u64, data: core::ops::Range<usize>, last: bool },
    Done { ticket: Ticket, outcome: Outcome },
}

/// 状態機械が「やってほしい」こと。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// テキストフレームを送る。
    SendText(String),
    /// ファイルの中身をバイナリフレームで送る（`id` = need の添字。読むのは serve。
    /// 送り終えたら [`Session::blob_sent`]、読めなければ [`Session::blob_failed`]）。
    SendBlob { id: u32, manifest: Arc<Manifest>, path: String, sha256: String, size: u64 },
    /// 知らせる。
    Emit(Event),
    /// 接続を閉じて、`after_ms` 後につなぎ直す。
    Reconnect { after_ms: u32 },
    /// 接続が要る（まだつないでいなければつなぐ）。
    Connect,
    /// もう用が無い（走っているジョブが無い）。serve は温める時間のあと静かに閉じる。
    Idle,
}

/// 状態機械。接続 1 本（ビルド係 1 つ）につき 1 つ。
pub struct Session {
    _private: (),
}

impl Session {
    pub fn new() -> Session {
        Session { _private: () }
    }

    /// 依頼を足す。同じプロジェクト・同じ種類の前の依頼は取り消す。
    pub fn submit(&mut self, now_us: u64, req: JobRequest) -> Ticket {
        let _ = (now_us, req);
        todo!("D2")
    }

    /// 依頼を取り消す（受付に `cancel` を送り、`done` を待つ。まだ送っていなければその場で
    /// [`Outcome::Finished`]（cancelled）を出す）。
    pub fn cancel(&mut self, now_us: u64, ticket: Ticket) {
        let _ = (now_us, ticket);
        todo!("D2")
    }

    /// WebSocket が開いた（`hello` を送るところから）。
    pub fn on_open(&mut self, now_us: u64) {
        let _ = now_us;
        todo!("D2")
    }

    /// テキストフレームが届いた。
    pub fn on_text(&mut self, now_us: u64, text: &str) {
        let _ = (now_us, text);
        todo!("D2")
    }

    /// バイナリフレームが届いた（成果物）。
    pub fn on_binary(&mut self, now_us: u64, frame: &[u8]) {
        let _ = (now_us, frame);
        todo!("D2")
    }

    /// 接続が切れた・つなげなかった。`fatal` は、やり直しても同じ失敗
    /// （認証・時計・オフライン）のとき: 走っている依頼を全部 [`Outcome::GaveUp`] にする。
    pub fn on_closed(&mut self, now_us: u64, fatal: bool) {
        let _ = (now_us, fatal);
        todo!("D2")
    }

    /// [`Action::SendBlob`] を送り終えた。
    pub fn blob_sent(&mut self, now_us: u64, id: u32) {
        let _ = (now_us, id);
        todo!("D2")
    }

    /// [`Action::SendBlob`] のファイルを読めなかった・目録と中身が変わっていた
    /// （走査のあとで書き換わった）。その木の依頼を、目録を作り直してやり直させる
    /// （[`Event::Done`] は出さず、serve に [`Action::Emit`] で知らせない。serve が
    /// `resubmit` する）。戻り値は影響を受けた依頼。
    pub fn blob_failed(&mut self, now_us: u64, id: u32) -> Vec<Ticket> {
        let _ = (now_us, id);
        todo!("D2")
    }

    /// 時間が来た（[`Session::deadline_us`]）。
    pub fn on_tick(&mut self, now_us: u64) {
        let _ = now_us;
        todo!("D2")
    }

    /// 次に [`Session::on_tick`] を呼んでほしい時刻（µs）。無ければ `None`。
    pub fn deadline_us(&self) -> Option<u64> {
        todo!("D2")
    }

    /// 溜まった「やってほしいこと」を 1 つ取り出す。入力を 1 つ与えるたびに、`None` に
    /// なるまで呼ぶ。
    pub fn poll_action(&mut self) -> Option<Action> {
        todo!("D2")
    }

    /// 走っている・待っている依頼の数。
    pub fn active(&self) -> usize {
        todo!("D2")
    }
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

// 受け入れ試験（課題 D2）は tests/session.rs に、台本（入力の列 → 期待する出力の列）の形で書く。
// docs/stage3-design.md §14 の表の場面を全部: 2 回目は need が空、中身を送っている途中で切れる、
// ログの途中で切れて attach（重複なし）、成果物の途中で切れて続きから、unknown-job → 目録から、
// done{lost} → やり直し、保存 2 回で前の check に cancel、同じ job id の依頼をまとめる、
// 3 回失敗して GaveUp、check はやり直さない、error{limit} はやり直さない、知らない type を無視。
