//! WebSocket の枠（RFC 6455）。I/O を持たない符号化と復号（課題 W1）。
//!
//! - クライアント（実機）は送るフレームにマスクを掛ける。サーバ（受付）は掛けない。
//!   受ける側は逆を確かめる（違えば [`WsError::Protocol`]）
//! - 拡張（permessage-deflate）は交渉しない。RSV ビットが立っていたらエラー
//! - 分割されたメッセージ（FIN = 0 の続き）は組み立てて 1 つにする。上限は
//!   [`Decoder::new`] の `max_message`
//! - 制御フレーム（ping / pong / close）は分割の途中にも来る。ping には呼ぶ側が pong を返す
//! - 送る側は 1 メッセージ = 1 フレーム（分割しない。大きなデータは上の層が
//!   [`crate::limits::CHUNK_BYTES`] に刻む）
//!
//! 実機ではヒープの確保を増やさない: [`Decoder`] は内部のバッファを使い回し、
//! [`Encoder`] は呼ぶ側の `Vec` に足す。マスクは 8 バイト単位で掛ける。
//!
//! 受ける側の細かい決まり:
//!
//! - 長さは最短の形でなければエラー（126 で 125 以下、127 で 65535 以下、64 ビットの最上位が 1）
//! - `max_message` を超える長さは、頭を読んだ時点で [`WsError::TooLarge`]（中身を待たない）。
//!   分割は組み立て中の合計で数える
//! - テキストの UTF-8 は、メッセージがそろってから確かめる（分割の境目で文字が切れてもよい）
//! - close の中身: 0 バイトは 1005、1 バイトはエラー、状態コードは 1000〜1003・1007〜1014・
//!   3000〜4999 だけ、理由は UTF-8
//! - close を返したあとは何も返さない（`Ok(None)`。足したバイト列も捨てる）
//! - 一度エラーを返したら、以後も同じエラーを返す（接続を閉じるしかない）

/// どちらの側か。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// 送るフレームにマスクを掛け、受けるフレームにマスクが無いことを確かめる。
    Client,
    /// その逆。
    Server,
}

/// 受け取ったメッセージ。借用は次の [`Decoder::next`] まで有効。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message<'a> {
    Text(&'a str),
    Binary(&'a [u8]),
    Ping(&'a [u8]),
    Pong(&'a [u8]),
    /// 閉じる合図（状態コード。無ければ 1005）。
    Close(u16),
}

/// 復号の失敗。接続を閉じる。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsError {
    /// 約束に反したフレーム（マスクの向き・RSV・知らない opcode・壊れた分割・
    /// 125 バイトを超える制御フレーム・UTF-8 でないテキスト）。
    Protocol(&'static str),
    /// `max_message` を超えた。
    TooLarge,
}

impl core::fmt::Display for WsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for WsError {}

const OP_CONT: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xa;

/// 制御フレームの中身の上限。
const MAX_CONTROL: usize = 125;

/// フレームの頭を読んだもの。
#[derive(Clone, Copy, Debug)]
struct Head {
    fin: bool,
    op: u8,
    mask: Option<[u8; 4]>,
    /// 頭の長さ（2〜14）。
    len: usize,
    /// 中身の長さ。
    payload: u64,
}

/// 頭を読む。足りなければ `Ok(None)`。読めたところまでで分かる違反はすぐエラーにする。
fn parse_head(b: &[u8], role: Role) -> Result<Option<Head>, WsError> {
    if b.len() < 2 {
        return Ok(None);
    }
    let (b0, b1) = (b[0], b[1]);
    if b0 & 0x70 != 0 {
        return Err(WsError::Protocol("RSV ビットが立っている"));
    }
    let fin = b0 & 0x80 != 0;
    let op = b0 & 0x0f;
    if !matches!(op, OP_CONT | OP_TEXT | OP_BINARY | OP_CLOSE | OP_PING | OP_PONG) {
        return Err(WsError::Protocol("知らない opcode"));
    }
    let masked = b1 & 0x80 != 0;
    match (role, masked) {
        (Role::Server, false) => return Err(WsError::Protocol("クライアントのフレームにマスクが無い")),
        (Role::Client, true) => return Err(WsError::Protocol("サーバのフレームにマスクがある")),
        _ => {}
    }
    let len7 = b1 & 0x7f;
    if op & 0x8 != 0 {
        if !fin {
            return Err(WsError::Protocol("制御フレームが分割されている"));
        }
        if len7 as usize > MAX_CONTROL {
            return Err(WsError::Protocol("制御フレームが 125 バイトを超える"));
        }
    }
    let (payload, mut len) = match len7 {
        126 => {
            if b.len() < 4 {
                return Ok(None);
            }
            let n = u16::from_be_bytes([b[2], b[3]]) as u64;
            if n < 126 {
                return Err(WsError::Protocol("長さが最短の形でない"));
            }
            (n, 4)
        }
        127 => {
            if b.len() < 10 {
                return Ok(None);
            }
            let n = u64::from_be_bytes(b[2..10].try_into().unwrap());
            if n >> 63 != 0 {
                return Err(WsError::Protocol("長さの最上位ビットが 1"));
            }
            if n < 0x1_0000 {
                return Err(WsError::Protocol("長さが最短の形でない"));
            }
            (n, 10)
        }
        n => (n as u64, 2),
    };
    let mask = if masked {
        if b.len() < len + 4 {
            return Ok(None);
        }
        let k = [b[len], b[len + 1], b[len + 2], b[len + 3]];
        len += 4;
        Some(k)
    } else {
        None
    };
    Ok(Some(Head { fin, op, mask, len, payload }))
}

/// マスクを掛ける（外すのも同じ）。`data` の先頭が鍵の 0 バイト目に当たる。
fn apply_mask(data: &mut [u8], key: [u8; 4]) {
    let k8 = u64::from_ne_bytes([key[0], key[1], key[2], key[3], key[0], key[1], key[2], key[3]]);
    let (chunks, rest) = data.as_chunks_mut::<8>();
    for c in chunks {
        *c = (u64::from_ne_bytes(*c) ^ k8).to_ne_bytes();
    }
    // 8 の倍数のあとなので、鍵の位置は 0 から数え直してよい
    for (i, b) in rest.iter_mut().enumerate() {
        *b ^= key[i & 3];
    }
}

fn close_code_ok(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999)
}

/// そろったメッセージの在りか（借用を返す前の段階。`next` で [`Message`] に直す）。
enum Ready {
    /// 単独のフレーム。`buf[start..end]`。
    Text(usize, usize),
    Binary(usize, usize),
    Ping(usize, usize),
    Pong(usize, usize),
    /// 組み立てたメッセージ（`frag`）。
    FragText,
    FragBinary,
    Close(u16),
}

/// 受ける側。読んだバイト列を [`Decoder::push`] で足し、[`Decoder::next`] で 1 つずつ取り出す。
#[derive(Debug)]
pub struct Decoder {
    role: Role,
    max_message: usize,
    /// 読んだバイト列。`pos` より前は取り出し済み（次の `push` で詰める）。
    buf: Vec<u8>,
    pos: usize,
    /// 組み立て中のメッセージ（分割の途中）。
    frag: Vec<u8>,
    /// 組み立て中のメッセージの種類（`OP_TEXT` / `OP_BINARY`）。
    frag_op: Option<u8>,
    /// `frag` は返したメッセージ（次の `next` で空にする）。
    frag_returned: bool,
    closed: bool,
    failed: Option<WsError>,
}

impl Decoder {
    pub fn new(role: Role, max_message: usize) -> Decoder {
        Decoder {
            role,
            max_message,
            buf: Vec::new(),
            pos: 0,
            frag: Vec::new(),
            frag_op: None,
            frag_returned: false,
            closed: false,
            failed: None,
        }
    }

    /// 読んだバイト列を足す。
    pub fn push(&mut self, bytes: &[u8]) {
        if self.closed || self.failed.is_some() {
            return;
        }
        if self.pos > 0 {
            // 取り出し済みの分を詰める（1 回の読み込みに 1 回だけ。`next` では詰めない）
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// そろっているメッセージを 1 つ返す。足りなければ `Ok(None)`。
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Message<'_>>, WsError> {
        let ready = match self.advance() {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(None),
            Err(e) => {
                self.failed = Some(e.clone());
                return Err(e);
            }
        };
        // UTF-8 は advance で確かめてある
        let msg = match ready {
            Ready::Text(s, e) => Message::Text(core::str::from_utf8(&self.buf[s..e]).expect("確かめ済み")),
            Ready::Binary(s, e) => Message::Binary(&self.buf[s..e]),
            Ready::Ping(s, e) => Message::Ping(&self.buf[s..e]),
            Ready::Pong(s, e) => Message::Pong(&self.buf[s..e]),
            Ready::FragText => Message::Text(core::str::from_utf8(&self.frag).expect("確かめ済み")),
            Ready::FragBinary => Message::Binary(&self.frag),
            Ready::Close(code) => Message::Close(code),
        };
        Ok(Some(msg))
    }

    /// まだ取り出していないバイト数（上限の監視用）。組み立て中のメッセージの分も含む。
    pub fn buffered(&self) -> usize {
        let frag = if self.frag_op.is_some() { self.frag.len() } else { 0 };
        self.buf.len() - self.pos + frag
    }

    fn advance(&mut self) -> Result<Option<Ready>, WsError> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        if self.closed {
            return Ok(None);
        }
        if self.frag_returned {
            self.frag.clear();
            self.frag_returned = false;
        }
        loop {
            let avail = &self.buf[self.pos..];
            let Some(h) = parse_head(avail, self.role)? else {
                return Ok(None);
            };
            // 中身を待たずに分かる違反
            match h.op {
                OP_CONT if self.frag_op.is_none() => return Err(WsError::Protocol("始まりの無い続きのフレーム")),
                OP_TEXT | OP_BINARY if self.frag_op.is_some() => {
                    return Err(WsError::Protocol("分割の途中に新しいメッセージ"))
                }
                _ => {}
            }
            if h.op & 0x8 == 0 {
                let so_far = if h.op == OP_CONT { self.frag.len() as u64 } else { 0 };
                if so_far + h.payload > self.max_message as u64 {
                    return Err(WsError::TooLarge);
                }
            }
            // ここまで来れば payload は max_message か 125 以下（usize に収まる）
            let payload = h.payload as usize;
            if avail.len() < h.len + payload {
                return Ok(None);
            }
            let start = self.pos + h.len;
            let end = start + payload;
            self.pos = end;
            if let Some(k) = h.mask {
                apply_mask(&mut self.buf[start..end], k);
            }
            match (h.op, h.fin) {
                (OP_PING, _) => return Ok(Some(Ready::Ping(start, end))),
                (OP_PONG, _) => return Ok(Some(Ready::Pong(start, end))),
                (OP_CLOSE, _) => {
                    let p = &self.buf[start..end];
                    let code = match p.len() {
                        0 => 1005,
                        1 => return Err(WsError::Protocol("close の中身が 1 バイト")),
                        _ => {
                            let c = u16::from_be_bytes([p[0], p[1]]);
                            if !close_code_ok(c) {
                                return Err(WsError::Protocol("close の状態コードが不正"));
                            }
                            if core::str::from_utf8(&p[2..]).is_err() {
                                return Err(WsError::Protocol("close の理由が UTF-8 でない"));
                            }
                            c
                        }
                    };
                    self.closed = true;
                    return Ok(Some(Ready::Close(code)));
                }
                (OP_TEXT, true) => {
                    if core::str::from_utf8(&self.buf[start..end]).is_err() {
                        return Err(WsError::Protocol("テキストが UTF-8 でない"));
                    }
                    return Ok(Some(Ready::Text(start, end)));
                }
                (OP_BINARY, true) => return Ok(Some(Ready::Binary(start, end))),
                (OP_TEXT | OP_BINARY, false) => {
                    self.frag_op = Some(h.op);
                    self.frag.extend_from_slice(&self.buf[start..end]);
                }
                (_, fin) => {
                    // OP_CONT
                    self.frag.extend_from_slice(&self.buf[start..end]);
                    if fin {
                        let op = self.frag_op.take().expect("始まりは確かめ済み");
                        self.frag_returned = true;
                        if op == OP_TEXT {
                            if core::str::from_utf8(&self.frag).is_err() {
                                return Err(WsError::Protocol("テキストが UTF-8 でない"));
                            }
                            return Ok(Some(Ready::FragText));
                        }
                        return Ok(Some(Ready::FragBinary));
                    }
                }
            }
        }
    }
}

/// 送る側。フレームを `out` の末尾に足す。クライアントのマスクの鍵は呼ぶ側が渡す
/// （乱数の出どころをこの crate に持たせない。`getrandom(2)` か時刻と番号の混ぜ物でよい:
/// マスクは秘密ではなく、途中の中継器の誤動作を避けるためのもの）。
///
/// サーバの側では `mask` は使わない。制御フレーム（ping / pong）の中身は 125 バイトで切る。
#[derive(Debug)]
pub struct Encoder {
    role: Role,
}

impl Encoder {
    pub fn new(role: Role) -> Encoder {
        Encoder { role }
    }

    pub fn text(&mut self, text: &str, mask: [u8; 4], out: &mut Vec<u8>) {
        self.frame(OP_TEXT, &[text.as_bytes()], mask, out);
    }

    pub fn binary(&mut self, data: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
        self.frame(OP_BINARY, &[data], mask, out);
    }

    /// 頭（`head`）と中身（`body`）を 1 つのバイナリフレームにする（[`crate::msg::BinHeader`] +
    /// ファイルの中身を、つなぎ直さずに送るため）。
    pub fn binary2(&mut self, head: &[u8], body: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
        self.frame(OP_BINARY, &[head, body], mask, out);
    }

    pub fn ping(&mut self, data: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
        self.frame(OP_PING, &[&data[..data.len().min(MAX_CONTROL)]], mask, out);
    }

    pub fn pong(&mut self, data: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
        self.frame(OP_PONG, &[&data[..data.len().min(MAX_CONTROL)]], mask, out);
    }

    /// 1005・1006・1015 は送ってはいけない値なので、そのときは状態コードなしで送る。
    pub fn close(&mut self, code: u16, mask: [u8; 4], out: &mut Vec<u8>) {
        let b = code.to_be_bytes();
        let body: &[u8] = if matches!(code, 1005 | 1006 | 1015) { &[] } else { &b };
        self.frame(OP_CLOSE, &[body], mask, out);
    }

    fn frame(&self, op: u8, parts: &[&[u8]], mask: [u8; 4], out: &mut Vec<u8>) {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        let client = self.role == Role::Client;
        out.reserve(14 + len);
        out.push(0x80 | op);
        let m = if client { 0x80 } else { 0 };
        if len < 126 {
            out.push(m | len as u8);
        } else if len <= 0xffff {
            out.push(m | 126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            out.push(m | 127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }
        if client {
            out.extend_from_slice(&mask);
        }
        let start = out.len();
        for p in parts {
            out.extend_from_slice(p);
        }
        if client {
            apply_mask(&mut out[start..], mask);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 試験用の乱数（xorshift）。
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
    }

    /// 持ち主のある形にした Message（比べやすくする）。
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Owned {
        Text(String),
        Binary(Vec<u8>),
        Ping(Vec<u8>),
        Pong(Vec<u8>),
        Close(u16),
    }

    fn own(m: Message<'_>) -> Owned {
        match m {
            Message::Text(t) => Owned::Text(t.into()),
            Message::Binary(b) => Owned::Binary(b.into()),
            Message::Ping(b) => Owned::Ping(b.into()),
            Message::Pong(b) => Owned::Pong(b.into()),
            Message::Close(c) => Owned::Close(c),
        }
    }

    /// 全部を取り出す。
    fn drain(d: &mut Decoder) -> Result<Vec<Owned>, WsError> {
        let mut v = Vec::new();
        while let Some(m) = d.next()? {
            v.push(own(m));
        }
        Ok(v)
    }

    /// 1 バイトずつ足して、取り出せるたびに取り出す。
    fn feed_bytewise(role: Role, max: usize, bytes: &[u8]) -> Result<Vec<Owned>, WsError> {
        let mut d = Decoder::new(role, max);
        let mut v = Vec::new();
        for b in bytes {
            d.push(&[*b]);
            v.extend(drain(&mut d)?);
        }
        Ok(v)
    }

    fn one(role: Role, bytes: &[u8]) -> Result<Vec<Owned>, WsError> {
        let mut d = Decoder::new(role, 1 << 20);
        d.push(bytes);
        drain(&mut d)
    }

    /// サーバが送る形（マスクなし）の頭を手で作る。
    fn raw(b0: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![b0];
        let n = payload.len();
        if n < 126 {
            v.push(n as u8);
        } else if n <= 0xffff {
            v.push(126);
            v.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            v.push(127);
            v.extend_from_slice(&(n as u64).to_be_bytes());
        }
        v.extend_from_slice(payload);
        v
    }

    // 受け入れ試験（課題 W1）: RFC 6455 §5.7 の例、1 バイトずつ届く入力、126 / 65536 の境目、
    // 分割 + 途中の ping、マスクの向きの間違い、RSV、制御フレームの長さ、壊れた UTF-8、
    // max_message 超え、クライアント → サーバ → クライアントの往復（乱数の入力で）。
    #[test]
    fn rfc6455_examples() {
        // マスクなしの "Hello"（サーバ → クライアント）
        let mut d = Decoder::new(Role::Client, 1 << 20);
        d.push(&[0x81, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f]);
        assert_eq!(d.next().unwrap(), Some(Message::Text("Hello")));
        assert_eq!(d.next().unwrap(), None);
        // マスクつきの "Hello"（クライアント → サーバ）
        let mut out = Vec::new();
        Encoder::new(Role::Client).text("Hello", [0x37, 0xfa, 0x21, 0x3d], &mut out);
        assert_eq!(out, [0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58]);
        let mut s = Decoder::new(Role::Server, 1 << 20);
        s.push(&out);
        assert_eq!(s.next().unwrap(), Some(Message::Text("Hello")));
        // サーバがマスクつきを送ってきたらエラー
        let mut c = Decoder::new(Role::Client, 1 << 20);
        c.push(&out);
        assert!(matches!(c.next(), Err(WsError::Protocol(_))));
    }

    #[test]
    fn rfc6455_more_examples() {
        // 分割された "Hel" + "lo"
        let frag = [0x01, 0x03, 0x48, 0x65, 0x6c, 0x80, 0x02, 0x6c, 0x6f];
        assert_eq!(one(Role::Client, &frag).unwrap(), [Owned::Text("Hello".into())]);
        // マスクなしの ping "Hello" と、マスクつきの pong "Hello"
        let ping = [0x89, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f];
        assert_eq!(one(Role::Client, &ping).unwrap(), [Owned::Ping(b"Hello".to_vec())]);
        let pong = [0x8a, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58];
        assert_eq!(one(Role::Server, &pong).unwrap(), [Owned::Pong(b"Hello".to_vec())]);
        let mut out = Vec::new();
        Encoder::new(Role::Client).pong(b"Hello", [0x37, 0xfa, 0x21, 0x3d], &mut out);
        assert_eq!(out, pong);
        // 256 バイトのバイナリ（16 ビットの長さ）と 64KiB（64 ビットの長さ）の頭
        let mut out = Vec::new();
        Encoder::new(Role::Server).binary(&[0u8; 256], [0; 4], &mut out);
        assert_eq!(&out[..4], [0x82, 0x7e, 0x01, 0x00]);
        let mut out = Vec::new();
        Encoder::new(Role::Server).binary(&vec![0u8; 65536], [0; 4], &mut out);
        assert_eq!(&out[..10], [0x82, 0x7f, 0, 0, 0, 0, 0, 1, 0, 0]);
        assert_eq!(out.len(), 10 + 65536);
    }

    #[test]
    fn length_boundaries() {
        for n in [0usize, 1, 125, 126, 127, 65535, 65536, 70000] {
            for role in [Role::Client, Role::Server] {
                let data: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
                let mut out = Vec::new();
                Encoder::new(role).binary(&data, [1, 2, 3, 4], &mut out);
                let head = match n {
                    0..=125 => 2,
                    126..=65535 => 4,
                    _ => 10,
                } + if role == Role::Client { 4 } else { 0 };
                assert_eq!(out.len(), head + n, "n={n}");
                let peer = if role == Role::Client { Role::Server } else { Role::Client };
                assert_eq!(one(peer, &out).unwrap(), [Owned::Binary(data)], "n={n}");
            }
        }
    }

    #[test]
    fn bytewise_input() {
        let mut out = Vec::new();
        let mut e = Encoder::new(Role::Client);
        e.text("こんにちは", [9, 8, 7, 6], &mut out);
        e.binary(&[0xaa; 300], [1, 1, 1, 1], &mut out);
        e.ping(b"p", [0, 0, 0, 1], &mut out);
        e.binary(&vec![0x55; 70000], [3, 4, 5, 6], &mut out);
        e.close(1000, [5, 5, 5, 5], &mut out);
        let got = feed_bytewise(Role::Server, 1 << 20, &out).unwrap();
        assert_eq!(
            got,
            [
                Owned::Text("こんにちは".into()),
                Owned::Binary(vec![0xaa; 300]),
                Owned::Ping(b"p".to_vec()),
                Owned::Binary(vec![0x55; 70000]),
                Owned::Close(1000),
            ]
        );
    }

    #[test]
    fn fragmented_with_ping_in_between() {
        let mut v = raw(0x01, "日本".as_bytes()); // text、FIN なし
        v.extend(raw(0x89, b"hb")); // 途中の ping
        // 「語」の 3 バイトを、2 つの続きのフレームにまたがらせる
        let go = "語".as_bytes();
        v.extend(raw(0x00, &go[..1]));
        v.extend(raw(0x8a, b"")); // 途中の pong（空）
        v.extend(raw(0x80, &go[1..]));
        let expect = [Owned::Ping(b"hb".to_vec()), Owned::Pong(vec![]), Owned::Text("日本語".into())];
        assert_eq!(one(Role::Client, &v).unwrap(), expect);
        assert_eq!(feed_bytewise(Role::Client, 1 << 20, &v).unwrap(), expect);
        // バイナリの分割
        let mut v = raw(0x02, b"ab");
        v.extend(raw(0x00, b"cd"));
        v.extend(raw(0x80, b"ef"));
        v.extend(raw(0x81, b"next"));
        assert_eq!(one(Role::Client, &v).unwrap(), [Owned::Binary(b"abcdef".to_vec()), Owned::Text("next".into())]);
    }

    #[test]
    fn broken_fragmentation() {
        // 始まりの無い続き
        assert!(matches!(one(Role::Client, &raw(0x80, b"x")), Err(WsError::Protocol(_))));
        // 分割の途中に新しいテキスト
        let mut v = raw(0x01, b"a");
        v.extend(raw(0x81, b"b"));
        assert!(matches!(one(Role::Client, &v), Err(WsError::Protocol(_))));
        // 分割された ping
        assert!(matches!(one(Role::Client, &raw(0x09, b"x")), Err(WsError::Protocol(_))));
    }

    #[test]
    fn mask_direction() {
        // サーバはマスクなしを断る
        assert!(matches!(one(Role::Server, &raw(0x81, b"hi")), Err(WsError::Protocol(_))));
        // サーバの Encoder はマスクを掛けない（鍵を渡しても）
        let mut out = Vec::new();
        Encoder::new(Role::Server).text("hi", [1, 2, 3, 4], &mut out);
        assert_eq!(out, [0x81, 0x02, b'h', b'i']);
        // 鍵が 0 でもマスクの印は立つ
        let mut out = Vec::new();
        Encoder::new(Role::Client).text("hi", [0; 4], &mut out);
        assert_eq!(out, [0x81, 0x82, 0, 0, 0, 0, b'h', b'i']);
    }

    #[test]
    fn rsv_opcode_control_length() {
        for b0 in [0xc1, 0xa1, 0x91] {
            assert!(matches!(one(Role::Client, &raw(b0, b"x")), Err(WsError::Protocol(_))), "{b0:#x}");
        }
        for op in [3u8, 7, 0xb, 0xf] {
            assert!(matches!(one(Role::Client, &raw(0x80 | op, b"")), Err(WsError::Protocol(_))), "{op}");
        }
        // 125 バイトの ping は通る。126 は頭だけで断る
        assert_eq!(one(Role::Client, &raw(0x89, &[1; 125])).unwrap(), [Owned::Ping(vec![1; 125])]);
        let mut d = Decoder::new(Role::Client, 1 << 20);
        d.push(&[0x89, 126, 0]);
        assert!(matches!(d.next(), Err(WsError::Protocol(_))));
        // Encoder は 125 で切る
        let mut out = Vec::new();
        Encoder::new(Role::Server).ping(&[7; 200], [0; 4], &mut out);
        assert_eq!(out.len(), 2 + 125);
    }

    #[test]
    fn non_minimal_lengths() {
        // 126 で 5 バイト
        let v = [0x82, 126, 0, 5, 1, 2, 3, 4, 5];
        assert!(matches!(one(Role::Client, &v), Err(WsError::Protocol(_))));
        // 127 で 300 バイト
        let mut v = vec![0x82, 127, 0, 0, 0, 0, 0, 0, 1, 44];
        v.extend([0; 300]);
        assert!(matches!(one(Role::Client, &v), Err(WsError::Protocol(_))));
        // 64 ビットの最上位
        let v = [0x82, 127, 0x80, 0, 0, 0, 0, 0, 0, 0];
        assert!(matches!(one(Role::Client, &v), Err(WsError::Protocol(_))));
    }

    #[test]
    fn invalid_utf8() {
        assert!(matches!(one(Role::Client, &raw(0x81, &[0xff, 0xfe])), Err(WsError::Protocol(_))));
        // 分割の最後で文字が切れたまま
        let mut v = raw(0x01, b"ok");
        v.extend(raw(0x80, &"語".as_bytes()[..2]));
        assert!(matches!(one(Role::Client, &v), Err(WsError::Protocol(_))));
        // バイナリなら何でもよい
        assert_eq!(one(Role::Client, &raw(0x82, &[0xff])).unwrap(), [Owned::Binary(vec![0xff])]);
    }

    #[test]
    fn too_large_is_detected_from_the_head() {
        // 頭だけで、中身が届く前に断る
        let mut d = Decoder::new(Role::Client, 100);
        d.push(&[0x82, 101]);
        assert_eq!(d.next(), Err(WsError::TooLarge));
        let mut d = Decoder::new(Role::Client, 200);
        d.push(&[0x82, 126, 0, 201]);
        assert_eq!(d.next(), Err(WsError::TooLarge));
        // ちょうどは通る
        assert_eq!(
            {
                let mut d = Decoder::new(Role::Client, 100);
                d.push(&raw(0x82, &[0; 100]));
                drain(&mut d).unwrap()
            },
            [Owned::Binary(vec![0; 100])]
        );
        // 分割の合計で数える
        let mut v = raw(0x02, &[0; 60]);
        v.extend(raw(0x80, &[0; 41]));
        let mut d = Decoder::new(Role::Client, 100);
        d.push(&v);
        assert_eq!(d.next(), Err(WsError::TooLarge));
        // 制御フレームは max_message に関係なく通る
        let mut d = Decoder::new(Role::Client, 4);
        d.push(&raw(0x89, &[0; 10]));
        assert_eq!(drain(&mut d).unwrap(), [Owned::Ping(vec![0; 10])]);
        // 64 ビットの巨大な長さ
        let mut d = Decoder::new(Role::Client, 1 << 20);
        d.push(&[0x82, 127, 0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(d.next(), Err(WsError::TooLarge));
    }

    #[test]
    fn errors_are_sticky() {
        let mut d = Decoder::new(Role::Client, 1 << 20);
        d.push(&raw(0xc1, b"x"));
        let e = d.next().unwrap_err();
        d.push(&raw(0x81, b"ok"));
        assert_eq!(d.next().unwrap_err(), e);
    }

    #[test]
    fn close_frames() {
        assert_eq!(one(Role::Client, &raw(0x88, b"")).unwrap(), [Owned::Close(1005)]);
        assert_eq!(one(Role::Client, &raw(0x88, &[0x03, 0xe8, b'b', b'y', b'e'])).unwrap(), [Owned::Close(1000)]);
        assert_eq!(one(Role::Client, &raw(0x88, &4001u16.to_be_bytes())).unwrap(), [Owned::Close(4001)]);
        assert!(matches!(one(Role::Client, &raw(0x88, &[0x03])), Err(WsError::Protocol(_))));
        for bad in [999u16, 1004, 1005, 1006, 1015, 2000, 5000] {
            assert!(matches!(one(Role::Client, &raw(0x88, &bad.to_be_bytes())), Err(WsError::Protocol(_))), "{bad}");
        }
        assert!(matches!(one(Role::Client, &raw(0x88, &[0x03, 0xe8, 0xff])), Err(WsError::Protocol(_))));
        // close のあとは何も返さない
        let mut v = raw(0x88, &1001u16.to_be_bytes());
        v.extend(raw(0x81, b"late"));
        let mut d = Decoder::new(Role::Client, 1 << 20);
        d.push(&v);
        assert_eq!(drain(&mut d).unwrap(), [Owned::Close(1001)]);
        d.push(&raw(0x81, b"later"));
        assert_eq!(d.next().unwrap(), None);
        // 送ってはいけない値は状態コードなしで送る
        let mut out = Vec::new();
        Encoder::new(Role::Server).close(1005, [0; 4], &mut out);
        assert_eq!(out, [0x88, 0x00]);
        let mut out = Vec::new();
        Encoder::new(Role::Client).close(1000, [1, 2, 3, 4], &mut out);
        assert_eq!(one(Role::Server, &out).unwrap(), [Owned::Close(1000)]);
    }

    #[test]
    fn binary2_is_one_frame() {
        let head = [1u8, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let body: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        for role in [Role::Client, Role::Server] {
            let mut a = Vec::new();
            let mut b = Vec::new();
            Encoder::new(role).binary2(&head, &body, [0xde, 0xad, 0xbe, 0xef], &mut a);
            let joined: Vec<u8> = head.iter().chain(&body).copied().collect();
            Encoder::new(role).binary(&joined, [0xde, 0xad, 0xbe, 0xef], &mut b);
            assert_eq!(a, b);
        }
    }

    #[test]
    fn buffered_counts_unread_bytes() {
        let mut d = Decoder::new(Role::Client, 1 << 20);
        let mut v = raw(0x81, b"abc");
        v.extend(raw(0x02, b"xy")); // 分割の途中
        v.extend([0x80]); // 次のフレームの頭の 1 バイト目
        d.push(&v);
        assert_eq!(d.buffered(), v.len());
        assert_eq!(d.next().unwrap(), Some(Message::Text("abc")));
        assert_eq!(d.next().unwrap(), None);
        // "xy"（組み立て中）+ 1 バイト
        assert_eq!(d.buffered(), 3);
        d.push(&[0x01, b'z']);
        assert_eq!(d.next().unwrap(), Some(Message::Binary(b"xyz")));
        assert_eq!(d.buffered(), 0);
    }

    #[test]
    fn mask_handles_all_alignments() {
        for n in 0..40 {
            let data: Vec<u8> = (0..n as u8).collect();
            let mut m = data.clone();
            apply_mask(&mut m, [0x11, 0x22, 0x33, 0x44]);
            for (i, (a, b)) in data.iter().zip(&m).enumerate() {
                assert_eq!(a ^ [0x11, 0x22, 0x33, 0x44][i % 4], *b);
            }
        }
    }

    /// クライアント → サーバ → クライアントの往復。種類・大きさ・刻み方を乱数で。
    #[test]
    fn random_round_trip() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for round in 0..200 {
            let mut msgs = Vec::new();
            for _ in 0..1 + rng.below(8) {
                let n = match rng.below(4) {
                    0 => rng.below(126),
                    1 => 126 + rng.below(1000),
                    2 => 65530 + rng.below(20),
                    _ => rng.below(3),
                };
                let m = match rng.below(4) {
                    0 => Owned::Text("あa".repeat(n / 4 + 1)),
                    1 => Owned::Binary(rng.bytes(n)),
                    2 => Owned::Ping(rng.bytes(n.min(125))),
                    _ => Owned::Pong(rng.bytes(n.min(125))),
                };
                msgs.push(m);
            }
            // クライアント → サーバ
            let mut wire = Vec::new();
            let mut e = Encoder::new(Role::Client);
            for m in &msgs {
                let key = (rng.next() as u32).to_le_bytes();
                match m {
                    Owned::Text(t) => e.text(t, key, &mut wire),
                    Owned::Binary(b) => e.binary(b, key, &mut wire),
                    Owned::Ping(b) => e.ping(b, key, &mut wire),
                    Owned::Pong(b) => e.pong(b, key, &mut wire),
                    Owned::Close(c) => e.close(*c, key, &mut wire),
                }
            }
            let got = feed_chunks(&mut rng, Role::Server, &wire);
            assert_eq!(got, msgs, "round {round}");
            // サーバ → クライアント（受けたものをそのまま返す）
            let mut wire = Vec::new();
            let mut e = Encoder::new(Role::Server);
            for m in &got {
                match m {
                    Owned::Text(t) => e.text(t, [0; 4], &mut wire),
                    Owned::Binary(b) => e.binary(b, [0; 4], &mut wire),
                    Owned::Ping(b) => e.ping(b, [0; 4], &mut wire),
                    Owned::Pong(b) => e.pong(b, [0; 4], &mut wire),
                    Owned::Close(c) => e.close(*c, [0; 4], &mut wire),
                }
            }
            assert_eq!(feed_chunks(&mut rng, Role::Client, &wire), msgs, "round {round}");
        }
    }

    fn feed_chunks(rng: &mut Rng, role: Role, wire: &[u8]) -> Vec<Owned> {
        let mut d = Decoder::new(role, 1 << 20);
        let mut got = Vec::new();
        let mut i = 0;
        while i < wire.len() {
            let n = (1 + rng.below(5000)).min(wire.len() - i);
            d.push(&wire[i..i + n]);
            i += n;
            got.extend(drain(&mut d).unwrap());
        }
        assert_eq!(d.buffered(), 0);
        got
    }
}
