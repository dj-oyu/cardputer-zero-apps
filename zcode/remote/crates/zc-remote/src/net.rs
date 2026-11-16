//! つなぐ・TLS・WebSocket（docs/stage3-design.md §7.3）。課題 D1。
//!
//! 全部ノンブロッキングで、呼ぶ側（[`crate::serve`]）の `poll()` に載せる。この中では
//! 待たない（唯一の例外は名前解決: `getaddrinfo` は取り消せないので、使い捨てのスレッドで
//! 引き、結果を eventfd で知らせる。時間切れになったらスレッドは捨てる）。
//!
//! ```text
//!   Dial: 経路の確認 → 名前解決(2s) → TCP(3s, IPv4/IPv6 を 250ms ずらして並行)
//!         → [TLS 1.3 のハンドシェイク(5s)] → HTTP Upgrade → Conn
//! ```
//!
//! - TLS は rustls の `read_tls` / `write_tls` を自前のループで回す。暗号は ring、
//!   **TLS 1.3 だけ・鍵交換は X25519 だけ・暗号は ChaCha20-Poly1305 だけ**（実機の CPU には
//!   AES の命令が無い）。ALPN は `http/1.1`。根の証明書は webpki-roots の埋め込み
//!   （`/etc/ssl/certs` を SD から読まない）。再開のチケットは RAM にだけ持つ
//! - PC の経路（`ws://`）は TLS なし（tailnet の中は WireGuard で暗号化済み）
//! - つなぐ前に分かることは待たない: 既定の経路が無ければ即 [`NetError::Offline`]
//! - 証明書が「まだ有効でない」で落ちたら [`NetError::Clock`]（時計の電池が無いので、
//!   NTP の同期前に起きる。`/run/systemd/timesync/synchronized` の有無も見る）
//! - 302 と HTML は認証の失敗（[`NetError::Auth`]）。たどらない
//! - 待機中に ping は打たない（無線を起こし続ける）。ジョブの実行中に
//!   [`Timeouts::silence`] のあいだ何も届かなければ [`NetError::Timeout`]
//!
//! # 使い方（serve の `poll()` のループ）
//!
//! ```text
//!   let mut dial = Dial::start(&ep, &mut cred, tls, Timeouts::default(), hint, now)?;
//!   loop {
//!       fds.clear(); dial.interests(&mut fds);          // fd は毎回変わりうる
//!       poll(fds, dial.deadline_us() - now);
//!       match dial.advance(now)? { DialStep::Pending => {}, DialStep::Open(conn, stats) => break }
//!   }
//!   // 開いたあと
//!   poll(conn.interest(), …);
//!   読める → while conn.on_readable(64 << 10, &mut |m| …)? { ほかの fd も見る }
//!   送る   → conn.send_text(…) / conn.send_binary(…) を好きなだけ呼んでから conn.on_writable()
//!   書ける → conn.on_writable()
//! ```
//!
//! - [`NetError::Auth`] を受けたら、serve が [`Credential::rejected`] を呼ぶ（[`Dial`] は
//!   資格を始めに 1 回借りるだけで、持ち続けない）
//! - 沈黙の判定は serve がする（ジョブがあるかを知っているのは状態機械）:
//!   [`Conn::is_silent`]
//!
//! # 決めたこと（実装）
//!
//! - **Upgrade の依頼は、ハンドシェイクが済む前に rustls に渡しておく**。クライアントの
//!   Finished と同じ送信に載るので、往復もシステムコールも増えない
//! - 前回の IP（`hint`）があれば、名前を引かずにそこへつなぎ始める。
//!   [`Timeouts::family_stagger`] のあいだにつながらなければ（またはすぐ断られたら）、
//!   そこで初めて名前を引く。つながれば、スレッドも DNS の問い合わせも使わない
//! - 並行につなぐのは 2 本まで。候補は 4 つまで（名前解決の順に、先頭と、別の族の先頭を
//!   先に試す）
//! - マスクの鍵は、起動時に `getrandom(2)` で種を取った xorshift（フレームごとに
//!   システムコールを使わない。マスクは秘密ではない）
//! - 送る列は 1 本の `Vec`。TLS では rustls の中の列（64KB まで）へ移せるだけ移し、
//!   ソケットへは `sendmsg` でまとめて書く（64KiB のフレーム 1 つがシステムコール 1 回）
//! - 読むのは、TLS では rustls の `read_tls`（1 回 4KB 前後）→ 16KB の作業用バッファ →
//!   枠の復号。ヒープの確保は rustls の中（レコードごと）だけ

// `zc-wire` の中身（課題 W1）が入るまでの間、試験のときだけ同じ形の代わりに差し替える。
// 統合したら、この 6 行を `use zc_wire as wire;` にして `tests/inproc/fake_wire.rs` を消す。
#[cfg(test)]
#[path = "../tests/inproc/fake_wire.rs"]
mod fake_wire;
#[cfg(test)]
use fake_wire as wire;
#[cfg(not(test))]
use zc_wire as wire;

use crate::auth::{AuthError, Credential};
use std::collections::VecDeque;
use std::io::{self, IoSlice, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wire::{http, ws};
use zc_core::time::now_us;

/// 行き先。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// `wss://`（TLS あり）か `ws://`（なし）。
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// 依頼先のパス（[`zc_wire::WS_PATH`]）。
    pub path: String,
}

impl Endpoint {
    /// `wss://host[:port]/path`・`ws://host[:port]/path`・`host:port`（TLS なし）から。
    /// パスが無ければ [`zc_wire::WS_PATH`]。ポートが無ければ `wss` は 443、`ws` は 80、
    /// `host` だけの形は [`zc_wire::DEFAULT_PORT`]。IPv6 は `[::1]:7411` の形。
    pub fn parse(s: &str) -> Option<Endpoint> {
        let (tls, rest, default_port) = if let Some(r) = s.strip_prefix("wss://") {
            (true, r, 443)
        } else if let Some(r) = s.strip_prefix("ws://") {
            (false, r, 80)
        } else if s.contains("://") {
            return None;
        } else {
            (false, s, wire::DEFAULT_PORT)
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, wire::WS_PATH),
        };
        let (host, port) = if let Some(r) = authority.strip_prefix('[') {
            let (h, after) = r.split_once(']')?;
            h.parse::<std::net::Ipv6Addr>().ok()?;
            match after.strip_prefix(':') {
                Some(p) => (h, p.parse().ok()?),
                None if after.is_empty() => (h, default_port),
                None => return None,
            }
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h, p.parse().ok()?),
                None => (authority, default_port),
            }
        };
        // ヘッダと依頼の行にそのまま入るので、空白・制御文字・区切りを通さない。
        let plain = |s: &str, extra: &[u8]| s.bytes().all(|b| (0x21..0x7f).contains(&b) && !extra.contains(&b));
        if host.is_empty() || port == 0 || !plain(host, b"@/[]") || !plain(path, b"") {
            return None;
        }
        Some(Endpoint { tls, host: host.to_owned(), port, path: path.to_owned() })
    }

    /// `Host` ヘッダの値。
    fn host_header(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == if self.tls { 443 } else { 80 } {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

/// 時間切れ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    pub dns: Duration,
    pub connect: Duration,
    pub handshake: Duration,
    /// ジョブの実行中に、これだけ何も届かなければ切れたとみなす（受付は 15 秒黙ったら
    /// ping を打つ）。
    pub silence: Duration,
    /// IPv4 と IPv6 をずらす時間。
    pub family_stagger: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            dns: Duration::from_secs(2),
            connect: Duration::from_secs(3),
            handshake: Duration::from_secs(5),
            silence: Duration::from_secs(40),
            family_stagger: Duration::from_millis(250),
        }
    }
}

/// どの段で止まったか（`--stats` と時間切れの報告）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Route,
    Dns,
    Tcp,
    Tls,
    Upgrade,
    Open,
}

/// 通信の失敗。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetError {
    /// 既定の経路が無い・インターフェースが落ちている（つなぐ前に分かった）。
    Offline,
    /// その段で時間切れ。
    Timeout(Stage),
    /// 名前を引けない・つなげない（`errno` つき）。
    Unreachable(Stage, i32),
    /// TLS の失敗（証明書・約束）。人に見せる 1 行。
    Tls(String),
    /// 証明書が「まだ有効でない」= 時計が合っていない。
    Clock,
    /// 401 / 403 / 302（認証の失敗）。資格を用意できなかったとき（ファイルが無い・権限が
    /// 広い）は 0。
    Auth(u16),
    /// 429 / 503（上限・混雑）。`Retry-After` があれば秒。
    Busy(u16, Option<u32>),
    /// 相手が約束に反した（HTTP・WebSocket）。
    Protocol(String),
    /// 相手が閉じた（WebSocket の状態コード。無ければ 1006）。
    Closed(u16),
}

impl core::fmt::Display for NetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for NetError {}

/// 段ごとの所要（µs）。`--stats` に出す。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DialStats {
    pub dns_us: u32,
    pub tcp_us: u32,
    pub tls_us: u32,
    pub upgrade_us: u32,
    /// TLS を再開（PSK）でつないだか。
    pub resumed: bool,
}

/// `poll()` に入れる fd と、待つ向き。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interest {
    pub fd: RawFd,
    pub read: bool,
    pub write: bool,
}

/// TLS の設定を作る（TLS 1.3・X25519・ChaCha20-Poly1305 だけ。ALPN は http/1.1。再開は RAM）。
/// serve が 1 つ作って使い回す（再開のチケットがここに溜まる）。試験は自前の根を渡す。
pub fn tls_config(roots: rustls::RootCertStore) -> Arc<rustls::ClientConfig> {
    let mut provider = rustls::crypto::ring::default_provider();
    provider.cipher_suites.retain(|s| s.suite() == rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256);
    provider.kx_groups.retain(|g| g.name() == rustls::NamedGroup::X25519);
    let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3 + ChaCha20-Poly1305 + X25519 は ring にある")
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(cfg)
}

/// webpki-roots の埋め込みの根。
pub fn embedded_roots() -> rustls::RootCertStore {
    rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() }
}

// ---- システムコールの薄い包み ----

fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// fd を `io::Read` / `io::Write` として rustls に渡す。SIGPIPE を起こさない（`MSG_NOSIGNAL`）。
struct FdIo(RawFd);

impl Read for FdIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            // SAFETY: buf は有効な書き込み先。
            let n = unsafe { libc::recv(self.0, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n >= 0 {
                return Ok(n as usize);
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
}

impl Write for FdIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_vectored(&[IoSlice::new(buf)])
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        loop {
            // SAFETY: IoSlice は iovec と同じ並び（std が保証）。msghdr のほかの項目は 0。
            let n = unsafe {
                let mut m: libc::msghdr = core::mem::zeroed();
                m.msg_iov = bufs.as_ptr() as *mut libc::iovec;
                m.msg_iovlen = bufs.len().min(64) as _;
                libc::sendmsg(self.0, &m, libc::MSG_NOSIGNAL)
            };
            if n >= 0 {
                return Ok(n as usize);
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// ノンブロッキングでつなぎ始める。
fn connect_start(addr: &SocketAddr) -> Result<OwnedFd, i32> {
    // SAFETY: sockaddr_storage は全部 0 でよい。書くのは族に合った先頭の部分だけ。
    unsafe {
        let mut ss: libc::sockaddr_storage = core::mem::zeroed();
        let (family, len) = match addr {
            SocketAddr::V4(a) => {
                let sin = &mut *(&mut ss as *mut libc::sockaddr_storage).cast::<libc::sockaddr_in>();
                sin.sin_family = libc::AF_INET as libc::sa_family_t;
                sin.sin_port = a.port().to_be();
                sin.sin_addr.s_addr = u32::from_ne_bytes(a.ip().octets());
                (libc::AF_INET, core::mem::size_of::<libc::sockaddr_in>())
            }
            SocketAddr::V6(a) => {
                let sin6 = &mut *(&mut ss as *mut libc::sockaddr_storage).cast::<libc::sockaddr_in6>();
                sin6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                sin6.sin6_port = a.port().to_be();
                sin6.sin6_addr.s6_addr = a.ip().octets();
                sin6.sin6_flowinfo = a.flowinfo();
                sin6.sin6_scope_id = a.scope_id();
                (libc::AF_INET6, core::mem::size_of::<libc::sockaddr_in6>())
            }
        };
        let fd = libc::socket(family, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(errno());
        }
        let fd = OwnedFd::from_raw_fd(fd);
        if libc::connect(fd.as_raw_fd(), (&ss as *const libc::sockaddr_storage).cast(), len as libc::socklen_t) < 0 {
            let e = errno();
            if e != libc::EINPROGRESS && e != libc::EINTR {
                return Err(e);
            }
        }
        Ok(fd)
    }
}

/// つながったか。まだなら `None`。
fn connect_poll(fd: RawFd) -> Option<Result<(), i32>> {
    let mut p = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
    // SAFETY: p は有効。待たない（0ms）。
    if unsafe { libc::poll(&mut p, 1, 0) } <= 0 || p.revents == 0 {
        return None;
    }
    let mut err: libc::c_int = 0;
    let mut len = core::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: err と len は有効な書き込み先。
    if unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_ERROR, (&mut err as *mut libc::c_int).cast(), &mut len) } < 0 {
        return Some(Err(errno()));
    }
    Some(if err == 0 { Ok(()) } else { Err(err) })
}

/// マスクの鍵と `Sec-WebSocket-Key` 用の乱数（秘密ではない）。
struct Rng(u64);

impl Rng {
    fn new() -> Rng {
        let mut b = [0u8; 8];
        // SAFETY: b は 8 バイトの書き込み先。
        let n = unsafe { libc::getrandom(b.as_mut_ptr().cast(), b.len(), libc::GRND_NONBLOCK) };
        let mut s = u64::from_ne_bytes(b);
        if n != 8 || s == 0 {
            s = (now_us() ^ ((std::process::id() as u64) << 40)) | 1;
        }
        Rng(s)
    }

    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn mask(&mut self) -> [u8; 4] {
        ((self.next() >> 32) as u32).to_ne_bytes()
    }
}

// ---- ソケット 1 本（TLS あり・なし） ----

enum ReadRes {
    Data(usize),
    WouldBlock,
    Eof,
}

/// 送る列がこれより大きいまま空になったら、返す。
const OUT_KEEP: usize = 512 << 10;
/// 送り済みの分がこれだけ溜まったら、前へ詰める。
const OUT_COMPACT: usize = 128 << 10;

struct Link {
    fd: OwnedFd,
    peer: IpAddr,
    tls: Option<Box<rustls::ClientConnection>>,
    /// まだ rustls（TLS なしならソケット）に渡していない平文。`out_pos` から先が残り。
    out: Vec<u8>,
    out_pos: usize,
    /// 直前の [`Link::flush`] で、ソケットがいっぱいだった。
    blocked: bool,
}

impl Link {
    fn pending(&self) -> usize {
        self.out.len() - self.out_pos
    }

    fn wants_write(&self) -> bool {
        self.pending() > 0 || self.tls.as_ref().is_some_and(|t| t.wants_write())
    }

    /// 送れるだけ送る。
    fn flush(&mut self) -> Result<(), NetError> {
        self.blocked = false;
        let mut io = FdIo(self.fd.as_raw_fd());
        match &mut self.tls {
            None => {
                while self.out_pos < self.out.len() {
                    match io.write(&self.out[self.out_pos..]) {
                        Ok(0) => return Err(NetError::Closed(1006)),
                        Ok(n) => self.out_pos += n,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            self.blocked = true;
                            break;
                        }
                        Err(_) => return Err(NetError::Closed(1006)),
                    }
                }
            }
            Some(tls) => loop {
                // 平文 → rustls の中の列（上限つき。入っただけ進む）
                let mut moved = 0;
                if self.out_pos < self.out.len() {
                    moved = tls.writer().write(&self.out[self.out_pos..]).unwrap_or(0);
                    self.out_pos += moved;
                }
                // rustls の中の列 → ソケット（まとめて 1 回の sendmsg）
                while tls.wants_write() {
                    match tls.write_tls(&mut io) {
                        Ok(0) => return Err(NetError::Closed(1006)),
                        Ok(_) => {}
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            self.blocked = true;
                            break;
                        }
                        Err(_) => return Err(NetError::Closed(1006)),
                    }
                }
                if self.blocked || self.out_pos == self.out.len() || moved == 0 {
                    break;
                }
            },
        }
        if self.out_pos == self.out.len() {
            self.out.clear();
            self.out_pos = 0;
            if self.out.capacity() > OUT_KEEP {
                self.out.shrink_to(OUT_COMPACT);
            }
        } else if self.out_pos >= OUT_COMPACT {
            self.out.drain(..self.out_pos);
            self.out_pos = 0;
        }
        Ok(())
    }

    /// 平文を読む。`buf` は空でないこと。
    fn read(&mut self, buf: &mut [u8]) -> Result<ReadRes, NetError> {
        let mut io = FdIo(self.fd.as_raw_fd());
        let Some(tls) = &mut self.tls else {
            return match io.read(buf) {
                Ok(0) => Ok(ReadRes::Eof),
                Ok(n) => Ok(ReadRes::Data(n)),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(ReadRes::WouldBlock),
                Err(_) => Err(NetError::Closed(1006)),
            };
        };
        loop {
            match tls.reader().read(buf) {
                Ok(0) => return Ok(ReadRes::Eof),
                Ok(n) => return Ok(ReadRes::Data(n)),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(ReadRes::Eof),
                Err(_) => return Err(NetError::Closed(1006)),
            }
            match tls.read_tls(&mut io) {
                Ok(0) => return Ok(ReadRes::Eof),
                Ok(_) => {
                    if let Err(e) = tls.process_new_packets() {
                        // 相手への警告（alert）を 1 回だけ送ってみる。
                        let _ = tls.write_tls(&mut io);
                        return Err(classify_tls(&e, clock_synchronized()));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(ReadRes::WouldBlock),
                Err(_) => return Err(NetError::Closed(1006)),
            }
        }
    }
}

/// TLS の失敗を読み分ける。`synced` は時計が NTP と同期済みか。
fn classify_tls(e: &rustls::Error, synced: bool) -> NetError {
    use rustls::CertificateError as C;
    match e {
        rustls::Error::InvalidCertificate(C::NotValidYet | C::NotValidYetContext { .. }) => NetError::Clock,
        // 同期前の時計は当てにならない。期限切れに見えても、時計のせいとして知らせる。
        rustls::Error::InvalidCertificate(C::Expired | C::ExpiredContext { .. }) if !synced => NetError::Clock,
        _ => NetError::Tls(e.to_string()),
    }
}

// ---- 名前解決（使い捨てのスレッド） ----

type ResolveFn = fn(&str, u16) -> io::Result<Vec<SocketAddr>>;

fn sys_resolve(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    use std::net::ToSocketAddrs;
    Ok((host, port).to_socket_addrs()?.collect())
}

struct ResShared {
    efd: OwnedFd,
    slot: Mutex<Option<io::Result<Vec<SocketAddr>>>>,
}

struct Resolver {
    shared: Arc<ResShared>,
    started_us: u64,
}

impl Resolver {
    /// スレッドを 1 本作って引かせる。結果が入ったら eventfd が読めるようになる。
    /// [`Resolver`] を捨てても、スレッドは引き終わるまで残る（終われば消える）。
    fn spawn(resolve: ResolveFn, host: &str, port: u16, now_us: u64) -> Result<Resolver, NetError> {
        // SAFETY: eventfd は新しい fd を返す。
        let efd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if efd < 0 {
            return Err(NetError::Unreachable(Stage::Dns, errno()));
        }
        // SAFETY: efd は今作った、ほかに持ち主のいない fd。
        let shared = Arc::new(ResShared { efd: unsafe { OwnedFd::from_raw_fd(efd) }, slot: Mutex::new(None) });
        let (theirs, host) = (shared.clone(), host.to_owned());
        std::thread::Builder::new()
            .name("zc-dns".to_owned())
            .stack_size(256 << 10)
            .spawn(move || {
                let r = resolve(&host, port);
                *theirs.slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(r);
                let one = 1u64.to_ne_bytes();
                // SAFETY: one は 8 バイト。efd は theirs が生かしている。
                unsafe { libc::write(theirs.efd.as_raw_fd(), one.as_ptr().cast(), 8) };
            })
            .map_err(|e| NetError::Unreachable(Stage::Dns, e.raw_os_error().unwrap_or(0)))?;
        Ok(Resolver { shared, started_us: now_us })
    }

    fn take(&self) -> Option<io::Result<Vec<SocketAddr>>> {
        self.shared.slot.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

// ---- つなぐ ----

/// 並行につなぐ本数。
const MAX_PARALLEL: usize = 2;
/// 試す候補の数。
const MAX_CANDIDATES: usize = 4;

struct Attempt {
    fd: OwnedFd,
    addr: SocketAddr,
    started_us: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Connect,
    Tls,
    Upgrade,
}

/// つないでいる途中。fd が変わりうる（名前解決 → ソケット → 2 本目のソケット）ので、
/// 毎回の `poll()` の前に [`Dial::interests`] を聞く。
pub struct Dial {
    host: String,
    port: u16,
    tls_cfg: Option<Arc<rustls::ClientConfig>>,
    t: Timeouts,
    /// Upgrade の依頼（資格のヘッダ入り。送るときに [`Link`] へ移す）。
    request: Vec<u8>,
    key: String,
    resolve: ResolveFn,
    start_us: u64,
    /// この時刻になったら名前を引き始める（`hint` を先に試している間）。
    resolve_at: Option<u64>,
    resolver: Option<Resolver>,
    /// これから試す行き先。
    cands: VecDeque<SocketAddr>,
    /// もう試した（試している）行き先。
    tried: Vec<IpAddr>,
    socks: [Option<Attempt>; MAX_PARALLEL],
    /// 次の候補を並行に始める時刻。
    next_at: u64,
    last_err: Option<NetError>,
    phase: Phase,
    link: Option<Link>,
    phase_start_us: u64,
    /// ハンドシェイク・Upgrade の時間切れ。
    phase_deadline_us: u64,
    /// Upgrade の返事（頭がそろうまで溜める）。
    rbuf: Vec<u8>,
    stats: DialStats,
    rng: Rng,
}

/// [`Dial::advance`] の結果。
pub enum DialStep {
    /// まだ途中。
    Pending,
    /// つながった（Upgrade まで済んだ）。
    Open(Conn, DialStats),
}

fn us(d: Duration) -> u64 {
    d.as_micros() as u64
}

impl Dial {
    /// つなぎ始める。`tls` は `ep.tls` のときに使う設定（[`tls_config`]）。`hint` は前回
    /// つながった IP（あれば名前解決を待たずに先に試す）。`now_us` は単調時計
    /// （`zc_core::time::now_us`）。
    ///
    /// 行き先がこの機械自身（`localhost`・ループバックの IP）でなければ、先に既定の経路を
    /// 確かめる（無ければ [`NetError::Offline`]）。
    pub fn start(ep: &Endpoint, cred: &mut dyn Credential, tls: Option<Arc<rustls::ClientConfig>>, timeouts: Timeouts, hint: Option<IpAddr>, now_us: u64) -> Result<Dial, NetError> {
        let local = ep.host == "localhost" || ep.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
        if !local && !has_default_route() {
            return Err(NetError::Offline);
        }
        Dial::start_with(ep, cred, tls, timeouts, hint, now_us, sys_resolve)
    }

    /// 経路を確かめずに始める。名前の引き方を差し替えられる（試験用）。
    fn start_with(ep: &Endpoint, cred: &mut dyn Credential, tls: Option<Arc<rustls::ClientConfig>>, timeouts: Timeouts, hint: Option<IpAddr>, now_us: u64, resolve: ResolveFn) -> Result<Dial, NetError> {
        let tls_cfg = match (ep.tls, tls) {
            (true, Some(cfg)) => Some(cfg),
            (true, None) => return Err(NetError::Tls("TLS の設定が無い".to_owned())),
            (false, _) => None,
        };
        let mut headers = Vec::new();
        cred.headers(&mut headers).map_err(|e| NetError::Auth(if let AuthError::Rejected(s) = e { s } else { 0 }))?;
        let mut rng = Rng::new();
        let mut nonce = [0u8; 16];
        nonce[..8].copy_from_slice(&rng.next().to_ne_bytes());
        nonce[8..].copy_from_slice(&rng.next().to_ne_bytes());
        let key = http::encode_key(nonce);
        let mut request = Vec::with_capacity(512);
        http::UpgradeRequest { host: &ep.host_header(), path: &ep.path, key: &key, headers: &headers }.write(&mut request);

        let mut d = Dial {
            host: ep.host.clone(),
            port: ep.port,
            tls_cfg,
            t: timeouts,
            request,
            key,
            resolve,
            start_us: now_us,
            resolve_at: None,
            resolver: None,
            cands: VecDeque::new(),
            tried: Vec::new(),
            socks: [None, None],
            next_at: 0,
            last_err: None,
            phase: Phase::Connect,
            link: None,
            phase_start_us: now_us,
            phase_deadline_us: u64::MAX,
            rbuf: Vec::new(),
            stats: DialStats::default(),
            rng,
        };
        match ep.host.parse::<IpAddr>() {
            // IP がそのまま書いてある: 引かない。
            Ok(ip) => d.cands.push_back(SocketAddr::new(ip, ep.port)),
            Err(_) => match hint {
                // 前回の IP を先に試す。名前を引くのは、少し待ってもつながらないときだけ。
                Some(ip) => {
                    d.cands.push_back(SocketAddr::new(ip, ep.port));
                    d.resolve_at = Some(now_us + us(timeouts.family_stagger));
                }
                None => d.resolver = Some(Resolver::spawn(resolve, &ep.host, ep.port, now_us)?),
            },
        }
        d.start_attempts(now_us);
        Ok(d)
    }

    /// 待つ fd を `out` に足す（最大 3 つ。`out` は消さない）。
    pub fn interests(&self, out: &mut Vec<Interest>) {
        if let Some(link) = &self.link {
            out.push(Interest { fd: link.fd.as_raw_fd(), read: true, write: link.wants_write() });
            return;
        }
        if let Some(r) = &self.resolver {
            out.push(Interest { fd: r.shared.efd.as_raw_fd(), read: true, write: false });
        }
        for a in self.socks.iter().flatten() {
            out.push(Interest { fd: a.fd.as_raw_fd(), read: false, write: true });
        }
    }

    /// 次の時間切れの時刻（µs）。
    pub fn deadline_us(&self) -> u64 {
        if self.link.is_some() {
            return self.phase_deadline_us;
        }
        let mut d = self.start_us + us(self.t.dns) + us(self.t.connect);
        if let Some(at) = self.resolve_at {
            d = d.min(at);
        }
        if let Some(r) = &self.resolver {
            d = d.min(r.started_us + us(self.t.dns));
        }
        for a in self.socks.iter().flatten() {
            d = d.min(a.started_us + us(self.t.connect));
        }
        if !self.cands.is_empty() && self.in_flight() < MAX_PARALLEL {
            d = d.min(self.next_at);
        }
        d
    }

    /// fd が起きた・時間が来たら呼ぶ。
    pub fn advance(&mut self, now_us: u64) -> Result<DialStep, NetError> {
        if self.link.is_none() {
            self.advance_connect(now_us)?;
        }
        if self.link.is_some() {
            return self.advance_stream(now_us);
        }
        Ok(DialStep::Pending)
    }

    fn in_flight(&self) -> usize {
        self.socks.iter().flatten().count()
    }

    /// 候補へつなぎ始める（1 本目はすぐ、2 本目は [`Timeouts::family_stagger`] ずらして）。
    fn start_attempts(&mut self, now_us: u64) {
        while self.in_flight() < MAX_PARALLEL && !self.cands.is_empty() && (self.in_flight() == 0 || now_us >= self.next_at) {
            let addr = self.cands.pop_front().unwrap();
            self.tried.push(addr.ip());
            match connect_start(&addr) {
                Ok(fd) => {
                    let slot = self.socks.iter_mut().find(|s| s.is_none()).unwrap();
                    *slot = Some(Attempt { fd, addr, started_us: now_us });
                    self.next_at = now_us + us(self.t.family_stagger);
                }
                // その場で分かる失敗（経路が無い族など）は、待たずに次へ。
                Err(e) => self.last_err = Some(NetError::Unreachable(Stage::Tcp, e)),
            }
        }
    }

    /// 引いた結果を候補に足す: 先頭と、別の族の先頭を先に。試したものは除く。
    fn add_candidates(&mut self, addrs: Vec<SocketAddr>) {
        let mut rest: Vec<SocketAddr> = Vec::new();
        for a in addrs {
            if !self.tried.contains(&a.ip()) && !rest.contains(&a) && !self.cands.contains(&a) {
                rest.push(a);
            }
        }
        if let Some(first) = rest.first().copied() {
            if let Some(p) = rest.iter().position(|a| a.is_ipv4() != first.is_ipv4()) {
                let other = rest.remove(p);
                rest.insert(1, other);
            }
        }
        let room = MAX_CANDIDATES.saturating_sub(self.cands.len());
        self.cands.extend(rest.into_iter().take(room));
    }

    fn advance_connect(&mut self, now_us: u64) -> Result<(), NetError> {
        // 名前解決の結果
        if let Some(r) = &self.resolver {
            if let Some(res) = r.take() {
                self.stats.dns_us = (now_us.saturating_sub(r.started_us)) as u32;
                self.resolver = None;
                match res {
                    Ok(addrs) if !addrs.is_empty() => self.add_candidates(addrs),
                    Ok(_) => self.last_err = Some(NetError::Unreachable(Stage::Dns, 0)),
                    Err(e) => self.last_err = Some(NetError::Unreachable(Stage::Dns, e.raw_os_error().unwrap_or(0))),
                }
            } else if now_us >= r.started_us + us(self.t.dns) {
                // スレッドは捨てる（getaddrinfo は取り消せない。戻ってくれば勝手に終わる）。
                self.resolver = None;
                self.last_err = Some(NetError::Timeout(Stage::Dns));
            }
        }
        // つなぎかけのソケット
        for i in 0..MAX_PARALLEL {
            let Some(a) = &self.socks[i] else { continue };
            match connect_poll(a.fd.as_raw_fd()) {
                Some(Ok(())) => {
                    let a = self.socks[i].take().unwrap();
                    self.socks = [None, None]; // 負けた方は閉じる
                    return self.connected(a, now_us);
                }
                Some(Err(e)) => {
                    self.socks[i] = None;
                    self.last_err = Some(NetError::Unreachable(Stage::Tcp, e));
                }
                None if now_us >= a.started_us + us(self.t.connect) => {
                    self.socks[i] = None;
                    self.last_err = Some(NetError::Timeout(Stage::Tcp));
                }
                None => {}
            }
        }
        self.start_attempts(now_us);
        // 前回の IP がだめ・遅いなら、名前を引く。
        if let Some(at) = self.resolve_at {
            if now_us >= at || (self.in_flight() == 0 && self.cands.is_empty()) {
                self.resolve_at = None;
                self.resolver = Some(Resolver::spawn(self.resolve, &self.host, self.port, now_us)?);
            }
        }
        let nothing_left = self.in_flight() == 0 && self.cands.is_empty() && self.resolver.is_none() && self.resolve_at.is_none();
        if nothing_left || now_us >= self.start_us + us(self.t.dns) + us(self.t.connect) {
            return Err(self.last_err.take().unwrap_or(NetError::Timeout(Stage::Tcp)));
        }
        Ok(())
    }

    /// TCP がつながった。TLS を始め、Upgrade の依頼を送る列に入れる。
    fn connected(&mut self, a: Attempt, now_us: u64) -> Result<(), NetError> {
        self.stats.tcp_us = now_us.saturating_sub(a.started_us) as u32;
        self.resolver = None;
        self.resolve_at = None;
        self.cands.clear();
        // 小さな制御メッセージ（cancel など）を Nagle で遅らせない。まとめるのは自分でやる。
        let one: libc::c_int = 1;
        // SAFETY: one は有効な c_int。
        unsafe { libc::setsockopt(a.fd.as_raw_fd(), libc::IPPROTO_TCP, libc::TCP_NODELAY, (&one as *const libc::c_int).cast(), core::mem::size_of::<libc::c_int>() as libc::socklen_t) };
        let tls = match &self.tls_cfg {
            Some(cfg) => {
                let name = rustls::pki_types::ServerName::try_from(self.host.clone()).map_err(|_| NetError::Tls("ホスト名の形がおかしい".to_owned()))?;
                let conn = rustls::ClientConnection::new(cfg.clone(), name).map_err(|e| NetError::Tls(e.to_string()))?;
                Some(Box::new(conn))
            }
            None => None,
        };
        self.phase = if tls.is_some() { Phase::Tls } else { Phase::Upgrade };
        self.phase_start_us = now_us;
        self.phase_deadline_us = now_us + us(self.t.handshake);
        self.link = Some(Link { fd: a.fd, peer: a.addr.ip(), tls, out: std::mem::take(&mut self.request), out_pos: 0, blocked: false });
        Ok(())
    }

    /// ハンドシェイクが済んだら、段を進める。
    fn handshake_done(&mut self, now_us: u64) {
        let Some(tls) = self.link.as_ref().and_then(|l| l.tls.as_ref()) else { return };
        if self.phase == Phase::Tls && !tls.is_handshaking() {
            self.stats.tls_us = now_us.saturating_sub(self.phase_start_us) as u32;
            self.stats.resumed = tls.handshake_kind() == Some(rustls::HandshakeKind::Resumed);
            self.phase = Phase::Upgrade;
            self.phase_start_us = now_us;
            self.phase_deadline_us = now_us + us(self.t.handshake);
        }
    }

    fn advance_stream(&mut self, now_us: u64) -> Result<DialStep, NetError> {
        let mut buf = [0u8; 2048];
        loop {
            let link = self.link.as_mut().unwrap();
            link.flush()?;
            self.handshake_done(now_us);
            let link = self.link.as_mut().unwrap();
            match link.read(&mut buf)? {
                ReadRes::Data(n) => {
                    self.rbuf.extend_from_slice(&buf[..n]);
                    match http::parse_upgrade_response(&self.rbuf, &self.key) {
                        http::UpgradeResponse::Incomplete => {}
                        http::UpgradeResponse::Switched { consumed } => {
                            self.handshake_done(now_us);
                            self.stats.upgrade_us = now_us.saturating_sub(self.phase_start_us) as u32;
                            let mut conn = Conn {
                                link: self.link.take().unwrap(),
                                dec: ws::Decoder::new(ws::Role::Client, wire::limits::MAX_TEXT_FRAME.max(wire::limits::CHUNK_BYTES + 64)),
                                enc: ws::Encoder::new(ws::Role::Client),
                                scratch: vec![0u8; SCRATCH].into_boxed_slice(),
                                last_rx_us: now_us,
                                rng: Rng(self.rng.next() | 1),
                            };
                            // 101 と同じ読み取りに、もう最初のフレームが入っていることがある。
                            if self.rbuf.len() > consumed {
                                conn.dec.push(&self.rbuf[consumed..]);
                            }
                            return Ok(DialStep::Open(conn, self.stats));
                        }
                        http::UpgradeResponse::Rejected { status, retry_after_s, .. } => {
                            return Err(match status {
                                429 | 503 => NetError::Busy(status, retry_after_s),
                                // 302（ログイン画面へ）と、101 でない 2xx（HTML が返ってきた）は認証の失敗。
                                // たどらない。
                                200..=399 | 401 | 403 | 407 => NetError::Auth(status),
                                _ => NetError::Protocol(format!("HTTP {status}")),
                            });
                        }
                        http::UpgradeResponse::Invalid(why) => return Err(NetError::Protocol(why.to_owned())),
                    }
                }
                ReadRes::WouldBlock => {
                    // 読んだ結果、送るもの（クライアントの Finished など）ができていれば続ける。
                    if link.wants_write() && !link.blocked {
                        continue;
                    }
                    self.handshake_done(now_us);
                    break;
                }
                ReadRes::Eof => return Err(NetError::Closed(1006)),
            }
        }
        if now_us >= self.phase_deadline_us {
            return Err(NetError::Timeout(if self.phase == Phase::Tls { Stage::Tls } else { Stage::Upgrade }));
        }
        Ok(DialStep::Pending)
    }
}

// ---- 開いた接続 ----

/// 受け取ったもの。借用は次の呼び出しまで有効。
#[derive(Debug, PartialEq, Eq)]
pub enum Incoming<'a> {
    Text(&'a str),
    Binary(&'a [u8]),
}

/// 読むときの作業用バッファ（TLS のレコード 1 つぶん）。
const SCRATCH: usize = 16 << 10;

/// 開いた WebSocket 1 本。ping への pong と close の返事は中で済ませる。
pub struct Conn {
    link: Link,
    dec: ws::Decoder,
    enc: ws::Encoder,
    scratch: Box<[u8]>,
    last_rx_us: u64,
    rng: Rng,
}

impl Conn {
    pub fn interest(&self) -> Interest {
        Interest { fd: self.link.fd.as_raw_fd(), read: true, write: self.link.wants_write() }
    }

    /// つながった相手の IP（次回の `hint`）。
    pub fn peer_ip(&self) -> IpAddr {
        self.link.peer
    }

    /// 読めるようになったら呼ぶ。届いたメッセージを 1 つずつ `sink` に渡す。1 回の呼び出しで
    /// 読むのは `budget` バイトまで（UNIX ソケットの相手を待たせない）。まだ残っていれば
    /// `Ok(true)`（**そのときは `poll()` を待たずにもう一度呼ぶ**: 残りは rustls と復号の
    /// バッファの中にあって、fd は読めるようにならない）。
    ///
    /// 相手が閉じたら [`NetError::Closed`]。その前に届いていたメッセージは `sink` に渡してある。
    pub fn on_readable(&mut self, budget: usize, sink: &mut dyn FnMut(Incoming<'_>)) -> Result<bool, NetError> {
        let mut used = 0;
        loop {
            // そろっているメッセージを全部渡す
            loop {
                match self.dec.next() {
                    Ok(Some(ws::Message::Text(t))) => sink(Incoming::Text(t)),
                    Ok(Some(ws::Message::Binary(b))) => sink(Incoming::Binary(b)),
                    Ok(Some(ws::Message::Ping(p))) => self.enc.pong(p, self.rng.mask(), &mut self.link.out),
                    Ok(Some(ws::Message::Pong(_))) => {}
                    Ok(Some(ws::Message::Close(code))) => {
                        self.enc.close(1000, self.rng.mask(), &mut self.link.out);
                        let _ = self.link.flush();
                        return Err(NetError::Closed(code));
                    }
                    Ok(None) => break,
                    Err(e) => return Err(NetError::Protocol(e.to_string())),
                }
            }
            if used >= budget {
                return Ok(true);
            }
            let want = self.scratch.len().min(budget - used);
            match self.link.read(&mut self.scratch[..want])? {
                ReadRes::Data(n) => {
                    used += n;
                    self.last_rx_us = now_us();
                    self.dec.push(&self.scratch[..n]);
                }
                ReadRes::WouldBlock => return Ok(false),
                ReadRes::Eof => return Err(NetError::Closed(1006)),
            }
        }
    }

    /// 書けるようになったら呼ぶ（溜まっている分を送る）。`send_*` を呼んだあとは、
    /// `poll()` を待たずに 1 回呼ぶとよい（まとめて 1 回の送信になる）。
    pub fn on_writable(&mut self) -> Result<(), NetError> {
        self.link.flush()
    }

    /// テキストを送る列に足す（待たない）。
    pub fn send_text(&mut self, text: &str) {
        self.enc.text(text, self.rng.mask(), &mut self.link.out);
    }

    /// バイナリ（頭 + 中身）を送る列に足す。
    pub fn send_binary(&mut self, head: &[u8], body: &[u8]) {
        self.enc.binary2(head, body, self.rng.mask(), &mut self.link.out);
    }

    /// まだ送っていないバイト数。呼ぶ側は、これが 256KB を超えている間は次の中身を
    /// 読まない（ファイルをヒープに溜めない）。
    pub fn queued_bytes(&self) -> usize {
        self.link.pending()
    }

    /// 最後に何か届いた時刻（µs）。[`Timeouts::silence`] の判定用。
    pub fn last_rx_us(&self) -> u64 {
        self.last_rx_us
    }

    /// `silence` のあいだ何も届いていないか。ジョブの実行中にこれが `true` になったら、
    /// 切れたものとして扱う（[`NetError::Timeout`]（[`Stage::Open`]））。待機中は調べない。
    pub fn is_silent(&self, now_us: u64, silence: Duration) -> bool {
        now_us.saturating_sub(self.last_rx_us) >= us(silence)
    }

    /// 静かに閉じる（close を送り、相手の close を待たずに fd を閉じてよい）。
    pub fn close(mut self) {
        self.enc.close(1000, self.rng.mask(), &mut self.link.out);
        let _ = self.link.flush();
        if let Some(tls) = &mut self.link.tls {
            tls.send_close_notify();
            let _ = self.link.flush();
        }
    }
}

// ---- つなぐ前に分かること ----

/// 既定の経路があるか（`/proc/net/route` と `/sys/class/net/<if>/operstate`。待たない）。
pub fn has_default_route() -> bool {
    default_route_in(Path::new("/proc/net"), Path::new("/sys/class/net"))
}

/// `proc_net` の経路の表に、落ちていないインターフェースを通る既定の経路があるか。
/// 表を読めないとき（分からないとき）は「ある」とする（つなぐのを止めない）。
fn default_route_in(proc_net: &Path, sys_net: &Path) -> bool {
    let v4 = std::fs::read_to_string(proc_net.join("route"));
    let v6 = std::fs::read_to_string(proc_net.join("ipv6_route"));
    if v4.is_err() && v6.is_err() {
        return true;
    }
    const RTF_UP: u32 = 0x1;
    const RTF_REJECT: u32 = 0x200;
    let usable = |flags: &str| u32::from_str_radix(flags, 16).is_ok_and(|f| f & RTF_UP != 0 && f & RTF_REJECT == 0);
    let oper_up = |iface: &str| {
        // up と unknown（tun・ppp など、状態を持たないもの）は通れる。読めなければ止めない。
        match std::fs::read_to_string(sys_net.join(iface).join("operstate")) {
            Ok(s) => !matches!(s.trim(), "down" | "lowerlayerdown" | "notpresent" | "dormant"),
            Err(_) => true,
        }
    };
    // Iface Destination Gateway Flags RefCnt Use Metric Mask …
    for line in v4.as_deref().unwrap_or("").lines().skip(1) {
        let f: Vec<&str> = line.split_ascii_whitespace().collect();
        if f.len() >= 8 && f[1] == "00000000" && f[7] == "00000000" && usable(f[3]) && oper_up(f[0]) {
            return true;
        }
    }
    // 宛先(32 桁) 長さ 送り元 長さ 次 メトリック 参照 使用 フラグ Iface
    for line in v6.as_deref().unwrap_or("").lines() {
        let f: Vec<&str> = line.split_ascii_whitespace().collect();
        if f.len() >= 10 && f[0].bytes().all(|b| b == b'0') && f[1] == "00" && usable(f[8]) && f[9] != "lo" && oper_up(f[9]) {
            return true;
        }
    }
    false
}

/// 時計が NTP と同期済みか（`/run/systemd/timesync/synchronized` があるか）。
pub fn clock_synchronized() -> bool {
    Path::new("/run/systemd/timesync/synchronized").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 決定 ①（ring）と「ChaCha20-Poly1305 と X25519 だけ」が rustls の設定として組めること。
    /// aarch64 のビルド（`build.sh`）と合わせて、雛形の時点で確かめておく。
    #[test]
    fn tls_config_offers_only_chacha20_and_x25519() {
        let cfg = tls_config(embedded_roots());
        let p = cfg.crypto_provider();
        assert_eq!(p.cipher_suites.len(), 1);
        assert_eq!(p.cipher_suites[0].suite(), rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256);
        assert_eq!(p.kx_groups.len(), 1);
        assert_eq!(p.kx_groups[0].name(), rustls::NamedGroup::X25519);
        assert_eq!(cfg.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }
}

// 受け入れ試験（課題 D1。docs/stage3-design.md §14 の表）: ループバックの受付（TLS なし）と
// 試験用の TLS サーバ（rustls のサーバ側 + tests/data/ の証明書）に対して、
// つなぐ → Upgrade → 往復、途中で切る、黙る、302 / 403 / 429、期限切れ・未来の証明書、
// AES-GCM しか出さないサーバ（つながらないこと）、再開、少しずつしか書けないソケット。
// `zc-wire` の中身が入るまでは、ここ（crate の中の試験）として動かす。
#[cfg(test)]
#[path = "../tests/inproc/net.rs"]
mod inproc;
