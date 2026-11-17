//! 接続の状態機械の受け入れ試験（課題 B1。docs/stage3-design.md §4.1・§5）。
//!
//! 本物の受付をループバックに立てて、WebSocket の相手を演じる。実行の口（[`Runner`]）だけ
//! 偽物。
//!
//! 受付は本物の `zc-wire`（課題 W1・W2）と中身の置き場（課題 B2）を通る。相手の役も
//! `zc-wire` の公開の関数だけで書いてある。

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zc_buildd::jobs::LOG_OMITTED;
use zc_buildd::pack::Packed;
use zc_buildd::runner::{JobCtx, JobFailure, JobOutput, JobResult, Runner};
use zc_buildd::server::{self, Handle, Options};
use zc_buildd::Config;
use zc_wire::http::{self, UpgradeRequest, UpgradeResponse};
use zc_wire::msg::{self, ArtifactKind, BinHeader, BinKind, ClientMsg, Diag, ErrorCode, FileDiags, FileEntry, JobOpts, Kind, Lang, ServerMsg, Severity, Status};
use zc_wire::tree::{self, Manifest};
use zc_wire::ws::{Decoder, Encoder, Message, Role};

const WAIT: Duration = Duration::from_secs(10);
const PROJECT: &str = "00000000000000a1";

// ───────────── 偽物の実行の口 ─────────────

/// 成果物の中身（圧縮済みの流れの役。3 フレームに分かれる大きさ）。
fn zdata() -> Vec<u8> {
    let mut x = 0x2545_f491u32;
    (0..150_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

/// 合図まで止めるための門。
#[derive(Clone)]
struct Gate {
    tx: Sender<()>,
    rx: Arc<Mutex<Receiver<()>>>,
}

impl Gate {
    fn new() -> Gate {
        let (tx, rx) = channel();
        Gate { tx, rx: Arc::new(Mutex::new(rx)) }
    }
    fn open(&self) {
        self.tx.send(()).unwrap();
    }
    fn pass(&self) {
        let _ = self.rx.lock().unwrap().recv_timeout(Duration::from_secs(30));
    }
}

/// 木の `main.rs` の中身で振る舞いを変える偽物。
///
/// - build: `main.rs` をログに出す（作業の木に写っていることの確認）。`WAIT` を含めば門で
///   止まる。`FLOOD` を含めば 24MB のログを出してから門で止まる。最後に成果物を出す
/// - check: 診断を 1 件返して失敗（終了コード 101）
/// - test: 取り消されるまで終わらない
struct Fake {
    gate: Gate,
    runs: AtomicUsize,
}

impl Runner for Fake {
    fn run(&self, ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let main = std::fs::read_to_string(ctx.src.join("src/main.rs")).map_err(|e| JobFailure { status: Status::Error, message: format!("main.rs: {e}") })?;
        match ctx.kind {
            Kind::Build => {
                out.log(b"   Compiling foo v0.1.0\n");
                out.log(main.as_bytes());
                if main.contains("FLOOD") {
                    for i in 0..240_000 {
                        out.log(format!("{i:099}\n").as_bytes());
                    }
                    self.gate.pass();
                }
                if main.contains("WAIT") {
                    self.gate.pass();
                }
                out.log(b"    Finished\n");
                let zpath = ctx.out_dir.join("artifact.zst");
                let z = zdata();
                std::fs::write(&zpath, &z).unwrap();
                Ok(JobResult {
                    exit: 0,
                    complete: true,
                    artifact: Some(Packed { kind: ArtifactKind::Exe, name: "foo".into(), sha256: "cd".repeat(32), bytes: 400_000, zpath, zbytes: z.len() as u64 }),
                    ..JobResult::default()
                })
            }
            Kind::Check => Ok(JobResult {
                exit: 101,
                complete: true,
                diagnostics: vec![FileDiags { path: "src/main.rs".into(), diags: vec![Diag { line: 0, col: Some(3), end: Some((0, 7)), severity: Severity::Error, code: Some("E0308".into()), message: "mismatched types".into() }] }],
                ..JobResult::default()
            }),
            Kind::Test => {
                while !ctx.cancelled.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(JobResult::default())
            }
        }
    }
}

// ───────────── 受付を立てる ─────────────

struct Server {
    handle: Handle,
    fake: Arc<Fake>,
    dir: PathBuf,
}

impl Server {
    fn start(name: &str) -> Server {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("zc-buildd-proto-{}-{}-{name}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::defaults();
        cfg.listen = "127.0.0.1:0".parse().unwrap();
        cfg.data_dir = dir.clone();
        let fake = Arc::new(Fake { gate: Gate::new(), runs: AtomicUsize::new(0) });
        let handle = server::spawn_with(cfg, Options { runner: Some(fake.clone()), whois: None }).unwrap();
        Server { handle, fake, dir }
    }
    fn addr(&self) -> SocketAddr {
        self.handle.addr()
    }
    fn states(&self) -> Vec<&'static str> {
        self.handle.registry().status().into_iter().map(|j| j.state).collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        for _ in 0..8 {
            self.fake.gate.open();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let until = Instant::now() + WAIT;
    while !f() {
        assert!(Instant::now() < until, "待っても成り立たない: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// ふつうの GET。状態コードと本文。
fn http_get(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(WAIT)).unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).unwrap();
    let mut all = String::new();
    s.read_to_string(&mut all).unwrap();
    let status = all.split(' ').nth(1).unwrap().parse().unwrap();
    let body = all.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
}

// ───────────── 相手（実機）の役 ─────────────

#[derive(Debug, PartialEq)]
enum Ev {
    Msg(ServerMsg),
    Bin(BinHeader, Vec<u8>),
    /// close を受けた（状態コード）。`None` は、close なしで切れた。
    Closed(Option<u16>),
}

struct Client {
    s: TcpStream,
    dec: Decoder,
    enc: Encoder,
    mask: u32,
}

impl Client {
    fn connect(addr: SocketAddr) -> Client {
        Client::connect_with(addr, &[]).expect("Upgrade できるはず")
    }

    /// Upgrade する。断られたら状態コード。
    fn connect_with(addr: SocketAddr, headers: &[(String, String)]) -> Result<Client, u16> {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(WAIT)).unwrap();
        s.set_nodelay(true).unwrap();
        let key = http::encode_key(*b"0123456789abcdef");
        let mut req = Vec::new();
        UpgradeRequest { host: "builder:7411", path: zc_wire::WS_PATH, key: &key, headers }.write(&mut req);
        s.write_all(&req).unwrap();
        let mut buf = Vec::new();
        let consumed = loop {
            match http::parse_upgrade_response(&buf, &key) {
                UpgradeResponse::Incomplete => {
                    let mut b = [0u8; 4096];
                    let n = s.read(&mut b).unwrap();
                    assert!(n > 0, "返事の途中で切れた");
                    buf.extend_from_slice(&b[..n]);
                }
                UpgradeResponse::Switched { consumed } => break consumed,
                UpgradeResponse::Rejected { status, .. } => return Err(status),
                UpgradeResponse::Invalid(why) => panic!("返事が読めない: {why}"),
            }
        };
        let mut dec = Decoder::new(Role::Client, 4 << 20);
        dec.push(&buf[consumed..]);
        Ok(Client { s, dec, enc: Encoder::new(Role::Client), mask: 1 })
    }

    /// `hello` まで済ませる。
    fn open(addr: SocketAddr) -> Client {
        let mut c = Client::connect(addr);
        c.send(&ClientMsg::Hello { v: 1, client: "zc-remote/test".into() });
        assert_eq!(c.msg(), ServerMsg::Hello { v: 1, server: zc_buildd::SERVER_NAME.into() });
        c
    }

    fn next_mask(&mut self) -> [u8; 4] {
        self.mask = self.mask.wrapping_mul(1_103_515_245).wrapping_add(12345);
        self.mask.to_le_bytes()
    }

    fn send(&mut self, m: &ClientMsg) {
        self.send_text(&msg::encode(m));
    }

    fn send_text(&mut self, text: &str) {
        let (mut out, mask) = (Vec::new(), self.next_mask());
        self.enc.text(text, mask, &mut out);
        self.s.write_all(&out).unwrap();
    }

    fn send_bin(&mut self, h: BinHeader, data: &[u8]) {
        let (mut out, mask) = (Vec::new(), self.next_mask());
        self.enc.binary2(&h.encode(), data, mask, &mut out);
        self.s.write_all(&out).unwrap();
    }

    /// 次に届いたもの（ping には pong を返して読み飛ばす）。
    fn recv(&mut self) -> Ev {
        loop {
            let mut pong: Option<Vec<u8>> = None;
            match self.dec.next().expect("受付は約束どおりの枠を送るはず") {
                Some(Message::Text(t)) => match msg::decode::<ServerMsg>(t).expect("読める JSON のはず") {
                    Some(m) => return Ev::Msg(m),
                    None => continue,
                },
                Some(Message::Binary(b)) => {
                    let (h, data) = BinHeader::split(b).expect("頭つきのはず");
                    return Ev::Bin(h, data.to_vec());
                }
                Some(Message::Ping(d)) => pong = Some(d.to_vec()),
                Some(Message::Pong(_)) => continue,
                Some(Message::Close(code)) => return Ev::Closed(Some(code)),
                None => {}
            }
            if let Some(d) = pong {
                let (mut out, mask) = (Vec::new(), self.next_mask());
                self.enc.pong(&d, mask, &mut out);
                self.s.write_all(&out).unwrap();
                continue;
            }
            let mut b = vec![0u8; 64 << 10];
            match self.s.read(&mut b) {
                Ok(0) => return Ev::Closed(None),
                Ok(n) => self.dec.push(&b[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => return Ev::Closed(None),
                Err(e) => panic!("時間内に届かない: {e}"),
            }
        }
    }

    fn msg(&mut self) -> ServerMsg {
        match self.recv() {
            Ev::Msg(m) => m,
            other => panic!("メッセージのはず: {other:?}"),
        }
    }

    /// `done` まで（`done` も含めて）全部受け取る。
    fn until_done(&mut self) -> Vec<Ev> {
        let mut v = Vec::new();
        loop {
            let e = self.recv();
            let end = matches!(e, Ev::Msg(ServerMsg::Done { .. }) | Ev::Closed(_));
            v.push(e);
            if end {
                return v;
            }
        }
    }

    /// 接続全体の失敗（`job` のない `error`）と、そのあとの close を受け取る。
    fn expect_fatal(&mut self, code: ErrorCode) {
        match self.recv() {
            Ev::Msg(ServerMsg::Error { job: None, code: c, .. }) => assert_eq!(c, code),
            other => panic!("error {{{code:?}}} のはず: {other:?}"),
        }
        assert!(matches!(self.recv(), Ev::Closed(Some(_))), "error のあとは close");
        assert_eq!(self.recv(), Ev::Closed(None), "そのあと切れる");
    }
}

// ───────────── 木と依頼 ─────────────

fn sha(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

struct Tree {
    files: Vec<(String, Vec<u8>)>,
    entries: Vec<FileEntry>,
    id: String,
}

fn tree_of(files: &[(&str, &str)]) -> Tree {
    let files: Vec<(String, Vec<u8>)> = files.iter().map(|(p, c)| (p.to_string(), c.as_bytes().to_vec())).collect();
    let entries: Vec<FileEntry> = files.iter().map(|(p, c)| FileEntry { path: p.clone(), size: c.len() as u64, sha256: sha(c), exec: false }).collect();
    let id = Manifest::new(entries.clone()).unwrap().tree_id();
    Tree { files, entries, id }
}

/// `main.rs` の中身だけが違う小さな木。
fn project(main: &str) -> Tree {
    tree_of(&[("Cargo.toml", "[package]\nname = \"foo\"\n"), ("src/main.rs", main)])
}

impl Tree {
    fn manifest(&self) -> ClientMsg {
        ClientMsg::Manifest { project: PROJECT.into(), tree: self.id.clone(), files: self.entries.clone() }
    }

    fn content(&self, sha256: &str) -> &[u8] {
        &self.files.iter().find(|(_, c)| sha(c) == sha256).expect("目録にある中身のはず").1
    }

    /// 目録を送り、言われた中身を送り、`ready` を受ける。送った中身の数を返す。
    fn upload(&self, c: &mut Client) -> usize {
        c.send(&self.manifest());
        let blobs = match c.msg() {
            ServerMsg::Need { tree, blobs } => {
                assert_eq!(tree, self.id);
                blobs
            }
            other => panic!("need のはず: {other:?}"),
        };
        for (i, h) in blobs.iter().enumerate() {
            // 2 フレームに分けて送る。
            let data = self.content(h);
            let mid = data.len() / 2;
            c.send_bin(BinHeader { kind: BinKind::Blob, last: false, id: i as u32, offset: 0 }, &data[..mid]);
            c.send_bin(BinHeader { kind: BinKind::Blob, last: true, id: i as u32, offset: mid as u64 }, &data[mid..]);
        }
        assert_eq!(c.msg(), ServerMsg::Ready { tree: self.id.clone() });
        blobs.len()
    }

    fn job_id(&self, kind: Kind) -> String {
        tree::job_id(&self.id, kind, Lang::Rust, &JobOpts::default())
    }

    fn job(&self, kind: Kind, log: bool) -> ClientMsg {
        ClientMsg::Job { job: self.job_id(kind), project: PROJECT.into(), tree: self.id.clone(), kind, lang: Lang::Rust, opts: JobOpts::default(), log }
    }
}

/// 種類の並び（続いた `log` と、続いたバイナリは 1 つに数える）。
fn shape(evs: &[Ev]) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    for e in evs {
        let name = match e {
            Ev::Msg(ServerMsg::Accepted { .. }) => "accepted",
            Ev::Msg(ServerMsg::Started { .. }) => "started",
            Ev::Msg(ServerMsg::Log { .. }) => "log",
            Ev::Msg(ServerMsg::Diagnostics { .. }) => "diagnostics",
            Ev::Msg(ServerMsg::Artifact { .. }) => "artifact",
            Ev::Msg(ServerMsg::Done { .. }) => "done",
            Ev::Msg(ServerMsg::Error { .. }) => "error",
            Ev::Msg(_) => "other",
            Ev::Bin(..) => "bytes",
            Ev::Closed(_) => "closed",
        };
        if !(matches!(name, "log" | "bytes") && v.last() == Some(&name)) {
            v.push(name);
        }
    }
    v
}

/// ログをつないだもの（始まりの位置つき）。offset が続いていることも確かめる。
fn log_of(evs: &[Ev]) -> (u64, String) {
    let (mut start, mut text) = (None, String::new());
    for e in evs {
        if let Ev::Msg(ServerMsg::Log { offset, text: t, .. }) = e {
            let s = *start.get_or_insert(*offset);
            assert_eq!(*offset, s + text.len() as u64, "ログの offset が続いていない");
            text.push_str(t);
        }
    }
    (start.unwrap_or(0), text)
}

/// 成果物のバイナリフレームをつないだもの（始まりの位置つき）。位置が続いていること、
/// 最後のフレームにだけ「最後」の印があることも確かめる。
fn artifact_of(evs: &[Ev]) -> (u64, Vec<u8>) {
    let frames: Vec<(&BinHeader, &Vec<u8>)> = evs.iter().filter_map(|e| if let Ev::Bin(h, d) = e { Some((h, d)) } else { None }).collect();
    let (mut start, mut data) = (None, Vec::new());
    for (i, (h, d)) in frames.iter().enumerate() {
        assert_eq!((h.kind, h.id), (BinKind::Artifact, 1));
        assert!(d.len() <= zc_wire::limits::CHUNK_BYTES);
        let s = *start.get_or_insert(h.offset);
        assert_eq!(h.offset, s + data.len() as u64, "成果物の位置が続いていない");
        assert_eq!(h.last, i + 1 == frames.len());
        data.extend_from_slice(d);
    }
    (start.unwrap_or(0), data)
}

fn done_of(evs: &[Ev]) -> Status {
    match evs.last() {
        Some(Ev::Msg(ServerMsg::Done { status, .. })) => *status,
        other => panic!("最後は done のはず: {other:?}"),
    }
}

// ───────────── HTTP の口 ─────────────

#[test]

fn healthz_status_and_other_paths() {
    let sv = Server::start("http");
    assert_eq!(http_get(sv.addr(), "/healthz"), (200, "ok".to_string()));
    let (code, body) = http_get(sv.addr(), "/v1/status");
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!((v["running"].as_u64(), v["queued"].as_u64(), v["jobs"].as_array().map(|a| a.len())), (Some(0), Some(0), Some(0)));
    assert!(v["uptime_s"].is_u64());
    assert_eq!(http_get(sv.addr(), "/nothing").0, 404);
    assert_eq!(http_get(sv.addr(), "/v1/ws").0, 426, "Upgrade でない /v1/ws");

    // 走っているジョブが /v1/status に出る。
    let mut c = Client::open(sv.addr());
    let t = project("fn main() {} // WAIT\n");
    t.upload(&mut c);
    c.send(&t.job(Kind::Build, true));
    wait_until("走り始める", || sv.states() == ["running"]);
    let v: serde_json::Value = serde_json::from_str(&http_get(sv.addr(), "/v1/status").1).unwrap();
    assert_eq!(v["running"], 1);
    assert_eq!(v["jobs"][0], serde_json::json!({ "job": t.job_id(Kind::Build), "project": PROJECT, "kind": "build", "state": "running" }));
}

#[test]

fn upgrade_needs_the_right_path_and_subprotocol() {
    let sv = Server::start("upgrade");
    // サブプロトコルなしの Upgrade は断る。
    let mut s = TcpStream::connect(sv.addr()).unwrap();
    s.set_read_timeout(Some(WAIT)).unwrap();
    s.write_all(b"GET /v1/ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").unwrap();
    let mut all = String::new();
    s.read_to_string(&mut all).unwrap();
    assert!(all.starts_with("HTTP/1.1 400"), "{all}");
    // Worker が付ける名前があっても同じように話せる。
    let mut c = Client::connect_with(sv.addr(), &[("X-Zc-Device".to_string(), "cz-1".to_string())]).unwrap();
    c.send(&ClientMsg::Hello { v: 1, client: "zc-remote/test".into() });
    assert!(matches!(c.msg(), ServerMsg::Hello { v: 1, .. }));
}

// ───────────── ひととおり ─────────────

#[test]

fn full_flow_then_cached_and_no_reupload() {
    let sv = Server::start("flow");
    let mut c = Client::open(sv.addr());
    let t = project("fn main() { println!(\"こんにちは\"); }\n");
    assert_eq!(t.upload(&mut c), 2, "最初は 2 つとも送る");
    c.send(&t.job(Kind::Build, true));
    let evs = c.until_done();
    assert_eq!(shape(&evs), ["accepted", "started", "log", "diagnostics", "artifact", "bytes", "done"]);
    assert_eq!(evs[0], Ev::Msg(ServerMsg::Accepted { job: t.job_id(Kind::Build), cached: false, queued: 0 }));
    assert_eq!(log_of(&evs), (0, "   Compiling foo v0.1.0\nfn main() { println!(\"こんにちは\"); }\n    Finished\n".to_string()), "作業の木に写した main.rs を実行の口が読めている");
    let meta = evs.iter().find(|e| matches!(e, Ev::Msg(ServerMsg::Artifact { .. }))).unwrap();
    assert_eq!(*meta, Ev::Msg(ServerMsg::Artifact { job: t.job_id(Kind::Build), chan: 1, kind: ArtifactKind::Exe, name: "foo".into(), sha256: "cd".repeat(32), bytes: 400_000, zbytes: 150_000, offset: 0 }));
    assert_eq!(artifact_of(&evs), (0, zdata()));
    assert!(matches!(evs.last(), Some(Ev::Msg(ServerMsg::Done { status: Status::Ok, exit: 0, .. }))));

    // 同じ依頼をもう一度: 実行せずに、残っていた結果を流す。
    c.send(&t.job(Kind::Build, true));
    let again = c.until_done();
    assert_eq!(again[0], Ev::Msg(ServerMsg::Accepted { job: t.job_id(Kind::Build), cached: true, queued: 0 }));
    assert_eq!(shape(&again), ["accepted", "log", "diagnostics", "artifact", "bytes", "done"]);
    assert_eq!(artifact_of(&again), (0, zdata()));
    assert_eq!(sv.fake.runs.load(Ordering::SeqCst), 1);

    // 1 ファイルだけ変えた木: 送るのは変わった中身 1 つ。check は失敗と診断。
    let t2 = project("fn main() { let x: u8 = \"\"; }\n");
    assert_eq!(t2.upload(&mut c), 1);
    c.send(&t2.job(Kind::Check, false));
    let evs = c.until_done();
    assert_eq!(shape(&evs), ["accepted", "started", "diagnostics", "done"], "log:false にはログを送らない");
    assert!(matches!(&evs[2], Ev::Msg(ServerMsg::Diagnostics { complete: true, truncated: false, files, .. }) if files[0].path == "src/main.rs" && files[0].diags[0].col == Some(3)));
    assert!(matches!(evs.last(), Some(Ev::Msg(ServerMsg::Done { status: Status::Failed, exit: 101, .. }))));

    // 別の接続から同じ木: 中身は 1 つも要らない。前の木のジョブも、この接続で ready を
    // 受け直せば頼める。
    let mut d = Client::open(sv.addr());
    assert_eq!(t.upload(&mut d), 0);
    d.send(&t.job(Kind::Build, false));
    assert_eq!(shape(&d.until_done()), ["accepted", "diagnostics", "artifact", "bytes", "done"]);
}

// ───────────── 約束の間違い ─────────────

#[test]

fn hello_rules() {
    let sv = Server::start("hello");
    // 版が違う。
    let mut c = Client::connect(sv.addr());
    c.send(&ClientMsg::Hello { v: 2, client: "zc-remote/9".into() });
    c.expect_fatal(ErrorCode::Version);
    // hello の前にほかのもの。
    let mut c = Client::connect(sv.addr());
    c.send(&ClientMsg::Cancel { job: "0".repeat(32) });
    c.expect_fatal(ErrorCode::Protocol);
    let mut c = Client::connect(sv.addr());
    c.send_bin(BinHeader { kind: BinKind::Blob, last: true, id: 0, offset: 0 }, b"x");
    c.expect_fatal(ErrorCode::Protocol);
    // JSON として読めない。
    let mut c = Client::connect(sv.addr());
    c.send_text("{\"type\":\"hello\",");
    c.expect_fatal(ErrorCode::Protocol);
    // hello は 1 回だけ。
    let mut c = Client::open(sv.addr());
    c.send(&ClientMsg::Hello { v: 1, client: "again".into() });
    c.expect_fatal(ErrorCode::Protocol);
    // 知らない type は無視して、続けられる。
    let mut c = Client::connect(sv.addr());
    c.send_text(r#"{"type":"future_thing","x":1}"#);
    c.send(&ClientMsg::Hello { v: 1, client: "zc-remote/test".into() });
    assert!(matches!(c.msg(), ServerMsg::Hello { .. }));
    c.send_text(r#"{"type":"another_future_thing"}"#);
    c.send(&ClientMsg::Attach { job: "0".repeat(32), log: 0, artifact: 0 });
    assert!(matches!(c.msg(), ServerMsg::Error { job: Some(_), code: ErrorCode::UnknownJob, .. }));
}

#[test]

fn job_errors_are_per_job_and_keep_the_connection() {
    let sv = Server::start("joberr");
    let mut c = Client::open(sv.addr());
    let t = project("fn main() {}\n");
    // ready を受けていない木。
    c.send(&t.job(Kind::Build, true));
    assert!(matches!(c.msg(), ServerMsg::Error { job: Some(j), code: ErrorCode::BadTree, .. } if j == t.job_id(Kind::Build)));
    t.upload(&mut c);
    // job id が、木・種類と合わない（check の id で build を頼む）。
    c.send(&ClientMsg::Job { job: t.job_id(Kind::Check), project: PROJECT.into(), tree: t.id.clone(), kind: Kind::Build, lang: Lang::Rust, opts: JobOpts::default(), log: true });
    assert!(matches!(c.msg(), ServerMsg::Error { job: Some(_), code: ErrorCode::BadTree, .. }));
    // 別のプロジェクトの名前で、この木を使う。
    c.send(&ClientMsg::Job { job: t.job_id(Kind::Build), project: "00000000000000b2".into(), tree: t.id.clone(), kind: Kind::Build, lang: Lang::Rust, opts: JobOpts::default(), log: true });
    assert!(matches!(c.msg(), ServerMsg::Error { job: Some(_), code: ErrorCode::BadTree, .. }));
    // 知らないジョブ。
    c.send(&ClientMsg::Attach { job: "ab".repeat(16), log: 0, artifact: 0 });
    assert!(matches!(c.msg(), ServerMsg::Error { job: Some(_), code: ErrorCode::UnknownJob, .. }));
    c.send(&ClientMsg::Cancel { job: "../../etc".into() });
    assert!(matches!(c.msg(), ServerMsg::Error { job: Some(_), code: ErrorCode::UnknownJob, .. }));
    assert_eq!(sv.fake.runs.load(Ordering::SeqCst), 0);
    // 接続は生きていて、正しい依頼は通る。
    c.send(&t.job(Kind::Build, true));
    assert_eq!(done_of(&c.until_done()), Status::Ok);
}

#[test]

fn bad_manifests_are_refused() {
    let sv = Server::start("manifest");
    let e = |p: &str, s: u64| FileEntry { path: p.into(), size: s, sha256: "ab".repeat(32), exec: false };
    let send = |files: Vec<FileEntry>, tree: String, project: &str| {
        let mut c = Client::open(sv.addr());
        c.send(&ClientMsg::Manifest { project: project.into(), tree, files });
        c
    };
    // 木の外を指すパス。
    for p in ["../x", "/etc/passwd", "a/../../b", "a\\b"] {
        send(vec![e(p, 1)], "0".repeat(64), PROJECT).expect_fatal(ErrorCode::BadTree);
    }
    // tree id が目録と合わない。
    send(vec![e("a.rs", 1)], "0".repeat(64), PROJECT).expect_fatal(ErrorCode::BadTree);
    // project が、フォルダの名前にできる形でない。
    let t = project("fn main() {}\n");
    send(t.entries.clone(), t.id.clone(), "../../../tmp/x").expect_fatal(ErrorCode::BadTree);
    // 上限。
    send((0..2001).map(|i| e(&format!("f{i}"), 1)).collect(), "0".repeat(64), PROJECT).expect_fatal(ErrorCode::TooLarge);
    send(vec![e("big", zc_wire::limits::MAX_FILE_BYTES + 1)], "0".repeat(64), PROJECT).expect_fatal(ErrorCode::TooLarge);
    assert!(!sv.dir.join("proj").join("x").exists());
}

#[test]

fn bad_blobs_are_refused() {
    let sv = Server::start("blob");
    let need = |c: &mut Client, t: &Tree| {
        c.send(&t.manifest());
        match c.msg() {
            ServerMsg::Need { blobs, .. } => blobs,
            other => panic!("{other:?}"),
        }
    };
    // 中身が、言ったハッシュと違う。
    let t = project("fn main() { /* 1 */ }\n");
    let mut c = Client::open(sv.addr());
    let blobs = need(&mut c, &t);
    let mut wrong = t.content(&blobs[0]).to_vec();
    wrong[0] ^= 1;
    c.send_bin(BinHeader { kind: BinKind::Blob, last: true, id: 0, offset: 0 }, &wrong);
    c.expect_fatal(ErrorCode::BadBlob);
    // 順番が違う（2 つ目から送る）。
    let mut c = Client::open(sv.addr());
    let blobs = need(&mut c, &t);
    assert_eq!(blobs.len(), 2, "合わなかった中身は置かれていない");
    c.send_bin(BinHeader { kind: BinKind::Blob, last: true, id: 1, offset: 0 }, t.content(&blobs[1]));
    c.expect_fatal(ErrorCode::BadBlob);
    // 目録より大きい。
    let mut c = Client::open(sv.addr());
    let blobs = need(&mut c, &t);
    let mut long = t.content(&blobs[0]).to_vec();
    long.push(b'x');
    c.send_bin(BinHeader { kind: BinKind::Blob, last: true, id: 0, offset: 0 }, &long);
    c.expect_fatal(ErrorCode::BadBlob);
    // need を出していないのに中身。
    let mut c = Client::open(sv.addr());
    c.send_bin(BinHeader { kind: BinKind::Blob, last: true, id: 0, offset: 0 }, b"x");
    c.expect_fatal(ErrorCode::Protocol);
    // 成果物の種類のフレームを実機が送ってくる。
    let mut c = Client::open(sv.addr());
    need(&mut c, &t);
    c.send_bin(BinHeader { kind: BinKind::Artifact, last: true, id: 0, offset: 0 }, b"x");
    c.expect_fatal(ErrorCode::Protocol);
    // 大きすぎるテキスト。
    let mut c = Client::open(sv.addr());
    c.send_text(&format!("{{\"type\":\"hello\",\"v\":1,\"client\":\"{}\"}}", "a".repeat(zc_wire::limits::MAX_TEXT_FRAME)));
    c.expect_fatal(ErrorCode::TooLarge);
}

// ───────────── 切れても続く・attach・cancel ─────────────

#[test]

fn job_survives_disconnect_and_attach_sends_only_the_rest() {
    let sv = Server::start("attach");
    let t = project("fn main() {} // WAIT\n");
    let job = t.job_id(Kind::Build);
    let mut c = Client::open(sv.addr());
    t.upload(&mut c);
    c.send(&t.job(Kind::Build, true));
    assert!(matches!(c.msg(), ServerMsg::Accepted { .. }));
    assert!(matches!(c.msg(), ServerMsg::Started { .. }));
    let first = match c.msg() {
        ServerMsg::Log { offset: 0, text, .. } => text,
        other => panic!("{other:?}"),
    };
    assert!(first.starts_with("   Compiling"));
    // 切れる。ジョブは続く。
    drop(c);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(sv.states(), ["running"]);
    sv.fake.gate.open();
    wait_until("終わる", || sv.states() == ["done"]);

    // つなぎ直す。目録は送らない。受け取り済みの位置（ログ・成果物 70000 バイト）を言う。
    let mut c = Client::open(sv.addr());
    c.send(&ClientMsg::Attach { job: job.clone(), log: first.len() as u64, artifact: 70_000 });
    let evs = c.until_done();
    assert_eq!(shape(&evs), ["started", "log", "diagnostics", "artifact", "bytes", "done"], "attach には accepted を出さない");
    assert_eq!(log_of(&evs), (first.len() as u64, "    Finished\n".to_string()), "ログは続きだけ");
    assert!(matches!(evs.iter().find(|e| matches!(e, Ev::Msg(ServerMsg::Artifact { .. }))), Some(Ev::Msg(ServerMsg::Artifact { offset: 70_000, zbytes: 150_000, .. }))));
    assert_eq!(artifact_of(&evs), (70_000, zdata()[70_000..].to_vec()), "成果物も続きだけ");
    assert_eq!(done_of(&evs), Status::Ok);

    // 全部受け取り済み: 成果物は「最後」の印だけの空のフレーム。
    c.send(&ClientMsg::Attach { job, log: u64::MAX, artifact: 150_000 });
    let evs = c.until_done();
    assert_eq!(shape(&evs), ["started", "diagnostics", "artifact", "bytes", "done"]);
    assert_eq!(artifact_of(&evs), (150_000, vec![]));
    assert_eq!(sv.fake.runs.load(Ordering::SeqCst), 1);
}

#[test]

fn attach_while_running_streams_the_rest_live() {
    let sv = Server::start("live");
    let t = project("fn main() {} // WAIT\n");
    let mut c = Client::open(sv.addr());
    t.upload(&mut c);
    c.send(&t.job(Kind::Build, true));
    drop(c);
    wait_until("走り始める", || sv.states() == ["running"]);
    let mut c = Client::open(sv.addr());
    c.send(&ClientMsg::Attach { job: t.job_id(Kind::Build), log: 0, artifact: 0 });
    assert!(matches!(c.msg(), ServerMsg::Started { .. }));
    assert!(matches!(c.msg(), ServerMsg::Log { offset: 0, .. }));
    sv.fake.gate.open();
    let evs = c.until_done();
    assert_eq!(shape(&evs), ["log", "diagnostics", "artifact", "bytes", "done"]);
    assert_eq!(artifact_of(&evs), (0, zdata()));
}

#[test]

fn cancel_ends_the_job_for_every_connection() {
    let sv = Server::start("cancel");
    let t = project("fn main() {}\n");
    let job = t.job_id(Kind::Test);
    let mut a = Client::open(sv.addr());
    t.upload(&mut a);
    a.send(&t.job(Kind::Test, true));
    assert!(matches!(a.msg(), ServerMsg::Accepted { .. }));
    assert!(matches!(a.msg(), ServerMsg::Started { .. }));
    // 2 本目の接続（同じ実機でも、別の実機でも）は、見ていないジョブも取り消せて、
    // 終わりの知らせを受ける。
    wait_until("実行の口が呼ばれる", || sv.fake.runs.load(Ordering::SeqCst) == 1);
    let mut b = Client::open(sv.addr());
    b.send(&ClientMsg::Cancel { job: job.clone() });
    let eb = b.until_done();
    assert_eq!(done_of(&eb), Status::Cancelled);
    assert!(eb.iter().any(|e| matches!(e, Ev::Msg(ServerMsg::Diagnostics { complete: false, .. }))));
    assert_eq!(done_of(&a.until_done()), Status::Cancelled);
    // 取り消しの結果は使い回さない: 同じ依頼はもう一度走る。
    a.send(&t.job(Kind::Test, true));
    assert_eq!(a.msg(), ServerMsg::Accepted { job: job.clone(), cached: false, queued: 0 });
    assert!(matches!(a.msg(), ServerMsg::Started { .. }));
    wait_until("実行の口が呼ばれる", || sv.fake.runs.load(Ordering::SeqCst) == 2);
    a.send(&ClientMsg::Cancel { job });
    assert_eq!(done_of(&a.until_done()), Status::Cancelled);
    assert_eq!(sv.fake.runs.load(Ordering::SeqCst), 2);
}

#[test]

fn two_connections_watch_the_same_job_and_a_repeat_does_not_duplicate() {
    let sv = Server::start("two");
    let t = project("fn main() {} // WAIT\n");
    let mut a = Client::open(sv.addr());
    let mut b = Client::open(sv.addr());
    t.upload(&mut a);
    t.upload(&mut b);
    a.send(&t.job(Kind::Build, true));
    b.send(&t.job(Kind::Build, true));
    // 同じ接続から同じジョブをもう一度頼む: 前の見る人と入れ替わる（2 重に流れない）。
    wait_until("走り始める", || sv.states() == ["running"]);
    a.send(&t.job(Kind::Build, true));
    sv.fake.gate.open();
    let (ea, eb) = (a.until_done(), b.until_done());
    assert_eq!(done_of(&ea), Status::Ok);
    assert_eq!(artifact_of(&eb), (0, zdata()));
    assert_eq!(ea.iter().filter(|e| matches!(e, Ev::Msg(ServerMsg::Diagnostics { .. }))).count(), 1);
    assert_eq!(sv.fake.runs.load(Ordering::SeqCst), 1);
    // そのあと何も余計に届かない（次に届くのは、こちらが頼んだものの返事）。
    a.send(&ClientMsg::Attach { job: "0".repeat(32), log: 0, artifact: 0 });
    assert!(matches!(a.msg(), ServerMsg::Error { code: ErrorCode::UnknownJob, .. }));
}

// ───────────── 遅い相手 ─────────────

#[test]

fn slow_client_is_dropped_but_the_job_goes_on() {
    let sv = Server::start("slow");
    let t = project("fn main() {} // FLOOD WAIT\n");
    let job = t.job_id(Kind::Build);
    let mut c = Client::open(sv.addr());
    t.upload(&mut c);
    c.send(&t.job(Kind::Build, true));
    assert!(matches!(c.msg(), ServerMsg::Accepted { .. }));
    // ここから読まない。受付は 24MB のログを抱え込まず（台帳の上限は 1MiB）、相手を待たず、
    // この接続を閉じる。
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(sv.states(), ["running"], "ジョブは、相手が読まなくても進む");
    let mut got = 0usize;
    let closed = loop {
        match c.recv() {
            Ev::Msg(ServerMsg::Log { text, .. }) => got += text.len(),
            Ev::Msg(ServerMsg::Started { .. }) => {}
            Ev::Closed(code) => break code,
            other => panic!("{other:?}"),
        }
    };
    assert!(got < 16 << 20, "全部は届かない（{got} バイト届いた）");
    assert!(closed.is_none() || closed == Some(1013), "{closed:?}");
    assert_eq!(sv.states(), ["running"], "ジョブは続いている");

    // つなぎ直して attach: 残っているログ（「省略」の印から）と結果を受け取れる。
    let mut c = Client::open(sv.addr());
    c.send(&ClientMsg::Attach { job, log: got as u64, artifact: 0 });
    sv.fake.gate.open();
    sv.fake.gate.open();
    let evs = c.until_done();
    let (_, text) = log_of(&evs);
    assert!(text.starts_with(LOG_OMITTED));
    assert!(text.ends_with("    Finished\n"));
    assert_eq!(artifact_of(&evs), (0, zdata()));
    assert_eq!(done_of(&evs), Status::Ok);
}

// ───────────── 止める ─────────────

#[test]

fn dropping_the_handle_closes_connections_and_stops_jobs() {
    let sv = Server::start("stop");
    let t = project("fn main() {}\n");
    let mut c = Client::open(sv.addr());
    t.upload(&mut c);
    c.send(&t.job(Kind::Test, true));
    assert!(matches!(c.msg(), ServerMsg::Accepted { .. }));
    assert!(matches!(c.msg(), ServerMsg::Started { .. }));
    let addr = sv.addr();
    drop(sv);
    let evs = c.until_done();
    assert!(matches!(evs.last(), Some(Ev::Msg(ServerMsg::Done { status: Status::Cancelled, .. })) | Some(Ev::Closed(_))), "{evs:?}");
    assert!(TcpStream::connect(addr).is_err() || http_get_fails(addr), "もう待ち受けていない");
}

fn http_get_fails(addr: SocketAddr) -> bool {
    let Ok(mut s) = TcpStream::connect(addr) else { return true };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.write_all(b"GET /healthz HTTP/1.1\r\n\r\n");
    let mut b = [0u8; 16];
    !matches!(s.read(&mut b), Ok(n) if n > 0)
}

/// LSP の口（docs/lsp-design.md）: ready を受けた木にだけ答える・答えは同じ id で必ず 1 つ・
/// 接続は続く。rust-analyzer があれば（ビルド係のイメージの中）、本物で hover まで通す。
#[test]
fn lsp_messages_over_the_socket() {
    use zc_wire::msg::{LspPhase, LspQuery};
    let srv = Server::start("lsp");
    let mut c = Client::open(srv.addr());
    c.s.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    let t = project("fn helper() -> u8 { 1 }\nfn main() { let _ = helper(); }\n");
    // 木がそろっていない
    c.send(&ClientMsg::Lsp { id: 5, project: PROJECT.into(), tree: t.id.clone(), query: LspQuery::Hover, path: "src/main.rs".into(), line: 1, col: 22, text: String::new() });
    assert!(matches!(c.msg(), ServerMsg::LspResult { id: 5, error: Some(ErrorCode::BadTree), .. }));
    c.send(&ClientMsg::LspWarm { project: PROJECT.into(), tree: t.id.clone(), path: "src/main.rs".into() });
    assert!(matches!(c.msg(), ServerMsg::LspState { state: LspPhase::Failed, error: Some(ErrorCode::BadTree), .. }));
    // 接続は続いている
    t.upload(&mut c);
    c.send(&ClientMsg::LspWarm { project: PROJECT.into(), tree: t.id.clone(), path: "src/main.rs".into() });
    let have_ra = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("rust-analyzer").is_file()));
    match c.msg() {
        ServerMsg::LspState { state: LspPhase::Loading, .. } if have_ra => {}
        ServerMsg::LspState { state: LspPhase::Failed, message, .. } if !have_ra => assert!(message.contains("rust-analyzer が無い"), "{message}"),
        other => panic!("{other:?}"),
    }
    let (_, body) = http_get(srv.addr(), "/v1/status");
    assert!(body.contains("\"lsp_cpu_ms\""), "{body}");
    if !have_ra {
        return;
    }
    loop {
        match c.msg() {
            ServerMsg::LspState { state: LspPhase::Ready, .. } => break,
            ServerMsg::LspState { state: LspPhase::Failed, message, .. } => panic!("{message}"),
            _ => {}
        }
    }
    c.send(&ClientMsg::Lsp { id: 6, project: PROJECT.into(), tree: t.id.clone(), query: LspQuery::Hover, path: "src/main.rs".into(), line: 1, col: 22, text: String::new() });
    match c.msg() {
        ServerMsg::LspResult { id: 6, error: None, text, cpu_ms, .. } => {
            assert!(text.contains("fn helper() -> u8"), "{text}");
            assert!(cpu_ms > 0);
        }
        other => panic!("{other:?}"),
    }
}
