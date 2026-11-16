//! 設定とトークン（docs/stage3-design.md §7.6・§12.3）。課題 D5。
//!
//! どちらも SD の `~/.config/zcode/`（`XDG_CONFIG_HOME` があればそちら）。人が 1 回書く。
//! zc-remote は読むだけ。
//!
//! `remote.json`（無ければ全部既定。既定ではどのビルド係も決まっていないので、
//! 依頼は `Auth`（設定が無い）で返る）:
//!
//! ```json
//! {
//!   "backend": "auto",
//!   "cloudflare": { "url": "wss://<ビルド係のホスト名>/v1/ws" },
//!   "pc": { "addr": "<PC の tailnet 上の名前か IP>:7411" },
//!   "check_on_save": "always"
//! }
//! ```
//!
//! `remote-token.json`（**権限 0600。それより広ければ読まない**。中身はログに出さない）:
//!
//! ```json
//! { "scheme": "cf-access-service-token", "client_id": "….access", "client_secret": "…" }
//! ```

use std::path::PathBuf;

/// どのビルド係を使うか。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BackendChoice {
    /// PC に届けば PC（300ms の接続の試み）、届かなければ Cloudflare。
    #[default]
    Auto,
    Pc,
    Cloudflare,
}

/// 保存時の check をいつ頼むか（zcode が読む設定ではなく、serve が依頼を受けたときに
/// 当てはめる: `Warm` なら冷えているとき `Offline` ですぐ返す）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CheckOnSave {
    #[default]
    Always,
    /// 接続が温かいときだけ（無線を起こす回数を減らす）。
    Warm,
    /// 頼まない。
    Manual,
}

/// `remote.json` の中身。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Config {
    pub backend: BackendChoice,
    /// `wss://host/v1/ws`（`ws://` は試験用にだけ受け付ける）。
    pub cloudflare_url: Option<String>,
    /// `host:port`（TLS なし。tailnet の中）。
    pub pc_addr: Option<String>,
    pub check_on_save: CheckOnSave,
}

/// 設定を読めなかった理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// JSON として読めない・知らない値（ファイルと理由）。
    Invalid(PathBuf, String),
    /// トークンのファイルの権限が 0600 より広い。
    TokenPermissions(PathBuf, u32),
    Io(PathBuf, String),
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ConfigError {}

/// `~/.config/zcode/`（`XDG_CONFIG_HOME` → `HOME` の順）。
pub fn config_dir() -> PathBuf {
    todo!("D5")
}

impl Config {
    /// `remote.json` を読む。無ければ既定。環境変数 `ZC_REMOTE_PC` / `ZC_REMOTE_URL` /
    /// `ZC_REMOTE_BACKEND` があれば上書きする（試験と CLI 用）。
    pub fn load() -> Result<Config, ConfigError> {
        todo!("D5")
    }

    /// JSON の文字列から（試験用）。
    pub fn parse(json: &str) -> Result<Config, String> {
        let _ = json;
        todo!("D5")
    }
}
