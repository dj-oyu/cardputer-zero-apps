//! WebSocket の Upgrade（HTTP/1.1）。I/O を持たない（課題 W1）。
//!
//! 実機側は依頼を作って返事を読む。受付は依頼を読んで返事を作る。どちらも頭（ヘッダ）は
//! 8KB まで。受付は Upgrade 以外に `GET /healthz`（`ok`）と `GET /v1/status`（JSON）にも
//! 答えるので、依頼の読み取りは Upgrade でないものも返す。

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
    pub fn write(&self, out: &mut Vec<u8>) {
        let _ = out;
        todo!("W1")
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
    Rejected { status: u16, location: Option<String>, retry_after_s: Option<u32> },
    /// HTTP として読めない・`Sec-WebSocket-Accept` が合わない・頭が 8KB を超えた。
    Invalid(&'static str),
}

/// 返事を読む。`key` は依頼に入れた `Sec-WebSocket-Key`。
pub fn parse_upgrade_response(buf: &[u8], key: &str) -> UpgradeResponse {
    let _ = (buf, key);
    todo!("W1")
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
    let _ = buf;
    todo!("W1")
}

/// 101 の返事を `out` に足す（受付側）。
pub fn write_switching(accept: &str, out: &mut Vec<u8>) {
    let _ = (accept, out);
    todo!("W1")
}

/// ふつうの返事（`Content-Length` つき、`Connection: close`）を `out` に足す（受付側）。
pub fn write_response(status: u16, content_type: &str, body: &[u8], out: &mut Vec<u8>) {
    let _ = (status, content_type, body, out);
    todo!("W1")
}

/// `Sec-WebSocket-Accept` = base64(SHA-1(key + GUID))。SHA-1 と base64 はこの中に持つ
/// （依存を足さない。ここ以外では使わない）。
pub fn accept_key(key: &str) -> String {
    let _ = key;
    todo!("W1")
}

/// 16 バイトを `Sec-WebSocket-Key` の形（base64）にする。
pub fn encode_key(nonce: [u8; 16]) -> String {
    let _ = nonce;
    todo!("W1")
}

#[cfg(test)]
mod tests {
    use super::*;

    // 受け入れ試験（課題 W1）: RFC の例、依頼 ⇔ 読み取りの往復、1 バイトずつ届く返事、
    // 302 / 403 / 429 の読み分け、Accept の食い違い、8KB 超え、ヘッダ名の大文字小文字。
    #[test]
    #[ignore = "W1"]
    fn rfc6455_accept_key() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        let resp = b"HTTP/1.1 302 Found\r\nLocation: https://example.cloudflareaccess.com/login\r\n\r\n";
        assert!(matches!(parse_upgrade_response(resp, "x"), UpgradeResponse::Rejected { status: 302, .. }));
    }
}
