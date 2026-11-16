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

use crate::auth::Credential;
use std::net::IpAddr;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::time::Duration;

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
    pub fn parse(s: &str) -> Option<Endpoint> {
        let _ = s;
        todo!("D1")
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
    /// 401 / 403 / 302（認証の失敗）。
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

/// つないでいる途中。fd が変わりうる（名前解決 → ソケット → 2 本目のソケット）ので、
/// 毎回の `poll()` の前に [`Dial::interests`] を聞く。
pub struct Dial {
    _private: (),
}

/// [`Dial::advance`] の結果。
pub enum DialStep {
    /// まだ途中。
    Pending,
    /// つながった（Upgrade まで済んだ）。
    Open(Conn, DialStats),
}

impl Dial {
    /// つなぎ始める。`tls` は `ep.tls` のときに使う設定（[`tls_config`]）。`hint` は前回
    /// つながった IP（あれば名前解決を待たずに先に試す）。`now_us` は単調時計
    /// （`zc_core::time::now_us`）。
    pub fn start(ep: &Endpoint, cred: &mut dyn Credential, tls: Option<Arc<rustls::ClientConfig>>, timeouts: Timeouts, hint: Option<IpAddr>, now_us: u64) -> Result<Dial, NetError> {
        let _ = (ep, cred, tls, timeouts, hint, now_us);
        todo!("D1")
    }

    /// 待つ fd（最大 3 つ）。
    pub fn interests(&self, out: &mut Vec<Interest>) {
        let _ = out;
        todo!("D1")
    }

    /// 次の時間切れの時刻（µs）。
    pub fn deadline_us(&self) -> u64 {
        todo!("D1")
    }

    /// fd が起きた・時間が来たら呼ぶ。
    pub fn advance(&mut self, now_us: u64) -> Result<DialStep, NetError> {
        let _ = now_us;
        todo!("D1")
    }
}

/// 受け取ったもの。借用は次の呼び出しまで有効。
#[derive(Debug, PartialEq, Eq)]
pub enum Incoming<'a> {
    Text(&'a str),
    Binary(&'a [u8]),
}

/// 開いた WebSocket 1 本。ping への pong と close の返事は中で済ませる。
pub struct Conn {
    _private: (),
}

impl Conn {
    pub fn interest(&self) -> Interest {
        todo!("D1")
    }

    /// つながった相手の IP（次回の `hint`）。
    pub fn peer_ip(&self) -> IpAddr {
        todo!("D1")
    }

    /// 読めるようになったら呼ぶ。届いたメッセージを 1 つずつ `sink` に渡す。1 回の呼び出しで
    /// 読むのは `budget` バイトまで（UNIX ソケットの相手を待たせない）。まだ残っていれば
    /// `Ok(true)`。
    pub fn on_readable(&mut self, budget: usize, sink: &mut dyn FnMut(Incoming<'_>)) -> Result<bool, NetError> {
        let _ = (budget, sink);
        todo!("D1")
    }

    /// 書けるようになったら呼ぶ（溜まっている分を送る）。
    pub fn on_writable(&mut self) -> Result<(), NetError> {
        todo!("D1")
    }

    /// テキストを送る列に足す（待たない）。
    pub fn send_text(&mut self, text: &str) {
        let _ = text;
        todo!("D1")
    }

    /// バイナリ（頭 + 中身）を送る列に足す。
    pub fn send_binary(&mut self, head: &[u8], body: &[u8]) {
        let _ = (head, body);
        todo!("D1")
    }

    /// まだ送っていないバイト数。呼ぶ側は、これが 256KB を超えている間は次の中身を
    /// 読まない（ファイルをヒープに溜めない）。
    pub fn queued_bytes(&self) -> usize {
        todo!("D1")
    }

    /// 最後に何か届いた時刻（µs）。[`Timeouts::silence`] の判定用。
    pub fn last_rx_us(&self) -> u64 {
        todo!("D1")
    }

    /// 静かに閉じる（close を送り、相手の close を待たずに fd を閉じてよい）。
    pub fn close(self) {
        todo!("D1")
    }
}

/// 既定の経路があるか（`/proc/net/route` と `/sys/class/net/<if>/operstate`。待たない）。
pub fn has_default_route() -> bool {
    todo!("D1")
}

/// 時計が NTP と同期済みか（`/run/systemd/timesync/synchronized` があるか）。
pub fn clock_synchronized() -> bool {
    todo!("D1")
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

    // 受け入れ試験（課題 D1。docs/stage3-design.md §14 の表）: ループバックの受付（TLS なし）と
    // 試験用の TLS サーバ（rustls のサーバ側 + その場で作った証明書）に対して、
    // つなぐ → Upgrade → 往復、途中で切る、黙る、302 / 403 / 429、期限切れ・未来の証明書、
    // AES-GCM しか出さないサーバ（つながらないこと）、再開、1 バイトずつしか書けないソケット。
}
