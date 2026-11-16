//! APPLaunch（本体のランチャー）の設定を読む。
//!
//! 設定は `~/.config/cardputerzero/config.json`（CardputerZero/launcher の
//! `cp0_lvgl_config.cpp`）。JSON のオブジェクトで、入れ子は点区切りのキーに
//! 対応し、値は文字列か数値。zcode が使うのは一番上の階層の 2 つだけ:
//!
//! | キー | 意味 | ランチャーでの扱い |
//! |---|---|---|
//! | `dark_time` | 無操作で画面を眠らせるまでの秒数（設定画面で 0 / 10 / 30 / 60 / 300） | 無ければ 30。0 は眠らせない |
//! | `brightness` | バックライトの明るさ | 0 より大きければ起動時に適用。無ければ触らない |
//!
//! APPLaunch はアプリの実行中は自分のタイマーを止める（スクリーンセーバーも
//! 動かない）ので、アプリの実行中に画面を眠らせるのは zcode の役目。同じ設定に
//! 従い、ユーザーが設定画面で選んだ時間で眠らせる。
//!
//! 書き込みはしない（ランチャーの持ち物）。読めなければ既定値で動く。

use std::path::{Path, PathBuf};
use std::time::Duration;

/// `dark_time` が無い・読めないときの既定（ランチャーと同じ）。
pub const DEFAULT_DARK_TIME_SECS: u64 = 30;

/// ランチャーの設定のうち、zcode が使うもの。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LauncherSettings {
    /// 無操作で画面を眠らせるまで。`None` なら眠らせない（`dark_time = 0`）。
    pub dark_time: Option<Duration>,
    /// 起こすときの明るさ（`brightness`。0 以下・無しなら `None`）。
    pub brightness: Option<u32>,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        LauncherSettings { dark_time: Some(Duration::from_secs(DEFAULT_DARK_TIME_SECS)), brightness: None }
    }
}

/// 設定ファイルの場所（`$HOME/.config/cardputerzero/config.json`）。
pub fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join(".config/cardputerzero/config.json"))
}

/// 設定を読む。ファイルが無い・壊れているときは既定値。
pub fn load() -> LauncherSettings {
    config_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| parse(&t)).unwrap_or_default()
}

/// 設定ファイルの中身から読む（テスト用に分けてある）。
pub fn parse(text: &str) -> LauncherSettings {
    let mut s = LauncherSettings::default();
    if let Some(v) = top_level_int(text, "dark_time") {
        s.dark_time = (v > 0).then(|| Duration::from_secs(v as u64));
    }
    if let Some(v) = top_level_int(text, "brightness") {
        s.brightness = (v > 0).then_some(v as u32);
    }
    s
}

/// 一番上の階層の `key` の値を整数として読む（数値でも、数字だけの文字列でも）。
/// JSON 全体は検証しない。入れ子のオブジェクト・配列・文字列は読み飛ばす。
fn top_level_int(text: &str, key: &str) -> Option<i64> {
    let b = text.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
    let mut expect_key = false;
    while i < b.len() {
        match b[i] {
            b'{' => {
                depth += 1;
                expect_key = depth == 1;
                i += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b',' => {
                expect_key = depth == 1;
                i += 1;
            }
            b'"' => {
                let (s, next) = read_string(b, i)?;
                i = next;
                if depth == 1 && expect_key {
                    expect_key = false;
                    // ':' まで進めて値を読む
                    while i < b.len() && b[i] != b':' {
                        i += 1;
                    }
                    i += 1;
                    while i < b.len() && b[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    if s == key {
                        return read_int_value(b, i);
                    }
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// `b[start]` の `"` から文字列を読む。（中身, 閉じた `"` の次の位置）。
fn read_string(b: &[u8], start: usize) -> Option<(String, usize)> {
    let mut out = Vec::new();
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                // キーの比較にしか使わないので、エスケープは素朴に扱う
                if let Some(&c) = b.get(i + 1) {
                    out.push(c);
                }
                i += 2;
            }
            b'"' => return Some((String::from_utf8_lossy(&out).into_owned(), i + 1)),
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    None
}

/// 値の位置から整数を読む。数値か、数字だけの文字列（`"60"`）。
fn read_int_value(b: &[u8], i: usize) -> Option<i64> {
    let (digits, _) = if b.get(i) == Some(&b'"') {
        let (s, next) = read_string(b, i)?;
        (s, next)
    } else {
        let mut j = i;
        while j < b.len() && (b[j] == b'-' || b[j].is_ascii_digit()) {
            j += 1;
        }
        (String::from_utf8_lossy(&b[i..j]).into_owned(), j)
    };
    digits.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_top_level_values() {
        // 実機 1 号機の形（入れ子の appstore の中に同名のキーがあっても拾わない）
        let t = r#"{
  "appstore": { "registry": { "0": { "entry": "Hub", "dark_time": 999 } }, "count": 1 },
  "dark_time": 60,
  "brightness": "80"
}"#;
        let s = parse(t);
        assert_eq!(s.dark_time, Some(Duration::from_secs(60)));
        assert_eq!(s.brightness, Some(80));
    }

    #[test]
    fn defaults_and_never() {
        assert_eq!(parse(r#"{"appstore": {"count": 1}, "volume": 30}"#), LauncherSettings::default());
        assert_eq!(parse(r#"{"dark_time": 0}"#).dark_time, None); // 0 は眠らせない
        assert_eq!(parse(r#"{"dark_time": "300"}"#).dark_time, Some(Duration::from_secs(300)));
        assert_eq!(parse(r#"{"brightness": 0}"#).brightness, None);
        assert_eq!(parse("not json"), LauncherSettings::default());
        assert_eq!(parse(""), LauncherSettings::default());
    }
}
