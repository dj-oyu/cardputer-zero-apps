//! `zc-remote`（別プロセス）の相手をする専用スレッド（docs/stage3-design.md §8）。
//!
//! エディタへのつなぎ込み（保存時の check、「試験を実行」、診断の取り込み）は課題 Z3。
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
//!
//! スレッドの中の決まり:
//!
//! - スレッドは eventfd（メインからの依頼の知らせ）とソケットを 1 つの `poll()` で待つ。
//!   ソケットは非ブロッキング。書けない分は溜めて、書けるようになってから書く
//! - つなぐのは依頼（`warm` / `submit`）が来たときだけ。つないだら最初に `Hello` を送り、
//!   返事の版が違えば [`RemoteEvent::Unavailable`]
//! - 切れたら（serve が終わった・死んだ）、走っていた自分の依頼に
//!   [`DoneStatus::Error`] の [`RemoteEvent::Done`] を送る。そのあと、serve を起動せずに
//!   つなぎ直しを試す（0.5 秒から倍々で 8 秒ごと。ほかの相手が起動した serve の診断を
//!   受け取るため）。次の依頼では、要れば serve を起動する
//! - 診断が分けて届いたら（`last = false`）、`last` までまとめて 1 回で送る。`Done` が先に
//!   来たら、そこまでの分を `complete = false` で送る
//! - 終わったジョブについての、まとめて持っていた進み具合は捨てる（`Done` のあとに古い
//!   進み具合を出さない）
//! - 未保存の中身の重ね（[`OverlaySource`]。打鍵が止まったときの check）は、メインでは
//!   文書の写しを取るだけ。バイト列にするのはこのスレッド。合計が
//!   [`zc_remote_ipc::MAX_OVERLAY_BYTES`] を超えたら送らず、[`DoneStatus::NotApplicable`] で返す
//! - LSP（docs/lsp-design.md）: [`RemoteClient::lsp`] の答えは [`RemoteEvent::LspResult`] で必ず
//!   1 回届く（つなげない・切れた・`zc-remote` が無いときも）。rust-analyzer の状態は
//!   [`RemoteEvent::LspState`]。どちらもすぐ送る（間引かない。打鍵 1 回に 1 つ程度）

use crate::app::AppEvent;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use zc_core::rt::{MainSender, Notifier};
use zc_core::LineSource;
use zc_remote_ipc::{
    decode_response, encode_request, ArtifactKind, Backend, ClientKind, DecodeError, DoneStatus, FileDiags, FrameReader, JobKind, JobTag, LspItem, LspKind, LspState, OverlayFile, Phase, RemoteLang,
    ReqId, Request, Response, SubmitFlags, TestReport, IPC_VERSION, MAX_OVERLAY_BYTES, MAX_REQUEST_FRAME,
};

/// メインを起こす最短の間隔（design.md §4.3 のターミナルの間引きと同じ 30fps）。
pub const MIN_WAKE_INTERVAL: Duration = Duration::from_millis(33);
/// まとめて持つログの上限。
pub const LOG_BUFFER_MAX: usize = 64 * 1024;
/// 専用スレッドのスタック。
pub const THREAD_STACK: usize = 256 * 1024;
/// `zc-remote serve` を起動してから、ソケットが現れるのを待つ上限。
pub const SPAWN_WAIT: Duration = Duration::from_secs(1);

/// 切れたあとのつなぎ直しの間隔（最初と上限）。
const RECONNECT_FIRST: Duration = Duration::from_millis(500);
const RECONNECT_MAX: Duration = Duration::from_secs(8);
/// serve を起動したあと、ソケットを試す間隔。
const SPAWN_POLL: Duration = Duration::from_millis(20);
/// 書けずに溜まった依頼の上限（serve が読んでいない。切る）。重ねつきの依頼 2 つぶん。
const OUT_MAX: usize = 2 * MAX_REQUEST_FRAME;
/// ログを捨てて詰めるとき、この範囲に改行があれば行の頭からにする。
const LOG_TRIM_LOOKAHEAD: usize = 256;

/// メインが振る依頼の番号（[`RemoteClient::submit`] が返す）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RemoteReq(pub u32);

/// ジョブの依頼。
#[derive(Clone, Debug)]
pub struct RemoteJob {
    pub kind: JobKind,
    pub lang: RemoteLang,
    pub flags: SubmitFlags,
    /// 保存したファイルの絶対パス。
    pub file: PathBuf,
    /// ディスクの代わりに使う未保存の中身（打鍵が止まったときの check。ふつうは空）。
    pub overlay: Vec<OverlaySource>,
}

impl RemoteJob {
    /// 重ね無しの依頼。
    pub fn new(kind: JobKind, lang: RemoteLang, flags: SubmitFlags, file: PathBuf) -> RemoteJob {
        RemoteJob { kind, lang, flags, file, overlay: Vec::new() }
    }
}

/// LSP の問い合わせ（[`RemoteClient::lsp`]）。位置は 0 始まりの行と、行の中の UTF-8 のバイト位置。
#[derive(Clone, Debug)]
pub struct LspAsk {
    pub kind: LspKind,
    /// 問い合わせる文書の絶対パス。
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
    /// workspace/symbol の検索の文字列（ほかは空）。
    pub text: String,
    /// 未保存の文書（打鍵が止まったときの check と同じ重ね）。
    pub overlay: Vec<OverlaySource>,
}

/// 開いている文書の未保存の中身（[`RemoteJob::overlay`]）。メインは文書の写し
/// （`TextDoc::snapshot_lines`。`zc_buffer` なら O(1)）を渡すだけで、バイト列にするのは
/// 専用スレッド（[`OverlaySource::bytes`]）。
#[derive(Clone)]
pub struct OverlaySource {
    /// 文書の絶対パス。
    pub path: PathBuf,
    lines: Arc<Mutex<Box<dyn LineSource + Send>>>,
}

impl OverlaySource {
    pub fn new(path: PathBuf, lines: Box<dyn LineSource + Send>) -> OverlaySource {
        OverlaySource { path, lines: Arc::new(Mutex::new(lines)) }
    }

    /// 中身（行を `\n` でつなぐ。最後の行が空なら末尾の改行になる）。`limit` バイトを
    /// 超えたら `None`。
    pub fn bytes(&self, limit: usize) -> Option<Vec<u8>> {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        let mut line = String::new();
        let n = lines.len_lines();
        for i in 0..n {
            lines.line(i, &mut line);
            let newline = usize::from(i + 1 < n);
            if out.len() + line.len() + newline > limit {
                return None;
            }
            out.extend_from_slice(line.as_bytes());
            if newline == 1 {
                out.push(b'\n');
            }
        }
        Some(out)
    }
}

impl std::fmt::Debug for OverlaySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OverlaySource({:?})", self.path)
    }
}

/// 重ねをバイト列にする。合計が `limit` を超えたら `None`。
fn overlay_bytes(src: &[OverlaySource], limit: usize) -> Option<Vec<OverlayFile>> {
    let mut left = limit;
    let mut out = Vec::with_capacity(src.len());
    for o in src {
        let bytes = o.bytes(left)?;
        left -= bytes.len();
        out.push(OverlayFile { path: o.path.clone(), bytes });
    }
    Some(out)
}

/// `zc-remote` の設定のファイル（`~/.config/zcode/remote.json`。`XDG_CONFIG_HOME` があれば
/// そちら）があるか。打鍵が止まったときの check は、これが無い人には専用スレッドも serve も
/// 作らない（中身は読まない。読むのは serve）。
pub fn remote_config_exists() -> bool {
    let abs = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
    let Some(base) = abs("XDG_CONFIG_HOME").or_else(|| abs("HOME").map(|h| h.join(".config"))) else { return false };
    base.join("zcode").join("remote.json").is_file()
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
    /// LSP の問い合わせの答え（[`RemoteClient::lsp`] 1 つに必ず 1 回）。`status` が `Ok` でなければ
    /// `message` に理由（「rust-analyzer が準備中」も `Error`）。
    LspResult { req: RemoteReq, status: DoneStatus, message: String, text: String, items: Vec<LspItem> },
    /// rust-analyzer の状態（`root` のプロジェクト）。
    LspState { root: PathBuf, state: LspState, message: String },
}

/// メイン → 専用スレッド。
enum Cmd {
    Warm(PathBuf),
    Submit(RemoteReq, RemoteJob, Instant),
    Lsp(RemoteReq, LspAsk),
    Cancel(RemoteReq),
    Quit,
}

/// 動いている専用スレッドへの口。
struct Handle {
    tx: mpsc::Sender<Cmd>,
    wake: Notifier,
}

/// 専用スレッドの持ち手。メインが持つ。捨てるとソケットを閉じてスレッドを終わらせる。
/// 自分で起動した serve は [`RemoteClient::shutdown`]（捨てるときも呼ぶ）で終わらせる。
///
/// serve は zcode と同じプロセスグループにいる（APPLaunch の Esc 長押しで一緒に終わるように）。
/// その代わり、zcode が先に終わっても serve が残ると、APPLaunch はグループの全員が終わるまで
/// 「loading...」のまま待つ（無通信で serve が自分で終わるまで数分。2026-11-17 実機で起きた）。
pub struct RemoteClient {
    results: MainSender<AppEvent>,
    program: Option<PathBuf>,
    socket: PathBuf,
    next_req: AtomicU32,
    /// 専用スレッドが起動した serve のプロセス番号（動いていなければ 0）。
    serve_pid: Arc<AtomicI32>,
    /// 最初の依頼で作る。`None` はスレッドを作れなかった。
    thread: OnceLock<Option<Handle>>,
}

impl RemoteClient {
    /// 持ち手を作る。スレッドはまだ作らない（最初の依頼で作る）。`program` は `zc-remote` の
    /// 実行ファイル（`None` なら `PATH` から探す）。
    pub fn new(results: MainSender<AppEvent>, program: Option<PathBuf>) -> RemoteClient {
        RemoteClient::with_socket(results, program, zc_remote_ipc::socket_path())
    }

    /// ソケットの場所を指定して作る（試験用）。起動する serve には、`socket` が
    /// `<dir>/zcode/remote.sock` の形なら `XDG_RUNTIME_DIR=<dir>` を渡す。
    pub fn with_socket(results: MainSender<AppEvent>, program: Option<PathBuf>, socket: PathBuf) -> RemoteClient {
        RemoteClient { results, program, socket, next_req: AtomicU32::new(1), serve_pid: Arc::new(AtomicI32::new(0)), thread: OnceLock::new() }
    }

    /// 自分が起動した serve のプロセス番号（試験用）。
    pub fn serve_pid(&self) -> Option<u32> {
        match self.serve_pid.load(Ordering::SeqCst) {
            0 => None,
            p => Some(p as u32),
        }
    }

    /// 終わる: スレッドを止め、自分が起動した serve に SIGTERM を送る（serve は控えを書いて
    /// すぐ終わる）。ほかが起動した serve には触らない。何度呼んでもよい。待たない。
    pub fn shutdown(&self) {
        if let Some(Some(h)) = self.thread.get() {
            let _ = h.tx.send(Cmd::Quit);
            h.wake.notify();
        }
        let pid = self.serve_pid.swap(0, Ordering::SeqCst);
        if pid > 0 {
            // SAFETY: 自分の子（まだ回収していない）にシグナルを送るだけ。
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }

    /// 先につないでおく（Rust のプロジェクトを開いた・最初の保存）。待たない。
    pub fn warm(&self, file: PathBuf) {
        self.send(Cmd::Warm(file));
    }

    fn next_req(&self) -> RemoteReq {
        let mut n = self.next_req.fetch_add(1, Ordering::Relaxed);
        if n == 0 {
            // 0 は「自分の依頼ではない」なので使わない（一巡したとき）
            n = self.next_req.fetch_add(1, Ordering::Relaxed);
        }
        RemoteReq(n)
    }

    /// LSP の問い合わせ。待たない。答えは [`RemoteEvent::LspResult`] で必ず 1 回届く。
    pub fn lsp(&self, ask: LspAsk) -> RemoteReq {
        let req = self.next_req();
        if !self.send(Cmd::Lsp(req, ask)) {
            self.results.send(AppEvent::Remote(RemoteEvent::LspResult { req, status: DoneStatus::Error, message: "zc-remote の専用スレッドを作れない".into(), text: String::new(), items: Vec::new() }));
        }
        req
    }

    /// ジョブを頼む。待たない。結果は [`RemoteEvent`] で届き、最後に必ず
    /// [`RemoteEvent::Done`] が 1 回届く（スレッドやソケットが壊れたときも）。
    pub fn submit(&self, job: RemoteJob) -> RemoteReq {
        let req = self.next_req();
        if !self.send(Cmd::Submit(req, job, Instant::now())) {
            self.results.send(AppEvent::Remote(RemoteEvent::Done {
                req: Some(req),
                tag: JobTag(0),
                status: DoneStatus::Error,
                exit: -1,
                backend: Backend::Unknown,
                elapsed_ms: 0,
                message: "zc-remote の専用スレッドを作れない".into(),
            }));
        }
        req
    }

    /// 依頼を取り消す。
    pub fn cancel(&self, req: RemoteReq) {
        self.send(Cmd::Cancel(req));
    }

    /// スレッドへ渡す（無ければ作る）。渡せなければ `false`。
    fn send(&self, cmd: Cmd) -> bool {
        let handle = self.thread.get_or_init(|| self.spawn_thread());
        match handle {
            Some(h) if h.tx.send(cmd).is_ok() => {
                h.wake.notify();
                true
            }
            _ => false,
        }
    }

    fn spawn_thread(&self) -> Option<Handle> {
        let wake = Notifier::new().ok()?;
        let (tx, rx) = mpsc::channel();
        let mut worker = Worker::new(self.results.clone(), self.program.clone(), self.socket.clone(), rx, wake.clone());
        worker.serve_pid = self.serve_pid.clone();
        std::thread::Builder::new().name("zc-remote".into()).stack_size(THREAD_STACK).spawn(move || worker.run()).ok()?;
        Some(Handle { tx, wake })
    }
}

impl Drop for RemoteClient {
    fn drop(&mut self) {
        // 待たない（スレッドは知らせを受けてソケットを閉じ、自分で終わる）
        self.shutdown();
    }
}

/// serve への接続。
struct Conn {
    stream: UnixStream,
    reader: FrameReader,
    /// 書けずに残っている依頼。
    out: Vec<u8>,
    /// serve の `Hello` を受けた。
    hello: bool,
}

/// 走っている自分の依頼。
struct JobState {
    tag: Option<JobTag>,
    started: Instant,
}

/// 分けて届いている診断。
struct DiagAcc {
    req: Option<RemoteReq>,
    root: PathBuf,
    complete: bool,
    files: Vec<FileDiags>,
}

/// つなげなかった理由。
enum ConnectFail {
    /// `zc-remote` が無い・版が違う（以後は「対象ではない」）。
    Unavailable,
    /// serve がいない・現れなかった。
    NoServe,
}

/// メインへすぐには送らず、まとめて持っている知らせ。
#[derive(Default)]
struct Held {
    /// ジョブごとの最新の進み具合（`(tag, イベント)`）。
    phases: Vec<(JobTag, RemoteEvent)>,
    /// 依頼ごとのログ（続けて同じ依頼なら 1 つにつなぐ）。
    logs: VecDeque<(RemoteReq, Vec<u8>)>,
    log_bytes: usize,
}

impl Held {
    fn is_empty(&self) -> bool {
        self.phases.is_empty() && self.logs.is_empty()
    }

    fn hold_phase(&mut self, tag: JobTag, ev: RemoteEvent) {
        match self.phases.iter_mut().find(|(t, _)| *t == tag) {
            Some(slot) => slot.1 = ev,
            None => self.phases.push((tag, ev)),
        }
    }

    fn drop_phase(&mut self, tag: JobTag) {
        self.phases.retain(|(t, _)| *t != tag);
    }

    /// ログを足す。上限を超えたら古い方から捨てる。
    fn hold_log(&mut self, req: RemoteReq, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        match self.logs.back_mut() {
            Some((r, b)) if *r == req => b.extend_from_slice(bytes),
            _ => self.logs.push_back((req, bytes.to_vec())),
        }
        self.log_bytes += bytes.len();
        while self.log_bytes > LOG_BUFFER_MAX {
            let over = self.log_bytes - LOG_BUFFER_MAX;
            let Some((_, front)) = self.logs.front_mut() else { break };
            if front.len() <= over {
                self.log_bytes -= front.len();
                self.logs.pop_front();
                continue;
            }
            // 切り口の近くに改行があれば、行の頭から残す（制御列の途中から始めない）
            let mut cut = over;
            let look = &front[cut..front.len().min(cut + LOG_TRIM_LOOKAHEAD)];
            if let Some(i) = look.iter().position(|&c| c == b'\n') {
                if cut + i + 1 < front.len() {
                    cut += i + 1;
                }
            }
            front.drain(..cut);
            self.log_bytes -= cut;
        }
    }

    fn take(&mut self) -> Vec<RemoteEvent> {
        let mut v: Vec<RemoteEvent> = self.phases.drain(..).map(|(_, e)| e).collect();
        v.extend(self.logs.drain(..).map(|(req, bytes)| RemoteEvent::Log { req, bytes }));
        self.log_bytes = 0;
        v
    }
}

/// 専用スレッドの中身。
struct Worker {
    results: MainSender<AppEvent>,
    program: Option<PathBuf>,
    socket: PathBuf,
    rx: mpsc::Receiver<Cmd>,
    wake: Notifier,
    conn: Option<Conn>,
    /// 接続を作り直すたびに増やす（`poll()` の結果が古い接続のものか見分ける）。
    conn_gen: u64,
    /// 自分で起動した serve（終わったら回収する）。
    child: Option<Child>,
    /// その番号を持ち手と分け合う（終わるときに持ち手が SIGTERM を送る）。
    serve_pid: Arc<AtomicI32>,
    /// `zc-remote` が使えない（知らせ済み）。
    unavailable: bool,
    /// 次につなぎ直しを試す時刻と、今の間隔。
    reconnect: Option<(Instant, Duration)>,
    jobs: HashMap<u32, JobState>,
    /// 答えを待っている LSP の問い合わせ。
    lsps: HashSet<u32>,
    diags: HashMap<JobTag, DiagAcc>,
    /// ジョブごとに、最後にすぐ送った段階。
    phases: HashMap<JobTag, Phase>,
    held: Held,
    last_wake: Option<Instant>,
    /// メインが終わっている。
    main_gone: bool,
}

impl Worker {
    fn new(results: MainSender<AppEvent>, program: Option<PathBuf>, socket: PathBuf, rx: mpsc::Receiver<Cmd>, wake: Notifier) -> Worker {
        Worker {
            results,
            program,
            socket,
            rx,
            wake,
            conn: None,
            conn_gen: 0,
            child: None,
            serve_pid: Arc::new(AtomicI32::new(0)),
            unavailable: false,
            reconnect: None,
            jobs: HashMap::new(),
            lsps: HashSet::new(),
            diags: HashMap::new(),
            phases: HashMap::new(),
            held: Held::default(),
            last_wake: None,
            main_gone: false,
        }
    }

    fn run(mut self) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let now = Instant::now();
            let mut deadline: Option<Instant> = None;
            if !self.held.is_empty() {
                let due = self.last_wake.map_or(now, |t| t + MIN_WAKE_INTERVAL);
                deadline = Some(due);
            }
            if self.conn.is_none() {
                if let Some((at, _)) = self.reconnect {
                    deadline = Some(deadline.map_or(at, |d| d.min(at)));
                }
            }
            let timeout_ms = match deadline {
                None => -1,
                Some(d) => {
                    let us = d.saturating_duration_since(now).as_micros();
                    us.div_ceil(1000).min(i32::MAX as u128) as i32
                }
            };

            let gen = self.conn_gen;
            let (sock_fd, want_out) = match &self.conn {
                Some(c) => (c.stream.as_raw_fd(), !c.out.is_empty()),
                None => (-1, false),
            };
            let mut fds = [
                libc::pollfd { fd: self.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: sock_fd, events: libc::POLLIN | if want_out { libc::POLLOUT } else { 0 }, revents: 0 },
            ];
            // SAFETY: fds は 2 つの初期化済みの pollfd。
            let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
            if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                // poll が壊れることは無いはずだが、回り続けないように少し待つ
                std::thread::sleep(Duration::from_millis(10));
            }

            if n > 0 && fds[0].revents != 0 {
                self.wake.drain();
                if !self.commands() {
                    return;
                }
            }
            if n > 0 && sock_fd >= 0 && gen == self.conn_gen && self.conn.is_some() {
                let rev = fds[1].revents;
                if rev & libc::POLLOUT != 0 {
                    self.flush_out();
                }
                if rev & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 && gen == self.conn_gen {
                    self.read_socket(&mut buf);
                }
            }

            let now = Instant::now();
            if self.conn.is_none() {
                if let Some((at, step)) = self.reconnect {
                    if now >= at {
                        self.reap_child();
                        if self.ensure_connected(false).is_err() && !self.unavailable {
                            let next = (step * 2).min(RECONNECT_MAX);
                            self.reconnect = Some((Instant::now() + next, next));
                        }
                    }
                }
            }
            if !self.held.is_empty() && self.last_wake.is_none_or(|t| now >= t + MIN_WAKE_INTERVAL) {
                self.flush_held();
            }
            if self.main_gone {
                return;
            }
        }
    }

    /// 届いている依頼を全部片づける。終わるときは `false`。
    fn commands(&mut self) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(Cmd::Quit) | Err(mpsc::TryRecvError::Disconnected) => return false,
                Err(mpsc::TryRecvError::Empty) => return true,
                Ok(Cmd::Warm(file)) => {
                    if self.ensure_connected(true).is_ok() {
                        self.write(&Request::Warm { file });
                    }
                }
                Ok(Cmd::Submit(req, job, started)) => self.submit(req, job, started),
                Ok(Cmd::Lsp(req, ask)) => self.lsp(req, ask),
                Ok(Cmd::Cancel(req)) => {
                    if self.conn.is_some() && self.jobs.contains_key(&req.0) {
                        self.write(&Request::Cancel { req: ReqId(req.0) });
                    }
                }
            }
        }
    }

    fn submit(&mut self, req: RemoteReq, job: RemoteJob, started: Instant) {
        // 未保存の中身は、つなぐ前にバイト列にする（大きすぎれば、つながずに断る）
        let Some(overlay) = overlay_bytes(&job.overlay, MAX_OVERLAY_BYTES) else {
            return self.done_now(req, None, started, DoneStatus::NotApplicable, format!("未保存の中身が {}MiB を超えている", MAX_OVERLAY_BYTES >> 20));
        };
        match self.ensure_connected(true) {
            Ok(()) => {
                self.jobs.insert(req.0, JobState { tag: None, started });
                self.write(&Request::Submit { req: ReqId(req.0), kind: job.kind, lang: job.lang, flags: job.flags, file: job.file, overlay });
            }
            Err(ConnectFail::Unavailable) => self.done_now(req, None, started, DoneStatus::NotApplicable, String::new()),
            Err(ConnectFail::NoServe) => self.done_now(req, None, started, DoneStatus::Error, "zc-remote serve につながらない".into()),
        }
    }

    fn lsp(&mut self, req: RemoteReq, ask: LspAsk) {
        let Some(overlay) = overlay_bytes(&ask.overlay, MAX_OVERLAY_BYTES) else {
            return self.lsp_now(req, DoneStatus::Limit, format!("未保存の中身が {}MiB を超えている", MAX_OVERLAY_BYTES >> 20));
        };
        match self.ensure_connected(true) {
            Ok(()) => {
                self.lsps.insert(req.0);
                self.write(&Request::Lsp { req: ReqId(req.0), kind: ask.kind, file: ask.file, line: ask.line, col: ask.col, text: ask.text, overlay });
            }
            Err(ConnectFail::Unavailable) => self.lsp_now(req, DoneStatus::NotApplicable, "zc-remote が使えない".into()),
            Err(ConnectFail::NoServe) => self.lsp_now(req, DoneStatus::Error, "zc-remote serve につながらない".into()),
        }
    }

    /// serve からではなく、ここで作る LSP の答え。
    fn lsp_now(&mut self, req: RemoteReq, status: DoneStatus, message: String) {
        self.emit_now(RemoteEvent::LspResult { req, status, message, text: String::new(), items: Vec::new() });
    }

    /// つながっていなければつなぐ。`spawn` なら、serve がいなければ起動して最大
    /// [`SPAWN_WAIT`] 待つ（このスレッドの中で待つ。メインは待たない）。
    fn ensure_connected(&mut self, spawn: bool) -> Result<(), ConnectFail> {
        if self.unavailable {
            return Err(ConnectFail::Unavailable);
        }
        if self.conn.is_some() {
            return Ok(());
        }
        if let Ok(s) = UnixStream::connect(&self.socket) {
            self.attach(s);
            return Ok(());
        }
        if !spawn {
            return Err(ConnectFail::NoServe);
        }
        self.spawn_serve()?;
        let until = Instant::now() + SPAWN_WAIT;
        loop {
            std::thread::sleep(SPAWN_POLL);
            if let Ok(s) = UnixStream::connect(&self.socket) {
                self.attach(s);
                return Ok(());
            }
            if Instant::now() >= until {
                return Err(ConnectFail::NoServe);
            }
        }
    }

    /// `zc-remote serve` を起動する（自分が起動したものがまだ動いていれば、起動しない）。
    fn spawn_serve(&mut self) -> Result<(), ConnectFail> {
        if let Some(c) = &mut self.child {
            if matches!(c.try_wait(), Ok(None)) {
                return Ok(());
            }
            let _ = self.serve_pid.compare_exchange(c.id() as i32, 0, Ordering::SeqCst, Ordering::SeqCst);
            self.child = None;
        }
        let program = self.program.clone().unwrap_or_else(|| PathBuf::from("zc-remote"));
        let mut cmd = Command::new(&program);
        cmd.arg("serve").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(dir) = runtime_dir_of(&self.socket) {
            cmd.env("XDG_RUNTIME_DIR", dir);
        }
        match cmd.spawn() {
            Ok(child) => {
                self.serve_pid.store(child.id() as i32, Ordering::SeqCst);
                self.child = Some(child);
                Ok(())
            }
            Err(e) => {
                let why = if e.kind() == io::ErrorKind::NotFound {
                    format!("{} が見つからない", program.display())
                } else {
                    format!("{} を起動できない: {e}", program.display())
                };
                self.mark_unavailable(why);
                Err(ConnectFail::Unavailable)
            }
        }
    }

    fn reap_child(&mut self) {
        if let Some(c) = &mut self.child {
            if !matches!(c.try_wait(), Ok(None)) {
                // 回収した番号はほかのプロセスに使い回されうるので、持ち手に送らせない
                let _ = self.serve_pid.compare_exchange(c.id() as i32, 0, Ordering::SeqCst, Ordering::SeqCst);
                self.child = None;
            }
        }
    }

    fn attach(&mut self, stream: UnixStream) {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
        self.conn = Some(Conn { stream, reader: FrameReader::new(), out: Vec::new(), hello: false });
        self.conn_gen += 1;
        self.reconnect = None;
        self.write(&Request::Hello { version: IPC_VERSION, client: ClientKind::Editor });
    }

    /// 依頼を書く（書けない分は溜めて、書けるようになってから）。
    fn write(&mut self, req: &Request) {
        let Some(c) = &mut self.conn else { return };
        encode_request(req, &mut c.out);
        self.flush_out();
    }

    fn flush_out(&mut self) {
        let Some(c) = &mut self.conn else { return };
        let mut done = 0;
        let mut broken = false;
        while done < c.out.len() {
            match c.stream.write(&c.out[done..]) {
                Ok(0) => {
                    broken = true;
                    break;
                }
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    broken = true;
                    break;
                }
            }
        }
        c.out.drain(..done);
        if broken || c.out.len() > OUT_MAX {
            self.disconnect(DoneStatus::Error, "zc-remote serve との接続が切れた");
        }
    }

    fn read_socket(&mut self, buf: &mut [u8]) {
        let gen = self.conn_gen;
        loop {
            let Some(c) = &mut self.conn else { return };
            match c.stream.read(buf) {
                Ok(0) => {
                    self.disconnect(DoneStatus::Error, "zc-remote serve との接続が切れた");
                    return;
                }
                Ok(n) => c.reader.push(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.disconnect(DoneStatus::Error, "zc-remote serve との接続が切れた");
                    return;
                }
            }
            // 読んだ分を、そろったメッセージから順に片づける
            loop {
                let next = {
                    let Some(c) = &mut self.conn else { return };
                    match c.reader.next_frame() {
                        Ok(Some(f)) => Some(decode_response(f)),
                        Ok(None) => None,
                        Err(e) => Some(Err(e)),
                    }
                };
                match next {
                    None => break,
                    Some(Ok(resp)) => self.on_response(resp),
                    Some(Err(DecodeError::UnknownKind(_))) => {}
                    Some(Err(_)) => {
                        self.disconnect(DoneStatus::Error, "zc-remote serve の返事が読めない");
                        return;
                    }
                }
                if gen != self.conn_gen || self.conn.is_none() {
                    return;
                }
            }
        }
    }

    fn on_response(&mut self, resp: Response) {
        let Some(c) = &mut self.conn else { return };
        if !c.hello {
            match resp {
                Response::Hello { version } if version == IPC_VERSION => c.hello = true,
                Response::Hello { version } => {
                    self.mark_unavailable(format!("zc-remote の約束の版が違う（相手 {version}、zcode {IPC_VERSION}）"));
                    self.disconnect(DoneStatus::NotApplicable, "");
                }
                _ => self.disconnect(DoneStatus::Error, "zc-remote serve の返事の順番が違う"),
            }
            return;
        }
        match resp {
            Response::Hello { .. } | Response::Status { .. } | Response::FileWritten { .. } => {}
            Response::Phase { req, tag, root, phase, done, total } => {
                let req = own(req);
                if let Some(j) = req.and_then(|r| self.jobs.get_mut(&r.0)) {
                    j.tag = Some(tag);
                }
                let ev = RemoteEvent::Phase { req, tag, root, phase, done, total };
                if self.phases.get(&tag) == Some(&phase) {
                    self.held.hold_phase(tag, ev);
                } else {
                    self.phases.insert(tag, phase);
                    self.held.drop_phase(tag);
                    self.emit_now(ev);
                }
            }
            Response::Log { req, tag: _, bytes } => {
                if let Some(r) = own(req) {
                    self.held.hold_log(r, &bytes);
                }
            }
            Response::Diagnostics { req, tag, root, complete, last, files } => {
                let acc = self.diags.entry(tag).or_insert_with(|| DiagAcc { req: own(req), root, complete: true, files: Vec::new() });
                acc.complete &= complete;
                acc.files.extend(files);
                if last {
                    if let Some(acc) = self.diags.remove(&tag) {
                        self.emit_now(RemoteEvent::Diagnostics { req: acc.req, tag, root: acc.root, complete: acc.complete, files: acc.files });
                    }
                }
            }
            Response::Tests { req, tag: _, root, report } => {
                if let Some(r) = own(req) {
                    self.emit_now(RemoteEvent::Tests { req: r, root, report });
                }
            }
            Response::Artifact { req, tag: _, kind, stale, path } => {
                if let Some(r) = own(req) {
                    self.emit_now(RemoteEvent::Artifact { req: r, kind, stale, path });
                }
            }
            Response::Done { req, tag, status, exit, backend, elapsed_ms, message } => {
                self.flush_partial_diags(tag);
                self.phases.remove(&tag);
                self.held.drop_phase(tag);
                let req = match own(req) {
                    None => None,
                    // 自分の依頼の Done は 1 回だけ（もう Done を送った依頼なら捨てる）
                    Some(r) => match self.jobs.remove(&r.0) {
                        Some(_) => Some(r),
                        None => return,
                    },
                };
                self.emit_now(RemoteEvent::Done { req, tag, status, exit, backend, elapsed_ms, message });
            }
            Response::LspResult { req, status, message, text, items } => {
                if self.lsps.remove(&req.0) {
                    self.emit_now(RemoteEvent::LspResult { req: RemoteReq(req.0), status, message, text, items });
                }
            }
            Response::LspState { root, state, message } => self.emit_now(RemoteEvent::LspState { root, state, message }),
        }
    }

    /// 分けて届いている途中の診断を、不完全な結果として送る。
    fn flush_partial_diags(&mut self, tag: JobTag) {
        if let Some(acc) = self.diags.remove(&tag) {
            self.emit_now(RemoteEvent::Diagnostics { req: acc.req, tag, root: acc.root, complete: false, files: acc.files });
        }
    }

    /// 接続を捨てる。走っていた自分の依頼には `status` の `Done` を送る。
    fn disconnect(&mut self, status: DoneStatus, message: &str) {
        self.conn = None;
        self.conn_gen += 1;
        let tags: Vec<JobTag> = self.diags.keys().copied().collect();
        for tag in tags {
            self.flush_partial_diags(tag);
        }
        self.phases.clear();
        self.held.phases.clear();
        let mut jobs: Vec<(u32, JobState)> = self.jobs.drain().collect();
        jobs.sort_by_key(|(r, _)| *r);
        for (r, j) in jobs {
            self.done_now(RemoteReq(r), j.tag, j.started, status, message.to_string());
        }
        let mut lsps: Vec<u32> = self.lsps.drain().collect();
        lsps.sort_unstable();
        for r in lsps {
            self.lsp_now(RemoteReq(r), status, message.to_string());
        }
        if !self.unavailable {
            self.reconnect = Some((Instant::now() + RECONNECT_FIRST, RECONNECT_FIRST));
        }
    }

    fn mark_unavailable(&mut self, why: String) {
        if !self.unavailable {
            self.unavailable = true;
            self.reconnect = None;
            self.emit_now(RemoteEvent::Unavailable(why));
        }
    }

    /// serve からではなく、ここで作る `Done`。
    fn done_now(&mut self, req: RemoteReq, tag: Option<JobTag>, started: Instant, status: DoneStatus, message: String) {
        let elapsed_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
        self.emit_now(RemoteEvent::Done { req: Some(req), tag: tag.unwrap_or(JobTag(0)), status, exit: -1, backend: Backend::Unknown, elapsed_ms, message });
    }

    /// すぐ送る（まとめて持っている分を先に送って、順番を保つ）。
    fn emit_now(&mut self, ev: RemoteEvent) {
        let mut v = self.held.take();
        v.push(ev);
        self.send_all(v);
    }

    fn flush_held(&mut self) {
        let v = self.held.take();
        if !v.is_empty() {
            self.send_all(v);
        }
    }

    fn send_all(&mut self, v: Vec<RemoteEvent>) {
        for ev in v {
            if !self.results.send(AppEvent::Remote(ev)) {
                self.main_gone = true;
            }
        }
        self.last_wake = Some(Instant::now());
    }
}

/// `0` は「自分の依頼ではない」。
fn own(req: ReqId) -> Option<RemoteReq> {
    (req != ReqId::NONE).then_some(RemoteReq(req.0))
}

/// `<dir>/zcode/remote.sock` の `<dir>`。
fn runtime_dir_of(socket: &Path) -> Option<&Path> {
    if socket.file_name()? != "remote.sock" {
        return None;
    }
    let zcode = socket.parent()?;
    if zcode.file_name()? != "zcode" {
        return None;
    }
    zcode.parent()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_buffer_drops_oldest_and_keeps_order() {
        let mut h = Held::default();
        let a = RemoteReq(1);
        let b = RemoteReq(2);
        h.hold_log(a, b"one\n");
        h.hold_log(a, b"two\n");
        h.hold_log(b, b"x");
        let v = h.take();
        assert_eq!(v, vec![RemoteEvent::Log { req: a, bytes: b"one\ntwo\n".to_vec() }, RemoteEvent::Log { req: b, bytes: b"x".to_vec() }]);
        assert!(h.is_empty());

        // あふれたら古い方から捨てる。切り口の近くの改行のあとから残す
        let mut h = Held::default();
        let mut line = vec![b'a'; 99];
        line.push(b'\n');
        for _ in 0..1000 {
            h.hold_log(a, &line);
        }
        assert!(h.log_bytes <= LOG_BUFFER_MAX);
        let v = h.take();
        let [RemoteEvent::Log { bytes, .. }] = v.as_slice() else { panic!("{v:?}") };
        assert!(bytes.len() <= LOG_BUFFER_MAX && bytes.len() > LOG_BUFFER_MAX - 200, "{}", bytes.len());
        assert_eq!(bytes.len() % 100, 0);
        assert!(bytes.ends_with(&line));

        // 1 つで上限を超えるものは、末尾だけ残る
        let mut h = Held::default();
        let big: Vec<u8> = (0..LOG_BUFFER_MAX * 2).map(|i| (i % 251) as u8).collect();
        h.hold_log(b, &big);
        let v = h.take();
        let [RemoteEvent::Log { bytes, .. }] = v.as_slice() else { panic!() };
        assert!(big.ends_with(bytes));
        assert!(bytes.len() <= LOG_BUFFER_MAX);
    }

    #[test]
    fn phases_keep_latest_per_job() {
        let mut h = Held::default();
        let ev = |done| RemoteEvent::Phase { req: None, tag: JobTag(1), root: "/p".into(), phase: Phase::Uploading, done, total: 10 };
        h.hold_phase(JobTag(1), ev(1));
        h.hold_phase(JobTag(1), ev(5));
        assert_eq!(h.take(), vec![ev(5)]);
        h.hold_phase(JobTag(1), ev(6));
        h.drop_phase(JobTag(1));
        assert!(h.is_empty());
    }

    #[test]
    fn runtime_dir_from_socket_path() {
        assert_eq!(runtime_dir_of(Path::new("/run/user/1000/zcode/remote.sock")), Some(Path::new("/run/user/1000")));
        assert_eq!(runtime_dir_of(Path::new("/tmp/x.sock")), None);
        assert_eq!(runtime_dir_of(Path::new("/tmp/other/remote.sock")), None);
    }
}
