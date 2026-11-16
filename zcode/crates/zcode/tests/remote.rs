//! zcode の専用スレッド（`zcode::remote`）を、偽物の serve（一時フォルダの UNIX ソケット）で
//! 試す（docs/stage3-design.md §13 の Z2）。

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use zc_core::rt::{main_channel, poll_fds, MainReceiver};
use zc_core::{Diagnostic, Severity};
use zc_remote_ipc::{
    decode_request, encode_response, Backend, ClientKind, DoneStatus, FileDiags, FrameReader, JobKind, JobTag, Phase, RemoteLang, ReqId, Request, Response, SubmitFlags, IPC_VERSION,
};
use zcode::app::AppEvent;
use zcode::remote::{RemoteClient, RemoteEvent, RemoteJob, RemoteReq, MIN_WAKE_INTERVAL};

/// 試験ごとに別の一時フォルダ（`<dir>/zcode/remote.sock` を置く）。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("zcr-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("zcode")).unwrap();
        TempDir(d)
    }
    fn socket(&self) -> PathBuf {
        self.0.join("zcode").join("remote.sock")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 偽物の serve の 1 接続。
struct Fake {
    stream: UnixStream,
    reader: FrameReader,
}

impl Fake {
    fn accept(listener: &UnixListener) -> Fake {
        listener.set_nonblocking(true).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    return Fake { stream, reader: FrameReader::new() };
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => panic!("accept: {e}"),
            }
        }
    }

    /// 次の依頼。相手が閉じたら `None`。
    fn recv(&mut self) -> Option<Request> {
        let mut buf = [0u8; 4096];
        loop {
            if let Some(f) = self.reader.next_frame().unwrap() {
                return Some(decode_request(f).unwrap());
            }
            let n = match self.stream.read(&mut buf) {
                Ok(n) => n,
                // 読み残しがあるまま閉じられると、UNIX ソケットでも ECONNRESET になる
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => 0,
                Err(e) => panic!("serve の読み込み: {e}"),
            };
            if n == 0 {
                return None;
            }
            self.reader.push(&buf[..n]);
        }
    }

    fn send(&mut self, r: &Response) {
        let mut buf = Vec::new();
        encode_response(r, &mut buf);
        self.stream.write_all(&buf).unwrap();
    }

    fn handshake(&mut self) {
        assert_eq!(self.recv(), Some(Request::Hello { version: IPC_VERSION, client: ClientKind::Editor }));
        self.send(&Response::Hello { version: IPC_VERSION });
    }
}

/// 届いた知らせを集める。`stop` を満たすものが来たら終わる。
fn collect(rx: &MainReceiver<AppEvent>, timeout: Duration, mut stop: impl FnMut(&RemoteEvent) -> bool) -> Vec<(Instant, RemoteEvent)> {
    let until = Instant::now() + timeout;
    let mut out = Vec::new();
    loop {
        let left = until.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "時間切れ。届いたもの: {:?}", out.iter().map(|(_, e)| e).collect::<Vec<_>>());
        let mut r = [false];
        poll_fds(&[rx.fd()], &mut r, left.as_millis() as i32 + 1).unwrap();
        let now = Instant::now();
        for ev in rx.drain() {
            let AppEvent::Remote(ev) = ev else { continue };
            let end = stop(&ev);
            out.push((now, ev));
            if end {
                return out;
            }
        }
    }
}

fn events(v: Vec<(Instant, RemoteEvent)>) -> Vec<RemoteEvent> {
    v.into_iter().map(|(_, e)| e).collect()
}

fn is_done(e: &RemoteEvent) -> bool {
    matches!(e, RemoteEvent::Done { .. })
}

fn check_job(file: &str) -> RemoteJob {
    RemoteJob { kind: JobKind::Check, lang: RemoteLang::Rust, flags: SubmitFlags::NONE, file: file.into() }
}

fn diag(line: u32) -> Diagnostic {
    Diagnostic { line, col: Some(2), end: Some((line, 5)), severity: Severity::Error, code: Some("E0308".into()), message: "mismatched types".into() }
}

fn no_program() -> Option<PathBuf> {
    Some(PathBuf::from("/nonexistent/zc-remote"))
}

/// 依頼 → 進み具合・診断（2 つに分けて届く）・終了が、この順でメインへ届く。
#[test]
fn submit_yields_events_then_done() {
    let dir = TempDir::new("flow");
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let (tx, rx) = main_channel().unwrap();
    let client = RemoteClient::with_socket(tx, no_program(), dir.socket());

    let req = client.submit(check_job("/p/src/main.rs"));
    let mut fake = Fake::accept(&listener);
    fake.handshake();
    let Some(Request::Submit { req: r, kind, lang, flags, file }) = fake.recv() else { panic!() };
    assert_eq!((r, kind, lang, flags, file.as_path()), (ReqId(req.0), JobKind::Check, RemoteLang::Rust, SubmitFlags::NONE, Path::new("/p/src/main.rs")));

    let tag = JobTag(77);
    fake.send(&Response::Phase { req: r, tag, root: "/p".into(), phase: Phase::Scanned, done: 0, total: 0 });
    fake.send(&Response::Diagnostics { req: r, tag, root: "/p".into(), complete: true, last: false, files: vec![FileDiags { path: "src/main.rs".into(), diags: vec![diag(1)] }] });
    fake.send(&Response::Diagnostics { req: r, tag, root: "/p".into(), complete: true, last: true, files: vec![FileDiags { path: "src/lib.rs".into(), diags: vec![diag(4)] }] });
    fake.send(&Response::Done { req: r, tag, status: DoneStatus::Failed, exit: 1, backend: Backend::Pc, elapsed_ms: 12, message: "".into() });

    let got = events(collect(&rx, Duration::from_secs(5), is_done));
    assert_eq!(
        got,
        vec![
            RemoteEvent::Phase { req: Some(req), tag, root: "/p".into(), phase: Phase::Scanned, done: 0, total: 0 },
            RemoteEvent::Diagnostics {
                req: Some(req),
                tag,
                root: "/p".into(),
                complete: true,
                files: vec![FileDiags { path: "src/main.rs".into(), diags: vec![diag(1)] }, FileDiags { path: "src/lib.rs".into(), diags: vec![diag(4)] }],
            },
            RemoteEvent::Done { req: Some(req), tag, status: DoneStatus::Failed, exit: 1, backend: Backend::Pc, elapsed_ms: 12, message: "".into() },
        ]
    );
}

/// ほかの相手（F5 の `zc-remote run`）が頼んだジョブの知らせは `req = None` で届く。
/// 自分の依頼でないログ・試験・成果物は流さない。
#[test]
fn other_clients_jobs_arrive_without_req() {
    let dir = TempDir::new("other");
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let (tx, rx) = main_channel().unwrap();
    let client = RemoteClient::with_socket(tx, no_program(), dir.socket());

    client.warm("/p/src/main.rs".into());
    let mut fake = Fake::accept(&listener);
    fake.handshake();
    assert_eq!(fake.recv(), Some(Request::Warm { file: "/p/src/main.rs".into() }));
    fake.send(&Response::Status { backend: Backend::Pc, connected: true, running_jobs: 1 });

    let tag = JobTag(3);
    let n = ReqId::NONE;
    fake.send(&Response::Phase { req: n, tag, root: "/p".into(), phase: Phase::Running, done: 0, total: 0 });
    fake.send(&Response::Log { req: n, tag, bytes: b"not mine".to_vec() });
    fake.send(&Response::Diagnostics { req: n, tag, root: "/p".into(), complete: true, last: true, files: vec![] });
    fake.send(&Response::Done { req: n, tag, status: DoneStatus::Ok, exit: 0, backend: Backend::Pc, elapsed_ms: 1, message: "".into() });

    let got = events(collect(&rx, Duration::from_secs(5), is_done));
    assert_eq!(
        got,
        vec![
            RemoteEvent::Phase { req: None, tag, root: "/p".into(), phase: Phase::Running, done: 0, total: 0 },
            RemoteEvent::Diagnostics { req: None, tag, root: "/p".into(), complete: true, files: vec![] },
            RemoteEvent::Done { req: None, tag, status: DoneStatus::Ok, exit: 0, backend: Backend::Pc, elapsed_ms: 1, message: "".into() },
        ]
    );
}

/// serve が死んでも、走っていた依頼には必ず Done が届く。次の依頼ではつなぎ直す。
#[test]
fn done_arrives_even_if_serve_dies() {
    let dir = TempDir::new("dies");
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let (tx, rx) = main_channel().unwrap();
    let client = RemoteClient::with_socket(tx, no_program(), dir.socket());

    let req = client.submit(check_job("/p/a.rs"));
    let mut fake = Fake::accept(&listener);
    fake.handshake();
    let Some(Request::Submit { req: r, .. }) = fake.recv() else { panic!() };
    // 途中まで診断を送ってから死ぬ
    fake.send(&Response::Phase { req: r, tag: JobTag(5), root: "/p".into(), phase: Phase::Scanned, done: 0, total: 0 });
    fake.send(&Response::Diagnostics { req: r, tag: JobTag(5), root: "/p".into(), complete: true, last: false, files: vec![FileDiags { path: "a.rs".into(), diags: vec![diag(0)] }] });
    drop(fake);

    let got = events(collect(&rx, Duration::from_secs(5), is_done));
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(matches!(&got[1], RemoteEvent::Diagnostics { complete: false, files, .. } if files.len() == 1), "{got:?}");
    let RemoteEvent::Done { req: Some(d), tag, status, .. } = &got[2] else { panic!("{got:?}") };
    assert_eq!((*d, *tag, *status), (req, JobTag(5), DoneStatus::Error));

    // 次の依頼は新しい接続で通る
    let req2 = client.submit(check_job("/p/a.rs"));
    let mut fake = Fake::accept(&listener);
    fake.handshake();
    let Some(Request::Submit { req: r2, .. }) = fake.recv() else { panic!() };
    assert_eq!(r2, ReqId(req2.0));
    fake.send(&Response::Done { req: r2, tag: JobTag(6), status: DoneStatus::Ok, exit: 0, backend: Backend::Pc, elapsed_ms: 1, message: "".into() });
    let got = events(collect(&rx, Duration::from_secs(5), is_done));
    assert!(matches!(got.last(), Some(RemoteEvent::Done { req: Some(r), status: DoneStatus::Ok, .. }) if *r == req2), "{got:?}");
}

/// ログと同じ段階の進み具合は、33ms より細かくメインを起こさない。中身は欠けない。
/// 段階の変化と終了はすぐ届く。
#[test]
fn logs_do_not_wake_main_more_often_than_33ms() {
    let dir = TempDir::new("logs");
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let (tx, rx) = main_channel().unwrap();
    let client = RemoteClient::with_socket(tx, no_program(), dir.socket());

    let req = client.submit(RemoteJob { kind: JobKind::Test, lang: RemoteLang::Rust, flags: SubmitFlags::WANT_LOG, file: "/p/a.rs".into() });
    let mut fake = Fake::accept(&listener);
    fake.handshake();
    let Some(Request::Submit { req: r, .. }) = fake.recv() else { panic!() };
    let tag = JobTag(1);

    let server = std::thread::spawn(move || {
        let mut sent = Vec::new();
        fake.send(&Response::Phase { req: r, tag, root: "/p".into(), phase: Phase::Running, done: 0, total: 0 });
        for i in 0..150u32 {
            let line = format!("line {i:04} {}\n", "x".repeat(80));
            sent.extend_from_slice(line.as_bytes());
            fake.send(&Response::Log { req: r, tag, bytes: line.into_bytes() });
            fake.send(&Response::Phase { req: r, tag, root: "/p".into(), phase: Phase::Running, done: i, total: 150 });
            std::thread::sleep(Duration::from_millis(2));
        }
        fake.send(&Response::Done { req: r, tag, status: DoneStatus::Ok, exit: 0, backend: Backend::Pc, elapsed_ms: 300, message: "".into() });
        (fake, sent)
    });

    let got = collect(&rx, Duration::from_secs(10), is_done);
    let (_fake, sent) = server.join().unwrap();

    let mut logs = Vec::new();
    let mut log_times = Vec::new();
    let mut phase_count = 0;
    for (t, e) in &got {
        match e {
            RemoteEvent::Log { req: lr, bytes } => {
                assert_eq!(*lr, req);
                logs.extend_from_slice(bytes);
                log_times.push(*t);
            }
            RemoteEvent::Phase { .. } => phase_count += 1,
            _ => {}
        }
    }
    assert_eq!(logs, sent, "ログが欠けた・入れ替わった");
    let first = log_times[0];
    let last = *log_times.last().unwrap();
    let span = last.duration_since(first).as_millis() as usize;
    let max = span / MIN_WAKE_INTERVAL.as_millis() as usize + 2;
    assert!(log_times.len() <= max, "ログ {} 回（{} ms の間、上限 {max}）", log_times.len(), span);
    assert!(log_times.len() < 150);
    assert!(phase_count < 150, "進み具合 {phase_count} 回");
    // 段階の変化（最初の Running）はすぐ、最後は Done
    assert!(matches!(&got[0].1, RemoteEvent::Phase { phase: Phase::Running, done: 0, .. }), "{:?}", got[0].1);
    assert!(is_done(&got.last().unwrap().1));
    // Done のあとに古い進み具合は来ない
    let after = collect_quiet(&rx, Duration::from_millis(100));
    assert!(after.is_empty(), "{after:?}");
}

/// しばらく待って、届いたものを全部返す（何も来ないことを確かめる用）。
fn collect_quiet(rx: &MainReceiver<AppEvent>, wait: Duration) -> Vec<RemoteEvent> {
    std::thread::sleep(wait);
    rx.drain().filter_map(|e| if let AppEvent::Remote(e) = e { Some(e) } else { None }).collect()
}

/// `zc-remote` が無いとき: 知らせは 1 回だけ、依頼は全部「対象ではない」で終わる。
#[test]
fn missing_program_reports_once_then_not_applicable() {
    let dir = TempDir::new("missing");
    let (tx, rx) = main_channel().unwrap();
    let client = RemoteClient::with_socket(tx, no_program(), dir.socket());

    let a = client.submit(check_job("/p/a.rs"));
    let b = client.submit(check_job("/p/a.rs"));
    client.warm("/p/a.rs".into());
    let mut dones = 0;
    let got = events(collect(&rx, Duration::from_secs(5), |e| {
        dones += is_done(e) as usize;
        dones == 2
    }));
    assert_eq!(got.len(), 3, "{got:?}");
    let RemoteEvent::Unavailable(why) = &got[0] else { panic!("{got:?}") };
    assert!(why.contains("zc-remote"), "{why}");
    for (e, r) in got[1..].iter().zip([a, b]) {
        let RemoteEvent::Done { req, status, .. } = e else { panic!("{got:?}") };
        assert_eq!((*req, *status), (Some(r), DoneStatus::NotApplicable));
    }
    assert!(collect_quiet(&rx, Duration::from_millis(50)).is_empty());
}

/// ソケットが無ければ serve を起動し、ソケットが現れるまで（最大 1 秒）待ってつなぐ。
#[test]
fn spawns_serve_and_waits_for_socket() {
    let dir = TempDir::new("spawn");
    let (tx, rx) = main_channel().unwrap();
    // `true serve` はすぐ終わる。ソケットは試験が遅れて作る（本物の serve の起動の代わり）
    let client = RemoteClient::with_socket(tx, Some(PathBuf::from("/bin/true")), dir.socket());
    let req = client.submit(check_job("/p/a.rs"));

    std::thread::sleep(Duration::from_millis(200));
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let mut fake = Fake::accept(&listener);
    fake.handshake();
    let Some(Request::Submit { req: r, .. }) = fake.recv() else { panic!() };
    fake.send(&Response::Done { req: r, tag: JobTag(1), status: DoneStatus::Ok, exit: 0, backend: Backend::Pc, elapsed_ms: 1, message: "".into() });
    let got = events(collect(&rx, Duration::from_secs(5), is_done));
    assert!(matches!(got.as_slice(), [RemoteEvent::Done { req: Some(d), status: DoneStatus::Ok, .. }] if *d == req), "{got:?}");
}

/// serve を起動してもソケットが現れなければ、その依頼は Error で終わる（「無い」とは言わない）。
#[test]
fn serve_that_never_listens_fails_the_request() {
    let dir = TempDir::new("nolisten");
    let (tx, rx) = main_channel().unwrap();
    let client = RemoteClient::with_socket(tx, Some(PathBuf::from("/bin/true")), dir.socket());
    let t = Instant::now();
    let req = client.submit(check_job("/p/a.rs"));
    let got = events(collect(&rx, Duration::from_secs(5), is_done));
    assert!(t.elapsed() >= Duration::from_millis(900));
    assert!(matches!(got.as_slice(), [RemoteEvent::Done { req: Some(d), status: DoneStatus::Error, .. }] if *d == req), "{got:?}");
}

#[test]
fn cancel_is_forwarded() {
    let dir = TempDir::new("cancel");
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let (tx, rx) = main_channel().unwrap();
    let client = RemoteClient::with_socket(tx, no_program(), dir.socket());

    let req = client.submit(check_job("/p/a.rs"));
    let mut fake = Fake::accept(&listener);
    fake.handshake();
    let Some(Request::Submit { req: r, .. }) = fake.recv() else { panic!() };
    client.cancel(req);
    assert_eq!(fake.recv(), Some(Request::Cancel { req: r }));
    fake.send(&Response::Done { req: r, tag: JobTag(2), status: DoneStatus::Cancelled, exit: 130, backend: Backend::Pc, elapsed_ms: 1, message: "".into() });
    // 同じ依頼の 2 回目の Done は流さない
    fake.send(&Response::Done { req: r, tag: JobTag(2), status: DoneStatus::Ok, exit: 0, backend: Backend::Pc, elapsed_ms: 1, message: "".into() });
    let got = events(collect(&rx, Duration::from_secs(5), is_done));
    assert!(matches!(got.as_slice(), [RemoteEvent::Done { req: Some(d), status: DoneStatus::Cancelled, .. }] if *d == req), "{got:?}");
    assert!(collect_quiet(&rx, Duration::from_millis(100)).is_empty());
}

/// 約束の版が違う serve: 知らせて、以後は「対象ではない」。
#[test]
fn version_mismatch_is_unavailable() {
    let dir = TempDir::new("version");
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let (tx, rx) = main_channel().unwrap();
    let client = RemoteClient::with_socket(tx, no_program(), dir.socket());

    let a = client.submit(check_job("/p/a.rs"));
    let mut fake = Fake::accept(&listener);
    assert!(matches!(fake.recv(), Some(Request::Hello { .. })));
    fake.send(&Response::Hello { version: IPC_VERSION + 1 });
    let got = events(collect(&rx, Duration::from_secs(5), is_done));
    assert!(matches!(&got[0], RemoteEvent::Unavailable(_)), "{got:?}");
    assert!(matches!(got.last(), Some(RemoteEvent::Done { req: Some(d), status: DoneStatus::NotApplicable, .. }) if *d == a), "{got:?}");

    let b = client.submit(check_job("/p/a.rs"));
    let got = events(collect(&rx, Duration::from_secs(5), is_done));
    assert!(matches!(got.as_slice(), [RemoteEvent::Done { req: Some(d), status: DoneStatus::NotApplicable, .. }] if *d == b), "{got:?}");
}

/// 切れたあと、依頼が無くても（serve を起動せずに）つなぎ直し、ほかの相手のジョブの診断を受け取る。
#[test]
fn reconnects_in_background_after_serve_restarts() {
    let dir = TempDir::new("reconnect");
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let (tx, rx) = main_channel().unwrap();
    let client = RemoteClient::with_socket(tx, no_program(), dir.socket());

    client.warm("/p/a.rs".into());
    let mut fake = Fake::accept(&listener);
    fake.handshake();
    assert!(matches!(fake.recv(), Some(Request::Warm { .. })));
    // serve が終わり、別の serve が同じ場所に立つ
    drop(fake);
    drop(listener);
    std::fs::remove_file(dir.socket()).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let mut fake = Fake::accept(&listener);
    fake.handshake();
    fake.send(&Response::Diagnostics { req: ReqId::NONE, tag: JobTag(9), root: "/p".into(), complete: true, last: true, files: vec![] });
    let got = events(collect(&rx, Duration::from_secs(5), |e| matches!(e, RemoteEvent::Diagnostics { .. })));
    assert!(matches!(got.as_slice(), [RemoteEvent::Diagnostics { req: None, tag: JobTag(9), .. }]), "{got:?}");
}

/// 持ち手を捨てるとソケットが閉じる（serve から見て相手が去る）。メインは待たない。
#[test]
fn drop_closes_socket() {
    let dir = TempDir::new("drop");
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let (tx, _rx) = main_channel().unwrap();
    let client = RemoteClient::with_socket(tx, no_program(), dir.socket());
    client.warm("/p/a.rs".into());
    let mut fake = Fake::accept(&listener);
    fake.handshake();
    assert!(matches!(fake.recv(), Some(Request::Warm { .. })));

    let t = Instant::now();
    drop(client);
    assert!(t.elapsed() < Duration::from_millis(20));
    assert_eq!(fake.recv(), None);
}

/// 依頼の番号は 0 を使わず、重ならない。
#[test]
fn request_numbers_are_unique_and_nonzero() {
    let dir = TempDir::new("reqno");
    let (tx, rx) = main_channel().unwrap();
    let client = RemoteClient::with_socket(tx, no_program(), dir.socket());
    let a = client.submit(check_job("/p/a.rs"));
    let b = client.submit(check_job("/p/a.rs"));
    assert_ne!(a, b);
    assert_ne!(a, RemoteReq(0));
    let mut n = 0;
    collect(&rx, Duration::from_secs(5), |e| {
        n += is_done(e) as usize;
        n == 2
    });
}
