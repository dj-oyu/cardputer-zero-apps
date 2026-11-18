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
//!   `ready`。`need` が空ならすぐ `ready`
//! - `job` はその接続で `ready` を受けた木に対してだけ受け付ける（違えば `error {bad-tree}`）。
//!   job id は受付でも計算し直して確かめる
//! - **ジョブは接続から切り離す**: 接続が切れてもジョブは止めない（[`crate::jobs::Registry`]）。
//!   つなぎ直した相手は `attach` で続きを受ける
//! - **遅い相手を待たない**: 送り待ちが 4MB を超えたら、その接続を閉じる（ジョブは続く。
//!   相手はつなぎ直して `attach` する）。ログは台帳に全部残っている
//! - 15 秒何も送っていなければ ping を打つ。60 秒何も届かなければ閉じる
//! - `X-Zc-Device`（Worker が付ける）があれば、ログに出す名前として使う。認証はしない
//!   （Cloudflare では Access と Worker、PC では Tailscale の ACL が門番）
//!
//! # 中の作り（課題 B1 で決めたこと）
//!
//! - 接続ごとのスレッドは 1 本。ソケットは非ブロッキングで、`poll()` で読み書きを待つ。
//!   見ているジョブがある間は 25ms ごとに台帳から続きを取りに行く（[`Watcher::recv`] を
//!   待たずに呼ぶ）。ジョブを見ていない接続は 1 秒に 1 回起きるだけ
//! - **送り待ちが 256KB を超えている間は、台帳から続きを取らない**（ログも成果物も台帳と
//!   ファイルに残っているので、相手が読んだ分だけ取ればよい）。それでも送り待ちが 4MB を
//!   超えた・60 秒 1 バイトも送れない・ログの続きが台帳の上限からこぼれた
//!   （[`WatchEnd::Lagged`]）接続は閉じる。ジョブは続く
//! - **作業の木に写すのは、ジョブが走り始める直前**（[`Registry::submit_prepared`]）。
//!   `ready` は「中身が全部そろった」の意味で返す。接続のスレッドで写すと、同じプロジェクトの
//!   ジョブが走っている最中の木を書き換えてしまう（F5 のビルド中に保存 → check）。待ちの列に
//!   いる間に次の目録が来ても、ジョブは自分の目録の木で走る
//! - **接続どうしは独立**。受付は「実機 1 台に接続 1 本」を前提にしない（Cloudflare 側は
//!   実機ごとに 4 本まで通す。PC 直結では相手を見分ける手段が無い）。2 本目が来ても 1 本目は
//!   切らない。そろった木（`ready`）は接続ごと、ジョブは台帳で共有: 同じ job id を 2 本の
//!   接続から頼めば、どちらも同じジョブを見る。同じ接続から同じジョブをもう一度頼む・
//!   `attach` し直すと、前の見る人と入れ替える（同じログを 2 回流さない）
//! - `job` のない `error`（接続全体の失敗）を返したら、close を送って閉じる。`job` つきの
//!   `error`（bad-tree・unknown-job・limit）は、そのジョブだけの話で、接続は続ける
//! - `cancel` は、この接続が見ていないジョブにも効く。そのときは、終わりの知らせ
//!   （`done {cancelled}`）が届くように、見る人として付けてから取り消す
//! - 相手の名前（ログ用）: `X-Zc-Device` → [`Options::whois`]（PC の経路で tailscaled に
//!   聞く、などを差し込む口。既定は無し）→ 相手の IP

use crate::jobs::{Feed, JobSpec, Registry, SubmitError, WatchEnd, Watcher};
use crate::lsp::{LspHub, Query};
use crate::runner::{Runner, Tools};
use crate::store::{BlobWriter, StoreError};
use crate::Config;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zc_wire::http;
use zc_wire::limits::{CHUNK_BYTES, MAX_TEXT_FRAME, MAX_TREE_BYTES, MAX_TREE_FILES};
use zc_wire::msg::{self, BinHeader, BinKind, ClientMsg, ErrorCode, FileEntry, JobOpts, Kind, Lang, ServerMsg};
use zc_wire::tree::{Manifest, ManifestError};
use zc_wire::ws::{self, Decoder, Encoder, Role, WsError};

/// 同時に相手する接続の数（超えたら 503）。
pub const MAX_CONNECTIONS: usize = 32;
/// 1 本の接続で同時に見られるジョブの数。
pub const MAX_WATCHED: usize = 16;
/// 送り待ちがこれを超えている間は、台帳から続きを取らない。
pub const SEND_HIGH_WATER: usize = 256 << 10;
/// 送り待ちがこれを超えた接続は閉じる。
pub const SEND_LIMIT: usize = 4 << 20;
/// 何も送っていなければ ping を打つまでの時間。
pub const PING_AFTER: Duration = Duration::from_secs(15);
/// 何も届かなければ閉じるまでの時間。
pub const SILENCE_LIMIT: Duration = Duration::from_secs(60);
/// 送り待ちがあるのに 1 バイトも送れないまま、閉じるまでの時間。
pub const STALL_LIMIT: Duration = Duration::from_secs(60);
/// HTTP の頭と `hello` を待つ時間。
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(10);
/// 見ているジョブがある間の、続きを取りに行く間隔。
const TICK: Duration = Duration::from_millis(25);
/// 閉じる前に、送り残しを出し切るのを待つ時間。
const LINGER: Duration = Duration::from_secs(2);
/// この接続で覚えておく「そろった木」の数。
const READY_TREES: usize = 4;
/// 置き場の掃除の間隔（接続もジョブも無い時にだけ）。
const GC_EVERY: Duration = Duration::from_secs(600);
/// LSP の問い合わせの答えを待つ時間（過ぎたら `internal` で返し、遅れた答えは捨てる）。
pub const LSP_TIMEOUT: Duration = Duration::from_secs(25);
/// 1 本の接続で同時に待てる LSP の問い合わせの数。
const MAX_LSP_PENDING: usize = 16;
/// 接続の通し番号（rust-analyzer の状態を知らせる相手を見分ける）。
static CONN_SEQ: AtomicU64 = AtomicU64::new(1);

/// 相手のアドレスから、ログに出す名前を引く口（PC の経路で、tailscaled に「相手はどの
/// ノードか」を聞く、など）。認証には使わない。
pub type Whois = dyn Fn(SocketAddr) -> Option<String> + Send + Sync;

/// [`spawn_with`] に渡すもの。
#[derive(Default)]
pub struct Options {
    /// ジョブの実行の口。無ければ本物（[`Tools`]）。試験では偽物を渡す。
    pub runner: Option<Arc<dyn Runner>>,
    /// 相手の名前を引く口。無ければ相手の IP をそのまま名前にする。
    pub whois: Option<Arc<Whois>>,
}

struct Shared {
    registry: Arc<Registry>,
    lsp: Arc<LspHub>,
    stop: AtomicBool,
    conns: AtomicUsize,
    whois: Option<Arc<Whois>>,
}

static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_terminate(_: libc::c_int) {
    TERMINATE.store(true, Ordering::SeqCst);
}

/// 待ち受けて、接続ごとにスレッドを作る（終わるまで戻らない）。SIGTERM を受けたら、
/// 新しい接続を止め、走っているジョブをプロセスグループごと止めてから戻る（Cloudflare は
/// 止めるとき SIGTERM を送り、最大 15 分後に SIGKILL する）。
pub fn run(cfg: Config) -> std::io::Result<()> {
    let handler = on_terminate as extern "C" fn(libc::c_int) as usize;
    // SAFETY: ハンドラは AtomicBool に書くだけ（シグナルの中で呼んでよい操作）。
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
    let handle = spawn(cfg)?;
    // 置き場が開けないなら、最初の依頼を待たずにここで終わる。
    handle.registry().store()?;
    eprintln!("zc-buildd: {} で待っている（置き場 {}）", handle.addr(), handle.registry().config().data_dir.display());
    while !TERMINATE.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!("zc-buildd: 止める");
    drop(handle);
    Ok(())
}

/// 待ち受けを始めて、待ち受けているアドレスと止めるための持ち手を返す（試験用。
/// ポート 0 で空いているポートを選べる）。
pub fn spawn(cfg: Config) -> std::io::Result<Handle> {
    spawn_with(cfg, Options::default())
}

/// [`spawn`] と同じ。実行の口と、相手の名前を引く口を差し替えられる。
pub fn spawn_with(cfg: Config, opts: Options) -> std::io::Result<Handle> {
    let listener = TcpListener::bind(cfg.listen)?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let runner = opts.runner.unwrap_or_else(|| Arc::new(Tools));
    let registry = Registry::new(&cfg, runner);
    let shared = Arc::new(Shared { registry, lsp: Arc::new(LspHub::new()), stop: AtomicBool::new(false), conns: AtomicUsize::new(0), whois: opts.whois });
    let sh = shared.clone();
    let thread = std::thread::Builder::new().name("zc-accept".into()).spawn(move || accept_loop(listener, sh))?;
    Ok(Handle { addr, shared, thread: Some(thread) })
}

/// [`spawn`] の持ち手。捨てると止まる（新しい接続を受けない・接続を閉じる・走っている
/// ジョブを止める）。
pub struct Handle {
    addr: SocketAddr,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Handle {
    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }
    pub fn registry(&self) -> Arc<Registry> {
        self.shared.registry.clone()
    }
    /// rust-analyzer の持ち主（試験用）。
    pub fn lsp(&self) -> Arc<LspHub> {
        self.shared.lsp.clone()
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.shared.registry.shutdown();
        self.shared.lsp.shutdown();
    }
}

/// 接続 1 本を最後まで相手する（接続ごとのスレッドの中身）。
pub fn serve_connection(stream: TcpStream, registry: Arc<Registry>) -> std::io::Result<()> {
    let shared = Shared { registry, lsp: Arc::new(LspHub::new()), stop: AtomicBool::new(false), conns: AtomicUsize::new(0), whois: None };
    serve(stream, &shared)
}

fn accept_loop(listener: TcpListener, sh: Arc<Shared>) {
    let mut last_gc = Instant::now();
    while !sh.stop.load(Ordering::SeqCst) {
        wait(&listener, false, Duration::from_millis(200));
        match listener.accept() {
            Ok((stream, _)) => {
                if sh.conns.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    let _ = stream.set_nonblocking(true);
                    let _ = respond(&stream, 503, "接続が多すぎる\n");
                    continue;
                }
                sh.conns.fetch_add(1, Ordering::SeqCst);
                let sh2 = sh.clone();
                let spawned = std::thread::Builder::new().name("zc-conn".into()).spawn(move || {
                    let _ = serve(stream, &sh2);
                    sh2.conns.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    sh.conns.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
        // 置き場の掃除は、接続もジョブも無い時にだけ（受け取り途中の中身を消さない）。
        if last_gc.elapsed() >= GC_EVERY && sh.conns.load(Ordering::SeqCst) == 0 && !sh.registry.busy() {
            last_gc = Instant::now();
            if let Ok(store) = sh.registry.store() {
                let _ = store.collect_garbage(sh.registry.config().disk_quota);
            }
        }
    }
}

/// 読める（`write` なら書ける）ようになるか、`timeout` まで待つ。
fn wait(fd: &impl AsRawFd, write: bool, timeout: Duration) {
    let mut p = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN | if write { libc::POLLOUT } else { 0 }, revents: 0 };
    let ms = timeout.as_millis().min(60_000) as libc::c_int;
    // SAFETY: p はこの呼び出しの間だけ使う、正しい pollfd 1 つ。
    unsafe {
        libc::poll(&mut p, 1, ms);
    }
}

/// 非ブロッキングのソケットに、期限つきで全部書く。
fn write_all(mut stream: &TcpStream, mut data: &[u8], limit: Duration) -> io::Result<()> {
    let deadline = Instant::now() + limit;
    while !data.is_empty() {
        match stream.write(data) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                wait(stream, true, deadline - now);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// ふつうの HTTP の返事を返して終わる。
fn respond(stream: &TcpStream, status: u16, body: &str) -> io::Result<()> {
    let mut out = Vec::new();
    http::write_response(status, "text/plain; charset=utf-8", body.as_bytes(), &mut out);
    write_all(stream, &out, LINGER)
}

/// ログに出す名前として使える形にする（表示できる ASCII・64 文字まで）。
fn clean_name(s: &str) -> Option<String> {
    let v: String = s.chars().filter(|c| c.is_ascii_graphic()).take(64).collect();
    (!v.is_empty()).then_some(v)
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn status_json(registry: &Registry, lsp: &LspHub) -> String {
    let jobs = registry.status();
    let count = |s: &str| jobs.iter().filter(|j| j.state == s).count();
    let list: Vec<serde_json::Value> = jobs.iter().map(|j| serde_json::json!({ "job": j.job, "project": j.project, "kind": j.kind, "state": j.state })).collect();
    // lsp_cpu_ms: rust-analyzer の CPU の通算（DO が費用の計りに使う。docs/lsp-design.md）
    let lsp_state = lsp.state().map(|(_, s)| serde_json::to_value(s).unwrap_or_default());
    serde_json::json!({ "running": count("running"), "queued": count("queued"), "jobs": list, "uptime_s": registry.uptime().as_secs(), "lsp_cpu_ms": lsp.cpu_ms(), "lsp": lsp_state }).to_string()
}

fn serve(stream: TcpStream, sh: &Shared) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    let _ = stream.set_nodelay(true);
    let peer = stream.peer_addr().ok();

    // HTTP の頭を読む。
    let deadline = Instant::now() + HANDSHAKE_LIMIT;
    let mut head: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 64 << 10];
    let req = loop {
        match http::parse_request(&head) {
            http::Request::Incomplete => {}
            other => break other,
        }
        if head.len() > 16 << 10 {
            return respond(&stream, 400, "頭が大きすぎる\n");
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        wait(&stream, false, deadline - now);
        match (&stream).read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => head.extend_from_slice(&buf[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
            Err(e) => return Err(e),
        }
    };
    let route = |p: &str| p.split('?').next().unwrap_or("").to_string();
    let (accept, device, consumed) = match req {
        http::Request::Get { path, .. } => {
            return match route(&path).as_str() {
                "/healthz" => respond(&stream, 200, "ok"),
                "/v1/status" => {
                    let mut out = Vec::new();
                    http::write_response(200, "application/json", status_json(&sh.registry, &sh.lsp).as_bytes(), &mut out);
                    write_all(&stream, &out, LINGER)
                }
                zc_wire::WS_PATH => respond(&stream, 426, "WebSocket の Upgrade が要る\n"),
                _ => respond(&stream, 404, "無い\n"),
            };
        }
        http::Request::Upgrade { path, accept, subprotocol_ok, device, consumed } => {
            if route(&path) != zc_wire::WS_PATH {
                return respond(&stream, 404, "無い\n");
            }
            if !subprotocol_ok {
                return respond(&stream, 400, "Sec-WebSocket-Protocol: zc-build.v1 が要る\n");
            }
            (accept, device, consumed)
        }
        http::Request::Invalid(why) => return respond(&stream, 400, &format!("{why}\n")),
        http::Request::Incomplete => return Ok(()),
    };
    let mut out = Vec::new();
    http::write_switching(&accept, &mut out);
    write_all(&stream, &out, LINGER)?;

    // Worker 経由なら X-Zc-Device が付いている。PC 直結では、差し込まれた口か相手の IP。
    let tagged = device.as_deref().and_then(clean_name);
    let name = tagged
        .clone()
        .or_else(|| peer.and_then(|p| sh.whois.as_ref().and_then(|w| w(p))).as_deref().and_then(clean_name))
        .or_else(|| peer.map(|p| p.ip().to_string()))
        .unwrap_or_else(|| "?".into());
    let now = Instant::now();
    let (lsp_tx, lsp_rx) = mpsc::channel();
    let mut conn = Conn {
        sh,
        id: CONN_SEQ.fetch_add(1, Ordering::SeqCst),
        lsp_tx,
        lsp_rx,
        lsp_pending: Vec::new(),
        stream,
        enc: Encoder::new(Role::Server),
        out: Vec::new(),
        out_pos: 0,
        name,
        device: tagged,
        hello: false,
        ready: Vec::new(),
        upload: None,
        watching: Vec::new(),
        opened: now,
        last_tx: now,
        last_rx: now,
        last_progress: now,
        closing: None,
    };
    let mut dec = Decoder::new(Role::Server, MAX_TEXT_FRAME);
    dec.push(&head[consumed.min(head.len())..]);
    drop(head);
    conn.drain(&mut dec);
    conn.run(&mut dec, &mut buf)
}

/// この接続でそろった木。
struct ReadyTree {
    project: String,
    tree: String,
    manifest: Arc<Manifest>,
}

/// 受け取り途中の木（`need` を出して、中身を待っている）。
struct Upload {
    project: String,
    tree: String,
    manifest: Arc<Manifest>,
    /// 足りない中身（SHA-256 と大きさ）。`need` の順。
    need: Vec<(String, u64)>,
    idx: usize,
    got: u64,
    writer: Option<BlobWriter>,
}

/// 見ているジョブ。
struct Watching {
    watcher: Watcher,
    /// 送っている途中の成果物。
    art: Option<ArtStream>,
}

struct ArtStream {
    file: File,
    chan: u32,
    offset: u64,
    zbytes: u64,
}

/// 続きを 1 つ取りに行った結果。
enum Step {
    Sent,
    Idle,
    Finished,
    Lagged,
}

struct Conn<'a> {
    sh: &'a Shared,
    /// 接続の通し番号（[`CONN_SEQ`]）。
    id: u64,
    /// rust-analyzer の答え・状態の受け口（[`LspHub`] が送る。接続のスレッドが読んで流す）。
    lsp_tx: Sender<ServerMsg>,
    lsp_rx: Receiver<ServerMsg>,
    /// 答えを待っている LSP の問い合わせ（id と締め切り）。
    lsp_pending: Vec<(u32, Instant)>,
    stream: TcpStream,
    enc: Encoder,
    /// 送り待ち（`out_pos` より前は送り済み）。
    out: Vec<u8>,
    out_pos: usize,
    /// ログに出す名前。
    name: String,
    /// `X-Zc-Device`（[`JobSpec::device`] に入れる）。
    device: Option<String>,
    hello: bool,
    ready: Vec<ReadyTree>,
    upload: Option<Upload>,
    watching: Vec<Watching>,
    opened: Instant,
    last_tx: Instant,
    last_rx: Instant,
    last_progress: Instant,
    /// 閉じると決めた（送り残しを出し切るか、この時刻が来たら終わる）。
    closing: Option<Instant>,
}

impl Conn<'_> {
    fn pending(&self) -> usize {
        self.out.len() - self.out_pos
    }

    fn send(&mut self, m: &ServerMsg) {
        self.enc.text(&msg::encode(m), [0; 4], &mut self.out);
    }

    /// close を送って、閉じる段に入る。
    fn close(&mut self, code: u16) {
        if self.closing.is_none() {
            self.enc.close(code, [0; 4], &mut self.out);
            self.closing = Some(Instant::now() + LINGER);
        }
    }

    /// 接続全体の失敗: `error` を返して閉じる。
    fn fail(&mut self, code: ErrorCode, message: impl Into<String>) {
        if self.closing.is_some() {
            return;
        }
        let message = message.into();
        eprintln!("zc-buildd: [{}] 閉じる: {code:?}: {message}", self.name);
        self.send(&ServerMsg::Error { job: None, code, message, retry_after_s: None });
        self.close(if code == ErrorCode::TooLarge { 1009 } else { 1008 });
    }

    /// そのジョブだけの失敗（接続は続ける）。
    fn job_error(&mut self, job: &str, code: ErrorCode, message: &str, retry_after_s: Option<u32>) {
        self.send(&ServerMsg::Error { job: Some(job.to_string()), code, message: message.into(), retry_after_s });
    }

    fn flush(&mut self) -> io::Result<()> {
        while self.out_pos < self.out.len() {
            match (&self.stream).write(&self.out[self.out_pos..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    self.out_pos += n;
                    self.last_tx = Instant::now();
                    self.last_progress = self.last_tx;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        if self.out_pos == self.out.len() {
            self.out.clear();
            self.out_pos = 0;
        } else if self.out_pos > 1 << 20 {
            self.out.drain(..self.out_pos);
            self.out_pos = 0;
        }
        Ok(())
    }

    fn run(&mut self, dec: &mut Decoder, buf: &mut [u8]) -> io::Result<()> {
        loop {
            if self.sh.stop.load(Ordering::SeqCst) {
                self.close(1001);
            }
            self.flush()?;
            let now = Instant::now();
            if let Some(until) = self.closing {
                if self.pending() > 0 && now < until {
                    wait(&self.stream, true, until - now);
                    continue;
                }
                // 送り切った。書く側だけ閉じて、相手が閉じるのを少し待つ（届いている途中の
                // ものを読まずに閉じると、相手は RST を受けて、今送った error を読めない）。
                let _ = self.stream.shutdown(std::net::Shutdown::Write);
                loop {
                    let now = Instant::now();
                    if now >= until {
                        return Ok(());
                    }
                    wait(&self.stream, false, until - now);
                    match (&self.stream).read(buf) {
                        Ok(0) => return Ok(()),
                        Ok(_) => {}
                        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                        Err(_) => return Ok(()),
                    }
                }
            }
            if self.pending() == 0 {
                self.last_progress = now;
            }
            if self.pending() > SEND_LIMIT || now - self.last_progress >= STALL_LIMIT {
                // 遅い相手を待たない。ジョブは続く（相手はつなぎ直して attach する）。
                eprintln!("zc-buildd: [{}] 閉じる: 相手が受け取らない（送り待ち {} バイト）", self.name, self.pending());
                return Ok(());
            }
            if now - self.last_rx >= SILENCE_LIMIT {
                return Ok(());
            }
            if !self.hello && now - self.opened >= HANDSHAKE_LIMIT {
                self.fail(ErrorCode::Protocol, "hello が来ない");
                continue;
            }
            self.pump();
            self.pump_lsp(now);
            if self.closing.is_some() {
                continue;
            }
            if self.pending() == 0 && now - self.last_tx >= PING_AFTER {
                self.enc.ping(b"", [0; 4], &mut self.out);
                self.last_tx = now;
            }
            if self.pending() > 0 {
                self.flush()?;
            }
            let pulling = (!self.watching.is_empty() || !self.lsp_pending.is_empty()) && self.pending() < SEND_HIGH_WATER;
            wait(&self.stream, self.pending() > 0, if pulling { TICK } else { Duration::from_secs(1) });

            // 届いた分を読む（1 回りで 1MB まで。送る方を待たせない）。
            let mut total = 0;
            while total < 1 << 20 && self.closing.is_none() {
                match (&self.stream).read(buf) {
                    Ok(0) => return Ok(()),
                    Ok(n) => {
                        total += n;
                        self.last_rx = Instant::now();
                        dec.push(&buf[..n]);
                        self.drain(dec);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }

    /// そろっているメッセージを全部さばく。
    fn drain(&mut self, dec: &mut Decoder) {
        while self.closing.is_none() {
            match dec.next() {
                Ok(Some(m)) => self.on_message(m),
                Ok(None) => break,
                Err(WsError::TooLarge) => self.fail(ErrorCode::TooLarge, "メッセージが大きすぎる"),
                Err(WsError::Protocol(why)) => {
                    eprintln!("zc-buildd: [{}] 閉じる: WebSocket の約束に反した: {why}", self.name);
                    self.close(1002);
                }
            }
        }
    }

    fn on_message(&mut self, m: ws::Message<'_>) {
        match m {
            ws::Message::Text(text) => match msg::decode::<ClientMsg>(text) {
                Ok(Some(m)) => self.on_client(m),
                // 知らない type は無視する（版を上げずに足せるように）。
                Ok(None) => {}
                Err(e) => self.fail(ErrorCode::Protocol, format!("読めないメッセージ: {e}")),
            },
            ws::Message::Binary(frame) => self.on_binary(frame),
            ws::Message::Ping(data) => self.enc.pong(data, [0; 4], &mut self.out),
            ws::Message::Pong(_) => {}
            ws::Message::Close(_) => self.close(1000),
        }
    }

    fn on_client(&mut self, m: ClientMsg) {
        if !self.hello {
            match m {
                ClientMsg::Hello { v, client } if v == zc_wire::PROTOCOL_VERSION => {
                    self.hello = true;
                    eprintln!("zc-buildd: [{}] つながった（{}）", self.name, clean_name(&client).unwrap_or_default());
                    self.send(&ServerMsg::Hello { v: zc_wire::PROTOCOL_VERSION, server: crate::SERVER_NAME.into() });
                }
                ClientMsg::Hello { v, .. } => self.fail(ErrorCode::Version, format!("約束の版が合わない（こちらは {}、相手は {v}）", zc_wire::PROTOCOL_VERSION)),
                _ => self.fail(ErrorCode::Protocol, "最初のメッセージは hello"),
            }
            return;
        }
        match m {
            ClientMsg::Hello { .. } => self.fail(ErrorCode::Protocol, "hello は 1 回だけ"),
            ClientMsg::Manifest { project, tree, files } => self.on_manifest(project, tree, files),
            ClientMsg::Job { job, project, tree, kind, lang, opts, log } => self.on_job(job, project, tree, kind, lang, opts, log),
            ClientMsg::Attach { job, log, artifact } => match self.sh.registry.attach(&job, log, artifact) {
                Ok(w) => self.watch(w),
                Err(_) => self.job_error(&job, ErrorCode::UnknownJob, "知らないジョブ", None),
            },
            ClientMsg::LspWarm { project, tree, path } => self.on_lsp_warm(project, tree, path),
            ClientMsg::Lsp { id, project, tree, query, path, line, col, text } => self.on_lsp(Query { id, query, path, line, col, text }, project, tree),
            ClientMsg::Cancel { job } => {
                // この接続が見ていないジョブなら、終わりの知らせが届くように付けてから取り消す。
                if !self.watching.iter().any(|w| w.watcher.job() == job) {
                    match self.sh.registry.attach(&job, u64::MAX, u64::MAX) {
                        Ok(w) => self.watch(w),
                        Err(_) => return self.job_error(&job, ErrorCode::UnknownJob, "知らないジョブ", None),
                    }
                }
                if self.sh.registry.cancel(&job).is_err() {
                    self.job_error(&job, ErrorCode::UnknownJob, "知らないジョブ", None);
                }
            }
        }
    }

    fn on_manifest(&mut self, project: String, tree: String, files: Vec<FileEntry>) {
        if !is_hex(&project, 16) || !is_hex(&tree, 64) {
            return self.fail(ErrorCode::BadTree, "project / tree の形が違う");
        }
        if files.len() > MAX_TREE_FILES {
            return self.fail(ErrorCode::TooLarge, format!("ファイルが多すぎる（{} 個。上限 {MAX_TREE_FILES}）", files.len()));
        }
        let manifest = match Manifest::new(files) {
            Ok(m) => m,
            Err(ManifestError::TooLarge(what)) => return self.fail(ErrorCode::TooLarge, format!("上限を超えた: {what}")),
            Err(e) => return self.fail(ErrorCode::BadTree, format!("目録を受け取れない: {e}")),
        };
        if manifest.total_bytes() > MAX_TREE_BYTES {
            return self.fail(ErrorCode::TooLarge, "木の合計が大きすぎる");
        }
        if manifest.tree_id() != tree {
            return self.fail(ErrorCode::BadTree, "tree id が目録と合わない");
        }
        let store = match self.sh.registry.store() {
            Ok(s) => s,
            Err(e) => return self.fail(ErrorCode::Internal, format!("置き場を開けない: {e}")),
        };
        let blobs = store.missing(&manifest);
        let mut need = Vec::with_capacity(blobs.len());
        for sha in &blobs {
            match manifest.files().iter().find(|f| &f.sha256 == sha) {
                Some(f) => need.push((sha.clone(), f.size)),
                None => return self.fail(ErrorCode::Internal, "置き場が、目録に無い中身を返した"),
            }
        }
        // 前の目録の受け取りが途中なら、捨てて新しい方に替える。
        self.upload = None;
        self.send(&ServerMsg::Need { tree: tree.clone(), blobs });
        let up = Upload { project, tree, manifest: Arc::new(manifest), need, idx: 0, got: 0, writer: None };
        if up.need.is_empty() {
            self.mark_ready(up);
        } else {
            self.upload = Some(up);
        }
    }

    fn mark_ready(&mut self, up: Upload) {
        self.ready.retain(|r| r.tree != up.tree || r.project != up.project);
        if self.ready.len() >= READY_TREES {
            self.ready.remove(0);
        }
        self.send(&ServerMsg::Ready { tree: up.tree.clone() });
        self.ready.push(ReadyTree { project: up.project, tree: up.tree, manifest: up.manifest });
    }

    fn on_binary(&mut self, frame: &[u8]) {
        if !self.hello {
            return self.fail(ErrorCode::Protocol, "最初のメッセージは hello");
        }
        let Some((h, data)) = BinHeader::split(frame) else {
            return self.fail(ErrorCode::Protocol, "バイナリフレームの頭が読めない");
        };
        if h.kind != BinKind::Blob || data.len() > CHUNK_BYTES {
            return self.fail(ErrorCode::Protocol, "受け取れないバイナリフレーム");
        }
        let registry = &{ self.sh }.registry;
        let Some(up) = self.upload.as_mut() else {
            return self.fail(ErrorCode::Protocol, "need を出していないのに中身が来た");
        };
        let (sha, size) = up.need[up.idx].clone();
        if h.id as usize != up.idx || h.offset != up.got || up.got + data.len() as u64 > size {
            return self.fail(ErrorCode::BadBlob, "中身の順番か大きさが違う");
        }
        let step = (|| -> Result<bool, StoreError> {
            if up.writer.is_none() {
                up.writer = Some(registry.store().map_err(StoreError::Io)?.begin_blob(&sha, size)?);
            }
            if let Some(w) = up.writer.as_mut() {
                w.write(h.offset, data)?;
            }
            up.got += data.len() as u64;
            if !h.last {
                return Ok(false);
            }
            if let Some(w) = up.writer.take() {
                w.finish()?;
            }
            up.idx += 1;
            up.got = 0;
            Ok(up.idx == up.need.len())
        })();
        match step {
            Ok(false) => {}
            Ok(true) => {
                if let Some(up) = self.upload.take() {
                    self.mark_ready(up);
                }
            }
            Err(StoreError::Mismatch) => self.fail(ErrorCode::BadBlob, "中身の大きさかハッシュが合わない"),
            Err(StoreError::TooLarge) => self.fail(ErrorCode::TooLarge, "置き場の上限を超えた"),
            Err(StoreError::Io(e)) => self.fail(ErrorCode::Internal, format!("置き場に書けない: {e}")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_job(&mut self, job: String, project: String, tree: String, kind: Kind, lang: Lang, opts: JobOpts, log: bool) {
        if lang == Lang::Unknown {
            // これより新しい実機の言語。そのジョブだけ断り、接続は続ける。
            return self.job_error(&job, ErrorCode::Protocol, zc_wire::msg::UNSUPPORTED_LANG, None);
        }
        let Some(manifest) = self.ready.iter().find(|r| r.tree == tree && r.project == project).map(|r| r.manifest.clone()) else {
            return self.job_error(&job, ErrorCode::BadTree, "この接続で ready を受けた木ではない", None);
        };
        if zc_wire::tree::job_id(&tree, kind, lang, &opts) != job {
            return self.job_error(&job, ErrorCode::BadTree, "job id が、木・種類・オプションと合わない", None);
        }
        let again = self.watching.iter().any(|w| w.watcher.job() == job);
        if !again && self.watching.len() >= MAX_WATCHED {
            return self.job_error(&job, ErrorCode::Limit, "この接続で見ているジョブが多すぎる", Some(5));
        }
        let store = match self.sh.registry.store() {
            Ok(s) => s,
            Err(e) => return self.fail(ErrorCode::Internal, format!("置き場を開けない: {e}")),
        };
        let p = project.clone();
        let prepare = Box::new(move || store.sync_tree(&p, &manifest).map(|_| ()).map_err(|e| format!("作業の木を作れない: {e:?}")));
        eprintln!("zc-buildd: [{}] job {} {kind:?} {lang:?}", self.name, &job[..8]);
        let spec = JobSpec { job: job.clone(), project, tree, kind, lang, opts, device: self.device.clone() };
        match self.sh.registry.submit_prepared(spec, log, prepare) {
            Ok(w) => self.watch(w),
            Err(SubmitError::QueueFull) => self.job_error(&job, ErrorCode::Limit, "待ちの列がいっぱい", Some(5)),
            Err(_) => self.job_error(&job, ErrorCode::BadTree, "job id の形が違う", None),
        }
    }

    /// この接続で ready を受けた木の目録。
    fn ready_manifest(&self, project: &str, tree: &str) -> Option<Arc<Manifest>> {
        self.ready.iter().find(|r| r.tree == tree && r.project == project).map(|r| r.manifest.clone())
    }

    fn on_lsp_warm(&mut self, project: String, tree: String, path: String) {
        let Some(manifest) = self.ready_manifest(&project, &tree) else {
            // 木がそろっていない。状態で知らせる（接続は続ける）
            let m = ServerMsg::LspState { cpu_ms: self.sh.lsp.cpu_ms(), project, state: zc_wire::msg::LspPhase::Failed, message: "この接続で ready を受けた木ではない".into(), error: Some(ErrorCode::BadTree) };
            return self.send(&m);
        };
        let store = match self.sh.registry.store() {
            Ok(s) => s,
            Err(e) => return self.fail(ErrorCode::Internal, format!("置き場を開けない: {e}")),
        };
        self.sh.lsp.warm(&store, &project, &manifest, &path, self.id, &self.lsp_tx);
    }

    fn on_lsp(&mut self, q: Query, project: String, tree: String) {
        let id = q.id;
        let err = |this: &mut Self, code: ErrorCode, message: &str| {
            let m = ServerMsg::LspResult { cpu_ms: this.sh.lsp.cpu_ms(), id, error: Some(code), message: message.into(), text: String::new(), items: vec![], retry_after_s: None };
            this.send(&m);
        };
        let Some(manifest) = self.ready_manifest(&project, &tree) else {
            return err(self, ErrorCode::BadTree, "この接続で ready を受けた木ではない");
        };
        if self.lsp_pending.len() >= MAX_LSP_PENDING {
            return err(self, ErrorCode::Limit, "問い合わせが多すぎる");
        }
        let store = match self.sh.registry.store() {
            Ok(s) => s,
            Err(e) => return self.fail(ErrorCode::Internal, format!("置き場を開けない: {e}")),
        };
        self.lsp_pending.retain(|(p, _)| *p != id);
        self.lsp_pending.push((id, Instant::now() + LSP_TIMEOUT));
        self.sh.lsp.query(&store, &project, &manifest, q, &self.lsp_tx);
    }

    /// rust-analyzer から届いた答え・状態を流す。待ちすぎた問い合わせは時間切れで返す。
    fn pump_lsp(&mut self, now: Instant) {
        while self.pending() < SEND_HIGH_WATER {
            let Ok(m) = self.lsp_rx.try_recv() else { break };
            if let ServerMsg::LspResult { id, .. } = &m {
                // 時間切れで返したもの・知らないものは捨てる
                let Some(i) = self.lsp_pending.iter().position(|(p, _)| p == id) else { continue };
                self.lsp_pending.swap_remove(i);
            }
            self.send(&m);
        }
        let mut expired = Vec::new();
        self.lsp_pending.retain(|(id, at)| {
            let keep = *at > now;
            if !keep {
                expired.push(*id);
            }
            keep
        });
        for id in expired {
            let m = ServerMsg::LspResult { cpu_ms: self.sh.lsp.cpu_ms(), id, error: Some(ErrorCode::Internal), message: format!("{} が答えない（時間切れ。冷えた起動の最初の 1 回は遅いことがある）", self.sh.lsp.server_name()), text: String::new(), items: vec![], retry_after_s: None };
            self.send(&m);
        }
    }

    /// 見る人を足す。同じジョブを見ていたら入れ替える。
    fn watch(&mut self, w: Watcher) {
        self.watching.retain(|x| x.watcher.job() != w.job());
        if self.watching.len() >= MAX_WATCHED {
            let job = w.job().to_string();
            return self.job_error(&job, ErrorCode::Limit, "この接続で見ているジョブが多すぎる", Some(5));
        }
        self.watching.push(Watching { watcher: w, art: None });
    }

    /// 見ているジョブから続きを取って、送り待ちに足す（送り待ちが多い間は取らない）。
    fn pump(&mut self) {
        let mut progress = true;
        while progress {
            progress = false;
            let mut i = 0;
            while i < self.watching.len() {
                if self.pending() >= SEND_HIGH_WATER || self.closing.is_some() {
                    return;
                }
                match self.pump_one(i) {
                    Step::Sent => progress = true,
                    Step::Idle => {}
                    Step::Finished => {
                        self.watching.remove(i);
                        progress = true;
                        continue;
                    }
                    Step::Lagged => {
                        // 続きがログの上限からこぼれた。閉じる（相手はつなぎ直して attach する）。
                        eprintln!("zc-buildd: [{}] 閉じる: ログの受け取りが遅い", self.name);
                        return self.close(1013);
                    }
                }
                i += 1;
            }
        }
    }

    fn pump_one(&mut self, i: usize) -> Step {
        if let Some(art) = self.watching[i].art.as_mut() {
            let left = art.zbytes.saturating_sub(art.offset);
            let mut chunk = vec![0u8; left.min(CHUNK_BYTES as u64) as usize];
            if art.file.read_exact(&mut chunk).is_err() {
                // 成果物のファイルが読めない（短い・消えた）。このジョブの見る人を外す。
                let job = self.watching[i].watcher.job().to_string();
                self.job_error(&job, ErrorCode::Internal, "成果物を読めない", None);
                return Step::Finished;
            }
            let h = BinHeader { kind: BinKind::Artifact, last: art.offset + chunk.len() as u64 >= art.zbytes, id: art.chan, offset: art.offset };
            art.offset += chunk.len() as u64;
            if h.last {
                self.watching[i].art = None;
            }
            self.enc.binary2(&h.encode(), &chunk, [0; 4], &mut self.out);
            return Step::Sent;
        }
        match self.watching[i].watcher.recv(Duration::ZERO) {
            Ok(Some(Feed::Msg(m))) => {
                self.send(&m);
                Step::Sent
            }
            Ok(Some(Feed::ArtifactFile { chan, path, offset, zbytes })) => {
                let opened = File::open(&path).and_then(|mut f| f.seek(SeekFrom::Start(offset)).map(|_| f));
                match opened {
                    Ok(file) => {
                        self.watching[i].art = Some(ArtStream { file, chan, offset, zbytes });
                        Step::Sent
                    }
                    Err(_) => {
                        let job = self.watching[i].watcher.job().to_string();
                        self.job_error(&job, ErrorCode::Internal, "成果物を読めない", None);
                        Step::Finished
                    }
                }
            }
            Ok(None) => Step::Idle,
            Err(WatchEnd::Finished) => Step::Finished,
            Err(WatchEnd::Lagged) => Step::Lagged,
        }
    }
}
