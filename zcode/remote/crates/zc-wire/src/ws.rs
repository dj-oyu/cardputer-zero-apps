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

/// 受ける側。読んだバイト列を [`Decoder::push`] で足し、[`Decoder::next`] で 1 つずつ取り出す。
#[derive(Debug)]
pub struct Decoder {
    _role: Role,
    _max_message: usize,
}

impl Decoder {
    pub fn new(role: Role, max_message: usize) -> Decoder {
        Decoder { _role: role, _max_message: max_message }
    }

    /// 読んだバイト列を足す。
    pub fn push(&mut self, bytes: &[u8]) {
        let _ = bytes;
        todo!("W1")
    }

    /// そろっているメッセージを 1 つ返す。足りなければ `Ok(None)`。
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Message<'_>>, WsError> {
        todo!("W1")
    }

    /// まだ取り出していないバイト数（上限の監視用）。
    pub fn buffered(&self) -> usize {
        todo!("W1")
    }
}

/// 送る側。フレームを `out` の末尾に足す。クライアントのマスクの鍵は呼ぶ側が渡す
/// （乱数の出どころをこの crate に持たせない。`getrandom(2)` か時刻と番号の混ぜ物でよい:
/// マスクは秘密ではなく、途中の中継器の誤動作を避けるためのもの）。
#[derive(Debug)]
pub struct Encoder {
    _role: Role,
}

impl Encoder {
    pub fn new(role: Role) -> Encoder {
        Encoder { _role: role }
    }

    pub fn text(&mut self, text: &str, mask: [u8; 4], out: &mut Vec<u8>) {
        let _ = (text, mask, out);
        todo!("W1")
    }

    pub fn binary(&mut self, data: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
        let _ = (data, mask, out);
        todo!("W1")
    }

    /// 頭（`head`）と中身（`body`）を 1 つのバイナリフレームにする（[`crate::msg::BinHeader`] +
    /// ファイルの中身を、つなぎ直さずに送るため）。
    pub fn binary2(&mut self, head: &[u8], body: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
        let _ = (head, body, mask, out);
        todo!("W1")
    }

    pub fn ping(&mut self, data: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
        let _ = (data, mask, out);
        todo!("W1")
    }

    pub fn pong(&mut self, data: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
        let _ = (data, mask, out);
        todo!("W1")
    }

    pub fn close(&mut self, code: u16, mask: [u8; 4], out: &mut Vec<u8>) {
        let _ = (code, mask, out);
        todo!("W1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 受け入れ試験（課題 W1）: RFC 6455 §5.7 の例、1 バイトずつ届く入力、126 / 65536 の境目、
    // 分割 + 途中の ping、マスクの向きの間違い、RSV、制御フレームの長さ、壊れた UTF-8、
    // max_message 超え、クライアント → サーバ → クライアントの往復（乱数の入力で）。
    #[test]
    #[ignore = "W1"]
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
}
