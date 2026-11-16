//! 通信の受け入れ試験（課題 D1）。同じプロセスの中に、ループバックの受付（TLS なし）と
//! 試験用の TLS サーバ（rustls のサーバ側 + `tests/data/` の証明書）を立てて、本物の
//! ソケットでつなぐ。Access の役は、ヘッダを見て 101 / 302 / 403 / 429 を返す小さな偽物。
//!
//! `zc-wire` の中身が入るまでは `src/net.rs` の中の試験として動かしている
//! （`tests/inproc/fake_wire.rs` を参照）。証明書は `tests/data/gen.sh` で作り直せる
//! （鍵は試験専用）。

use super::wire::{http, ws};
use super::*;
use crate::auth::{AccessServiceToken, NoCredential};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

const CA: &[u8] = include_bytes!("../data/ca.pem");
const KEY: &[u8] = include_bytes!("../data/leaf.key");
const GOOD: &[u8] = include_bytes!("../data/leaf-good.pem");
const EXPIRED: &[u8] = include_bytes!("../data/leaf-expired.pem");
const FUTURE: &[u8] = include_bytes!("../data/leaf-future.pem");

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// 試験用の短い時間切れ。
fn fast() -> Timeouts {
    Timeouts { dns: Duration::from_millis(300), connect: Duration::from_millis(300), handshake: Duration::from_millis(400), silence: Duration::from_millis(200), family_stagger: Duration::from_millis(40) }
}

// ---- サーバ側 ----

/// ループバックで待ち、接続ごとに `script(何本目か, 流れ)` を呼ぶ。ポートを返す。
fn serve_plain(script: impl Fn(usize, Box<dyn Stream>) + Send + Sync + 'static) -> u16 {
    serve_on(TcpListener::bind("127.0.0.1:0").unwrap(), None, script)
}

fn serve_tls(cfg: Arc<rustls::ServerConfig>, script: impl Fn(usize, Box<dyn Stream>) + Send + Sync + 'static) -> u16 {
    serve_on(TcpListener::bind("127.0.0.1:0").unwrap(), Some(cfg), script)
}

fn serve_on(l: TcpListener, tls: Option<Arc<rustls::ServerConfig>>, script: impl Fn(usize, Box<dyn Stream>) + Send + Sync + 'static) -> u16 {
    let port = l.local_addr().unwrap().port();
    let script = Arc::new(script);
    thread::spawn(move || {
        for (i, s) in l.incoming().enumerate() {
            let Ok(s) = s else { break };
            let (script, tls) = (script.clone(), tls.clone());
            thread::spawn(move || match tls {
                None => script(i, Box::new(s)),
                Some(cfg) => script(i, Box::new(rustls::StreamOwned::new(rustls::ServerConnection::new(cfg).unwrap(), s))),
            });
        }
    });
    port
}

fn server_cfg(leaf: &[u8], aes_only: bool) -> Arc<rustls::ServerConfig> {
    let mut provider = rustls::crypto::ring::default_provider();
    if aes_only {
        provider.cipher_suites.retain(|s| s.suite() != rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256);
    }
    let certs = vec![CertificateDer::from_pem_slice(leaf).unwrap()];
    let key = PrivateKeyDer::from_pem_slice(KEY).unwrap();
    Arc::new(rustls::ServerConfig::builder_with_provider(Arc::new(provider)).with_protocol_versions(&[&rustls::version::TLS13]).unwrap().with_no_client_auth().with_single_cert(certs, key).unwrap())
}

fn client_cfg() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from_pem_slice(CA).unwrap()).unwrap();
    tls_config(roots)
}

/// 依頼の頭を読む。(頭の文字列, 読み取った結果)。
fn read_request(s: &mut dyn Stream) -> Option<(String, http::Request)> {
    let mut buf = Vec::new();
    let mut b = [0u8; 1024];
    loop {
        match http::parse_request(&buf) {
            http::Request::Incomplete => {}
            r => return Some((String::from_utf8_lossy(&buf).into_owned(), r)),
        }
        match s.read(&mut b) {
            Ok(n) if n > 0 => buf.extend_from_slice(&b[..n]),
            _ => return None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Msg {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close(u16),
}

/// サーバ側の WebSocket。
struct Srv {
    s: Box<dyn Stream>,
    dec: ws::Decoder,
    enc: ws::Encoder,
}

impl Srv {
    /// Upgrade を受ける。`first` は 101 と同じ書き込みに載せるフレーム（テキスト）。
    fn accept(mut s: Box<dyn Stream>, first: &[&str]) -> Option<Srv> {
        let (head, req) = read_request(&mut *s)?;
        let http::Request::Upgrade { accept, subprotocol_ok, path, .. } = req else { return None };
        assert!(subprotocol_ok && head.contains("Sec-WebSocket-Version: 13\r\n"), "{head}");
        assert_eq!(path, wire::WS_PATH);
        let mut out = Vec::new();
        http::write_switching(&accept, &mut out);
        let mut enc = ws::Encoder::new(ws::Role::Server);
        for t in first {
            enc.text(t, [0; 4], &mut out);
        }
        s.write_all(&out).ok()?;
        Some(Srv { s, dec: ws::Decoder::new(ws::Role::Server, 2 << 20), enc })
    }

    fn text(&mut self, t: &str) {
        let mut out = Vec::new();
        self.enc.text(t, [0; 4], &mut out);
        let _ = self.s.write_all(&out);
    }

    fn binary(&mut self, b: &[u8]) {
        let mut out = Vec::new();
        self.enc.binary(b, [0; 4], &mut out);
        let _ = self.s.write_all(&out);
    }

    fn raw(&mut self, b: &[u8]) {
        let _ = self.s.write_all(b);
    }

    fn recv(&mut self) -> Option<Msg> {
        let mut b = [0u8; 8192];
        loop {
            match self.dec.next().expect("クライアントのフレームは正しい（マスクつき）") {
                Some(ws::Message::Text(t)) => return Some(Msg::Text(t.to_owned())),
                Some(ws::Message::Binary(d)) => return Some(Msg::Binary(d.to_vec())),
                Some(ws::Message::Ping(d)) => return Some(Msg::Ping(d.to_vec())),
                Some(ws::Message::Pong(d)) => return Some(Msg::Pong(d.to_vec())),
                Some(ws::Message::Close(c)) => return Some(Msg::Close(c)),
                None => {}
            }
            match self.s.read(&mut b) {
                Ok(n) if n > 0 => self.dec.push(&b[..n]),
                _ => return None,
            }
        }
    }
}

/// 受け取ったものをそのまま返すサーバ（"bye" で close を送る）。
fn echo(s: Box<dyn Stream>) {
    let Some(mut srv) = Srv::accept(s, &[]) else { return };
    while let Some(m) = srv.recv() {
        match m {
            Msg::Text(t) if t == "bye" => {
                let mut out = Vec::new();
                srv.enc.close(4001, [0; 4], &mut out);
                srv.raw(&out);
            }
            Msg::Text(t) => srv.text(&t),
            Msg::Binary(b) => srv.binary(&b),
            _ => {}
        }
    }
}

// ---- クライアント側 ----

fn wait(fds: &[Interest], timeout_us: u64) {
    let mut p: Vec<libc::pollfd> = fds.iter().map(|i| libc::pollfd { fd: i.fd, events: if i.read { libc::POLLIN } else { 0 } | if i.write { libc::POLLOUT } else { 0 }, revents: 0 }).collect();
    // SAFETY: p は有効な配列。
    unsafe { libc::poll(p.as_mut_ptr(), p.len() as libc::nfds_t, (timeout_us / 1000) as i32 + 1) };
}

fn dial_with(ep: &Endpoint, cred: &mut dyn Credential, tls: Option<Arc<rustls::ClientConfig>>, t: Timeouts, hint: Option<IpAddr>, resolve: ResolveFn) -> Result<(Conn, DialStats), NetError> {
    let mut d = Dial::start_with(ep, cred, tls, t, hint, now_us(), resolve)?;
    let mut fds = Vec::new();
    let begin = now_us();
    loop {
        fds.clear();
        d.interests(&mut fds);
        assert!(fds.len() <= 3);
        wait(&fds, d.deadline_us().saturating_sub(now_us()).min(1_000_000));
        match d.advance(now_us())? {
            DialStep::Pending => assert!(now_us() - begin < 10_000_000, "つなぐのが終わらない"),
            DialStep::Open(c, s) => return Ok((c, s)),
        }
    }
}

fn dial(ep: &str, tls: Option<Arc<rustls::ClientConfig>>) -> Result<(Conn, DialStats), NetError> {
    dial_with(&Endpoint::parse(ep).unwrap(), &mut NoCredential, tls, fast(), None, sys_resolve)
}

fn err(r: Result<(Conn, DialStats), NetError>) -> NetError {
    match r {
        Ok(_) => panic!("つながってしまった"),
        Err(e) => e,
    }
}

/// `until` が満たされるまで読み書きする。
fn pump(conn: &mut Conn, got: &mut Vec<Msg>, until: impl Fn(&[Msg]) -> bool) -> Result<(), NetError> {
    let begin = now_us();
    loop {
        conn.on_writable()?;
        let mut sink = |m: Incoming<'_>| {
            got.push(match m {
                Incoming::Text(t) => Msg::Text(t.to_owned()),
                Incoming::Binary(b) => Msg::Binary(b.to_vec()),
            })
        };
        while conn.on_readable(64 << 10, &mut sink)? {}
        if until(got) {
            return Ok(());
        }
        assert!(now_us() - begin < 10_000_000, "待っているものが届かない: {} 個", got.len());
        wait(&[conn.interest()], 50_000);
    }
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

// ---- 試験 ----

#[test]
fn endpoint_parse() {
    let e = |tls, host: &str, port, path: &str| Some(Endpoint { tls, host: host.into(), port, path: path.into() });
    assert_eq!(Endpoint::parse("wss://build.example.com/v1/ws"), e(true, "build.example.com", 443, "/v1/ws"));
    assert_eq!(Endpoint::parse("wss://build.example.com"), e(true, "build.example.com", 443, "/v1/ws"));
    assert_eq!(Endpoint::parse("ws://127.0.0.1:7411/x?y=1"), e(false, "127.0.0.1", 7411, "/x?y=1"));
    assert_eq!(Endpoint::parse("ws://pc"), e(false, "pc", 80, "/v1/ws"));
    assert_eq!(Endpoint::parse("pc.example.ts.net:7411"), e(false, "pc.example.ts.net", 7411, "/v1/ws"));
    assert_eq!(Endpoint::parse("pc"), e(false, "pc", 7411, "/v1/ws"));
    assert_eq!(Endpoint::parse("[::1]:7411"), e(false, "::1", 7411, "/v1/ws"));
    assert_eq!(Endpoint::parse("wss://[fd7a::1]/v1/ws"), e(true, "fd7a::1", 443, "/v1/ws"));
    for bad in ["", "http://x/", "wss://", "wss://:443/", "pc:port", "pc:0", "pc:70000", "[::1", "[zz]:1", "ws://a b/", "ws://h/a b", "ws://u@h/", "ws://h/\r\nX: 1"] {
        assert_eq!(Endpoint::parse(bad), None, "{bad:?}");
    }
    assert_eq!(Endpoint::parse("wss://h").unwrap().host_header(), "h");
    assert_eq!(Endpoint::parse("wss://h:8443").unwrap().host_header(), "h:8443");
    assert_eq!(Endpoint::parse("h:7411").unwrap().host_header(), "h:7411");
    assert_eq!(Endpoint::parse("[::1]:7411").unwrap().host_header(), "[::1]:7411");
}

#[test]
fn plain_ws_round_trip() {
    let port = serve_plain(|_, s| echo(s));
    let (mut c, stats) = dial(&format!("127.0.0.1:{port}"), None).unwrap();
    assert_eq!(c.peer_ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
    assert!(!stats.resumed && stats.tls_us == 0 && stats.dns_us == 0);
    assert_eq!(c.interest(), Interest { fd: c.interest().fd, read: true, write: false });

    let big = pattern(wire::limits::CHUNK_BYTES, 3);
    c.send_text("hello");
    c.send_text("日本語");
    c.send_binary(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &big);
    assert!(c.queued_bytes() > big.len());
    assert!(c.interest().write);
    let mut got = Vec::new();
    pump(&mut c, &mut got, |g| g.len() == 3).unwrap();
    assert_eq!(c.queued_bytes(), 0);
    assert_eq!(got[0], Msg::Text("hello".into()));
    assert_eq!(got[1], Msg::Text("日本語".into()));
    let Msg::Binary(b) = &got[2] else { panic!() };
    assert_eq!(b.len(), 16 + big.len());
    assert_eq!(&b[16..], &big[..]);

    // 相手が close を送ってきた: 状態コードつきで知らせる
    c.send_text("bye");
    assert_eq!(pump(&mut c, &mut got, |_| false), Err(NetError::Closed(4001)));
}

#[test]
fn ping_fragments_and_first_frame_with_101() {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    let port = serve_plain(move |_, s| {
        // 101 と同じ書き込みに最初のフレームを載せる
        let mut srv = Srv::accept(s, &["early-1", "early-2"]).unwrap();
        // 分割されたテキストの途中に ping
        srv.raw(&[0x01, 2, b'h', b'e', 0x89, 2, b'h', b'i', 0x80, 3, b'l', b'l', b'o']);
        let pong = srv.recv();
        // close を待つ
        let close = srv.recv();
        tx.lock().unwrap().send((pong, close)).unwrap();
    });
    let (mut c, _) = dial(&format!("ws://127.0.0.1:{port}/v1/ws"), None).unwrap();
    let mut got = Vec::new();
    pump(&mut c, &mut got, |g| g.len() == 3).unwrap();
    assert_eq!(got, [Msg::Text("early-1".into()), Msg::Text("early-2".into()), Msg::Text("hello".into())]);
    // 届いた時刻と、沈黙の判定
    let rx_at = c.last_rx_us();
    assert!(rx_at <= now_us() && rx_at > 0);
    assert!(!c.is_silent(rx_at + 10, Duration::from_micros(20)));
    assert!(c.is_silent(rx_at + 20, Duration::from_micros(20)));
    // 静かに閉じる: close が相手に届く
    c.on_writable().unwrap();
    c.close();
    let (pong, close) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(pong, Some(Msg::Pong(b"hi".to_vec())));
    assert_eq!(close, Some(Msg::Close(1000)));
}

#[test]
fn tls_round_trip_through_name_resolution_and_resumption() {
    let cfg = server_cfg(GOOD, false);
    let port = serve_tls(cfg, |_, s| echo(s));
    let tls = client_cfg();
    // 名前で（使い捨てのスレッドで引く。::1 が先に返っても 127.0.0.1 に落ち着く）
    let (mut c, s1) = dial(&format!("wss://localhost:{port}/v1/ws"), Some(tls.clone())).unwrap();
    assert!(!s1.resumed);
    assert!(s1.tls_us > 0);
    let big = pattern(wire::limits::CHUNK_BYTES, 9);
    c.send_text("over tls");
    c.send_binary(&[2; 16], &big);
    let mut got = Vec::new();
    pump(&mut c, &mut got, |g| g.len() == 2).unwrap();
    assert_eq!(got[0], Msg::Text("over tls".into()));
    assert_eq!(got[1], Msg::Binary([&[2u8; 16][..], &big[..]].concat()));
    let ip = c.peer_ip();
    c.close();

    // 2 回目: 再開（チケットは設定の中 = RAM）。前回の IP を渡すと名前を引かない
    fn must_not_resolve(_: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
        panic!("前回の IP でつながるのに名前を引いた");
    }
    let ep = Endpoint::parse(&format!("wss://localhost:{port}")).unwrap();
    let (mut c, s2) = dial_with(&ep, &mut NoCredential, Some(tls), fast(), Some(ip), must_not_resolve).unwrap();
    assert!(s2.resumed, "{s2:?}");
    assert_eq!(s2.dns_us, 0);
    c.send_text("again");
    pump(&mut c, &mut got, |g| g.len() == 3).unwrap();
    assert_eq!(got[2], Msg::Text("again".into()));

    // TLS の設定を渡し忘れたら、つながない
    assert!(matches!(err(dial(&format!("wss://localhost:{port}"), None)), NetError::Tls(_)));
}

#[test]
fn tls_server_drops_the_connection() {
    let port = serve_tls(server_cfg(GOOD, false), |_, s| {
        let mut srv = Srv::accept(s, &[]).unwrap();
        srv.text("last words");
        // close も close_notify も送らずに切る
    });
    let (mut c, _) = dial(&format!("wss://127.0.0.1:{port}"), Some(client_cfg())).unwrap();
    let mut got = Vec::new();
    assert_eq!(pump(&mut c, &mut got, |_| false), Err(NetError::Closed(1006)));
    // 切れる前に届いていたものは渡してある
    assert_eq!(got, [Msg::Text("last words".into())]);
}

#[test]
fn tls_refuses_aes_only_server_and_bad_certificates() {
    // AES-GCM しか出さないサーバ: 共通の暗号が無いので、つながらない
    let port = serve_tls(server_cfg(GOOD, true), |_, s| echo(s));
    let e = err(dial(&format!("wss://localhost:{port}"), Some(client_cfg())));
    assert!(matches!(e, NetError::Tls(_)), "{e:?}");

    // まだ有効でない証明書 = 時計が合っていない
    let port = serve_tls(server_cfg(FUTURE, false), |_, s| echo(s));
    assert_eq!(err(dial(&format!("wss://localhost:{port}"), Some(client_cfg()))), NetError::Clock);

    // 期限切れ: 時計が同期済みなら TLS の失敗、同期前なら時計のせい（どちらかは環境による）
    let port = serve_tls(server_cfg(EXPIRED, false), |_, s| echo(s));
    let e = err(dial(&format!("wss://localhost:{port}"), Some(client_cfg())));
    if clock_synchronized() {
        assert!(matches!(&e, NetError::Tls(m) if m.to_lowercase().contains("expired")), "{e:?}");
    } else {
        assert_eq!(e, NetError::Clock);
    }

    // 知らない認証局（埋め込みの根には、試験用の認証局は無い）
    let port = serve_tls(server_cfg(GOOD, false), |_, s| echo(s));
    let e = err(dial(&format!("wss://localhost:{port}"), Some(tls_config(embedded_roots()))));
    assert!(matches!(e, NetError::Tls(_)), "{e:?}");

    // 名前が合わない（証明書は localhost と 127.0.0.1 / ::1 だけ）
    fn to_loopback(_: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(vec![SocketAddr::new("127.0.0.1".parse().unwrap(), port)])
    }
    let ep = Endpoint::parse(&format!("wss://other.example:{port}")).unwrap();
    let e = err(dial_with(&ep, &mut NoCredential, Some(client_cfg()), fast(), None, to_loopback));
    assert!(matches!(e, NetError::Tls(_)), "{e:?}");
}

#[test]
fn tls_errors_are_classified() {
    use rustls::pki_types::UnixTime;
    use rustls::CertificateError as C;
    let t = UnixTime::since_unix_epoch(Duration::from_secs(1));
    let cert = |c| rustls::Error::InvalidCertificate(c);
    for synced in [true, false] {
        assert_eq!(classify_tls(&cert(C::NotValidYet), synced), NetError::Clock);
        assert_eq!(classify_tls(&cert(C::NotValidYetContext { time: t, not_before: t }), synced), NetError::Clock);
        assert!(matches!(classify_tls(&cert(C::UnknownIssuer), synced), NetError::Tls(_)));
        assert!(matches!(classify_tls(&rustls::Error::AlertReceived(rustls::AlertDescription::HandshakeFailure), synced), NetError::Tls(_)));
    }
    assert!(matches!(classify_tls(&cert(C::Expired), true), NetError::Tls(_)));
    assert!(matches!(classify_tls(&cert(C::ExpiredContext { time: t, not_after: t }), true), NetError::Tls(_)));
    assert_eq!(classify_tls(&cert(C::Expired), false), NetError::Clock);
    assert_eq!(classify_tls(&cert(C::ExpiredContext { time: t, not_after: t }), false), NetError::Clock);
}

/// Access の役: `/v1/ws` はトークンが合えば 101、合わなければ 403。ほかのパスは決まった返事。
fn gate(mut s: Box<dyn Stream>) {
    let Some((head, req)) = read_request(&mut *s) else { return };
    let path = match &req {
        http::Request::Upgrade { path, .. } | http::Request::Get { path, .. } => path.clone(),
        _ => return,
    };
    let reply: &[u8] = match path.as_str() {
        "/v1/ws" if head.contains("CF-Access-Client-Id: abc.access\r\n") && head.contains("CF-Access-Client-Secret: s3cr3t\r\n") => {
            let http::Request::Upgrade { accept, .. } = req else { return };
            let mut out = Vec::new();
            http::write_switching(&accept, &mut out);
            ws::Encoder::new(ws::Role::Server).text("welcome", [0; 4], &mut out);
            let _ = s.write_all(&out);
            let mut b = [0u8; 64];
            while matches!(s.read(&mut b), Ok(n) if n > 0) {}
            return;
        }
        "/v1/ws" => b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n",
        "/login" => b"HTTP/1.1 302 Found\r\nLocation: https://team.example/cdn-cgi/access/login\r\nContent-Length: 0\r\n\r\n",
        "/busy" => b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7\r\nContent-Length: 0\r\n\r\n",
        "/down" => b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
        "/html" => b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 13\r\n\r\n<html></html>",
        "/unauth" => b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n",
        "/teapot" => b"HTTP/1.1 418 I'm a teapot\r\nContent-Length: 0\r\n\r\n",
        "/badaccept" => b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: AAAAAAAAAAAAAAAAAAAAAAAAAAA=\r\n\r\n",
        _ => b"SSH-2.0-OpenSSH_9.9\r\n\r\n",
    };
    let _ = s.write_all(reply);
}

#[test]
fn access_gate_statuses_and_token() {
    let port = serve_tls(server_cfg(GOOD, false), |_, s| gate(s));
    let tls = client_cfg();
    let go = |path: &str, cred: &mut dyn Credential| dial_with(&Endpoint::parse(&format!("wss://localhost:{port}{path}")).unwrap(), cred, Some(tls.clone()), fast(), None, sys_resolve);

    // トークンなし → 403。302（ログイン画面）と HTML もたどらずに認証の失敗
    assert_eq!(err(go("/v1/ws", &mut NoCredential)), NetError::Auth(403));
    assert_eq!(err(go("/login", &mut NoCredential)), NetError::Auth(302));
    assert_eq!(err(go("/html", &mut NoCredential)), NetError::Auth(200));
    assert_eq!(err(go("/unauth", &mut NoCredential)), NetError::Auth(401));
    // 上限・混雑
    assert_eq!(err(go("/busy", &mut NoCredential)), NetError::Busy(429, Some(7)));
    assert_eq!(err(go("/down", &mut NoCredential)), NetError::Busy(503, None));
    // そのほか
    assert_eq!(err(go("/teapot", &mut NoCredential)), NetError::Protocol("HTTP 418".into()));
    assert!(matches!(err(go("/badaccept", &mut NoCredential)), NetError::Protocol(_)));
    assert!(matches!(err(go("/ssh", &mut NoCredential)), NetError::Protocol(_)));

    // 0600 のトークンで通る
    use std::os::unix::fs::PermissionsExt;
    let p = std::env::temp_dir().join(format!("zc-remote-net-token-{}.json", std::process::id()));
    std::fs::write(&p, r#"{"scheme":"cf-access-service-token","client_id":"abc.access","client_secret":"s3cr3t"}"#).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut token = AccessServiceToken::load(&p).unwrap();
    let (mut c, _) = go("/v1/ws", &mut token).unwrap();
    let mut got = Vec::new();
    pump(&mut c, &mut got, |g| g.len() == 1).unwrap();
    assert_eq!(got, [Msg::Text("welcome".into())]);

    // 断られたトークンは、ファイルを直すまでつなぎに行かない（Auth のまま、すぐ返る）
    assert_eq!(token.rejected(403), crate::auth::Retry::GiveUp);
    assert_eq!(err(go("/v1/ws", &mut token)), NetError::Auth(403));
    std::fs::remove_file(&p).unwrap();
}

#[test]
fn black_holes_time_out_at_each_stage() {
    // 受けるだけで何も返さない相手
    let hold = |_: usize, mut s: Box<dyn Stream>| {
        let mut b = [0u8; 64];
        while matches!(s.read(&mut b), Ok(n) if n > 0) {}
    };
    // TLS なし: Upgrade の返事が来ない
    let port = serve_plain(hold);
    let t0 = now_us();
    assert_eq!(err(dial(&format!("127.0.0.1:{port}"), None)), NetError::Timeout(Stage::Upgrade));
    let took = now_us() - t0;
    assert!((350_000..2_000_000).contains(&took), "{took}");
    // TLS あり: ハンドシェイクが進まない（TCP は受けるが、TLS としては黙る）
    let port = serve_plain(hold);
    assert_eq!(err(dial(&format!("wss://localhost:{port}"), Some(client_cfg()))), NetError::Timeout(Stage::Tls));

    // 名前解決が戻らない: スレッドは捨てて、時間切れ
    fn hang(_: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
        thread::sleep(Duration::from_secs(3));
        Ok(Vec::new())
    }
    let ep = Endpoint::parse("slow-dns.example:7411").unwrap();
    let t0 = now_us();
    assert_eq!(err(dial_with(&ep, &mut NoCredential, None, fast(), None, hang)), NetError::Timeout(Stage::Dns));
    let took = now_us() - t0;
    assert!((250_000..1_500_000).contains(&took), "{took}");

    // 名前が無い
    fn nx(_: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
        Err(io::Error::other("no such host"))
    }
    assert_eq!(err(dial_with(&ep, &mut NoCredential, None, fast(), None, nx)), NetError::Unreachable(Stage::Dns, 0));

    // 誰も待っていないポート: すぐ断られる
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    assert_eq!(err(dial(&format!("127.0.0.1:{port}"), None)), NetError::Unreachable(Stage::Tcp, libc::ECONNREFUSED));
}

/// 待ち受けの列をいっぱいにして、SYN に返事をしないポートを作る（Linux）。
/// 返した値を持っている間だけ効く。
fn syn_black_hole(ip: &str, port: u16) -> Option<(TcpListener, Vec<TcpStream>)> {
    let l = TcpListener::bind((ip, port)).ok()?;
    // SAFETY: l の fd は有効。列の長さを 0 に変えるだけ。
    unsafe { libc::listen(l.as_raw_fd(), 0) };
    let addr = l.local_addr().unwrap();
    let fill = (0..4).filter_map(|_| TcpStream::connect_timeout(&addr, Duration::from_millis(100)).ok()).collect();
    Some((l, fill))
}

#[test]
fn tcp_black_hole_times_out_and_second_address_wins() {
    let hole = syn_black_hole("127.0.0.1", 0).unwrap();
    let port = hole.0.local_addr().unwrap().port();
    // 列がいっぱいになっていること（環境によっては効かないので、そのときは試験を飛ばす）
    if TcpStream::connect_timeout(&hole.0.local_addr().unwrap(), Duration::from_millis(100)).is_ok() {
        eprintln!("SYN を捨てるポートを作れなかったので飛ばす");
        return;
    }
    let t0 = now_us();
    assert_eq!(err(dial(&format!("127.0.0.1:{port}"), None)), NetError::Timeout(Stage::Tcp));
    let took = now_us() - t0;
    assert!((250_000..1_500_000).contains(&took), "{took}");

    // 1 つ目の行き先が黙っていても、少しずらして始めた 2 つ目でつながる
    // （127.0.0.2 の同じポートを黙らせ、127.0.0.1 で待つ）
    let (good_port, _hole2) = (0..20)
        .find_map(|_| {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let p = l.local_addr().unwrap().port();
            let hole = syn_black_hole("127.0.0.2", p)?;
            serve_on(l, None, |_, s| echo(s));
            Some((p, hole))
        })
        .unwrap();
    fn hole_then_good(_: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(vec![SocketAddr::new("127.0.0.2".parse().unwrap(), port), SocketAddr::new("127.0.0.1".parse().unwrap(), port)])
    }
    let ep = Endpoint::parse(&format!("two-addrs.example:{good_port}")).unwrap();
    let t0 = now_us();
    let (c, _) = dial_with(&ep, &mut NoCredential, None, fast(), None, hole_then_good).unwrap();
    let took = now_us() - t0;
    assert_eq!(c.peer_ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
    // ずらす時間（40ms）は待つが、1 つ目の時間切れ（300ms）は待たない
    assert!((30_000..280_000).contains(&took), "{took}");
}

#[test]
fn falls_back_across_families_and_from_a_stale_hint() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    // ::1 では誰も待っていない（IPv6 が無い環境でも、その場で失敗して次へ進む）
    fn v6_then_v4(_: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(vec![SocketAddr::new("::1".parse().unwrap(), port), SocketAddr::new("::1".parse().unwrap(), port), SocketAddr::new("127.0.0.1".parse().unwrap(), port)])
    }
    let port = serve_plain(|_, s| echo(s));
    let ep = Endpoint::parse(&format!("dual.example:{port}")).unwrap();
    let (c, stats) = dial_with(&ep, &mut NoCredential, None, fast(), None, v6_then_v4).unwrap();
    assert_eq!(c.peer_ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
    assert!(stats.dns_us > 0);
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);

    // 前回の IP がもう違う（そこでは誰も待っていない）: 名前を引き直してつながる
    let stale: IpAddr = "127.0.0.3".parse().unwrap();
    let (c, _) = dial_with(&ep, &mut NoCredential, None, fast(), Some(stale), v6_then_v4).unwrap();
    assert_eq!(c.peer_ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);

    // 資格を用意できないときは、つなぎに行かない
    struct Broken;
    impl Credential for Broken {
        fn headers(&mut self, _: &mut Vec<(String, String)>) -> Result<(), AuthError> {
            Err(AuthError::Missing)
        }
        fn rejected(&mut self, _: u16) -> crate::auth::Retry {
            crate::auth::Retry::GiveUp
        }
    }
    assert_eq!(err(dial_with(&ep, &mut Broken, None, fast(), None, v6_then_v4)), NetError::Auth(0));
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);
}

/// 少しずつしか書けないソケットで、大きな中身を送り切る。
fn upload_through_a_narrow_socket(tls: bool) {
    const FRAMES: usize = 24;
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    let script = move |_: usize, s: Box<dyn Stream>| {
        let mut srv = Srv::accept(s, &[]).unwrap();
        let mut ok = true;
        for i in 0..FRAMES {
            // ゆっくり読む相手
            thread::sleep(Duration::from_millis(2));
            let Some(Msg::Binary(b)) = srv.recv() else { return };
            ok &= b[16..] == pattern(wire::limits::CHUNK_BYTES, i as u8)[..] && b[..16] == [i as u8; 16];
        }
        srv.text(if ok { "all good" } else { "corrupt" });
        tx.lock().unwrap().send(()).unwrap();
        // クライアントが閉じるまで待つ（先に切ると、返事と一緒に「切れた」が届く）
        let _ = srv.recv();
    };
    let (port, cfg) = if tls { (serve_tls(server_cfg(GOOD, false), script), Some(client_cfg())) } else { (serve_plain(script), None) };
    let (mut c, _) = dial(&format!("{}://localhost:{port}", if tls { "wss" } else { "ws" }), cfg).unwrap();
    // 送信バッファを最小にする
    let small: libc::c_int = 1;
    // SAFETY: small は有効な c_int。
    unsafe { libc::setsockopt(c.interest().fd, libc::SOL_SOCKET, libc::SO_SNDBUF, (&small as *const libc::c_int).cast(), 4) };

    let (mut next, mut got, mut max_queued) = (0, Vec::new(), 0);
    let begin = now_us();
    while got.is_empty() {
        // serve と同じ決まり: 送り待ちが 256KB を超えている間は、次の中身を足さない
        while next < FRAMES && c.queued_bytes() <= (256 << 10) {
            c.send_binary(&[next as u8; 16], &pattern(wire::limits::CHUNK_BYTES, next as u8));
            next += 1;
        }
        max_queued = max_queued.max(c.queued_bytes());
        pump(&mut c, &mut got, |_| true).unwrap();
        wait(&[c.interest()], 20_000);
        assert!(now_us() - begin < 20_000_000);
    }
    assert_eq!(got, [Msg::Text("all good".into())]);
    assert_eq!(c.queued_bytes(), 0);
    assert!(!c.interest().write);
    // 溜めたのは 256KB + フレーム 1 つぶんまで
    assert!(max_queued <= (256 << 10) + wire::limits::CHUNK_BYTES + 64, "{max_queued}");
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn narrow_socket_plain() {
    upload_through_a_narrow_socket(false);
}

#[test]
fn narrow_socket_tls() {
    upload_through_a_narrow_socket(true);
}

/// 読む量の上限: 1 回の呼び出しで読みすぎず、残りがあれば `Ok(true)` で知らせる。
fn read_budget(tls: bool) {
    const N: usize = 400;
    let script = |_: usize, s: Box<dyn Stream>| {
        let mut srv = Srv::accept(s, &[]).unwrap();
        // 全部を 1 回の書き込みで（クライアントの中のバッファに溜まる形にする）
        let mut out = Vec::new();
        for i in 0..N {
            srv.enc.text(&format!("message number {i:04}"), [0; 4], &mut out);
        }
        srv.enc.binary(&pattern(100_000, 1), [0; 4], &mut out);
        srv.raw(&out);
        let _ = srv.recv();
    };
    let (port, cfg) = if tls { (serve_tls(server_cfg(GOOD, false), script), Some(client_cfg())) } else { (serve_plain(script), None) };
    let (mut c, _) = dial(&format!("{}://localhost:{port}", if tls { "wss" } else { "ws" }), cfg).unwrap();
    let (mut got, mut more_calls, mut most) = (Vec::new(), 0, 0);
    let begin = now_us();
    while got.len() < N + 1 {
        wait(&[c.interest()], 50_000);
        loop {
            let before = got.len();
            let more = c.on_readable(512, &mut |m| got.push(matches!(m, Incoming::Text(_)))).unwrap();
            most = most.max(got.len() - before);
            if !more {
                break;
            }
            more_calls += 1;
        }
        assert!(now_us() - begin < 10_000_000);
    }
    assert!(got[..N].iter().all(|t| *t) && !got[N]);
    // 512 バイトずつなので、何十回も「まだ残っている」と返る。1 回で渡すのは 512 バイト + 1 つぶんまで
    assert!(more_calls > 100, "{more_calls}");
    assert!(most <= 512 / 22 + 2, "{most}");
}

#[test]
fn read_budget_plain() {
    read_budget(false);
}

#[test]
fn read_budget_tls() {
    read_budget(true);
}

#[test]
fn protocol_violations_from_the_server() {
    // マスクつきのフレーム（サーバは掛けてはいけない）
    let port = serve_plain(|_, s| {
        let mut srv = Srv::accept(s, &[]).unwrap();
        srv.raw(&[0x81, 0x81, 1, 2, 3, 4, b'x' ^ 1]);
        let _ = srv.recv();
    });
    let (mut c, _) = dial(&format!("127.0.0.1:{port}"), None).unwrap();
    assert!(matches!(pump(&mut c, &mut Vec::new(), |_| false), Err(NetError::Protocol(_))));

    // 大きすぎるメッセージ
    let port = serve_plain(|_, s| {
        let mut srv = Srv::accept(s, &[]).unwrap();
        srv.raw(&[0x82, 127, 0, 0, 0, 0, 0x10, 0, 0, 0]);
        let _ = srv.recv();
    });
    let (mut c, _) = dial(&format!("127.0.0.1:{port}"), None).unwrap();
    assert!(matches!(pump(&mut c, &mut Vec::new(), |_| false), Err(NetError::Protocol(_))));
}

#[test]
fn default_route_detection() {
    let dir = std::env::temp_dir().join(format!("zc-remote-net-route-{}", std::process::id()));
    let (proc_net, sys_net) = (dir.join("proc"), dir.join("sys"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&proc_net).unwrap();
    let oper = |iface: &str, state: &str| {
        std::fs::create_dir_all(sys_net.join(iface)).unwrap();
        std::fs::write(sys_net.join(iface).join("operstate"), format!("{state}\n")).unwrap();
    };
    let check = || default_route_in(&proc_net, &sys_net);
    const HEAD: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n";
    let v4 = |rows: &str| std::fs::write(proc_net.join("route"), format!("{HEAD}{rows}")).unwrap();
    let v6 = |rows: &str| std::fs::write(proc_net.join("ipv6_route"), rows).unwrap();
    const LAN: &str = "wlan0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n";
    const V6_LO: &str = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";

    // 表が読めない（/proc が無い）: 止めない
    assert!(check());
    // 既定の経路が無い（LAN の経路だけ）
    v4(LAN);
    assert!(!check());
    // Wi-Fi の既定の経路
    v4(&format!("wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n{LAN}"));
    oper("wlan0", "up");
    assert!(check());
    // インターフェースが落ちている
    oper("wlan0", "down");
    assert!(!check());
    oper("wlan0", "dormant");
    assert!(!check());
    // Bluetooth のテザリング（bnep0）は状態が unknown でも通れる
    v4("wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\nbnep0\t00000000\t01140AAC\t0003\t0\t0\t700\t00000000\t0\t0\t0\n");
    oper("bnep0", "unknown");
    assert!(check());
    // 経路はあるが UP でない
    v4("wlan0\t00000000\t0101A8C0\t0002\t0\t0\t600\t00000000\t0\t0\t0\n");
    oper("wlan0", "up");
    assert!(!check());
    // IPv6 だけの既定の経路。lo の「届かない」印の行は数えない
    v6(V6_LO);
    assert!(!check());
    v6(&format!("{V6_LO}00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000258 00000002 00000000 00000003 wlan0\n"));
    assert!(check());
    oper("wlan0", "lowerlayerdown");
    assert!(!check());
    std::fs::remove_dir_all(&dir).unwrap();

    // 本物の関数は、待たずに答える（結果は環境による）
    let t0 = now_us();
    let _ = (has_default_route(), clock_synchronized());
    assert!(now_us() - t0 < 100_000);
    // この機械自身あての行き先は、経路が無くてもつなぎに行く
    let port = serve_plain(|_, s| echo(s));
    let ep = Endpoint::parse(&format!("localhost:{port}")).unwrap();
    assert!(Dial::start(&ep, &mut NoCredential, None, fast(), None, now_us()).is_ok());
}
