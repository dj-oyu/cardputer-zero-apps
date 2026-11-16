//! `zc-wire` の代わり（**試験専用。統合したら消す**）。
//!
//! 課題 D1 / D2 を作った時点では、`zc-wire` の関数の中身（課題 W1 / W2）がまだ `todo!()`
//! だった。`net.rs` と `session.rs` は `zc-wire` の公開の形にだけ頼って書き、試験のときだけ
//! 同じ形のこのモジュールに差し替えている（各ファイルの先頭の `use ... as wire`）。
//!
//! 統合のあとでやること:
//!
//! 1. このファイルを消す
//! 2. `net.rs` / `session.rs` の `#[cfg(test)] ... mod fake_wire` と `as wire` の切り替えを
//!    `use zc_wire as wire;` の 1 行にする
//! 3. `cargo test` が通ることを確かめる（試験は公開の形しか使っていない）
//!
//! 型（メッセージ・`Role`・`Message` など）は本物の `zc-wire` のものをそのまま使う。
//! ここにあるのは中身のある関数だけ。約束（docs/stage3-design.md §5）のとおりに書いてあるが、
//! 正本は `zc-wire`。
#![allow(dead_code, unused_imports)]

pub use zc_wire::{limits, DEFAULT_PORT, PROTOCOL_VERSION, SUBPROTOCOL, WS_PATH};

pub mod msg {
    pub use zc_wire::msg::*;

    /// JSON から読む。知らない `type` は `Ok(None)`。
    pub fn decode<'a, T: serde::Deserialize<'a>>(text: &'a str) -> Result<Option<T>, serde_json::Error> {
        match serde_json::from_str::<T>(text) {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                let v: serde_json::Value = serde_json::from_str(text)?;
                if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
                    if e.to_string().starts_with(&format!("unknown variant `{t}`")) {
                        return Ok(None);
                    }
                }
                Err(e)
            }
        }
    }

    /// バイナリフレームの頭（16 バイト）。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct BinHeader {
        pub kind: BinKind,
        pub last: bool,
        pub id: u32,
        pub offset: u64,
    }

    impl BinHeader {
        pub const LEN: usize = 16;

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = self.kind as u8;
            b[1] = self.last as u8;
            b[4..8].copy_from_slice(&self.id.to_le_bytes());
            b[8..16].copy_from_slice(&self.offset.to_le_bytes());
            b
        }

        pub fn split(frame: &[u8]) -> Option<(BinHeader, &[u8])> {
            if frame.len() < Self::LEN {
                return None;
            }
            let kind = match frame[0] {
                1 => BinKind::Blob,
                2 => BinKind::Artifact,
                _ => return None,
            };
            let id = u32::from_le_bytes(frame[4..8].try_into().unwrap());
            let offset = u64::from_le_bytes(frame[8..16].try_into().unwrap());
            Some((BinHeader { kind, last: frame[1] & 1 != 0, id, offset }, &frame[Self::LEN..]))
        }
    }
}

pub mod tree {
    pub use zc_wire::tree::ManifestError;
    use sha2::{Digest, Sha256};
    use zc_wire::msg::{FileEntry, JobOpts, Kind, Lang};

    #[derive(Clone, Debug, PartialEq, Eq, Default)]
    pub struct Manifest {
        files: Vec<FileEntry>,
    }

    impl Manifest {
        pub fn new(mut files: Vec<FileEntry>) -> Result<Manifest, ManifestError> {
            files.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
            for w in files.windows(2) {
                if w[0].path == w[1].path {
                    return Err(ManifestError::Duplicate(w[0].path.clone()));
                }
            }
            for f in &files {
                if !is_canonical_path(&f.path) {
                    return Err(ManifestError::BadPath(f.path.clone()));
                }
            }
            Ok(Manifest { files })
        }

        pub fn files(&self) -> &[FileEntry] {
            &self.files
        }

        pub fn total_bytes(&self) -> u64 {
            self.files.iter().map(|f| f.size).sum()
        }

        pub fn tree_id(&self) -> String {
            let mut h = Sha256::new();
            for f in &self.files {
                h.update(f.path.as_bytes());
                h.update(b"\0");
                h.update(f.size.to_string().as_bytes());
                h.update(b"\0");
                h.update(f.sha256.as_bytes());
                h.update(b"\0");
                h.update(if f.exec { b"x\n" } else { b"-\n" });
            }
            hex(&h.finalize())
        }
    }

    pub fn is_canonical_path(path: &str) -> bool {
        !path.is_empty() && path.len() <= 1024 && !path.contains('\\') && !path.bytes().any(|b| b < 0x20) && path.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && c.len() <= 255)
    }

    pub fn job_id(tree_id: &str, kind: Kind, lang: Lang, opts: &JobOpts) -> String {
        let name = |v: serde_json::Value| v.as_str().unwrap().to_owned();
        let mut h = Sha256::new();
        h.update(b"zc-job-v1\0");
        h.update(tree_id.as_bytes());
        h.update(b"\0");
        h.update(name(serde_json::to_value(kind).unwrap()).as_bytes());
        h.update(b"\0");
        h.update(name(serde_json::to_value(lang).unwrap()).as_bytes());
        h.update(b"\0");
        h.update(serde_json::to_string(opts).unwrap().as_bytes());
        hex(&h.finalize()[..16])
    }

    pub fn project_id(root: &[u8]) -> String {
        hex(&Sha256::digest(root)[..8])
    }

    pub fn hex(digest: &[u8]) -> String {
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}

pub mod ws {
    pub use zc_wire::ws::{Message, Role, WsError};

    #[derive(Debug)]
    pub struct Decoder {
        role: Role,
        max: usize,
        buf: Vec<u8>,
        pos: usize,
        /// 分割されたメッセージの組み立て。
        msg: Vec<u8>,
        /// 組み立て中の opcode（0 = 無し）。
        msg_op: u8,
    }

    enum Out {
        /// `buf` の中の範囲。
        Buf(u8, usize, usize),
        /// `msg` の全体。
        Msg(u8),
        Close(u16),
    }

    impl Decoder {
        pub fn new(role: Role, max_message: usize) -> Decoder {
            Decoder { role, max: max_message, buf: Vec::new(), pos: 0, msg: Vec::new(), msg_op: 0 }
        }

        pub fn push(&mut self, bytes: &[u8]) {
            if self.pos == self.buf.len() {
                self.buf.clear();
                self.pos = 0;
            } else if self.pos > (64 << 10) {
                self.buf.drain(..self.pos);
                self.pos = 0;
            }
            self.buf.extend_from_slice(bytes);
        }

        fn step(&mut self) -> Result<Option<Out>, WsError> {
            loop {
                let a = &self.buf[self.pos..];
                if a.len() < 2 {
                    return Ok(None);
                }
                let (b0, b1) = (a[0], a[1]);
                if b0 & 0x70 != 0 {
                    return Err(WsError::Protocol("rsv"));
                }
                let fin = b0 & 0x80 != 0;
                let op = b0 & 0x0f;
                let masked = b1 & 0x80 != 0;
                if masked != (self.role == Role::Server) {
                    return Err(WsError::Protocol("mask"));
                }
                let (mut hl, len) = match b1 & 0x7f {
                    126 => {
                        if a.len() < 4 {
                            return Ok(None);
                        }
                        (4, u16::from_be_bytes([a[2], a[3]]) as u64)
                    }
                    127 => {
                        if a.len() < 10 {
                            return Ok(None);
                        }
                        (10, u64::from_be_bytes(a[2..10].try_into().unwrap()))
                    }
                    n => (2, n as u64),
                };
                if op >= 8 && (len > 125 || !fin) {
                    return Err(WsError::Protocol("control"));
                }
                if len > self.max as u64 {
                    return Err(WsError::TooLarge);
                }
                let len = len as usize;
                let mut key = [0u8; 4];
                if masked {
                    if a.len() < hl + 4 {
                        return Ok(None);
                    }
                    key.copy_from_slice(&a[hl..hl + 4]);
                    hl += 4;
                }
                if a.len() < hl + len {
                    return Ok(None);
                }
                let start = self.pos + hl;
                let end = start + len;
                self.pos = end;
                if masked {
                    for (i, b) in self.buf[start..end].iter_mut().enumerate() {
                        *b ^= key[i & 3];
                    }
                }
                match op {
                    0 => {
                        if self.msg_op == 0 {
                            return Err(WsError::Protocol("continuation"));
                        }
                        if self.msg.len() + len > self.max {
                            return Err(WsError::TooLarge);
                        }
                        self.msg.extend_from_slice(&self.buf[start..end]);
                        if fin {
                            let op = self.msg_op;
                            self.msg_op = 0;
                            return Ok(Some(Out::Msg(op)));
                        }
                    }
                    1 | 2 => {
                        if self.msg_op != 0 {
                            return Err(WsError::Protocol("fragment"));
                        }
                        if fin {
                            return Ok(Some(Out::Buf(op, start, end)));
                        }
                        self.msg_op = op;
                        self.msg.clear();
                        self.msg.extend_from_slice(&self.buf[start..end]);
                    }
                    8 => {
                        let code = if len >= 2 { u16::from_be_bytes([self.buf[start], self.buf[start + 1]]) } else { 1005 };
                        return Ok(Some(Out::Close(code)));
                    }
                    9 | 10 => return Ok(Some(Out::Buf(op, start, end))),
                    _ => return Err(WsError::Protocol("opcode")),
                }
            }
        }

        #[allow(clippy::should_implement_trait)]
        pub fn next(&mut self) -> Result<Option<Message<'_>>, WsError> {
            let (op, data): (u8, &[u8]) = match self.step()? {
                None => return Ok(None),
                Some(Out::Close(code)) => return Ok(Some(Message::Close(code))),
                Some(Out::Buf(op, s, e)) => (op, &self.buf[s..e]),
                Some(Out::Msg(op)) => (op, &self.msg[..]),
            };
            Ok(Some(match op {
                1 => Message::Text(core::str::from_utf8(data).map_err(|_| WsError::Protocol("utf-8"))?),
                2 => Message::Binary(data),
                9 => Message::Ping(data),
                _ => Message::Pong(data),
            }))
        }

        pub fn buffered(&self) -> usize {
            self.buf.len() - self.pos
        }
    }

    #[derive(Debug)]
    pub struct Encoder {
        role: Role,
    }

    impl Encoder {
        pub fn new(role: Role) -> Encoder {
            Encoder { role }
        }

        fn frame(&mut self, op: u8, parts: [&[u8]; 2], mask: [u8; 4], out: &mut Vec<u8>) {
            let len = parts[0].len() + parts[1].len();
            let m = if self.role == Role::Client { 0x80 } else { 0 };
            out.push(0x80 | op);
            if len < 126 {
                out.push(m | len as u8);
            } else if len < 65536 {
                out.push(m | 126);
                out.extend_from_slice(&(len as u16).to_be_bytes());
            } else {
                out.push(m | 127);
                out.extend_from_slice(&(len as u64).to_be_bytes());
            }
            if m == 0 {
                out.extend_from_slice(parts[0]);
                out.extend_from_slice(parts[1]);
                return;
            }
            out.extend_from_slice(&mask);
            let start = out.len();
            out.extend_from_slice(parts[0]);
            out.extend_from_slice(parts[1]);
            for (i, b) in out[start..].iter_mut().enumerate() {
                *b ^= mask[i & 3];
            }
        }

        pub fn text(&mut self, text: &str, mask: [u8; 4], out: &mut Vec<u8>) {
            self.frame(1, [text.as_bytes(), &[]], mask, out)
        }

        pub fn binary(&mut self, data: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
            self.frame(2, [data, &[]], mask, out)
        }

        pub fn binary2(&mut self, head: &[u8], body: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
            self.frame(2, [head, body], mask, out)
        }

        pub fn ping(&mut self, data: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
            self.frame(9, [data, &[]], mask, out)
        }

        pub fn pong(&mut self, data: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
            self.frame(10, [data, &[]], mask, out)
        }

        pub fn close(&mut self, code: u16, mask: [u8; 4], out: &mut Vec<u8>) {
            self.frame(8, [&code.to_be_bytes(), &[]], mask, out)
        }
    }
}

pub mod http {
    pub use zc_wire::http::{Request, UpgradeResponse};

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct UpgradeRequest<'a> {
        pub host: &'a str,
        pub path: &'a str,
        pub key: &'a str,
        pub headers: &'a [(String, String)],
    }

    impl UpgradeRequest<'_> {
        pub fn write(&self, out: &mut Vec<u8>) {
            let mut s = format!(
                "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: {}\r\n",
                self.path,
                self.host,
                self.key,
                zc_wire::SUBPROTOCOL
            );
            for (k, v) in self.headers {
                s.push_str(k);
                s.push_str(": ");
                s.push_str(v);
                s.push_str("\r\n");
            }
            s.push_str("\r\n");
            out.extend_from_slice(s.as_bytes());
        }
    }

    const MAX_HEAD: usize = 8192;

    /// 頭（最初の空行まで）を行に分ける。
    fn head(buf: &[u8]) -> Result<Option<(Vec<&str>, usize)>, &'static str> {
        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            return if buf.len() > MAX_HEAD { Err("head too large") } else { Ok(None) };
        };
        if end > MAX_HEAD {
            return Err("head too large");
        }
        let text = core::str::from_utf8(&buf[..end]).map_err(|_| "not utf-8")?;
        Ok(Some((text.split("\r\n").collect(), end + 4)))
    }

    fn header<'a>(lines: &[&'a str], name: &str) -> Option<&'a str> {
        lines.iter().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }

    pub fn parse_upgrade_response(buf: &[u8], key: &str) -> UpgradeResponse {
        let (lines, consumed) = match head(buf) {
            Ok(Some(x)) => x,
            Ok(None) => return UpgradeResponse::Incomplete,
            Err(e) => return UpgradeResponse::Invalid(e),
        };
        let mut it = lines[0].split(' ');
        let (Some(ver), Some(code)) = (it.next(), it.next()) else { return UpgradeResponse::Invalid("status line") };
        let Ok(status) = code.parse::<u16>() else { return UpgradeResponse::Invalid("status line") };
        if !ver.starts_with("HTTP/1.") {
            return UpgradeResponse::Invalid("status line");
        }
        if status != 101 {
            return UpgradeResponse::Rejected { status, location: header(&lines, "location").map(str::to_owned), retry_after_s: header(&lines, "retry-after").and_then(|v| v.parse().ok()) };
        }
        if header(&lines, "sec-websocket-accept") != Some(accept_key(key).as_str()) {
            return UpgradeResponse::Invalid("accept");
        }
        UpgradeResponse::Switched { consumed }
    }

    pub fn parse_request(buf: &[u8]) -> Request {
        let (lines, consumed) = match head(buf) {
            Ok(Some(x)) => x,
            Ok(None) => return Request::Incomplete,
            Err(e) => return Request::Invalid(e),
        };
        let mut it = lines[0].split(' ');
        let (Some("GET"), Some(path)) = (it.next(), it.next()) else { return Request::Invalid("request line") };
        let upgrade = header(&lines, "upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
        match (upgrade, header(&lines, "sec-websocket-key")) {
            (true, Some(key)) => Request::Upgrade {
                path: path.to_owned(),
                accept: accept_key(key),
                subprotocol_ok: header(&lines, "sec-websocket-protocol").is_some_and(|v| v.split(',').any(|p| p.trim() == zc_wire::SUBPROTOCOL)),
                device: header(&lines, "x-zc-device").map(str::to_owned),
                consumed,
            },
            _ => Request::Get { path: path.to_owned(), consumed },
        }
    }

    pub fn write_switching(accept: &str, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Protocol: {}\r\n\r\n", zc_wire::SUBPROTOCOL).as_bytes());
    }

    pub fn write_response(status: u16, content_type: &str, body: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(format!("HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
        out.extend_from_slice(body);
    }

    pub fn accept_key(key: &str) -> String {
        let mut data = key.as_bytes().to_vec();
        data.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        base64(&sha1(&data))
    }

    pub fn encode_key(nonce: [u8; 16]) -> String {
        base64(&nonce)
    }

    fn base64(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            s.push(T[(n >> 18) as usize & 63] as char);
            s.push(T[(n >> 12) as usize & 63] as char);
            s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
            s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
        }
        s
    }

    fn sha1(data: &[u8]) -> [u8; 20] {
        let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
        let mut m = data.to_vec();
        m.push(0x80);
        while m.len() % 64 != 56 {
            m.push(0);
        }
        m.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
        for block in m.chunks(64) {
            let mut w = [0u32; 80];
            for i in 0..16 {
                w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
            }
            for i in 16..80 {
                w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
            }
            let [mut a, mut b, mut c, mut d, mut e] = h;
            for (i, wi) in w.iter().enumerate() {
                let (f, k) = match i {
                    0..=19 => ((b & c) | (!b & d), 0x5A827999),
                    20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                    40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                    _ => (b ^ c ^ d, 0xCA62C1D6u32),
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
        for (i, x) in h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
        }
        out
    }

    #[test]
    fn fake_accept_key_matches_rfc() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
