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
//! - 道具の出力を読むスレッドは、見る人が遅くても止まらない
//!
//! # 中の作り
//!
//! **見る人ごとの列は持たない。** ログも結果も全部ジョブの側に残っているので、見る人は
//! 「どこまで受け取ったか」の位置（ログの位置・結果の何番目か・成果物の位置）だけを持ち、
//! [`Watcher::recv`] のたびにジョブから続きを読む。だから道具の出力を読むスレッドは、
//! 見る人の数にも速さにも関係なく、ジョブのログに足すだけで済む。見る人が遅れすぎて、
//! 続きがログの上限からこぼれたら、その見る人を外す（[`WatchEnd::Lagged`]。相手は
//! つなぎ直して `attach` すれば、「省略」の印つきで続きを受け取れる）。
//!
//! 流す順（1 人の見る人から見て）:
//!
//! ```text
//!   accepted（submit のときだけ）→ started（実行したとき）→ log… →
//!   diagnostics → tests → file… → artifact + 成果物の中身 → done
//! ```
//!
//! - `attach` には `accepted` を出さない。走り始めていれば `started` から出す。診断・試験・
//!   書き戻すファイルは出し直す（受け取り済みかを相手が言う項目が無い。2 回届いても同じ中身）
//! - 残っていた結果を流すとき（`cached`）は `started` を出さない（今回は実行していない）
//! - 取り消し・時間切れ・受付の失敗でも、`diagnostics {complete:false}` は必ず 1 回出す。
//!   受付の失敗の理由は、ログの最後の 1 行と、位置なし（パスが空）の診断 1 件で伝える
//! - 取り消しを頼んだあとで道具がふつうに終わっても、結果は「取り消し」にする
//!   （頼んだ側が、結果を待っていない）
//! - 取り消し中（まだ道具が死んでいない）のジョブと同じ id の依頼が来たら、新しいジョブを
//!   作って待たせる（保存 → 取り消し → 中身を変えずにもう一度保存、で起きる）。古い方が
//!   終わるまで走らせない
//! - 終わったジョブは、結果を使い回さないもの（取り消し・時間切れ・失敗）も
//!   [`crate::Config::keep_done`] のあいだ残す。切れていた相手が `attach` したときに、
//!   「知らないジョブ」ではなく本当の終わり方を返すため
//!
//! スレッド: ジョブごとに 1 本（[`Runner::run`] を呼ぶ）と、台帳に見張りが 1 本
//! （時間切れ・SIGTERM の 2 秒後の SIGKILL・古い結果の片づけ）。
//!
//! 置き場所は [`crate::Config::data_dir`] の下の決まった名前（`proj/<project id>/src`・
//! `out/<job id>`）を自分で組み立てる。`out/` は台帳の持ち物で、起動のたびに空にする
//! （前の受付の結果は、台帳ごと消えているので使えない）。

use crate::pack::Packed;
use crate::runner::{JobCtx, JobFailure, JobOutput, JobResult, Runner};
use crate::store::Store;
use crate::Config;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use zc_wire::limits::{MAX_DIAGNOSTICS, MAX_LOG_BYTES, MAX_WRITEBACK_BYTES};
use zc_wire::msg::{Diag, FileDiags, JobOpts, Kind, Lang, ServerMsg, Severity, Status};

/// SIGTERM から SIGKILL までの待ち。
pub const KILL_GRACE: Duration = Duration::from_secs(2);
/// ログをまとめる大きさ（これだけたまったら、待たずに渡す）。
pub const LOG_BATCH_BYTES: usize = 4096;
/// ログをまとめる時間（少ししかたまっていなくても、これだけ待ったら渡す）。
pub const LOG_BATCH_WAIT: Duration = Duration::from_millis(50);
/// `log` メッセージ 1 つに入れる量の上限。
pub const LOG_MSG_BYTES: usize = 32 << 10;
/// ログの先頭を捨てたときに入れる 1 行。
pub const LOG_OMITTED: &str = "…（省略）…\n";
/// 成果物の流れの番号（1 ジョブに成果物は 1 つ）。
pub const ARTIFACT_CHAN: u32 = 1;

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

/// 実行の直前に、ジョブのスレッドで 1 回呼ぶ準備（作業の木への写し）。失敗の理由は人に見せる
/// 1 行。同じプロジェクトのジョブが走っていない時に呼ばれる（[`Registry::submit_prepared`]）。
pub type Prepare = Box<dyn FnOnce() -> Result<(), String> + Send>;

/// 見る人（接続）の受け口。[`Registry::submit`] / [`Registry::attach`] が返す。捨てると外れる。
pub struct Watcher {
    job: Arc<Job>,
    cur: Mutex<Cursor>,
}

/// 見る人が「どこまで受け取ったか」。
struct Cursor {
    /// まだ渡していない `accepted`。
    accepted: Option<ServerMsg>,
    /// `started` を渡したか（渡さない場合も真にしておく）。
    started: bool,
    want_log: bool,
    log_pos: u64,
    /// ログを 1 回でも渡したか（遅れて外すかどうかの判定に使う）。
    got_log: bool,
    /// まだ渡していないログに、最初に気づいた時刻（まとめる時間の起点）。
    pending_since: Option<Instant>,
    /// 終わりの知らせ（[`JobState::finals`]）の何番目まで渡したか。
    final_idx: usize,
    /// 成果物の `artifact` メッセージを渡したか（次は中身）。
    art_announced: bool,
    art_off: u64,
    ended: Option<WatchEnd>,
}

impl Watcher {
    fn new(job: Arc<Job>, accepted: Option<ServerMsg>, started: bool, want_log: bool, log_pos: u64, art_off: u64) -> Watcher {
        Watcher {
            job,
            cur: Mutex::new(Cursor { accepted, started, want_log, log_pos, got_log: false, pending_since: None, final_idx: 0, art_announced: false, art_off, ended: None }),
        }
    }

    /// 見ているジョブの id。
    pub fn job(&self) -> &str {
        &self.job.spec.job
    }

    /// 次に渡すものを待つ。`timeout` までに無ければ `Ok(None)`。ジョブが終わって渡すものが
    /// 尽きたら `Err(Finished)`。受け渡しが遅くて外されたら `Err(Lagged)`。
    pub fn recv(&self, timeout: std::time::Duration) -> Result<Option<Feed>, WatchEnd> {
        let mut c = lock(&self.cur);
        if let Some(end) = c.ended {
            return Err(end);
        }
        let deadline = Instant::now() + timeout;
        let id = &self.job.spec.job;
        let mut js = lock(&self.job.m);
        loop {
            if let Some(m) = c.accepted.take() {
                return Ok(Some(Feed::Msg(m)));
            }
            if !c.started && js.started.is_some() {
                c.started = true;
                return Ok(Some(Feed::Msg(ServerMsg::Started { job: id.clone() })));
            }
            let now = Instant::now();
            let mut wake_at = deadline;
            if c.want_log {
                let log = &js.log;
                if c.log_pos < log.base {
                    if c.got_log {
                        // 見ている途中で、続きがログの上限からこぼれた。
                        c.ended = Some(WatchEnd::Lagged);
                        return Err(WatchEnd::Lagged);
                    }
                    // 付いた時点で、もう先頭が捨てられていた。「省略」の 1 行を、捨てた分の
                    // 最後の位置に置く（offset が、捨てた分も数えたままになるように）。
                    let chunk = log.chunk(log.base, LOG_MSG_BYTES);
                    let (offset, text) = if log.base - c.log_pos >= LOG_OMITTED.len() as u64 {
                        (log.base - LOG_OMITTED.len() as u64, format!("{LOG_OMITTED}{chunk}"))
                    } else {
                        (log.base, chunk.to_string())
                    };
                    c.log_pos = log.base + chunk.len() as u64;
                    c.got_log = true;
                    c.pending_since = None;
                    return Ok(Some(Feed::Msg(ServerMsg::Log { job: id.clone(), offset, text })));
                }
                let avail = log.end() - c.log_pos;
                if avail > 0 {
                    let since = *c.pending_since.get_or_insert(now);
                    if avail >= LOG_BATCH_BYTES as u64 || js.phase == Phase::Done || now >= since + LOG_BATCH_WAIT {
                        let chunk = log.chunk(c.log_pos, LOG_MSG_BYTES);
                        let msg = ServerMsg::Log { job: id.clone(), offset: c.log_pos, text: chunk.to_string() };
                        c.log_pos += chunk.len() as u64;
                        c.got_log = true;
                        c.pending_since = None;
                        return Ok(Some(Feed::Msg(msg)));
                    }
                    wake_at = wake_at.min(since + LOG_BATCH_WAIT);
                }
            }
            if js.phase == Phase::Done {
                let Some(f) = js.finals.get(c.final_idx) else {
                    c.ended = Some(WatchEnd::Finished);
                    return Err(WatchEnd::Finished);
                };
                match f {
                    Final::Msg(m) => {
                        c.final_idx += 1;
                        return Ok(Some(Feed::Msg(m.clone())));
                    }
                    Final::Artifact(p) => {
                        let offset = c.art_off.min(p.zbytes);
                        if !c.art_announced {
                            c.art_announced = true;
                            return Ok(Some(Feed::Msg(ServerMsg::Artifact {
                                job: id.clone(),
                                chan: ARTIFACT_CHAN,
                                kind: p.kind,
                                name: p.name.clone(),
                                sha256: p.sha256.clone(),
                                bytes: p.bytes,
                                zbytes: p.zbytes,
                                offset,
                            })));
                        }
                        c.final_idx += 1;
                        return Ok(Some(Feed::ArtifactFile { chan: ARTIFACT_CHAN, path: p.zpath.clone(), offset, zbytes: p.zbytes }));
                    }
                }
            }
            if now >= deadline {
                return Ok(None);
            }
            js = self.job.cv.wait_timeout(js, wake_at.min(deadline) - now).unwrap_or_else(|e| e.into_inner()).0;
        }
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
    ///
    /// 台帳が確かめるのは形（job は 16 進 32 桁、project は 16 桁、tree は 64 桁）だけ。
    /// id はフォルダの名前になるので、形の違うものは通さない。計算のし直しは、木を知っている
    /// 接続の側（[`crate::server`]）がする。
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Queued,
    Running,
    Done,
}

/// 止める理由。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Cancel,
    Timeout,
}

/// 終わるときに 1 回ずつ流すもの。
enum Final {
    Msg(ServerMsg),
    /// `artifact` メッセージと中身（見る人ごとに始まりの位置が違う）。
    Artifact(Packed),
}

/// 残してあるログ。`base` は `buf` の先頭の、ログ全体の中での位置。
struct LogBuf {
    base: u64,
    buf: String,
}

impl LogBuf {
    fn end(&self) -> u64 {
        self.base + self.buf.len() as u64
    }

    fn push(&mut self, s: &str) {
        self.buf.push_str(s);
        if self.buf.len() > MAX_LOG_BYTES {
            // 上限の 3/4 まで一度に捨てる（あふれるたびに少しずつずらさない）。切れ目は
            // 文字の境目、近くに改行があればその後ろ。
            let mut cut = self.buf.len() - MAX_LOG_BYTES / 4 * 3;
            while !self.buf.is_char_boundary(cut) {
                cut += 1;
            }
            if let Some(i) = self.buf.as_bytes()[cut..].iter().take(4096).position(|&b| b == b'\n') {
                cut += i + 1;
            }
            self.buf.drain(..cut);
            self.base += cut as u64;
        }
    }

    /// `pos` から `max` バイトまで（文字の途中で切らない）。`pos` は `base..=end` の中。
    fn chunk(&self, pos: u64, max: usize) -> &str {
        let start = (pos - self.base) as usize;
        let mut end = self.buf.len().min(start + max);
        while !self.buf.is_char_boundary(end) {
            end -= 1;
        }
        &self.buf[start..end]
    }
}

struct JobState {
    phase: Phase,
    /// ログを欲しがった依頼があったか（`attach` はこれに従う）。
    want_log: bool,
    log: LogBuf,
    /// 文字の途中で切れた、ログの最後の数バイト（次の出力とつなぐ）。
    utf8_tail: Vec<u8>,
    finals: Vec<Final>,
    status: Option<Status>,
    submitted: Instant,
    started: Option<Instant>,
    finished: Option<Instant>,
    deadline: Option<Instant>,
    /// いま動いている道具のプロセスグループ。
    pgid: Option<i32>,
    stop: Option<Stop>,
    /// SIGKILL を送る時刻（SIGTERM の 2 秒後）。
    kill_at: Option<Instant>,
    /// SIGKILL の段に入った（このあと起動された道具はすぐ SIGKILL）。
    hard: bool,
    prepare: Option<Prepare>,
}

impl JobState {
    fn push_log(&mut self, bytes: &[u8]) {
        let joined;
        let mut rest: &[u8] = if self.utf8_tail.is_empty() {
            bytes
        } else {
            let mut v = std::mem::take(&mut self.utf8_tail);
            v.extend_from_slice(bytes);
            joined = v;
            &joined
        };
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    self.log.push(s);
                    break;
                }
                Err(e) => {
                    let (ok, bad) = rest.split_at(e.valid_up_to());
                    if let Ok(s) = std::str::from_utf8(ok) {
                        self.log.push(s);
                    }
                    match e.error_len() {
                        Some(n) => {
                            self.log.push("\u{FFFD}");
                            rest = &bad[n..];
                        }
                        None => {
                            // 文字の途中で切れている。続きを待つ。
                            self.utf8_tail = bad.to_vec();
                            break;
                        }
                    }
                }
            }
        }
    }

    /// 止める。もう頼んであれば何もしない。
    fn request_stop(&mut self, job: &Job, why: Stop) {
        if self.stop.is_some() || self.phase == Phase::Done {
            return;
        }
        self.stop = Some(why);
        job.cancelled.store(true, Ordering::SeqCst);
        if let Some(pg) = self.pgid {
            signal_group(pg, libc::SIGTERM);
        }
        self.kill_at = Some(Instant::now() + KILL_GRACE);
    }
}

struct Job {
    spec: JobSpec,
    /// 実行する側が見る（[`JobCtx::cancelled`]）。
    cancelled: AtomicBool,
    m: Mutex<JobState>,
    cv: Condvar,
}

impl JobOutput for Job {
    fn log(&self, bytes: &[u8]) {
        let mut js = lock(&self.m);
        if js.phase != Phase::Running {
            return;
        }
        js.push_log(bytes);
        drop(js);
        self.cv.notify_all();
    }

    fn set_process_group(&self, pgid: Option<i32>) {
        let mut js = lock(&self.m);
        js.pgid = pgid.filter(|&pg| pg > 1);
        if let (Some(pg), Some(_)) = (js.pgid, js.stop) {
            // 止めると決めたあとで起動された道具。
            signal_group(pg, if js.hard { libc::SIGKILL } else { libc::SIGTERM });
        }
    }
}

/// 毒の入ったロックもそのまま使う（道具の出力を読むスレッドが落ちても、台帳は止めない）。
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// プロセスグループにシグナルを送る。自分のグループと、1 以下（`kill(-1)` は全部を殺す）には
/// 送らない。
fn signal_group(pgid: i32, sig: libc::c_int) {
    // SAFETY: getpgrp / kill は引数だけで決まる呼び出しで、メモリに触らない。
    unsafe {
        if pgid > 1 && pgid != libc::getpgrp() {
            libc::kill(-pgid, sig);
        }
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn sha256_hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for b in Sha256::digest(data) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn ms(d: Duration) -> u32 {
    d.as_millis().min(u32::MAX as u128) as u32
}

/// 結果を使い回せる終わり方か（成功と、コンパイルエラー・試験の失敗）。
fn reusable(status: Option<Status>) -> bool {
    matches!(status, Some(Status::Ok | Status::Failed))
}

struct State {
    /// id → いちばん新しいジョブ（待ち・実行中・終わって残してあるもの）。
    jobs: HashMap<String, Arc<Job>>,
    /// 待ち（届いた順）。
    queue: Vec<Arc<Job>>,
    running: Vec<Arc<Job>>,
    /// 終わって残してあるもの（古い方が前）。
    done: VecDeque<Arc<Job>>,
    /// 止めている途中（新しい依頼を受けない）。
    closing: bool,
}

struct Inner {
    cfg: Config,
    runner: Arc<dyn Runner>,
    state: Mutex<State>,
    /// 見張りと [`Registry::shutdown`] を起こす。
    wake: Condvar,
    stop: AtomicBool,
    created: Instant,
    store: Mutex<Option<Arc<Store>>>,
}

/// 台帳。受付に 1 つ。
pub struct Registry {
    inner: Arc<Inner>,
}

impl Registry {
    /// 台帳を作る。`runner` はジョブの実行の口（試験では偽物を渡す）。
    pub fn new(cfg: &Config, runner: Arc<dyn Runner>) -> Arc<Registry> {
        // 前の受付の成果物は、台帳ごと消えているので使えない。
        let _ = std::fs::remove_dir_all(cfg.data_dir.join("out"));
        let inner = Arc::new(Inner {
            cfg: cfg.clone(),
            runner,
            state: Mutex::new(State { jobs: HashMap::new(), queue: Vec::new(), running: Vec::new(), done: VecDeque::new(), closing: false }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            created: Instant::now(),
            store: Mutex::new(None),
        });
        let sup = inner.clone();
        // 見張りが作れなくても受付は動く（時間切れと SIGKILL が効かなくなるだけ）。
        let _ = std::thread::Builder::new().name("zc-jobs".into()).spawn(move || sup.supervise());
        Arc::new(Registry { inner })
    }

    /// ジョブを頼む（同じ id のジョブがあればそれに付く）。`want_log` が偽なら、見る人に
    /// ログを渡さない（台帳には残す）。
    pub fn submit(&self, spec: JobSpec, want_log: bool) -> Result<Watcher, SubmitError> {
        self.submit_prepared(spec, want_log, Box::new(|| Ok(())))
    }

    /// [`Registry::submit`] と同じ。新しく走らせるときは、実行の直前に `prepare` を
    /// ジョブのスレッドで 1 回呼ぶ（作業の木への写し。同じプロジェクトのジョブが
    /// 走っていない時に呼ぶので、道具が読んでいる最中の木を書き換えない）。もうあるジョブに
    /// 付いたときは呼ばない。
    pub fn submit_prepared(&self, spec: JobSpec, want_log: bool, prepare: Prepare) -> Result<Watcher, SubmitError> {
        if !is_hex(&spec.job, 32) || !is_hex(&spec.project, 16) || !is_hex(&spec.tree, 64) {
            return Err(SubmitError::BadJobId);
        }
        let inner = &self.inner;
        let mut st = lock(&inner.state);
        if st.closing {
            return Err(SubmitError::QueueFull);
        }
        inner.expire(&mut st);
        let id = spec.job.clone();
        if let Some(j) = st.jobs.get(&id).cloned() {
            let mut js = lock(&j.m);
            match js.phase {
                Phase::Queued | Phase::Running if js.stop.is_none() => {
                    js.want_log |= want_log;
                    drop(js);
                    let queued = inner.ahead(&st, &j);
                    let accepted = ServerMsg::Accepted { job: id, cached: false, queued };
                    return Ok(Watcher::new(j, Some(accepted), false, want_log, 0, 0));
                }
                Phase::Done if reusable(js.status) => {
                    js.want_log |= want_log;
                    drop(js);
                    let accepted = ServerMsg::Accepted { job: id, cached: true, queued: 0 };
                    return Ok(Watcher::new(j, Some(accepted), true, want_log, 0, 0));
                }
                // 取り消し中か、使い回さない終わり方。新しく作る（下）。
                _ => {}
            }
        }
        let job = Arc::new(Job {
            spec,
            cancelled: AtomicBool::new(false),
            m: Mutex::new(JobState {
                phase: Phase::Queued,
                want_log,
                log: LogBuf { base: 0, buf: String::new() },
                utf8_tail: Vec::new(),
                finals: Vec::new(),
                status: None,
                submitted: Instant::now(),
                started: None,
                finished: None,
                deadline: None,
                pgid: None,
                stop: None,
                kill_at: None,
                hard: false,
                prepare: Some(prepare),
            }),
            cv: Condvar::new(),
        });
        let prev = st.jobs.insert(id.clone(), job.clone());
        st.queue.push(job.clone());
        Inner::schedule(inner, &mut st);
        let mut queued = 0;
        if st.queue.iter().any(|q| Arc::ptr_eq(q, &job)) {
            if st.queue.len() > inner.cfg.max_queued {
                st.queue.retain(|q| !Arc::ptr_eq(q, &job));
                match prev {
                    Some(p) => st.jobs.insert(id, p),
                    None => st.jobs.remove(&id),
                };
                return Err(SubmitError::QueueFull);
            }
            queued = inner.ahead(&st, &job);
        }
        // 入れ替えた古いジョブ（使い回さない終わり方）は、残してある一覧からも外す。
        if let Some(p) = &prev {
            st.done.retain(|d| !Arc::ptr_eq(d, p));
        }
        Ok(Watcher::new(job, Some(ServerMsg::Accepted { job: id, cached: false, queued }), false, want_log, 0, 0))
    }

    /// 続きを受け取る。`log` / `artifact` は受け取り済みの位置。
    pub fn attach(&self, job: &str, log: u64, artifact: u64) -> Result<Watcher, SubmitError> {
        let inner = &self.inner;
        let mut st = lock(&inner.state);
        inner.expire(&mut st);
        let j = st.jobs.get(job).cloned().ok_or(SubmitError::Unknown)?;
        let js = lock(&j.m);
        let want_log = js.want_log;
        // 今あるログより先を言ってきたら（同じ id でやり直したジョブ）、今の終わりから。
        let log_pos = log.min(js.log.end());
        drop(js);
        Ok(Watcher::new(j, None, false, want_log, log_pos, artifact))
    }

    /// 取り消す。
    pub fn cancel(&self, job: &str) -> Result<(), SubmitError> {
        let inner = &self.inner;
        let mut st = lock(&inner.state);
        let j = st.jobs.get(job).cloned().ok_or(SubmitError::Unknown)?;
        let mut js = lock(&j.m);
        match js.phase {
            Phase::Done => {}
            Phase::Running => {
                js.request_stop(&j, Stop::Cancel);
                drop(js);
                inner.wake.notify_all();
            }
            Phase::Queued => {
                js.stop = Some(Stop::Cancel);
                j.cancelled.store(true, Ordering::SeqCst);
                drop(js);
                st.queue.retain(|q| !Arc::ptr_eq(q, &j));
                inner.complete(&mut st, &j, Err(JobFailure { status: Status::Cancelled, message: String::new() }));
            }
        }
        Ok(())
    }

    /// 走っている・待っているジョブと、終わって結果を残してあるジョブ（`state` で見分ける）。
    ///
    /// 終わったものも出す理由: Cloudflare 側の見張りは「走っていたはずのジョブがここに無い」を
    /// 「失くした」と読む。相手が切れている間に終わったジョブを、失くしたことにさせない。
    pub fn status(&self) -> Vec<JobInfo> {
        let mut st = lock(&self.inner.state);
        self.inner.expire(&mut st);
        let info = |j: &Arc<Job>, state| JobInfo { job: j.spec.job.clone(), project: j.spec.project.clone(), kind: j.spec.kind, state };
        let mut v: Vec<JobInfo> = st.running.iter().map(|j| info(j, "running")).collect();
        v.extend(self.inner.order(&st).into_iter().map(|j| info(j, "queued")));
        v.extend(st.done.iter().map(|j| info(j, "done")));
        v
    }

    /// 走っているジョブを全部止める（終了時）。待っているジョブは取り消す。道具が死ぬまで
    /// 待つ（SIGTERM → 2 秒後に SIGKILL。そのあと少し待っても終わらなければ諦めて戻る）。
    pub fn shutdown(&self) {
        let inner = &self.inner;
        let mut st = lock(&inner.state);
        st.closing = true;
        for j in std::mem::take(&mut st.queue) {
            lock(&j.m).stop = Some(Stop::Cancel);
            j.cancelled.store(true, Ordering::SeqCst);
            inner.complete(&mut st, &j, Err(JobFailure { status: Status::Cancelled, message: String::new() }));
        }
        for j in &st.running {
            lock(&j.m).request_stop(j, Stop::Cancel);
        }
        inner.wake.notify_all();
        let give_up = Instant::now() + KILL_GRACE + Duration::from_secs(3);
        while !st.running.is_empty() {
            let now = Instant::now();
            if now >= give_up {
                break;
            }
            st = inner.wake.wait_timeout(st, give_up - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// 台帳を作ってからの時間（`/v1/status` の `uptime_s`）。
    pub fn uptime(&self) -> Duration {
        self.inner.created.elapsed()
    }

    /// 中身の置き場（受付に 1 つ。最初に使うときに開く）。
    pub(crate) fn store(&self) -> std::io::Result<Arc<Store>> {
        let mut s = lock(&self.inner.store);
        if let Some(store) = &*s {
            return Ok(store.clone());
        }
        let store = Arc::new(Store::open(&self.inner.cfg.data_dir)?);
        *s = Some(store.clone());
        Ok(store)
    }

    /// 受付の設定。
    pub(crate) fn config(&self) -> &Config {
        &self.inner.cfg
    }

    /// 走っている・待っているジョブがあるか。
    pub(crate) fn busy(&self) -> bool {
        let st = lock(&self.inner.state);
        !st.running.is_empty() || !st.queue.is_empty()
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.shutdown();
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.wake.notify_all();
    }
}

impl Inner {
    fn out_dir(&self, job: &str) -> PathBuf {
        self.cfg.data_dir.join("out").join(job)
    }

    /// 待ちの列を、走らせる順に並べたもの（check が先。あとは届いた順）。
    fn order<'a>(&self, st: &'a State) -> Vec<&'a Arc<Job>> {
        let mut v: Vec<&Arc<Job>> = st.queue.iter().filter(|j| j.spec.kind == Kind::Check).collect();
        v.extend(st.queue.iter().filter(|j| j.spec.kind != Kind::Check));
        v
    }

    /// 待ちの列で、このジョブの前に並んでいる数（走っていれば 0）。
    fn ahead(&self, st: &State, job: &Arc<Job>) -> u32 {
        self.order(st).iter().position(|j| Arc::ptr_eq(j, job)).unwrap_or(0) as u32
    }

    /// 走らせられるだけ走らせる。同じプロジェクトのジョブと、同じ id の古いジョブ
    /// （取り消し中）が走っている間は待たせる。
    fn schedule(this: &Arc<Inner>, st: &mut State) {
        while !st.closing && st.running.len() < this.cfg.max_running.max(1) {
            let next = this
                .order(st)
                .into_iter()
                .find(|j| !st.running.iter().any(|r| r.spec.project == j.spec.project || r.spec.job == j.spec.job))
                .cloned();
            let Some(job) = next else { break };
            st.queue.retain(|q| !Arc::ptr_eq(q, &job));
            {
                let mut js = lock(&job.m);
                let now = Instant::now();
                js.phase = Phase::Running;
                js.started = Some(now);
                js.deadline = Some(now + this.cfg.job_timeout);
            }
            job.cv.notify_all();
            st.running.push(job.clone());
            let (inner, j) = (this.clone(), job.clone());
            let name = format!("zc-job-{}", &job.spec.job[..8]);
            if std::thread::Builder::new().name(name).spawn(move || Inner::run_job(&inner, &j)).is_err() {
                st.running.retain(|r| !Arc::ptr_eq(r, &job));
                this.complete(st, &job, Err(JobFailure { status: Status::Error, message: "スレッドを作れない".into() }));
            }
        }
        this.wake.notify_all();
    }

    /// ジョブのスレッドの中身。
    fn run_job(this: &Arc<Inner>, job: &Arc<Job>) {
        let prepare = lock(&job.m).prepare.take();
        let spec = &job.spec;
        let project_dir = this.cfg.data_dir.join("proj").join(&spec.project);
        let src = project_dir.join("src");
        let out_dir = this.out_dir(&spec.job);
        let work = || -> Result<JobResult, JobFailure> {
            let failed = |message: String| JobFailure { status: Status::Error, message };
            if job.cancelled.load(Ordering::SeqCst) {
                return Err(JobFailure { status: Status::Cancelled, message: String::new() });
            }
            let _ = std::fs::remove_dir_all(&out_dir);
            std::fs::create_dir_all(&out_dir).map_err(|e| failed(format!("{}: {e}", out_dir.display())))?;
            std::fs::create_dir_all(&project_dir).map_err(|e| failed(format!("{}: {e}", project_dir.display())))?;
            if let Some(p) = prepare {
                p().map_err(failed)?;
            }
            if job.cancelled.load(Ordering::SeqCst) {
                return Err(JobFailure { status: Status::Cancelled, message: String::new() });
            }
            let ctx = JobCtx { job: &spec.job, kind: spec.kind, lang: spec.lang, opts: &spec.opts, src: &src, project_dir: &project_dir, out_dir: &out_dir, cancelled: &job.cancelled };
            this.runner.run(&ctx, &**job)
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .unwrap_or_else(|_| Err(JobFailure { status: Status::Error, message: "受付の中で異常終了した".into() }));
        let mut st = lock(&this.state);
        st.running.retain(|r| !Arc::ptr_eq(r, job));
        this.complete(&mut st, job, outcome);
        Inner::schedule(this, &mut st);
    }

    /// ジョブを終わらせる: 終わりの知らせを作って残し、見る人を起こす。待ち・実行中の一覧
    /// からは、呼ぶ側が先に外しておく。
    fn complete(&self, st: &mut State, job: &Arc<Job>, outcome: Result<JobResult, JobFailure>) {
        let id = job.spec.job.clone();
        let mut js = lock(&job.m);
        if js.phase == Phase::Done {
            return;
        }
        let now = Instant::now();
        if let Some(pg) = js.pgid.take() {
            // 実行する側が外し忘れた道具を残さない。
            signal_group(pg, libc::SIGKILL);
        }
        if !js.utf8_tail.is_empty() {
            js.utf8_tail.clear();
            js.log.push("\u{FFFD}");
        }
        let stopped = js.stop;
        let (status, result, why) = match (stopped, outcome) {
            (Some(Stop::Timeout), o) => (Status::Timeout, o.ok(), None),
            (Some(Stop::Cancel), o) => (Status::Cancelled, o.ok(), None),
            (None, Ok(r)) => (if r.exit == 0 { Status::Ok } else { Status::Failed }, Some(r), None),
            (None, Err(f)) => (f.status, None, Some(f.message).filter(|m| !m.is_empty())),
        };
        let finished = matches!(status, Status::Ok | Status::Failed);
        let r = result.unwrap_or_default();
        let mut finals = Vec::new();

        // 診断（必ず 1 回）。上限を超えた分は切る。
        let mut files: Vec<FileDiags> = Vec::new();
        let mut left = MAX_DIAGNOSTICS;
        let mut truncated = false;
        for mut f in r.diagnostics {
            if f.diags.len() > left {
                f.diags.truncate(left);
                truncated = true;
            }
            left -= f.diags.len();
            if !f.diags.is_empty() {
                files.push(f);
            }
        }
        if let Some(message) = why {
            // 受付の失敗の理由: ログの最後の 1 行と、位置なしの診断。
            if js.log.buf.as_bytes().last().is_some_and(|&b| b != b'\n') {
                js.log.push("\n");
            }
            js.log.push(&format!("zc-buildd: {message}\n"));
            files.push(FileDiags { path: String::new(), diags: vec![Diag { line: 0, col: None, end: None, severity: Severity::Error, code: None, message }] });
        }
        finals.push(Final::Msg(ServerMsg::Diagnostics { job: id.clone(), complete: finished && r.complete, truncated, files }));
        if finished {
            if let Some(t) = r.tests {
                finals.push(Final::Msg(ServerMsg::Tests { job: id.clone(), passed: t.passed, failed: t.failed, ignored: t.ignored, cases: t.cases }));
            }
            for (path, text) in r.write_back {
                if text.len() <= MAX_WRITEBACK_BYTES {
                    let sha256 = sha256_hex(text.as_bytes());
                    finals.push(Final::Msg(ServerMsg::File { job: id.clone(), path, sha256, text }));
                }
            }
        }
        let artifact = r.artifact.filter(|_| status == Status::Ok);
        let keeps_files = artifact.is_some();
        if let Some(p) = artifact {
            finals.push(Final::Artifact(p));
        }
        let queue_ms = ms(js.started.unwrap_or(now) - js.submitted);
        let run_ms = js.started.map_or(0, |s| ms(now - s));
        finals.push(Final::Msg(ServerMsg::Done { job: id.clone(), status, exit: r.exit, queue_ms, run_ms }));

        js.finals = finals;
        js.status = Some(status);
        js.finished = Some(now);
        js.kill_at = None;
        js.phase = Phase::Done;
        drop(js);
        job.cv.notify_all();

        // この id のいちばん新しいジョブがこれなら、成果物のフォルダはこのジョブのもの。
        let current = st.jobs.get(&id).is_some_and(|j| Arc::ptr_eq(j, job));
        if current && !keeps_files {
            let _ = std::fs::remove_dir_all(self.out_dir(&id));
        }
        st.done.push_back(job.clone());
        while st.done.len() > self.cfg.keep_done_max {
            if let Some(old) = st.done.pop_front() {
                self.forget(st, &old);
            }
        }
        self.wake.notify_all();
    }

    /// 残してあった結果を捨てる。
    fn forget(&self, st: &mut State, job: &Arc<Job>) {
        let id = &job.spec.job;
        if st.jobs.get(id).is_some_and(|j| Arc::ptr_eq(j, job)) {
            st.jobs.remove(id);
            let _ = std::fs::remove_dir_all(self.out_dir(id));
        }
    }

    /// 残す時間を過ぎた結果を捨てる。
    fn expire(&self, st: &mut State) {
        let now = Instant::now();
        while let Some(old) = st.done.front().cloned() {
            let finished = lock(&old.m).finished.unwrap_or(now);
            if now.duration_since(finished) < self.cfg.keep_done {
                break;
            }
            st.done.pop_front();
            self.forget(st, &old);
        }
    }

    /// 見張りのスレッド: 時間切れ・SIGTERM の 2 秒後の SIGKILL・古い結果の片づけ。
    fn supervise(&self) {
        let mut st = lock(&self.state);
        while !self.stop.load(Ordering::SeqCst) {
            let now = Instant::now();
            let mut next = now + Duration::from_secs(1);
            for j in &st.running {
                let mut js = lock(&j.m);
                if js.stop.is_none() && js.deadline.is_some_and(|d| d <= now) {
                    js.request_stop(j, Stop::Timeout);
                }
                if let Some(k) = js.kill_at {
                    if k <= now {
                        js.hard = true;
                        js.kill_at = None;
                        if let Some(pg) = js.pgid {
                            signal_group(pg, libc::SIGKILL);
                        }
                    } else {
                        next = next.min(k);
                    }
                }
                if let (None, Some(d)) = (js.stop, js.deadline) {
                    next = next.min(d);
                }
            }
            self.expire(&mut st);
            let wait = next.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
            st = self.wake.wait_timeout(st, wait).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

// 受け入れ試験（課題 B1）は tests/jobs.rs と tests/protocol.rs に。実行の口は偽物
// （台本どおりにログ・診断・成果物を出し、合図まで止まる Runner）:
// 同じ id の 2 人が同じ結果を受ける、終わった後の依頼は cached、失敗の結果も cached・
// 取り消しはやり直し、接続を切ってもジョブが続く → attach で続きだけ（ログの位置・成果物の
// 位置）、cancel で道具のプロセスグループが消える（本物の `sleep` で）、時間切れ、
// 同じプロジェクトは直列・別のプロジェクトは並列、check が先、列がいっぱい、ログの上限、
// 遅い見る人が外される、protocol: hello の版・順番の間違い・bad-tree・bad-blob・上限。
