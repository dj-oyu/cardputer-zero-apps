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
//!
//! 決めたこと（実装）:
//!
//! - 知らない項目は無視する（版を上げずに足せるように）。知っている項目の知らない値
//!   （`"backend": "aws"` など）は読み間違いとして断る
//! - 行き先の形（`wss://…`・`host:port`）はここで確かめる。`pc.addr` は TLS なしだけ
//!   （`wss://` は断る。tailnet の外へ平文で出る設定を書かせないのではなく、PC の経路には
//!   資格を付けないので、TLS の先へ資格なしでつなぐ設定を作らせない）
//! - 環境変数 `ZC_REMOTE_PC`・`ZC_REMOTE_URL`・`ZC_REMOTE_BACKEND` はファイルより強い。
//!   空の値は「指定なし」
//! - トークンは [`crate::auth::AccessServiceToken`] が読む（権限の確かめもそちら）。ここは
//!   場所（[`token_path`]）を決めるだけ

use crate::net::Endpoint;
use std::path::{Path, PathBuf};

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
/// 当てはめる: `Warm` なら冷えているとき `Offline` ですぐ返す）。打鍵が止まったときの check
/// （`SubmitFlags::AUTO`）は `Idle` のときだけ受け、ほかは「対象ではない」で返す。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CheckOnSave {
    /// 保存のたびに（既定）。
    #[default]
    Always,
    /// 保存のたびに加えて、打鍵が止まったとき（未保存の中身で）も。
    Idle,
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
        match self {
            ConfigError::Invalid(p, why) => write!(f, "{} を読めない: {why}", p.display()),
            ConfigError::TokenPermissions(p, mode) => write!(f, "{} の権限が広い（{mode:o}）。chmod 600 にする", p.display()),
            ConfigError::Io(p, why) => write!(f, "{} を開けない: {why}", p.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

/// `~/.config/zcode/`（`XDG_CONFIG_HOME` → `HOME` の順）。
pub fn config_dir() -> PathBuf {
    let abs = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
    let base = abs("XDG_CONFIG_HOME").or_else(|| abs("HOME").map(|h| h.join(".config"))).unwrap_or_else(|| PathBuf::from("/nonexistent/.config"));
    base.join("zcode")
}

/// `remote.json` の場所。
pub fn config_path() -> PathBuf {
    config_dir().join("remote.json")
}

/// `remote-token.json` の場所。
pub fn token_path() -> PathBuf {
    config_dir().join("remote-token.json")
}

/// 設定のファイルの上限（これより大きければ設定ではない）。
const MAX_CONFIG_FILE: u64 = 64 << 10;

#[derive(serde::Deserialize, Default)]
struct Raw {
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    cloudflare: Option<RawCloudflare>,
    #[serde(default)]
    pc: Option<RawPc>,
    #[serde(default)]
    check_on_save: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct RawCloudflare {
    #[serde(default)]
    url: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct RawPc {
    #[serde(default)]
    addr: Option<String>,
}

fn parse_backend(s: &str) -> Result<BackendChoice, String> {
    match s {
        "auto" => Ok(BackendChoice::Auto),
        "pc" => Ok(BackendChoice::Pc),
        "cloudflare" => Ok(BackendChoice::Cloudflare),
        _ => Err(format!("backend は auto / pc / cloudflare のどれか: {s:?}")),
    }
}

fn check_url(s: &str) -> Result<(), String> {
    match Endpoint::parse(s) {
        Some(_) if s.starts_with("wss://") || s.starts_with("ws://") => Ok(()),
        _ => Err(format!("cloudflare.url は wss://<ホスト名>/v1/ws の形: {s:?}")),
    }
}

fn check_addr(s: &str) -> Result<(), String> {
    match Endpoint::parse(s) {
        Some(ep) if !ep.tls => Ok(()),
        _ => Err(format!("pc.addr は <名前か IP>:7411 の形（TLS なし）: {s:?}")),
    }
}

impl Config {
    /// `remote.json` を読む。無ければ既定。環境変数 `ZC_REMOTE_PC` / `ZC_REMOTE_URL` /
    /// `ZC_REMOTE_BACKEND` があれば上書きする（試験と CLI 用）。
    pub fn load() -> Result<Config, ConfigError> {
        let path = config_path();
        Config::load_from(&path, &|k| std::env::var(k).ok())
    }

    /// [`Config::load`] の中身（場所と、環境変数の読み方を渡す。試験用）。
    pub fn load_from(path: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        use std::io::Read;
        let mut cfg = match std::fs::File::open(path) {
            Ok(f) => {
                let mut text = String::new();
                f.take(MAX_CONFIG_FILE + 1).read_to_string(&mut text).map_err(|e| ConfigError::Io(path.to_owned(), e.to_string()))?;
                if text.len() as u64 > MAX_CONFIG_FILE {
                    return Err(ConfigError::Invalid(path.to_owned(), "大きすぎる".to_owned()));
                }
                Config::parse(&text).map_err(|why| ConfigError::Invalid(path.to_owned(), why))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(ConfigError::Io(path.to_owned(), e.to_string())),
        };
        let var = |k: &str| env(k).filter(|v| !v.is_empty());
        let bad = |k: &str, why: String| ConfigError::Invalid(PathBuf::from(format!("${k}")), why);
        if let Some(v) = var("ZC_REMOTE_PC") {
            check_addr(&v).map_err(|w| bad("ZC_REMOTE_PC", w))?;
            cfg.pc_addr = Some(v);
        }
        if let Some(v) = var("ZC_REMOTE_URL") {
            check_url(&v).map_err(|w| bad("ZC_REMOTE_URL", w))?;
            cfg.cloudflare_url = Some(v);
        }
        if let Some(v) = var("ZC_REMOTE_BACKEND") {
            cfg.backend = parse_backend(&v).map_err(|w| bad("ZC_REMOTE_BACKEND", w))?;
        }
        Ok(cfg)
    }

    /// JSON の文字列から（試験用）。
    pub fn parse(json: &str) -> Result<Config, String> {
        // 理由に中身は入れない（行と桁だけ）。設定に秘密は無いが、トークンを間違えてここに
        // 貼った人の秘密をログに出さない。
        let raw: Raw = serde_json::from_str(json).map_err(|e| format!("JSON として読めない（{} 行 {} 桁）", e.line(), e.column()))?;
        let mut cfg = Config::default();
        if let Some(b) = raw.backend {
            cfg.backend = parse_backend(&b)?;
        }
        if let Some(url) = raw.cloudflare.and_then(|c| c.url).filter(|u| !u.is_empty()) {
            check_url(&url)?;
            cfg.cloudflare_url = Some(url);
        }
        if let Some(addr) = raw.pc.and_then(|p| p.addr).filter(|a| !a.is_empty()) {
            check_addr(&addr)?;
            cfg.pc_addr = Some(addr);
        }
        if let Some(c) = raw.check_on_save {
            cfg.check_on_save = match c.as_str() {
                "always" => CheckOnSave::Always,
                "idle" => CheckOnSave::Idle,
                "warm" => CheckOnSave::Warm,
                "manual" => CheckOnSave::Manual,
                _ => return Err(format!("check_on_save は always / idle / warm / manual のどれか: {c:?}")),
            };
        }
        Ok(cfg)
    }

    /// PC の行き先（`backend` が `cloudflare` なら使わない）。
    pub fn pc_endpoint(&self) -> Option<Endpoint> {
        if self.backend == BackendChoice::Cloudflare {
            return None;
        }
        self.pc_addr.as_deref().and_then(Endpoint::parse)
    }

    /// Cloudflare の行き先（`backend` が `pc` なら使わない）。
    pub fn cloudflare_endpoint(&self) -> Option<Endpoint> {
        if self.backend == BackendChoice::Pc {
            return None;
        }
        self.cloudflare_url.as_deref().and_then(Endpoint::parse)
    }

    /// 使えるビルド係が 1 つも決まっていない理由（決まっていれば `None`）。
    pub fn unusable(&self) -> Option<&'static str> {
        if self.pc_endpoint().is_some() || self.cloudflare_endpoint().is_some() {
            return None;
        }
        Some(match self.backend {
            BackendChoice::Pc => "remote.json の backend が pc なのに pc.addr が無い",
            BackendChoice::Cloudflare => "remote.json の backend が cloudflare なのに cloudflare.url が無い",
            BackendChoice::Auto => "ビルド係の設定が無い（~/.config/zcode/remote.json に pc.addr か cloudflare.url を書く）",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_form() {
        let c = Config::parse(r#"{ "backend": "auto", "cloudflare": { "url": "wss://build.example.com/v1/ws" }, "pc": { "addr": "pc.example.ts.net:7411" }, "check_on_save": "warm" }"#).unwrap();
        assert_eq!(c, Config { backend: BackendChoice::Auto, cloudflare_url: Some("wss://build.example.com/v1/ws".into()), pc_addr: Some("pc.example.ts.net:7411".into()), check_on_save: CheckOnSave::Warm });
        assert_eq!(c.pc_endpoint().unwrap().port, 7411);
        assert!(c.cloudflare_endpoint().unwrap().tls);
        assert_eq!(c.unusable(), None);
    }

    #[test]
    fn empty_and_unknown_keys() {
        assert_eq!(Config::parse("{}").unwrap(), Config::default());
        // 知らない項目は無視する
        let c = Config::parse(r#"{ "backend": "pc", "pc": { "addr": "10.0.0.1:7411", "note": 1 }, "future": [1, 2] }"#).unwrap();
        assert_eq!((c.backend, c.pc_addr.as_deref()), (BackendChoice::Pc, Some("10.0.0.1:7411")));
        // 既定はどのビルド係も決まっていない
        assert!(Config::default().unusable().is_some());
        // backend と行き先が食い違う
        assert!(Config::parse(r#"{ "backend": "cloudflare", "pc": { "addr": "pc:7411" } }"#).unwrap().unusable().is_some());
        assert!(Config::parse(r#"{ "backend": "pc", "cloudflare": { "url": "wss://h/v1/ws" } }"#).unwrap().unusable().is_some());
    }

    #[test]
    fn bad_values_are_rejected_without_echoing_the_file() {
        for bad in [
            "not json",
            r#"{ "backend": "aws" }"#,
            r#"{ "check_on_save": "sometimes" }"#,
            r#"{ "cloudflare": { "url": "https://h/" } }"#,
            r#"{ "cloudflare": { "url": "build.example.com" } }"#,
            r#"{ "pc": { "addr": "wss://pc/v1/ws" } }"#,
            r#"{ "pc": { "addr": "pc:port" } }"#,
            r#"{ "backend": 3 }"#,
        ] {
            assert!(Config::parse(bad).is_err(), "{bad}");
        }
        let e = Config::parse(r#"{ "client_secret": "s3cr3t", "backend": }"#).unwrap_err();
        assert_eq!(Config::parse(r#"{ "check_on_save": "idle" }"#).unwrap().check_on_save, CheckOnSave::Idle);
        assert!(!e.contains("s3cr3t"), "{e}");
    }

    #[test]
    fn file_and_environment() {
        let dir = std::env::temp_dir().join(format!("zc-remote-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("remote.json");
        let none = |_: &str| None;
        // 無ければ既定
        assert_eq!(Config::load_from(&path, &none).unwrap(), Config::default());
        std::fs::write(&path, r#"{ "backend": "pc", "pc": { "addr": "pc:7411" } }"#).unwrap();
        assert_eq!(Config::load_from(&path, &none).unwrap().pc_addr.as_deref(), Some("pc:7411"));
        // 環境変数はファイルより強い。空は指定なし
        let env = |k: &str| match k {
            "ZC_REMOTE_PC" => Some("127.0.0.1:9".to_owned()),
            "ZC_REMOTE_URL" => Some("ws://127.0.0.1:8/v1/ws".to_owned()),
            "ZC_REMOTE_BACKEND" => Some(String::new()),
            _ => None,
        };
        let c = Config::load_from(&path, &env).unwrap();
        assert_eq!((c.backend, c.pc_addr.as_deref(), c.cloudflare_url.as_deref()), (BackendChoice::Pc, Some("127.0.0.1:9"), Some("ws://127.0.0.1:8/v1/ws")));
        let env = |k: &str| (k == "ZC_REMOTE_BACKEND").then(|| "nope".to_owned());
        assert!(matches!(Config::load_from(&path, &env), Err(ConfigError::Invalid(..))));
        std::fs::write(&path, "{").unwrap();
        let e = Config::load_from(&path, &none).unwrap_err();
        assert!(matches!(e, ConfigError::Invalid(..)) && e.to_string().contains("remote.json"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
