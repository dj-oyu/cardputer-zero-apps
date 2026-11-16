//! 認証の口（docs/stage3-design.md §7.5）。課題 D1。
//!
//! 認証の方式を 1 つの口（[`Credential`]）にまとめる。いまの実装は 2 つ:
//! [`NoCredential`]（PC・tailnet。何も付けない）と [`AccessServiceToken`]（Cloudflare
//! Access のサービストークン）。Tailscale のノード ID トークンが使えるようになったら、
//! `TailscaleIdToken { audience }` を足すだけで乗り換えられる（Worker 側は身元を
//! `device_id` に直してから先へ渡すので、DO と受付は方式を知らない）。

use std::path::Path;

/// 認証できない理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// トークンのファイルが無い。
    Missing,
    /// 権限が 0600 より広い（読まない）。
    Permissions(u32),
    /// 形がおかしい・知らない方式。
    Invalid(String),
    /// 断られた（401 / 403、またはログイン画面への 302）。
    Rejected(u16),
}

impl core::fmt::Display for AuthError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for AuthError {}

/// 断られた後にどうするか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    /// 資格を取り直したので、もう 1 回だけ試す（短命のトークン用）。
    Once,
    /// やり直しても同じ。人が直すまで [`AuthError`] を返し続ける。
    GiveUp,
}

/// Upgrade の依頼に付ける資格。
pub trait Credential {
    /// 依頼に足すヘッダを `out` に入れる。**値をログに出さない。**
    fn headers(&mut self, out: &mut Vec<(String, String)>) -> Result<(), AuthError>;
    /// 401 / 403 / 302 を受けたとき。
    fn rejected(&mut self, status: u16) -> Retry;
}

/// 何も付けない（PC の経路。認証は Tailscale の ACL）。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCredential;

impl Credential for NoCredential {
    fn headers(&mut self, _out: &mut Vec<(String, String)>) -> Result<(), AuthError> {
        Ok(())
    }
    fn rejected(&mut self, _status: u16) -> Retry {
        Retry::GiveUp
    }
}

/// Cloudflare Access のサービストークン（`CF-Access-Client-Id` / `CF-Access-Client-Secret`）。
pub struct AccessServiceToken {
    _client_id: String,
    _client_secret: String,
}

impl AccessServiceToken {
    /// `remote-token.json` を読む（[`crate::config`] の形）。権限が 0600 より広ければ読まない。
    pub fn load(path: &Path) -> Result<AccessServiceToken, AuthError> {
        let _ = path;
        todo!("D1")
    }
}

impl Credential for AccessServiceToken {
    fn headers(&mut self, out: &mut Vec<(String, String)>) -> Result<(), AuthError> {
        let _ = out;
        todo!("D1")
    }
    fn rejected(&mut self, status: u16) -> Retry {
        let _ = status;
        todo!("D1")
    }
}

// Debug で秘密を出さない。
impl core::fmt::Debug for AccessServiceToken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("AccessServiceToken(..)")
    }
}
