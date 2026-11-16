//! 液晶のバックライト（`/sys/class/backlight/*`）と、zcode 自身の画面スリープ。
//!
//! APPLaunch はアプリの実行中は自分のタイマーを止める（スクリーンセーバーも
//! 動かない）。そのため、アプリの実行中に無操作で画面を眠らせるのはアプリの役目。
//! zcode は、ランチャーの設定 `dark_time` の時間だけ打鍵が無ければ
//! [`Backlight::sleep`] で消し、次の打鍵で [`Backlight::ensure_on`] で起こす
//! （起こした打鍵はアプリに渡さない。イベントループが捨てる）。
//!
//! バックライトを見張り続けることはしない。読み書きするのは、起動時・眠らせる
//! とき・起こすとき・終了時だけ。
//!
//! 点ける明るさは、指定（`--backlight`。ランチャーの設定 `brightness` もここに
//! 入る）→ 起動時の値（0 でなければ）→ 眠らせる直前の値 → 最大値 の順に決める。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// バックライト 1 つ。
#[derive(Debug)]
pub struct Backlight {
    brightness: PathBuf,
    max: u32,
    /// 点けるときの明るさ。
    level: u32,
    /// 起動時の値。
    initial: u32,
    /// 点け直した回数（計測用）。
    pub restores: u32,
    /// zcode が自分で消して眠っているか。
    asleep: bool,
    /// 眠らせた回数（計測用）。
    pub sleeps: u32,
}

impl Backlight {
    /// `/sys/class/backlight/` の最初のデバイスを開く。無ければ `Ok(None)`
    /// （PNG 出力や、バックライトの無い環境）。
    pub fn open_default(level: Option<u32>) -> io::Result<Option<Backlight>> {
        let dir = Path::new("/sys/class/backlight");
        let Ok(rd) = fs::read_dir(dir) else { return Ok(None) };
        let mut names: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        names.sort();
        match names.first() {
            Some(p) => Backlight::open(p, level).map(Some),
            None => Ok(None),
        }
    }

    /// デバイスのディレクトリ（`brightness` と `max_brightness` を持つ）を開く。
    pub fn open(dir: &Path, level: Option<u32>) -> io::Result<Backlight> {
        let max = read_u32(&dir.join("max_brightness"))?;
        let brightness = dir.join("brightness");
        let initial = read_u32(&brightness)?;
        let level = pick_level(level, initial, max);
        Ok(Backlight { brightness, max, level, initial, restores: 0, asleep: false, sleeps: 0 })
    }

    /// 起動時の値。
    pub fn initial(&self) -> u32 {
        self.initial
    }

    /// 点けるときの明るさ。
    pub fn level(&self) -> u32 {
        self.level
    }

    /// zcode が自分で消して眠っているか。
    pub fn is_asleep(&self) -> bool {
        self.asleep
    }

    /// 画面を消して眠る（無操作が続いたとき）。消せたら `true`。
    pub fn sleep(&mut self) -> bool {
        if self.asleep {
            return false;
        }
        // 消す前の明るさを覚えておく（起こすときに戻す）
        if let Ok(cur) = read_u32(&self.brightness) {
            if cur > 0 {
                self.level = cur.min(self.max);
            }
        }
        if fs::write(&self.brightness, "0\n").is_ok() {
            self.asleep = true;
            self.sleeps += 1;
            true
        } else {
            false
        }
    }

    /// 消えていたら点ける（眠っていたら起こす）。点けたら `true`。
    /// 読めない・書けないときは何もしない（画面の更新を止めない）。
    pub fn ensure_on(&mut self) -> bool {
        let was_asleep = std::mem::replace(&mut self.asleep, false);
        let Ok(cur) = read_u32(&self.brightness) else { return was_asleep };
        if cur > 0 {
            // ユーザーや APPLaunch が明るさを変えたら、それに合わせる
            self.level = cur.min(self.max);
            return false;
        }
        if fs::write(&self.brightness, format!("{}\n", self.level)).is_ok() {
            self.restores += 1;
            true
        } else {
            was_asleep
        }
    }
}

/// 点ける明るさを決める（純粋な関数。テスト用に分けてある）。
pub fn pick_level(requested: Option<u32>, initial: u32, max: u32) -> u32 {
    let max = max.max(1);
    match requested {
        Some(v) => v.clamp(1, max),
        None if initial > 0 => initial.min(max),
        // 何も分からなければ最大（カーネルとランチャーの既定と同じ）
        None => max,
    }
}

fn read_u32(p: &Path) -> io::Result<u32> {
    let s = fs::read_to_string(p)?;
    s.trim().parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {s:?}", p.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_choice() {
        assert_eq!(pick_level(None, 0, 100), 100); // 眠っていた → 最大
        assert_eq!(pick_level(None, 35, 100), 35); // 点いていた → その値
        assert_eq!(pick_level(Some(80), 0, 100), 80);
        assert_eq!(pick_level(Some(0), 0, 100), 1); // 0 は「消す」なので 1 にする
        assert_eq!(pick_level(Some(500), 0, 100), 100);
        assert_eq!(pick_level(None, 0, 0), 1);
    }

    #[test]
    fn ensure_on_restores_zero_and_follows_changes() {
        let dir = std::env::temp_dir().join(format!("zc-backlight-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("max_brightness"), "100\n").unwrap();
        fs::write(dir.join("brightness"), "0\n").unwrap();
        let mut bl = Backlight::open(&dir, None).unwrap();
        assert_eq!(bl.initial(), 0);
        assert!(bl.ensure_on());
        assert_eq!(fs::read_to_string(dir.join("brightness")).unwrap().trim(), "100");
        assert!(!bl.ensure_on()); // 点いているので何もしない
        fs::write(dir.join("brightness"), "25\n").unwrap(); // 誰かが明るさを変えた
        assert!(!bl.ensure_on());
        fs::write(dir.join("brightness"), "0\n").unwrap(); // APPLaunch が眠らせた
        assert!(bl.ensure_on());
        assert_eq!(fs::read_to_string(dir.join("brightness")).unwrap().trim(), "25");
        assert_eq!(bl.restores, 2);
        // 自分で眠る → 打鍵で起こす（消す前の明るさに戻す）
        assert!(bl.sleep());
        assert!(bl.is_asleep());
        assert_eq!(fs::read_to_string(dir.join("brightness")).unwrap().trim(), "0");
        assert!(bl.ensure_on());
        assert!(!bl.is_asleep());
        assert_eq!(fs::read_to_string(dir.join("brightness")).unwrap().trim(), "25");
        assert_eq!(bl.sleeps, 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
