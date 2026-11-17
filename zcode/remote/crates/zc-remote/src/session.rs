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
//!
//! 実装で決めた細かい点（課題 D2）:
//!
//! - **`hello` の返事を待たずに先へ進む**（`hello` に続けて `attach` / `manifest` を送る）。
//!   往復が 1 回減る。版が合わなければ、受付の `hello` か `error {version}` を見た時点で
//!   全部の依頼を [`Outcome::Rejected`] にする
//! - 木の確認（`manifest` → `need` → 中身 → `ready`）は、1 本の接続で 1 つずつ。中身も
//!   1 つずつ頼む（[`Action::SendBlob`] は、前の [`Session::blob_sent`] のあとに次を出す）
//! - やり直しの数え方は 2 つ: 通信の失敗（つなげない・切れた）は「続けて 3 回」で、
//!   そのジョブの知らせ（started・log・成果物）が届いたら 0 に戻す。`unknown-job` と
//!   `done {lost}`（目録からのやり直し）は戻さず、合わせて 3 回まで
//! - **温まっていた接続が実は死んでいた**（待機中に相手が切っていた）ときは、数に入れずに
//!   1 回だけつなぎ直す。check にも効く（そうしないと、75 秒ぶりの保存が必ず失敗する）
//! - 取り消したジョブ（`cancel` を送って `done` を待っている）の知らせは、`done` 以外
//!   捨てる。`done` が 5 秒届かなければ、取り消せたことにする（[`Session::on_tick`]）。
//!   同じ job id を出し直す依頼は、前のものの `done` を待ってから送る
//! - 同じ job id の依頼をまとめたとき、[`Event::ArtifactData`] は**先頭の依頼にだけ**出す
//!   （中身を 2 回書かせない）。ほかの知らせは全員に出す。serve は [`Session::job_of`] で
//!   同じジョブの依頼を見分ける
//! - **LSP**（docs/lsp-design.md）: 問い合わせ（[`Session::lsp`]）もジョブと同じく、目録の木が
//!   `ready` になってから `lsp` / `lsp_warm` を送る（木の確認はジョブより先に回す。打鍵の返事なので）。
//!   wire の `id` は [`Ticket`] の番号。答え（`lsp_result`）は [`Event::LspDone`]、状態
//!   （`lsp_state`）は [`Event::LspState`]。同じプロジェクト・同じ種類の、まだ送っていない前の
//!   問い合わせは `Cancelled` にする。やり直さない（切れた・時間切れ（[`LSP_WAIT_US`]）は
//!   `GaveUp` / `Failed`。温まっていた接続が死んでいただけなら 1 回だけ送り直す）。`lsp_warm` は
//!   送ったところで終わる（答えは状態で届く）
//! - [`Action::Reconnect`] を出した時点で、状態機械は「切れた」ものとして先へ進んでいる。
//!   serve は接続を閉じてつなぎ直すだけでよく、その閉じたことを [`Session::on_closed`] で
//!   知らせない（知らせると、失敗が 1 回余分に数えられる）。つなぎ直しに失敗したら
//!   `on_closed` を呼ぶ

use zc_wire as wire;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use wire::msg::{decode, ArtifactKind, BinHeader, BinKind, ClientMsg, ErrorCode, FileDiags, FileEntry, JobOpts, Kind, Lang, LspItem, LspPhase, LspQuery, ServerMsg, Status, TestCase};
use wire::tree::{job_id, Manifest};

/// 通信の失敗で、続けてやり直す回数（build / test）。
const MAX_RETRIES: u8 = 3;
/// やり直しの待ち（ms）。ここから倍々。
const BACKOFF_MS: u32 = 500;
/// `cancel` を送ってから `done` を待つ時間（µs）。
const CANCEL_WAIT_US: u64 = 5_000_000;
/// 1 本の接続で覚えておく「そろった木」の数。
const READY_KEEP: usize = 8;
/// 切れている間に取り消したジョブ（つながったら `cancel` を送る）を覚えておく数。
const ORPHAN_KEEP: usize = 8;
/// LSP の問い合わせを送ってから答えを待つ時間（µs）。受付は 25 秒で自分から時間切れを返す。
pub const LSP_WAIT_US: u64 = 30_000_000;

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

/// LSP の問い合わせ（[`Session::lsp`]）。
#[derive(Clone, Debug)]
pub struct LspRequest {
    pub project: String,
    /// 未保存の中身を重ねた目録。
    pub manifest: Arc<Manifest>,
    /// `None` は `lsp_warm`（先に起こすだけ）。
    pub query: Option<LspQuery>,
    /// 木の根からの相対。
    pub path: String,
    pub line: u32,
    pub col: u32,
    pub text: String,
}

/// LSP の問い合わせの終わり方（[`Event::LspDone`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LspOutcome {
    /// 答え（何も無ければ両方空）。`lsp_warm` は送ったところで空の答え。
    Answer { text: String, items: Vec<LspItem> },
    /// 受付・DO が断った（準備中・上限・木がそろっていない・時間切れ）。
    Failed { code: ErrorCode, message: String },
    /// 通信の失敗（理由は serve が付ける）。
    GaveUp,
    /// 新しい問い合わせに替わった・取り消した。
    Cancelled,
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
    /// 中の範囲（コピーしない）。`last` で終わり。同じ job id の依頼をまとめたときは、
    /// 先頭の依頼にだけ出る。
    ArtifactData { ticket: Ticket, offset: u64, data: core::ops::Range<usize>, last: bool },
    Done { ticket: Ticket, outcome: Outcome },
    /// LSP の問い合わせの終わり（必ず 1 回）。
    LspDone { ticket: Ticket, outcome: LspOutcome },
    /// rust-analyzer の状態（受付の `lsp_state`。どの依頼のものでもない）。
    LspState { project: String, state: LspPhase, message: String, error: Option<ErrorCode> },
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Link {
    /// つないでいない（頼んでもいない）。
    Down,
    /// つないでほしいと頼んだ（[`Action::Connect`] / [`Action::Reconnect`]）。
    Dialing,
    Up,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    /// まだ受付に出していない（木の確認から）。
    Pending,
    /// 受付は知っている。つながったら `attach` を送る。
    Attach,
    /// 今の接続に `job` か `attach` を送ってある。
    Live,
    /// `cancel` を送って `done` を待っている。
    Cancelling,
}

struct Sub {
    ticket: Ticket,
    want_log: bool,
}

struct Job {
    id: String,
    project: String,
    tree: String,
    manifest: Arc<Manifest>,
    kind: Kind,
    lang: Lang,
    opts: JobOpts,
    subs: Vec<Sub>,
    st: St,
    /// 受付が受け付けた（`accepted` を見た）= つなぎ直したら `attach` できる。
    accepted: bool,
    running: bool,
    /// このジョブの知らせ（started・log・成果物…）が 1 つでも届いた。
    heard: bool,
    /// つながっている接続に出した依頼（その接続が死んでいたら、数えずに 1 回やり直す）。
    on_warm: bool,
    /// 受け取り済みのログの位置。
    log_rx: u64,
    /// 成果物の番号と、受け取り済みの位置（圧縮後）。
    art: Option<(u32, u64)>,
    /// 続けて失敗した回数（通信）。
    fails: u8,
    /// 目録からやり直した回数（unknown-job・lost）。
    restarts: u8,
    /// `Cancelling` のとき、ここまでに `done` が来なければ取り消せたことにする。
    cancel_deadline: u64,
}

enum Phase {
    /// `manifest` を送って `need` を待っている。
    Sent,
    /// 中身を送っている。`need` は目録の添字、`next` は今送っているもの。
    Upload { need: Vec<u32>, next: usize, done: u64, total: u64 },
    /// 全部送って `ready` を待っている。
    WaitReady,
}

/// LSP の問い合わせ 1 つ。
struct LspWork {
    ticket: Ticket,
    req: LspRequest,
    tree: String,
    /// 今の接続に送った。
    sent: bool,
    /// 送ったときに温まっていた接続（その接続が死んでいたら、数えずに 1 回送り直す）。
    on_warm: bool,
    /// 送ってから答えを待つ締め切り。
    deadline: u64,
}

/// 木の確認（1 本の接続で 1 つずつ）。
struct Sync {
    project: String,
    tree: String,
    manifest: Arc<Manifest>,
    phase: Phase,
}

/// `manifest` を、目録を写さずに JSON にするための形（[`ClientMsg::Manifest`] と同じ JSON）。
#[derive(serde::Serialize)]
struct ManifestMsg<'a> {
    #[serde(rename = "type")]
    ty: &'static str,
    project: &'a str,
    tree: &'a str,
    files: &'a [FileEntry],
}

const CANCELLED: Outcome = Outcome::Finished { status: Status::Cancelled, exit: 0, queue_ms: 0, run_ms: 0 };

/// 状態機械。接続 1 本（ビルド係 1 つ）につき 1 つ。
pub struct Session {
    link: Link,
    /// 今の接続で、受付の `hello` を受けた。
    hello_rx: bool,
    /// 今の接続は版が合わない（新しい依頼もすぐ断る）。
    bad_version: bool,
    jobs: Vec<Job>,
    lsps: Vec<LspWork>,
    /// 最後に受けた時刻（送った LSP の締め切りを決める）。
    now: u64,
    cur: Option<Sync>,
    /// 今の接続でそろった木（project, tree）。
    ready: Vec<(String, String)>,
    /// 切れている間に取り消したジョブ。つながったら `cancel` を送る。
    orphans: Vec<String>,
    /// 接続全体の `error`（このあと切れる）。やり直しを使い切ったときの理由にする。
    conn_err: Option<(ErrorCode, String)>,
    actions: VecDeque<Action>,
    next_ticket: u32,
    /// ジョブが減った（空になっていたら [`Action::Idle`] を出す）。
    check_idle: bool,
}

impl Session {
    pub fn new() -> Session {
        Session { link: Link::Down, hello_rx: false, bad_version: false, jobs: Vec::new(), lsps: Vec::new(), now: 0, cur: None, ready: Vec::new(), orphans: Vec::new(), conn_err: None, actions: VecDeque::new(), next_ticket: 1, check_idle: false }
    }

    /// 依頼を足す。同じプロジェクト・同じ種類の前の依頼は取り消す。
    pub fn submit(&mut self, now_us: u64, req: JobRequest) -> Ticket {
        self.now = now_us;
        let ticket = Ticket(self.next_ticket);
        self.next_ticket = self.next_ticket.wrapping_add(1).max(1);
        let tree = req.manifest.tree_id();
        let id = job_id(&tree, req.kind, req.lang, &req.opts);

        // 同じプロジェクト・同じ種類で、中身の違う前の依頼を取り消す。
        let mut i = 0;
        while i < self.jobs.len() {
            let j = &self.jobs[i];
            if j.project == req.project && j.kind == req.kind && j.id != id && j.st != St::Cancelling && self.cancel_job(now_us, i) {
                continue; // その場で消えた
            }
            i += 1;
        }

        // 同じ job id の依頼が生きていれば、そこに加わる。
        if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id && j.st != St::Cancelling) {
            j.subs.push(Sub { ticket, want_log: req.want_log });
            if j.running {
                self.actions.push_back(Action::Emit(Event::Running { ticket }));
            }
            self.settle();
            return ticket;
        }

        // 切れている間に取り消したものと同じなら、取り消しをやめて続きを受ける。
        let known = match self.orphans.iter().position(|o| *o == id) {
            Some(p) => {
                self.orphans.remove(p);
                true
            }
            None => false,
        };
        self.jobs.push(Job {
            id,
            project: req.project,
            tree,
            manifest: req.manifest,
            kind: req.kind,
            lang: req.lang,
            opts: req.opts,
            subs: vec![Sub { ticket, want_log: req.want_log }],
            st: if known { St::Attach } else { St::Pending },
            accepted: known,
            running: false,
            heard: false,
            on_warm: self.link == Link::Up && self.hello_rx,
            log_rx: 0,
            art: None,
            fails: 0,
            restarts: 0,
            cancel_deadline: 0,
        });
        if self.link == Link::Down {
            self.link = Link::Dialing;
            self.actions.push_back(Action::Connect);
        }
        self.settle();
        ticket
    }

    /// LSP の問い合わせを足す（答えは [`Event::LspDone`]）。同じプロジェクト・同じ種類の、まだ送って
    /// いない前の問い合わせは取り消す。
    pub fn lsp(&mut self, now_us: u64, req: LspRequest) -> Ticket {
        self.now = now_us;
        let ticket = Ticket(self.next_ticket);
        self.next_ticket = self.next_ticket.wrapping_add(1).max(1);
        let mut i = 0;
        while i < self.lsps.len() {
            let w = &self.lsps[i];
            if !w.sent && w.req.project == req.project && w.req.query == req.query {
                self.finish_lsp(i, LspOutcome::Cancelled);
            } else {
                i += 1;
            }
        }
        let tree = req.manifest.tree_id();
        self.lsps.push(LspWork { ticket, req, tree, sent: false, on_warm: false, deadline: 0 });
        if self.link == Link::Down {
            self.link = Link::Dialing;
            self.actions.push_back(Action::Connect);
        }
        self.settle();
        ticket
    }

    /// LSP の問い合わせを取り消す（送ってあれば、答えは届いても捨てる）。
    pub fn lsp_cancel(&mut self, ticket: Ticket) {
        if let Some(i) = self.lsps.iter().position(|w| w.ticket == ticket) {
            self.finish_lsp(i, LspOutcome::Cancelled);
        }
        self.settle();
    }

    /// 依頼を取り消す（受付に `cancel` を送り、`done` を待つ。まだ送っていなければその場で
    /// [`Outcome::Finished`]（cancelled）を出す）。同じジョブをほかの依頼も待っていれば、
    /// この依頼だけ外す（ジョブは続く）。
    pub fn cancel(&mut self, now_us: u64, ticket: Ticket) {
        self.now = now_us;
        let Some(i) = self.jobs.iter().position(|j| j.subs.iter().any(|s| s.ticket == ticket)) else { return };
        if self.jobs[i].subs.len() > 1 {
            self.jobs[i].subs.retain(|s| s.ticket != ticket);
            self.actions.push_back(Action::Emit(Event::Done { ticket, outcome: CANCELLED }));
        } else {
            self.cancel_job(now_us, i);
        }
        self.settle();
    }

    /// WebSocket が開いた（`hello` を送るところから）。
    pub fn on_open(&mut self, now_us: u64) {
        self.now = now_us;
        for w in &mut self.lsps {
            w.sent = false;
        }
        self.link = Link::Up;
        self.hello_rx = false;
        self.bad_version = false;
        self.cur = None;
        self.ready.clear();
        // 切れたと聞いていないのに開き直された場合に備える。
        for j in &mut self.jobs {
            if j.st == St::Live {
                j.st = if j.accepted { St::Attach } else { St::Pending };
            }
        }
        self.send(&ClientMsg::Hello { v: wire::PROTOCOL_VERSION, client: crate::CLIENT_NAME.to_owned() });
        for job in std::mem::take(&mut self.orphans) {
            self.send(&ClientMsg::Cancel { job });
        }
        self.settle();
    }

    /// テキストフレームが届いた。
    pub fn on_text(&mut self, now_us: u64, text: &str) {
        self.now = now_us;
        // 読めないもの・知らない type は無視する（版を上げずに足せるように）。
        if let Ok(Some(msg)) = decode::<ServerMsg>(text) {
            self.on_msg(now_us, msg);
        }
        self.settle();
    }

    /// バイナリフレームが届いた（成果物）。
    pub fn on_binary(&mut self, now_us: u64, frame: &[u8]) {
        self.now = now_us;
        let Some((h, body)) = BinHeader::split(frame) else { return };
        if h.kind != BinKind::Artifact {
            return;
        }
        let Some(i) = self.jobs.iter().position(|j| j.st == St::Live && j.art.is_some_and(|(c, _)| c == h.id)) else { return };
        let j = &mut self.jobs[i];
        let (chan, got) = j.art.unwrap();
        let end = h.offset + body.len() as u64;
        if h.offset == got {
            j.art = Some((chan, end));
            j.heard = true;
            j.fails = 0;
            let ticket = j.subs[0].ticket;
            let start = frame.len() - body.len();
            self.actions.push_back(Action::Emit(Event::ArtifactData { ticket, offset: h.offset, data: start..frame.len(), last: h.last }));
        } else if end > got {
            // 位置が飛んだ・半端に重なった。つなぎ直して、受け取り済みの位置から続きをもらう。
            self.link_lost(true, true);
        }
        // 全部受け取り済みの範囲なら捨てるだけ。
        self.settle();
    }

    /// 接続が切れた・つなげなかった。`fatal` は、やり直しても同じ失敗
    /// （認証・時計・オフライン）のとき: 走っている依頼を全部 [`Outcome::GaveUp`] にする。
    pub fn on_closed(&mut self, now_us: u64, fatal: bool) {
        self.now = now_us;
        if fatal {
            self.reset_link();
            self.conn_err = None;
            while !self.lsps.is_empty() {
                self.finish_lsp(0, LspOutcome::GaveUp);
            }
            while let Some(j) = self.jobs.last() {
                let out = if j.st == St::Cancelling { CANCELLED } else { Outcome::GaveUp };
                self.finish(self.jobs.len() - 1, out);
            }
        } else {
            self.link_lost(true, false);
        }
        self.settle();
    }

    /// [`Action::SendBlob`] を送り終えた。
    pub fn blob_sent(&mut self, now_us: u64, id: u32) {
        self.now = now_us;
        let Some(cur) = &mut self.cur else { return };
        let Phase::Upload { need, next, done, total } = &mut cur.phase else { return };
        if *next != id as usize {
            return;
        }
        *done += cur.manifest.files()[need[*next] as usize].size;
        *next += 1;
        let (d, t, more) = (*done, *total, *next < need.len());
        if !more {
            cur.phase = Phase::WaitReady;
        }
        self.emit_upload(d, t);
        if more {
            self.push_blob();
        }
        self.settle();
    }

    /// [`Action::SendBlob`] のファイルを読めなかった・目録と中身が変わっていた
    /// （走査のあとで書き換わった）。その木の依頼を、目録を作り直してやり直させる
    /// （[`Event::Done`] は出さず、serve に [`Action::Emit`] で知らせない。serve が
    /// `resubmit` する）。戻り値は影響を受けた依頼。
    ///
    /// 受付は中身の続きを待ったままなので、接続は作り直す（[`Action::Reconnect`] が出る。
    /// ほかのジョブは `attach` で続く。失敗には数えない）。
    pub fn blob_failed(&mut self, now_us: u64, id: u32) -> Vec<Ticket> {
        self.now = now_us;
        let Some(cur) = &self.cur else { return Vec::new() };
        match &cur.phase {
            Phase::Upload { next, .. } if *next == id as usize => {}
            _ => return Vec::new(),
        }
        let (project, tree) = (cur.project.clone(), cur.tree.clone());
        let mut hit = Vec::new();
        self.jobs.retain(|j| {
            let gone = j.st == St::Pending && j.project == project && j.tree == tree;
            if gone {
                hit.extend(j.subs.iter().map(|s| s.ticket));
            }
            !gone
        });
        // LSP の問い合わせも serve が作り直す（Done も LspDone も出さない）
        self.lsps.retain(|w| {
            let gone = !w.sent && w.req.project == project && w.tree == tree;
            if gone {
                hit.push(w.ticket);
            }
            !gone
        });
        self.check_idle = true;
        self.link_lost(false, true);
        self.settle();
        hit
    }

    /// 時間が来た（[`Session::deadline_us`]）。
    pub fn on_tick(&mut self, now_us: u64) {
        self.now = now_us;
        let mut i = 0;
        while i < self.lsps.len() {
            if self.lsps[i].sent && self.lsps[i].deadline <= now_us {
                self.finish_lsp(i, LspOutcome::Failed { code: ErrorCode::Internal, message: "ビルド係が答えない（時間切れ）".into() });
            } else {
                i += 1;
            }
        }
        let mut i = 0;
        while i < self.jobs.len() {
            if self.jobs[i].st == St::Cancelling && self.jobs[i].cancel_deadline <= now_us {
                self.finish(i, CANCELLED);
            } else {
                i += 1;
            }
        }
        self.settle();
    }

    /// 次に [`Session::on_tick`] を呼んでほしい時刻（µs）。無ければ `None`。
    pub fn deadline_us(&self) -> Option<u64> {
        let jobs = self.jobs.iter().filter(|j| j.st == St::Cancelling).map(|j| j.cancel_deadline);
        let lsps = self.lsps.iter().filter(|w| w.sent).map(|w| w.deadline);
        jobs.chain(lsps).min()
    }

    /// 溜まった「やってほしいこと」を 1 つ取り出す。入力を 1 つ与えるたびに、`None` に
    /// なるまで呼ぶ。
    pub fn poll_action(&mut self) -> Option<Action> {
        self.actions.pop_front()
    }

    /// 走っている・待っている依頼の数（LSP の問い合わせも含む）。
    pub fn active(&self) -> usize {
        self.jobs.len() + self.lsps.len()
    }

    /// 依頼が乗っているジョブの job id（終わった依頼は `None`）。同じ job id の依頼は
    /// 1 つのジョブにまとまっている。
    pub fn job_of(&self, ticket: Ticket) -> Option<&str> {
        self.jobs.iter().find(|j| j.subs.iter().any(|s| s.ticket == ticket)).map(|j| j.id.as_str())
    }

    // ---- ここから中 ----

    fn send<T: serde::Serialize>(&mut self, msg: &T) {
        self.actions.push_back(Action::SendText(wire::msg::encode(msg)));
    }

    fn emit(&mut self, i: usize, f: impl Fn(Ticket) -> Event) {
        for s in &self.jobs[i].subs {
            self.actions.push_back(Action::Emit(f(s.ticket)));
        }
    }

    /// 中身つきの知らせを全員に出す。写すのは 2 人目からだけ（ふつうは 1 人なので写さない）。
    fn emit_with<T: Clone>(&mut self, i: usize, payload: T, f: impl Fn(Ticket, T) -> Event) {
        let subs = &self.jobs[i].subs;
        let mut payload = Some(payload);
        for (n, s) in subs.iter().enumerate() {
            let p = if n + 1 == subs.len() { payload.take().unwrap() } else { payload.clone().unwrap() };
            self.actions.push_back(Action::Emit(f(s.ticket, p)));
        }
    }

    /// ジョブを片づけて、待っている依頼の全部に終わりを知らせる。
    fn finish(&mut self, i: usize, outcome: Outcome) {
        let j = self.jobs.remove(i);
        for s in &j.subs {
            self.actions.push_back(Action::Emit(Event::Done { ticket: s.ticket, outcome: outcome.clone() }));
        }
        self.check_idle = true;
    }

    fn finish_lsp(&mut self, i: usize, outcome: LspOutcome) {
        let w = self.lsps.remove(i);
        self.actions.push_back(Action::Emit(Event::LspDone { ticket: w.ticket, outcome }));
        self.check_idle = true;
    }

    /// 入力 1 つの終わりに呼ぶ: 送れるものを送り、空になっていたら知らせる。
    fn settle(&mut self) {
        self.pump();
        if std::mem::take(&mut self.check_idle) && self.jobs.is_empty() && self.lsps.is_empty() {
            self.actions.push_back(Action::Idle);
        }
    }

    fn reset_link(&mut self) {
        self.link = Link::Down;
        self.hello_rx = false;
        self.bad_version = false;
        self.cur = None;
        self.ready.clear();
    }

    /// ジョブを取り消す。その場で消えたら `true`。
    fn cancel_job(&mut self, now_us: u64, i: usize) -> bool {
        match self.jobs[i].st {
            St::Pending => {
                self.finish(i, CANCELLED);
                true
            }
            St::Attach => {
                // 受付では走っている。つながったら cancel を送る（依頼した側は待たせない）。
                if self.orphans.len() == ORPHAN_KEEP {
                    self.orphans.remove(0);
                }
                self.orphans.push(self.jobs[i].id.clone());
                self.finish(i, CANCELLED);
                true
            }
            St::Live => {
                let job = self.jobs[i].id.clone();
                self.send(&ClientMsg::Cancel { job });
                self.jobs[i].st = St::Cancelling;
                self.jobs[i].cancel_deadline = now_us + CANCEL_WAIT_US;
                false
            }
            St::Cancelling => false,
        }
    }

    /// 接続が無くなった（切れた・自分で作り直す）。`count` なら失敗に数える。
    /// `force` なら、ジョブが残っていなくてもつなぎ直させる（接続の中身が壊れているとき）。
    fn link_lost(&mut self, count: bool, force: bool) {
        self.reset_link();
        let err = self.conn_err.take();
        let gave_up = |err: &Option<(ErrorCode, String)>| match err {
            Some((code, message)) => Outcome::Rejected { code: *code, message: message.clone() },
            None => Outcome::GaveUp,
        };
        // 送ってあった LSP の問い合わせは、やり直さない（温まっていた接続が死んでいただけなら送り直す）
        let mut i = 0;
        while i < self.lsps.len() {
            let w = &mut self.lsps[i];
            let stale = std::mem::take(&mut w.on_warm);
            if w.sent && count && !stale {
                let out = match &err {
                    Some((code, message)) => LspOutcome::Failed { code: *code, message: message.clone() },
                    None => LspOutcome::GaveUp,
                };
                self.finish_lsp(i, out);
                continue;
            }
            w.sent = false;
            i += 1;
        }
        let mut i = 0;
        while i < self.jobs.len() {
            let j = &mut self.jobs[i];
            if j.st == St::Cancelling {
                self.finish(i, CANCELLED);
                continue;
            }
            // 温まっていた接続に出して、何も返ってこないまま切れた = もともと死んでいた。
            let stale = j.on_warm && !j.heard && !j.accepted;
            j.on_warm = false;
            if count && !stale {
                if j.kind == Kind::Check {
                    self.finish(i, gave_up(&err));
                    continue;
                }
                j.fails += 1;
                if j.fails > MAX_RETRIES {
                    self.finish(i, gave_up(&err));
                    continue;
                }
            }
            j.st = if j.accepted { St::Attach } else { St::Pending };
            i += 1;
        }
        if force || !self.jobs.is_empty() || !self.lsps.is_empty() {
            let worst = self.jobs.iter().map(|j| j.fails).max().unwrap_or(0);
            let after_ms = if worst == 0 { 0 } else { BACKOFF_MS << (worst - 1) };
            self.link = Link::Dialing;
            self.actions.push_back(Action::Reconnect { after_ms });
        }
    }

    /// 目録からやり直す（unknown-job・lost・bad-tree）。回数を使い切った・check なら `fallback` で終わる。
    fn restart(&mut self, i: usize, fallback: Outcome) {
        let j = &mut self.jobs[i];
        j.restarts += 1;
        if j.kind == Kind::Check || j.restarts > MAX_RETRIES {
            self.finish(i, fallback);
            return;
        }
        j.st = St::Pending;
        j.accepted = false;
        j.running = false;
        j.log_rx = 0;
        j.art = None;
    }

    fn is_ready(&self, project: &str, tree: &str) -> bool {
        self.ready.iter().any(|(p, t)| p == project && t == tree)
    }

    /// 送れるものを送る。
    fn pump(&mut self) {
        if self.link != Link::Up {
            return;
        }
        if self.bad_version {
            while !self.lsps.is_empty() {
                self.finish_lsp(0, LspOutcome::Failed { code: ErrorCode::Version, message: "約束の版が合わない".to_owned() });
            }
            while !self.jobs.is_empty() {
                let out = if self.jobs[0].st == St::Cancelling { CANCELLED } else { Outcome::Rejected { code: ErrorCode::Version, message: "約束の版が合わない".to_owned() } };
                self.finish(0, out);
            }
            return;
        }
        for i in 0..self.jobs.len() {
            let j = &self.jobs[i];
            match j.st {
                St::Attach => {
                    let msg = ClientMsg::Attach { job: j.id.clone(), log: j.log_rx, artifact: j.art.map_or(0, |(_, got)| got) };
                    self.send(&msg);
                    self.jobs[i].st = St::Live;
                }
                St::Pending => {
                    // 同じ job id の前のもの（取り消し中）の done を待つ。
                    if self.jobs.iter().any(|o| o.st == St::Cancelling && o.id == j.id) {
                        continue;
                    }
                    if self.is_ready(&j.project, &j.tree) {
                        let msg = ClientMsg::Job { job: j.id.clone(), project: j.project.clone(), tree: j.tree.clone(), kind: j.kind, lang: j.lang, opts: j.opts.clone(), log: j.subs.iter().any(|s| s.want_log) };
                        self.send(&msg);
                        self.jobs[i].st = St::Live;
                    }
                }
                St::Live | St::Cancelling => {}
            }
        }
        // LSP: 木がそろっていれば送る
        let warm = self.hello_rx;
        let mut i = 0;
        while i < self.lsps.len() {
            let w = &self.lsps[i];
            if w.sent || !self.is_ready(&w.req.project, &w.tree) {
                i += 1;
                continue;
            }
            let r = &w.req;
            let msg = match r.query {
                Some(query) => ClientMsg::Lsp { id: w.ticket.0, project: r.project.clone(), tree: w.tree.clone(), query, path: r.path.clone(), line: r.line, col: r.col, text: r.text.clone() },
                None => ClientMsg::LspWarm { project: r.project.clone(), tree: w.tree.clone(), path: r.path.clone() },
            };
            let is_warm = r.query.is_none();
            self.send(&msg);
            if is_warm {
                // 答えは状態（lsp_state）で届く。ここで終わり
                self.finish_lsp(i, LspOutcome::Answer { text: String::new(), items: Vec::new() });
                continue;
            }
            let now = self.now;
            let w = &mut self.lsps[i];
            w.sent = true;
            w.on_warm = warm;
            w.deadline = now + LSP_WAIT_US;
            i += 1;
        }
        // 木の確認は、LSP の問い合わせを先に（打鍵の返事なので）
        if self.cur.is_none() {
            let next = self.lsps.iter().find(|w| !w.sent && !self.is_ready(&w.req.project, &w.tree)).map(|w| (w.req.project.clone(), w.tree.clone(), w.req.manifest.clone()));
            if let Some((project, tree, manifest)) = next {
                let sync = Sync { project, tree, manifest, phase: Phase::Sent };
                self.send(&ManifestMsg { ty: "manifest", project: &sync.project, tree: &sync.tree, files: sync.manifest.files() });
                self.cur = Some(sync);
            }
        }
        if self.cur.is_none() {
            let next = self.jobs.iter().find(|j| j.st == St::Pending && !self.is_ready(&j.project, &j.tree) && !self.jobs.iter().any(|o| o.st == St::Cancelling && o.id == j.id));
            if let Some(j) = next {
                let sync = Sync { project: j.project.clone(), tree: j.tree.clone(), manifest: j.manifest.clone(), phase: Phase::Sent };
                self.send(&ManifestMsg { ty: "manifest", project: &sync.project, tree: &sync.tree, files: sync.manifest.files() });
                self.cur = Some(sync);
            }
        }
    }

    /// 今の木を待っている依頼に、送った量を知らせる。
    fn emit_upload(&mut self, done: u64, total: u64) {
        let Some(cur) = &self.cur else { return };
        for j in &self.jobs {
            if j.st == St::Pending && j.project == cur.project && j.tree == cur.tree {
                for s in &j.subs {
                    self.actions.push_back(Action::Emit(Event::Uploading { ticket: s.ticket, done, total }));
                }
            }
        }
    }

    /// 次の中身を頼む。
    fn push_blob(&mut self) {
        let Some(cur) = &self.cur else { return };
        let Phase::Upload { need, next, .. } = &cur.phase else { return };
        let f = &cur.manifest.files()[need[*next] as usize];
        self.actions.push_back(Action::SendBlob { id: *next as u32, manifest: cur.manifest.clone(), path: f.path.clone(), sha256: f.sha256.clone(), size: f.size });
    }

    fn on_need(&mut self, tree: String, blobs: Vec<String>) {
        let Some(cur) = &mut self.cur else { return };
        if cur.tree != tree || !matches!(cur.phase, Phase::Sent) {
            return;
        }
        if blobs.is_empty() {
            cur.phase = Phase::WaitReady;
            return;
        }
        let manifest = cur.manifest.clone();
        let files = manifest.files();
        // 少なければ端から探す。多ければ表を作る（2000 × 2000 の比較を避ける）。
        let table: Option<HashMap<&str, u32>> = (blobs.len() > 8).then(|| files.iter().enumerate().rev().map(|(i, f)| (f.sha256.as_str(), i as u32)).collect());
        let mut need = Vec::with_capacity(blobs.len());
        let mut total = 0;
        for b in &blobs {
            let found = match &table {
                Some(t) => t.get(b.as_str()).copied(),
                None => files.iter().position(|f| f.sha256 == *b).map(|i| i as u32),
            };
            let Some(i) = found else {
                // 目録に無い中身を求められた。受付と話が合っていないので、つなぎ直す。
                self.link_lost(true, true);
                return;
            };
            total += files[i as usize].size;
            need.push(i);
        }
        cur.phase = Phase::Upload { need, next: 0, done: 0, total };
        self.emit_upload(0, total);
        self.push_blob();
    }

    /// ジョブあての知らせの届け先（今の接続に出してあるもの）。
    fn live(&self, job: &str) -> Option<usize> {
        self.jobs.iter().position(|j| j.id == job && matches!(j.st, St::Live | St::Cancelling))
    }

    /// `Live` のジョブ（取り消し中のものの知らせは捨てる）。届いたことも記す。
    fn heard(&mut self, job: &str) -> Option<usize> {
        let i = self.live(job)?;
        let j = &mut self.jobs[i];
        if j.st != St::Live {
            return None;
        }
        j.heard = true;
        j.fails = 0;
        Some(i)
    }

    fn on_msg(&mut self, now_us: u64, msg: ServerMsg) {
        self.now = now_us;
        match msg {
            ServerMsg::Hello { v, .. } => {
                self.hello_rx = true;
                self.conn_err = None;
                if v != wire::PROTOCOL_VERSION {
                    self.bad_version = true;
                }
            }
            ServerMsg::Need { tree, blobs } => self.on_need(tree, blobs),
            ServerMsg::LspResult { id, error, message, text, items, retry_after_s, .. } => {
                let Some(i) = self.lsps.iter().position(|w| w.sent && w.ticket.0 == id) else { return };
                let out = match error {
                    Some(code) => LspOutcome::Failed { code, message: match retry_after_s { Some(s) => format!("{message}（あと {s} 秒）"), None => message } },
                    None => LspOutcome::Answer { text, items },
                };
                self.finish_lsp(i, out);
            }
            ServerMsg::LspState { project, state, message, error, .. } => {
                self.actions.push_back(Action::Emit(Event::LspState { project, state, message, error }));
            }
            ServerMsg::Ready { tree } => {
                if let Some(cur) = self.cur.take_if(|c| c.tree == tree) {
                    if self.ready.len() == READY_KEEP {
                        self.ready.remove(0);
                    }
                    self.ready.push((cur.project, cur.tree));
                }
            }
            ServerMsg::Accepted { job, cached, queued } => {
                if let Some(i) = self.live(&job) {
                    if self.jobs[i].st == St::Live {
                        self.jobs[i].accepted = true;
                        if !cached {
                            self.emit(i, |ticket| Event::Queued { ticket, ahead: queued });
                        }
                    }
                }
            }
            ServerMsg::Started { job } => {
                if let Some(i) = self.heard(&job) {
                    self.jobs[i].running = true;
                    self.emit(i, |ticket| Event::Running { ticket });
                }
            }
            ServerMsg::Log { job, offset, text } => {
                let Some(i) = self.heard(&job) else { return };
                let j = &mut self.jobs[i];
                let end = offset + text.len() as u64;
                // 受け取り済みの分を捨てる（つなぎ直しの直後に重なりうる）。位置が先へ飛んで
                // いたら、受付が先頭を捨てたということなので、そのまま受け取る。
                let mut skip = j.log_rx.saturating_sub(offset).min(text.len() as u64) as usize;
                while !text.is_char_boundary(skip) {
                    skip += 1;
                }
                j.log_rx = j.log_rx.max(end);
                if skip == text.len() {
                    return;
                }
                let mut text = text;
                if skip > 0 {
                    text.drain(..skip);
                }
                let mut want = j.subs.iter().filter(|s| s.want_log).map(|s| s.ticket).peekable();
                while let Some(ticket) = want.next() {
                    let t = if want.peek().is_some() { text.clone() } else { std::mem::take(&mut text) };
                    self.actions.push_back(Action::Emit(Event::Log { ticket, text: t }));
                }
            }
            ServerMsg::Diagnostics { job, complete, files, .. } => {
                if let Some(i) = self.heard(&job) {
                    self.emit_with(i, files, |ticket, files| Event::Diagnostics { ticket, complete, files });
                }
            }
            ServerMsg::Tests { job, passed, failed, ignored, cases } => {
                if let Some(i) = self.heard(&job) {
                    self.emit_with(i, cases, |ticket, cases| Event::Tests { ticket, passed, failed, ignored, cases });
                }
            }
            ServerMsg::File { job, path, sha256, text } => {
                if let Some(i) = self.heard(&job) {
                    self.emit_with(i, (path, sha256, text), |ticket, (path, sha256, text)| Event::WriteBack { ticket, path, sha256, text });
                }
            }
            ServerMsg::Artifact { job, chan, kind, name, sha256, bytes, zbytes, offset } => {
                if let Some(i) = self.heard(&job) {
                    // 受付の言う位置が正（attach で頼んだ位置と違えば、そこから受け直す）。
                    self.jobs[i].art = Some((chan, offset));
                    self.emit_with(i, (name, sha256), |ticket, (name, sha256)| Event::ArtifactBegin { ticket, kind, name, sha256, bytes, zbytes, offset });
                }
            }
            ServerMsg::Done { job, status, exit, queue_ms, run_ms } => {
                let Some(i) = self.live(&job) else { return };
                let out = Outcome::Finished { status, exit, queue_ms, run_ms };
                if self.jobs[i].st == St::Cancelling {
                    // 取り消しと行き違いで終わっていても、結果は渡していないので「取り消し」。
                    self.finish(i, Outcome::Finished { status: Status::Cancelled, exit, queue_ms, run_ms });
                } else if status == Status::Lost {
                    self.restart(i, out);
                } else {
                    self.finish(i, out);
                }
            }
            ServerMsg::Error { job: Some(job), code, message, .. } => {
                let Some(i) = self.live(&job) else { return };
                if self.jobs[i].st == St::Cancelling {
                    self.finish(i, CANCELLED);
                    return;
                }
                let out = Outcome::Rejected { code, message };
                match code {
                    ErrorCode::UnknownJob => self.restart(i, out),
                    ErrorCode::BadTree => {
                        let (p, t) = (self.jobs[i].project.clone(), self.jobs[i].tree.clone());
                        self.ready.retain(|(rp, rt)| *rp != p || *rt != t);
                        self.restart(i, out);
                    }
                    _ => self.finish(i, out),
                }
            }
            ServerMsg::Error { job: None, code, message, retry_after_s } => {
                let message = match retry_after_s {
                    Some(s) => format!("{message}（あと {s} 秒）"),
                    None => message,
                };
                match code {
                    ErrorCode::Version => self.bad_version = true,
                    // 起こせない・中の失敗は、切れたあとのやり直しに任せる（理由だけ覚える）。
                    ErrorCode::Unavailable | ErrorCode::Internal => self.conn_err = Some((code, message)),
                    // 送っている木の話。その木を待っている依頼だけ断る。
                    ErrorCode::TooLarge | ErrorCode::BadTree | ErrorCode::BadBlob if self.cur.is_some() => {
                        let cur = self.cur.take().unwrap();
                        let mut i = 0;
                        while i < self.lsps.len() {
                            let w = &self.lsps[i];
                            if !w.sent && w.req.project == cur.project && w.tree == cur.tree {
                                self.finish_lsp(i, LspOutcome::Failed { code, message: message.clone() });
                            } else {
                                i += 1;
                            }
                        }
                        let mut i = 0;
                        while i < self.jobs.len() {
                            let j = &self.jobs[i];
                            if j.st == St::Pending && j.project == cur.project && j.tree == cur.tree {
                                self.finish(i, Outcome::Rejected { code, message: message.clone() });
                            } else {
                                i += 1;
                            }
                        }
                    }
                    // 上限・約束の間違いは、やり直しても同じ。
                    _ => {
                        while !self.lsps.is_empty() {
                            self.finish_lsp(0, LspOutcome::Failed { code, message: message.clone() });
                        }
                        while !self.jobs.is_empty() {
                            let out = if self.jobs[0].st == St::Cancelling { CANCELLED } else { Outcome::Rejected { code, message: message.clone() } };
                            self.finish(0, out);
                        }
                    }
                }
            }
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

// 受け入れ試験（課題 D2）は tests/session.rs（台本の形: 入力の列 → 期待する出力の列）。
