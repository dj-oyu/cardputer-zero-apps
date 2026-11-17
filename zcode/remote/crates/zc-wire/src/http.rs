//! WebSocket の Upgrade（HTTP/1.1）。I/O を持たない（課題 W1）。
//!
//! 実機側は依頼を作って返事を読む。受付は依頼を読んで返事を作る。どちらも頭（ヘッダ）は
//! 8KB まで。受付は Upgrade 以外に `GET /healthz`（`ok`）と `GET /v1/status`（JSON）にも
//! 答えるので、依頼の読み取りは Upgrade でないものも返す。
//!
//! 読み取りの決まり:
//!
//! - 頭の終わりは空行。行の区切りは CRLF（LF だけも受ける）。読めたところまでで頭が
//!   8KB を超えたら [`UpgradeResponse::Invalid`] / [`Request::Invalid`]
//! - ヘッダ名は大文字小文字を区別しない。`Connection` などの値はカンマ区切りの語として見る
//! - 行の折り返し（空白で始まる行）は受けない
//! - 返事（実機側）: 101 なら `Upgrade: websocket`・`Connection: upgrade`・
//!   `Sec-WebSocket-Accept` を確かめる。`Sec-WebSocket-Protocol` は、あれば
//!   [`crate::SUBPROTOCOL`] と一致すること（無いのは許す: 中継が付け忘れても動くように）。
//!   `Sec-WebSocket-Extensions` があれば断る（拡張は頼んでいない）
//! - 依頼（受付側）: `GET` だけ。パスから `?` 以降を落とす。Upgrade なら版 13 と、16 バイトを
//!   表す `Sec-WebSocket-Key` を確かめる。中身（`Content-Length` が 0 でない・
//!   `Transfer-Encoding`）のある依頼は断る

/// 頭の上限。
const MAX_HEAD: usize = 8 << 10;

/// RFC 6455 の GUID（`Sec-WebSocket-Accept` の計算に使う）。
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Upgrade の依頼（実機側が作る）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeRequest<'a> {
    /// `Host` ヘッダ（ポートつきでもよい）。
    pub host: &'a str,
    /// 依頼先（[`crate::WS_PATH`]）。
    pub path: &'a str,
    /// `Sec-WebSocket-Key`（16 バイトの乱数の base64）。
    pub key: &'a str,
    /// 認証などの追加のヘッダ（`CF-Access-Client-Id` など）。**ログに出さない。**
    pub headers: &'a [(String, String)],
}

impl UpgradeRequest<'_> {
    /// 依頼のバイト列を `out` に足す。`Sec-WebSocket-Protocol` は [`crate::SUBPROTOCOL`]、
    /// `Sec-WebSocket-Version` は 13。
    ///
    /// 名前か値に CR・LF・NUL を含む追加のヘッダは書かない（ヘッダの差し込みを防ぐ）。
    pub fn write(&self, out: &mut Vec<u8>) {
        let mut s = String::with_capacity(256);
        s.push_str("GET ");
        s.push_str(self.path);
        s.push_str(" HTTP/1.1\r\nHost: ");
        s.push_str(self.host);
        s.push_str("\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ");
        s.push_str(self.key);
        s.push_str("\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: ");
        s.push_str(crate::SUBPROTOCOL);
        s.push_str("\r\n");
        let bad = |t: &str| t.is_empty() || t.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0));
        for (name, value) in self.headers {
            if bad(name) || name.contains(':') || value.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
                continue;
            }
            s.push_str(name);
            s.push_str(": ");
            s.push_str(value);
            s.push_str("\r\n");
        }
        s.push_str("\r\n");
        out.extend_from_slice(s.as_bytes());
    }
}

/// Upgrade の返事を読んだ結果（実機側）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpgradeResponse {
    /// まだ頭がそろっていない。
    Incomplete,
    /// 101。`consumed` は頭の長さ（その後ろはもう WebSocket のフレーム）。
    Switched { consumed: usize },
    /// 101 以外。`status` と、`Location`（302 のとき）・`Retry-After`（429 / 503）。
    /// **302 と HTML は認証の失敗として扱い、たどらない**（Access の方針が Service Auth で
    /// ないとログイン画面へ飛ばされる）。
    ///
    /// `location` は返事にあれば status によらず入れる。`retry_after_s` は秒数の形のときだけ
    /// （日時の形は `None`）。
    Rejected { status: u16, location: Option<String>, retry_after_s: Option<u32> },
    /// HTTP として読めない・`Sec-WebSocket-Accept` が合わない・頭が 8KB を超えた。
    Invalid(&'static str),
}

/// 頭を読んだもの。
struct Head<'a> {
    /// 最初の行。
    first: &'a str,
    headers: Vec<(&'a str, &'a str)>,
    /// 頭の長さ（空行の後ろまで）。
    consumed: usize,
}

impl<'a> Head<'a> {
    /// 名前の一致するヘッダの値（最初のもの）。
    fn get(&self, name: &str) -> Option<&'a str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| *v)
    }

    /// 名前の一致するヘッダの値を、全部の行にわたってカンマで分けた語。
    fn tokens<'s>(&'s self, name: &'s str) -> impl Iterator<Item = &'a str> + 's {
        self.headers
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .flat_map(|(_, v)| v.split(','))
            .map(|t| t.trim_matches(|c| c == ' ' || c == '\t'))
            .filter(|t| !t.is_empty())
    }

    fn has_token(&self, name: &str, token: &str) -> bool {
        self.tokens(name).any(|t| t.eq_ignore_ascii_case(token))
    }
}

/// 頭の終わり（空行の後ろの位置）を探す。
fn find_head_end(buf: &[u8]) -> Option<usize> {
    let mut start = 0;
    while let Some(i) = buf[start..].iter().position(|&b| b == b'\n') {
        let nl = start + i;
        let line = &buf[start..nl];
        if line.is_empty() || line == b"\r" {
            return Some(nl + 1);
        }
        start = nl + 1;
    }
    None
}

/// 頭を読む。`Ok(None)` はまだそろっていない。
fn parse_head(buf: &[u8]) -> Result<Option<Head<'_>>, &'static str> {
    // 8KB より先は探さない（そこで終わる頭は、どのみち上限を超えている）
    let Some(end) = find_head_end(&buf[..buf.len().min(MAX_HEAD)]) else {
        return if buf.len() >= MAX_HEAD { Err("頭が 8KB を超えた") } else { Ok(None) };
    };
    let text = core::str::from_utf8(&buf[..end]).map_err(|_| "頭が UTF-8 でない")?;
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let first = lines.next().unwrap_or("");
    if first.is_empty() {
        return Err("最初の行が無い");
    }
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            return Err("ヘッダの折り返し");
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err("ヘッダに : が無い");
        };
        if name.is_empty() || name.bytes().any(|b| b <= b' ' || b == 0x7f) {
            return Err("ヘッダ名が不正");
        }
        headers.push((name, value.trim_matches(|c| c == ' ' || c == '\t')));
    }
    Ok(Some(Head { first, headers, consumed: end }))
}

/// 返事を読む。`key` は依頼に入れた `Sec-WebSocket-Key`。
pub fn parse_upgrade_response(buf: &[u8], key: &str) -> UpgradeResponse {
    let head = match parse_head(buf) {
        Ok(Some(h)) => h,
        Ok(None) => return UpgradeResponse::Incomplete,
        Err(e) => return UpgradeResponse::Invalid(e),
    };
    // HTTP/1.x 101 Switching Protocols
    let mut parts = head.first.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    let code = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") || code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return UpgradeResponse::Invalid("HTTP の返事でない");
    }
    let status: u16 = code.parse().expect("3 桁の数字");
    if status != 101 {
        let location = head.get("Location").map(str::to_owned);
        let retry_after_s = head.get("Retry-After").and_then(|v| v.parse::<u32>().ok());
        return UpgradeResponse::Rejected { status, location, retry_after_s };
    }
    if !head.has_token("Upgrade", "websocket") {
        return UpgradeResponse::Invalid("Upgrade: websocket が無い");
    }
    if !head.has_token("Connection", "upgrade") {
        return UpgradeResponse::Invalid("Connection: upgrade が無い");
    }
    match head.get("Sec-WebSocket-Accept") {
        Some(a) if a == accept_key(key) => {}
        Some(_) => return UpgradeResponse::Invalid("Sec-WebSocket-Accept が合わない"),
        None => return UpgradeResponse::Invalid("Sec-WebSocket-Accept が無い"),
    }
    if let Some(p) = head.get("Sec-WebSocket-Protocol") {
        if p != crate::SUBPROTOCOL {
            return UpgradeResponse::Invalid("頼んでいないサブプロトコル");
        }
    }
    if head.get("Sec-WebSocket-Extensions").is_some() {
        return UpgradeResponse::Invalid("頼んでいない拡張");
    }
    UpgradeResponse::Switched { consumed: head.consumed }
}

/// 受付が読んだ依頼。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Incomplete,
    /// WebSocket の Upgrade。`accept` は返事に入れる `Sec-WebSocket-Accept`。
    /// `device` は Worker が付けた `X-Zc-Device`（PC 直結では無い）。
    Upgrade { path: String, accept: String, subprotocol_ok: bool, device: Option<String>, consumed: usize },
    /// ふつうの GET（`/healthz`・`/v1/status`）。
    Get { path: String, consumed: usize },
    Invalid(&'static str),
}

/// 依頼を読む（受付側）。
pub fn parse_request(buf: &[u8]) -> Request {
    let head = match parse_head(buf) {
        Ok(Some(h)) => h,
        Ok(None) => return Request::Incomplete,
        Err(e) => return Request::Invalid(e),
    };
    let mut parts = head.first.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Request::Invalid("依頼の行が読めない");
    };
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Request::Invalid("HTTP の版");
    }
    if method != "GET" {
        return Request::Invalid("GET でない");
    }
    if !target.starts_with('/') {
        return Request::Invalid("パスが / で始まらない");
    }
    let path = target.split_once('?').map_or(target, |(p, _)| p).to_owned();
    if head.get("Transfer-Encoding").is_some() || head.get("Content-Length").is_some_and(|v| v != "0") {
        return Request::Invalid("中身のある依頼");
    }
    if !head.has_token("Upgrade", "websocket") {
        return Request::Get { path, consumed: head.consumed };
    }
    if version != "HTTP/1.1" {
        return Request::Invalid("Upgrade は HTTP/1.1 だけ");
    }
    if !head.has_token("Connection", "upgrade") {
        return Request::Invalid("Connection: upgrade が無い");
    }
    if head.get("Sec-WebSocket-Version") != Some("13") {
        return Request::Invalid("Sec-WebSocket-Version が 13 でない");
    }
    let Some(key) = head.get("Sec-WebSocket-Key").filter(|k| is_key(k)) else {
        return Request::Invalid("Sec-WebSocket-Key が不正");
    };
    let subprotocol_ok = head.has_token("Sec-WebSocket-Protocol", crate::SUBPROTOCOL);
    let device = head.get("X-Zc-Device").filter(|d| !d.is_empty()).map(str::to_owned);
    Request::Upgrade { path, accept: accept_key(key), subprotocol_ok, device, consumed: head.consumed }
}

/// 101 の返事を `out` に足す（受付側）。
pub fn write_switching(accept: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ");
    out.extend_from_slice(accept.as_bytes());
    out.extend_from_slice(b"\r\nSec-WebSocket-Protocol: ");
    out.extend_from_slice(crate::SUBPROTOCOL.as_bytes());
    out.extend_from_slice(b"\r\n\r\n");
}

/// ふつうの返事（`Content-Length` つき、`Connection: close`）を `out` に足す（受付側）。
/// `content_type` が空なら `Content-Type` を書かない。
pub fn write_response(status: u16, content_type: &str, body: &[u8], out: &mut Vec<u8>) {
    let head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    out.extend_from_slice(head.as_bytes());
    if !content_type.is_empty() {
        out.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    }
    out.extend_from_slice(format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
    out.extend_from_slice(body);
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Content Too Large",
        426 => "Upgrade Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

/// `Sec-WebSocket-Accept` = base64(SHA-1(key + GUID))。SHA-1 と base64 はこの中に持つ
/// （依存を足さない。ここ以外では使わない）。
pub fn accept_key(key: &str) -> String {
    let mut input = Vec::with_capacity(key.len() + WS_GUID.len());
    input.extend_from_slice(key.as_bytes());
    input.extend_from_slice(WS_GUID.as_bytes());
    base64(&sha1(&input))
}

/// 16 バイトを `Sec-WebSocket-Key` の形（base64）にする。
pub fn encode_key(nonce: [u8; 16]) -> String {
    base64(&nonce)
}

/// 16 バイトを表す base64 か（24 文字・末尾 `==`）。
fn is_key(k: &str) -> bool {
    let b = k.as_bytes();
    b.len() == 24
        && b[22] == b'='
        && b[23] == b'='
        && b[..22].iter().all(|&c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/')
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(B64[(n >> 18) as usize & 63] as char);
        s.push(B64[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    s
}

/// SHA-1（FIPS 180-4）。`Sec-WebSocket-Accept` にだけ使う（安全のためのものではない）。
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (o, x) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *o = x.to_be_bytes();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
    const ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

    fn hexs(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn switched(extra: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {ACCEPT}\r\n{extra}\r\n"
        )
        .into_bytes()
    }

    // 受け入れ試験（課題 W1）: RFC の例、依頼 ⇔ 読み取りの往復、1 バイトずつ届く返事、
    // 302 / 403 / 429 の読み分け、Accept の食い違い、8KB 超え、ヘッダ名の大文字小文字。
    #[test]
    fn rfc6455_accept_key() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        let resp = b"HTTP/1.1 302 Found\r\nLocation: https://example.cloudflareaccess.com/login\r\n\r\n";
        assert!(matches!(parse_upgrade_response(resp, "x"), UpgradeResponse::Rejected { status: 302, .. }));
    }

    #[test]
    fn sha1_and_base64_vectors() {
        assert_eq!(hexs(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hexs(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hexs(&sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        // 55・56・64 バイト（詰め物の境目）
        assert_eq!(hexs(&sha1(&[b'a'; 64])), "0098ba824b5c16427bd7a1122a5a442a25ec644d");
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // RFC の key は "the sample nonce"
        assert_eq!(encode_key(*b"the sample nonce"), KEY);
    }

    #[test]
    fn request_round_trip() {
        let headers = vec![
            ("CF-Access-Client-Id".to_owned(), "id.access".to_owned()),
            ("X-Bad".to_owned(), "a\r\nInjected: 1".to_owned()),
            ("Bad:Name".to_owned(), "v".to_owned()),
        ];
        let req = UpgradeRequest { host: "build.example:7411", path: crate::WS_PATH, key: KEY, headers: &headers };
        let mut out = Vec::new();
        req.write(&mut out);
        let text = String::from_utf8(out.clone()).unwrap();
        assert!(text.starts_with("GET /v1/ws HTTP/1.1\r\nHost: build.example:7411\r\n"), "{text}");
        assert!(text.contains("\r\nCF-Access-Client-Id: id.access\r\n"));
        assert!(!text.contains("Injected"));
        assert!(!text.contains("Bad:Name"));
        assert!(text.ends_with("\r\n\r\n"));
        // 受付が読む（後ろに続くバイトがあっても consumed は頭の長さ）
        let n = out.len();
        out.extend_from_slice(&[0x81, 0x80]);
        let Request::Upgrade { path, accept, subprotocol_ok, device, consumed } = parse_request(&out) else {
            panic!("{:?}", parse_request(&out));
        };
        assert_eq!((path.as_str(), accept.as_str(), subprotocol_ok, device, consumed), ("/v1/ws", ACCEPT, true, None, n));
        // 受付の返事を実機が読む
        let mut resp = Vec::new();
        write_switching(&accept, &mut resp);
        let m = resp.len();
        resp.extend_from_slice(&[0x81, 0x00]);
        assert_eq!(parse_upgrade_response(&resp, KEY), UpgradeResponse::Switched { consumed: m });
    }

    #[test]
    fn bytewise_response() {
        let mut full = switched("Sec-WebSocket-Protocol: zc-build.v1\r\n");
        let head_len = full.len();
        full.extend_from_slice(&[0x81, 0x02, b'h', b'i']);
        for i in 0..head_len {
            assert_eq!(parse_upgrade_response(&full[..i], KEY), UpgradeResponse::Incomplete, "{i}");
        }
        for i in head_len..=full.len() {
            assert_eq!(parse_upgrade_response(&full[..i], KEY), UpgradeResponse::Switched { consumed: head_len });
        }
    }

    #[test]
    fn bytewise_request() {
        let mut out = Vec::new();
        UpgradeRequest { host: "h", path: "/v1/ws", key: KEY, headers: &[] }.write(&mut out);
        for i in 0..out.len() {
            assert_eq!(parse_request(&out[..i]), Request::Incomplete, "{i}");
        }
        assert!(matches!(parse_request(&out), Request::Upgrade { .. }));
    }

    #[test]
    fn rejections_are_told_apart() {
        let r = parse_upgrade_response(b"HTTP/1.1 302 Found\r\nlocation: https://a.example/login\r\n\r\n", KEY);
        assert_eq!(r, UpgradeResponse::Rejected { status: 302, location: Some("https://a.example/login".into()), retry_after_s: None });
        let r = parse_upgrade_response(b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/html\r\nContent-Length: 5\r\n\r\n<html", KEY);
        assert_eq!(r, UpgradeResponse::Rejected { status: 403, location: None, retry_after_s: None });
        let r = parse_upgrade_response(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 17\r\n\r\n", KEY);
        assert_eq!(r, UpgradeResponse::Rejected { status: 429, location: None, retry_after_s: Some(17) });
        let r = parse_upgrade_response(b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: Wed, 21 Oct 2015 07:28:00 GMT\r\n\r\n", KEY);
        assert_eq!(r, UpgradeResponse::Rejected { status: 503, location: None, retry_after_s: None });
        // HTML の 200 も失敗（ログイン画面）
        let r = parse_upgrade_response(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n", KEY);
        assert!(matches!(r, UpgradeResponse::Rejected { status: 200, .. }));
        // 理由の句が無い・HTTP/1.0
        let r = parse_upgrade_response(b"HTTP/1.0 401\r\n\r\n", KEY);
        assert!(matches!(r, UpgradeResponse::Rejected { status: 401, .. }));
    }

    #[test]
    fn invalid_responses() {
        // Accept の食い違い・無い
        let bad = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: AAAA\r\n\r\n";
        assert!(matches!(parse_upgrade_response(bad, KEY), UpgradeResponse::Invalid(_)));
        let none = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
        assert!(matches!(parse_upgrade_response(none, KEY), UpgradeResponse::Invalid(_)));
        // 別の key で頼んだことにする
        assert!(matches!(parse_upgrade_response(&switched(""), "AAAAAAAAAAAAAAAAAAAAAA=="), UpgradeResponse::Invalid(_)));
        // Upgrade・Connection が無い
        let r = format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {ACCEPT}\r\n\r\n");
        assert!(matches!(parse_upgrade_response(r.as_bytes(), KEY), UpgradeResponse::Invalid(_)));
        let r = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {ACCEPT}\r\n\r\n");
        assert!(matches!(parse_upgrade_response(r.as_bytes(), KEY), UpgradeResponse::Invalid(_)));
        // 頼んでいないサブプロトコル・拡張
        let r = switched("Sec-WebSocket-Protocol: chat\r\n");
        assert!(matches!(parse_upgrade_response(&r, KEY), UpgradeResponse::Invalid(_)));
        let r = switched("Sec-WebSocket-Extensions: permessage-deflate\r\n");
        assert!(matches!(parse_upgrade_response(&r, KEY), UpgradeResponse::Invalid(_)));
        // サブプロトコルが無いのは許す
        assert!(matches!(parse_upgrade_response(&switched(""), KEY), UpgradeResponse::Switched { .. }));
        // HTTP でない
        assert!(matches!(parse_upgrade_response(b"SSH-2.0-OpenSSH\r\n\r\n", KEY), UpgradeResponse::Invalid(_)));
        assert!(matches!(parse_upgrade_response(b"HTTP/1.1 abc x\r\n\r\n", KEY), UpgradeResponse::Invalid(_)));
        assert!(matches!(parse_upgrade_response(b"\r\n\r\n", KEY), UpgradeResponse::Invalid(_)));
        // 折り返し・: の無い行
        let r = b"HTTP/1.1 403 Forbidden\r\nX: a\r\n b\r\n\r\n";
        assert!(matches!(parse_upgrade_response(r, KEY), UpgradeResponse::Invalid(_)));
        let r = b"HTTP/1.1 403 Forbidden\r\nnocolon\r\n\r\n";
        assert!(matches!(parse_upgrade_response(r, KEY), UpgradeResponse::Invalid(_)));
    }

    #[test]
    fn head_over_8kb() {
        // 空行が来ないまま 8KB を超えた
        let mut r = b"HTTP/1.1 101 Switching Protocols\r\n".to_vec();
        while r.len() <= MAX_HEAD {
            r.extend_from_slice(b"X-Pad: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
        }
        assert!(matches!(parse_upgrade_response(&r, KEY), UpgradeResponse::Invalid(_)));
        assert!(matches!(parse_request(&r), Request::Invalid(_)));
        // 空行はあるが、頭が 8KB を超えている
        r.extend_from_slice(b"\r\n");
        assert!(matches!(parse_upgrade_response(&r, KEY), UpgradeResponse::Invalid(_)));
        // ちょうど 8KB 以内なら読む
        let mut ok = b"HTTP/1.1 403 Forbidden\r\n".to_vec();
        let pad = MAX_HEAD - ok.len() - b"X: \r\n\r\n".len();
        ok.extend_from_slice(b"X: ");
        ok.extend(std::iter::repeat_n(b'a', pad));
        ok.extend_from_slice(b"\r\n\r\n");
        assert_eq!(ok.len(), MAX_HEAD);
        assert!(matches!(parse_upgrade_response(&ok, KEY), UpgradeResponse::Rejected { status: 403, .. }));
    }

    #[test]
    fn header_names_are_case_insensitive() {
        let r = format!(
            "HTTP/1.1 101 Switching Protocols\r\nupgrade: WebSocket\r\nCONNECTION: keep-alive, Upgrade\r\nsec-websocket-accept: {ACCEPT}\r\nsec-websocket-protocol: zc-build.v1\r\n\r\n"
        );
        assert!(matches!(parse_upgrade_response(r.as_bytes(), KEY), UpgradeResponse::Switched { .. }));
        // LF だけの行の区切りも受ける
        let r = r.replace("\r\n", "\n");
        assert_eq!(parse_upgrade_response(r.as_bytes(), KEY), UpgradeResponse::Switched { consumed: r.len() });
        let req = format!(
            "GET /v1/ws?x=1 HTTP/1.1\r\nhost: h\r\nUPGRADE: websocket\r\nconnection: keep-alive,upgrade\r\nsec-websocket-key: {KEY}\r\nsec-websocket-version: 13\r\nsec-websocket-protocol: other, zc-build.v1\r\nx-zc-device: cz-1\r\n\r\n"
        );
        assert_eq!(
            parse_request(req.as_bytes()),
            Request::Upgrade { path: "/v1/ws".into(), accept: ACCEPT.into(), subprotocol_ok: true, device: Some("cz-1".into()), consumed: req.len() }
        );
    }

    #[test]
    fn plain_gets_and_bad_requests() {
        let r = b"GET /healthz HTTP/1.1\r\nHost: h\r\n\r\n";
        assert_eq!(parse_request(r), Request::Get { path: "/healthz".into(), consumed: r.len() });
        let r = b"GET /v1/status?full HTTP/1.0\r\n\r\n";
        assert_eq!(parse_request(r), Request::Get { path: "/v1/status".into(), consumed: r.len() });
        let up = |extra: &str| {
            format!("GET /v1/ws HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n{extra}\r\n")
        };
        // サブプロトコルが無い・違う
        let r = up(&format!("Sec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: chat\r\n"));
        assert!(matches!(parse_request(r.as_bytes()), Request::Upgrade { subprotocol_ok: false, .. }));
        for bad in [
            up(&format!("Sec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 8\r\n")),
            up(&format!("Sec-WebSocket-Key: {KEY}\r\n")),
            up("Sec-WebSocket-Key: short==\r\nSec-WebSocket-Version: 13\r\n"),
            up("Sec-WebSocket-Version: 13\r\n"),
            up(&format!("Sec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\nContent-Length: 4\r\n")),
            up(&format!("Sec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\nTransfer-Encoding: chunked\r\n")),
            format!("GET /v1/ws HTTP/1.1\r\nUpgrade: websocket\r\nSec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\n\r\n"),
            format!("GET /v1/ws HTTP/1.0\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\n\r\n"),
            "POST /v1/ws HTTP/1.1\r\n\r\n".into(),
            "GET v1/ws HTTP/1.1\r\n\r\n".into(),
            "GET /v1/ws HTTP/2\r\n\r\n".into(),
            "GET /a b HTTP/1.1\r\n\r\n".into(),
            "GET /a\r\n\r\n".into(),
        ] {
            assert!(matches!(parse_request(bad.as_bytes()), Request::Invalid(_)), "{bad:?}");
        }
        // UTF-8 でない頭
        assert!(matches!(parse_request(b"GET /\xff HTTP/1.1\r\n\r\n"), Request::Invalid(_)));
    }

    #[test]
    fn plain_responses() {
        let mut out = Vec::new();
        write_response(200, "text/plain", b"ok", &mut out);
        assert_eq!(out, b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let mut out = Vec::new();
        write_response(404, "", b"", &mut out);
        assert_eq!(out, b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        // 実機側が読めば Rejected
        assert!(matches!(parse_upgrade_response(&out, KEY), UpgradeResponse::Rejected { status: 404, .. }));
    }
}
