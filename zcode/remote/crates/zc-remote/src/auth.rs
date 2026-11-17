//! 認証の口（docs/stage3-design.md §7.5）。課題 D1。
//!
//! 認証の方式を 1 つの口（[`Credential`]）にまとめる。いまの実装は 2 つ:
//! [`NoCredential`]（PC・tailnet。何も付けない）と [`AccessServiceToken`]（Cloudflare
//! Access のサービストークン）。Tailscale のノード ID トークンが使えるようになったら、
//! `TailscaleIdToken { audience }` を足すだけで乗り換えられる（Worker 側は身元を
//! `device_id` に直してから先へ渡すので、DO と受付は方式を知らない）。

use std::path::{Path, PathBuf};

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
///
/// 断られたら（[`Credential::rejected`]）、ファイルが書き換わるまで [`AuthError::Rejected`] を
/// 返し続ける（だめなトークンでつなぎ続けて、Worker の回数の上限を使わない）。人がファイルを
/// 直せば、次の [`Credential::headers`] で読み直す。
pub struct AccessServiceToken {
    path: PathBuf,
    stamp: Stamp,
    client_id: String,
    client_secret: String,
    /// 断られたときの状態コード。
    rejected: Option<u16>,
}

/// ファイルが書き換わったかの目印（大きさ・mtime・inode）。
type Stamp = (u64, i64, i64, u64);

/// トークンのファイルの上限。これより大きければ、トークンではない。
const MAX_TOKEN_FILE: u64 = 4096;

#[derive(serde::Deserialize)]
struct TokenFile {
    scheme: String,
    client_id: String,
    client_secret: String,
}

/// ヘッダの値に入れてよい文字だけか（改行を混ぜて別のヘッダを足されないように）。
fn header_safe(s: &str) -> bool {
    !s.is_empty() && s.len() <= 512 && s.bytes().all(|b| (0x21..0x7f).contains(&b))
}

fn stamp_of(md: &std::fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt;
    (md.len(), md.mtime(), md.mtime_nsec(), md.ino())
}

fn read_token(path: &Path) -> Result<(Stamp, String, String), AuthError> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(AuthError::Missing),
        Err(e) => return Err(AuthError::Invalid(e.kind().to_string())),
    };
    // 開いたものを調べる（調べてから開くと、その間に差し替えられる）。
    let md = f.metadata().map_err(|e| AuthError::Invalid(e.kind().to_string()))?;
    if !md.is_file() {
        return Err(AuthError::Invalid("ふつうのファイルではない".to_owned()));
    }
    if md.mode() & 0o077 != 0 {
        return Err(AuthError::Permissions(md.mode() & 0o777));
    }
    if md.len() > MAX_TOKEN_FILE {
        return Err(AuthError::Invalid("大きすぎる".to_owned()));
    }
    let mut text = String::new();
    f.take(MAX_TOKEN_FILE).read_to_string(&mut text).map_err(|e| AuthError::Invalid(e.kind().to_string()))?;
    // 理由に中身を入れない（serde のエラーからは行と桁だけ使う）。
    let t: TokenFile = serde_json::from_str(&text).map_err(|e| AuthError::Invalid(format!("JSON として読めない（{} 行 {} 桁）", e.line(), e.column())))?;
    if t.scheme != "cf-access-service-token" {
        return Err(AuthError::Invalid("知らない scheme".to_owned()));
    }
    if !header_safe(&t.client_id) || !header_safe(&t.client_secret) {
        return Err(AuthError::Invalid("client_id / client_secret の形がおかしい".to_owned()));
    }
    Ok((stamp_of(&md), t.client_id, t.client_secret))
}

impl AccessServiceToken {
    /// `remote-token.json` を読む（[`crate::config`] の形）。権限が 0600 より広ければ読まない。
    pub fn load(path: &Path) -> Result<AccessServiceToken, AuthError> {
        let (stamp, client_id, client_secret) = read_token(path)?;
        Ok(AccessServiceToken { path: path.to_owned(), stamp, client_id, client_secret, rejected: None })
    }

    /// ファイルが読んだときから変わっていれば読み直す。読み直せたら `true`。
    fn reload_if_changed(&mut self) -> bool {
        let Ok(md) = std::fs::metadata(&self.path) else { return false };
        if stamp_of(&md) == self.stamp {
            return false;
        }
        match read_token(&self.path) {
            Ok((stamp, id, secret)) => {
                (self.stamp, self.client_id, self.client_secret, self.rejected) = (stamp, id, secret, None);
                true
            }
            Err(_) => false,
        }
    }
}

impl Credential for AccessServiceToken {
    fn headers(&mut self, out: &mut Vec<(String, String)>) -> Result<(), AuthError> {
        if let Some(status) = self.rejected {
            if !self.reload_if_changed() {
                return Err(AuthError::Rejected(status));
            }
        }
        out.push(("CF-Access-Client-Id".to_owned(), self.client_id.clone()));
        out.push(("CF-Access-Client-Secret".to_owned(), self.client_secret.clone()));
        Ok(())
    }

    fn rejected(&mut self, status: u16) -> Retry {
        // 断られるまでの間に人がファイルを直していたら、新しいトークンでもう 1 回。
        if self.reload_if_changed() {
            return Retry::Once;
        }
        self.rejected = Some(status);
        Retry::GiveUp
    }
}

// Debug で秘密を出さない。
impl core::fmt::Debug for AccessServiceToken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("AccessServiceToken(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const GOOD: &str = r#"{ "scheme": "cf-access-service-token", "client_id": "abc.access", "client_secret": "s3cr3t" }"#;

    /// 試験ごとに別の一時ファイル（並列に走っても消し合わない）。
    fn token(name: &str, text: &str, mode: u32) -> PathBuf {
        let p = std::env::temp_dir().join(format!("zc-remote-auth-{}-{name}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, text).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    fn headers(c: &mut dyn Credential) -> Result<Vec<(String, String)>, AuthError> {
        let mut h = Vec::new();
        c.headers(&mut h).map(|()| h)
    }

    #[test]
    fn loads_a_0600_token_and_adds_two_headers() {
        let p = token("good", GOOD, 0o600);
        let mut t = AccessServiceToken::load(&p).unwrap();
        assert_eq!(headers(&mut t).unwrap(), [("CF-Access-Client-Id".to_owned(), "abc.access".to_owned()), ("CF-Access-Client-Secret".to_owned(), "s3cr3t".to_owned())]);
        // 0400 でもよい。Debug に秘密は出ない
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o400)).unwrap();
        let t = AccessServiceToken::load(&p).unwrap();
        assert_eq!(format!("{t:?}"), "AccessServiceToken(..)");
        std::fs::remove_file(&p).unwrap();
        assert_eq!(headers(&mut NoCredential).unwrap(), []);
        assert_eq!(NoCredential.rejected(403), Retry::GiveUp);
    }

    #[test]
    fn refuses_wide_permissions_and_bad_files() {
        for mode in [0o644, 0o640, 0o604, 0o660] {
            let p = token(&format!("wide{mode:o}"), GOOD, mode);
            assert_eq!(AccessServiceToken::load(&p).unwrap_err(), AuthError::Permissions(mode));
            std::fs::remove_file(&p).unwrap();
        }
        assert_eq!(AccessServiceToken::load(&std::env::temp_dir().join("zc-remote-auth-no-such-file.json")).unwrap_err(), AuthError::Missing);
        let bad = [
            "not json",
            r#"{"scheme":"other","client_id":"a","client_secret":"b"}"#,
            r#"{"scheme":"cf-access-service-token","client_id":"a"}"#,
            r#"{"scheme":"cf-access-service-token","client_id":"","client_secret":"b"}"#,
            // 改行を混ぜて別のヘッダを足そうとするもの
            r#"{"scheme":"cf-access-service-token","client_id":"a\r\nX-Evil: 1","client_secret":"b"}"#,
        ];
        for (i, text) in bad.iter().enumerate() {
            let p = token(&format!("bad{i}"), text, 0o600);
            let e = AccessServiceToken::load(&p).unwrap_err();
            assert!(matches!(e, AuthError::Invalid(_)), "{text}: {e:?}");
            // 理由に中身（秘密）を入れない
            assert!(!format!("{e:?}").contains("X-Evil"));
            std::fs::remove_file(&p).unwrap();
        }
    }

    #[test]
    fn rejected_token_stays_rejected_until_the_file_changes() {
        let p = token("rej", GOOD, 0o600);
        let mut t = AccessServiceToken::load(&p).unwrap();
        assert_eq!(t.rejected(403), Retry::GiveUp);
        assert_eq!(headers(&mut t).unwrap_err(), AuthError::Rejected(403));
        assert_eq!(headers(&mut t).unwrap_err(), AuthError::Rejected(403));
        // 人が直した（大きさが変わるので、mtime の細かさによらず気づく）
        std::fs::write(&p, GOOD.replace("s3cr3t", "new-secret")).unwrap();
        assert_eq!(headers(&mut t).unwrap()[1].1, "new-secret");
        // 断られた時点でもう直っていたら、もう 1 回
        std::fs::write(&p, GOOD.replace("s3cr3t", "newer-secret!")).unwrap();
        assert_eq!(t.rejected(403), Retry::Once);
        assert_eq!(headers(&mut t).unwrap()[1].1, "newer-secret!");
        // 直したつもりで権限を広げていたら、読まない
        std::fs::write(&p, GOOD).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(t.rejected(401), Retry::GiveUp);
        assert_eq!(headers(&mut t).unwrap_err(), AuthError::Rejected(401));
        std::fs::remove_file(&p).unwrap();
    }
}
