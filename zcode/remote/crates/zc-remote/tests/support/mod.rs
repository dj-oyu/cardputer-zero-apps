//! 通信の試験の道具（課題 D1）。同じプロセスの中に、ループバックの受付（TLS なし）と
//! 試験用の TLS サーバ（rustls のサーバ側 + `tests/data/` の証明書）を立てて、本物の
//! ソケットでつなぐ。Access の役は、ヘッダを見て 101 / 302 / 403 / 429 を返す小さな偽物。
//!
//! 2 か所から使う: `tests/net.rs`（公開の形だけを使う試験）と、`src/net.rs` の中の試験
//! （名前の引き方を差し替えるなど、中の関数が要るもの）。読み込む側が `api` という
//! モジュールに `zc_remote::net` と `zc_remote::auth` の中身を並べておく。
//! 証明書は `tests/data/gen.sh` で作り直せる（鍵は試験専用）。
#![allow(dead_code)]

use super::api::*;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use zc_core::time::now_us;
use zc_wire::{http, ws};

pub const CA: &[u8] = include_bytes!("../data/ca.pem");
pub const KEY: &[u8] = include_bytes!("../data/leaf.key");
pub const GOOD: &[u8] = include_bytes!("../data/leaf-good.pem");
pub const EXPIRED: &[u8] = include_bytes!("../data/leaf-expired.pem");
pub const FUTURE: &[u8] = include_bytes!("../data/leaf-future.pem");

pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// 試験用の短い時間切れ。
pub fn fast() -> Timeouts {
    Timeouts { dns: Duration::from_millis(300), connect: Duration::from_millis(300), handshake: Duration::from_millis(400), silence: Duration::from_millis(200), family_stagger: Duration::from_millis(40) }
}

// ---- サーバ側 ----

/// ループバックで待ち、接続ごとに `script(何本目か, 流れ)` を呼ぶ。ポートを返す。
pub fn serve_plain(script: impl Fn(usize, Box<dyn Stream>) + Send + Sync + 'static) -> u16 {
    serve_on(TcpListener::bind("127.0.0.1:0").unwrap(), None, script)
}

pub fn serve_tls(cfg: Arc<rustls::ServerConfig>, script: impl Fn(usize, Box<dyn Stream>) + Send + Sync + 'static) -> u16 {
    serve_on(TcpListener::bind("127.0.0.1:0").unwrap(), Some(cfg), script)
}

pub fn serve_on(l: TcpListener, tls: Option<Arc<rustls::ServerConfig>>, script: impl Fn(usize, Box<dyn Stream>) + Send + Sync + 'static) -> u16 {
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

pub fn server_cfg(leaf: &[u8], aes_only: bool) -> Arc<rustls::ServerConfig> {
    let mut provider = rustls::crypto::ring::default_provider();
    if aes_only {
        provider.cipher_suites.retain(|s| s.suite() != rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256);
    }
    let certs = vec![CertificateDer::from_pem_slice(leaf).unwrap()];
    let key = PrivateKeyDer::from_pem_slice(KEY).unwrap();
    Arc::new(rustls::ServerConfig::builder_with_provider(Arc::new(provider)).with_protocol_versions(&[&rustls::version::TLS13]).unwrap().with_no_client_auth().with_single_cert(certs, key).unwrap())
}

pub fn client_cfg() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from_pem_slice(CA).unwrap()).unwrap();
    tls_config(roots)
}

/// 依頼の頭を読む。(頭の文字列, 読み取った結果)。
pub fn read_request(s: &mut dyn Stream) -> Option<(String, http::Request)> {
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
pub enum Msg {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close(u16),
}

/// サーバ側の WebSocket。
pub struct Srv {
    pub s: Box<dyn Stream>,
    pub dec: ws::Decoder,
    pub enc: ws::Encoder,
}

impl Srv {
    /// Upgrade を受ける。`first` は 101 と同じ書き込みに載せるフレーム（テキスト）。
    pub fn accept(mut s: Box<dyn Stream>, first: &[&str]) -> Option<Srv> {
        let (head, req) = read_request(&mut *s)?;
        let http::Request::Upgrade { accept, subprotocol_ok, path, .. } = req else { return None };
        assert!(subprotocol_ok && head.contains("Sec-WebSocket-Version: 13\r\n"), "{head}");
        assert_eq!(path, zc_wire::WS_PATH);
        let mut out = Vec::new();
        http::write_switching(&accept, &mut out);
        let mut enc = ws::Encoder::new(ws::Role::Server);
        for t in first {
            enc.text(t, [0; 4], &mut out);
        }
        s.write_all(&out).ok()?;
        Some(Srv { s, dec: ws::Decoder::new(ws::Role::Server, 2 << 20), enc })
    }

    pub fn text(&mut self, t: &str) {
        let mut out = Vec::new();
        self.enc.text(t, [0; 4], &mut out);
        let _ = self.s.write_all(&out);
    }

    pub fn binary(&mut self, b: &[u8]) {
        let mut out = Vec::new();
        self.enc.binary(b, [0; 4], &mut out);
        let _ = self.s.write_all(&out);
    }

    pub fn raw(&mut self, b: &[u8]) {
        let _ = self.s.write_all(b);
    }

    pub fn recv(&mut self) -> Option<Msg> {
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
pub fn echo(s: Box<dyn Stream>) {
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

pub fn wait(fds: &[Interest], timeout_us: u64) {
    let mut p: Vec<libc::pollfd> = fds.iter().map(|i| libc::pollfd { fd: i.fd, events: if i.read { libc::POLLIN } else { 0 } | if i.write { libc::POLLOUT } else { 0 }, revents: 0 }).collect();
    // SAFETY: p は有効な配列。
    unsafe { libc::poll(p.as_mut_ptr(), p.len() as libc::nfds_t, (timeout_us / 1000) as i32 + 1) };
}

/// つなぎ終わるまで回す。
pub fn drive(mut d: Dial) -> Result<(Conn, DialStats), NetError> {
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

pub fn dial_to(ep: &str, cred: &mut dyn Credential, tls: Option<Arc<rustls::ClientConfig>>, hint: Option<IpAddr>) -> Result<(Conn, DialStats), NetError> {
    drive(Dial::start(&Endpoint::parse(ep).unwrap(), cred, tls, fast(), hint, now_us())?)
}

pub fn dial(ep: &str, tls: Option<Arc<rustls::ClientConfig>>) -> Result<(Conn, DialStats), NetError> {
    dial_to(ep, &mut NoCredential, tls, None)
}

pub fn err(r: Result<(Conn, DialStats), NetError>) -> NetError {
    match r {
        Ok(_) => panic!("つながってしまった"),
        Err(e) => e,
    }
}

/// `until` が満たされるまで読み書きする。
pub fn pump(conn: &mut Conn, got: &mut Vec<Msg>, until: impl Fn(&[Msg]) -> bool) -> Result<(), NetError> {
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

pub fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

/// Access の役: `/v1/ws` はトークンが合えば 101、合わなければ 403。ほかのパスは決まった返事。
pub fn gate(mut s: Box<dyn Stream>) {
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

/// 待ち受けの列をいっぱいにして、SYN に返事をしないポートを作る（Linux）。
/// 返した値を持っている間だけ効く。
pub fn syn_black_hole(ip: &str, port: u16) -> Option<(TcpListener, Vec<TcpStream>)> {
    let l = TcpListener::bind((ip, port)).ok()?;
    // SAFETY: l の fd は有効。列の長さを 0 に変えるだけ。
    unsafe { libc::listen(l.as_raw_fd(), 0) };
    let addr = l.local_addr().unwrap();
    let fill = (0..4).filter_map(|_| TcpStream::connect_timeout(&addr, Duration::from_millis(100)).ok()).collect();
    Some((l, fill))
}

