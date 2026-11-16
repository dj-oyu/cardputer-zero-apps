//! 液晶のバックライト（`/sys/class/backlight/*`）。
//!
//! APPLaunch は、キー入力がしばらく無いとバックライトを 0 にして画面を眠らせ、
//! キー入力で起こす。zcode はキーボードを独占（`EVIOCGRAB`）するので、実行中の
//! 打鍵は APPLaunch に届かない。放っておくと、APPLaunch の時間切れで画面が消え、
//! 何を押しても戻らなくなる（2026-11-16、実機 2 号機で起きた）。
//!
//! そこで zcode の実行中は、zcode がバックライトを受け持つ:
//!
//! - 起動時に 0 なら点ける
//! - 打鍵のたびと 1 秒ごとに読み、0 にされていたら点け直す
//! - 終了時は触らない（点けたまま）。独占を放せば打鍵は APPLaunch に届くので、
//!   その先は APPLaunch が自分の状態に合わせて扱う
//!
//! 点ける明るさは、起動時の値（0 でなければ）→ 実行中に見た 0 でない値 →
//! 最大値の 60% の順に決める。`--backlight <値>` で指定もできる。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 既定の明るさ（最大値に対する割合、%）。APPLaunch の保存値の場所が分からないため。
pub const DEFAULT_PERCENT: u32 = 60;

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
        Ok(Backlight { brightness, max, level, initial, restores: 0 })
    }

    /// 起動時の値。
    pub fn initial(&self) -> u32 {
        self.initial
    }

    /// 点けるときの明るさ。
    pub fn level(&self) -> u32 {
        self.level
    }

    /// 消えていたら点ける。点けたら `true`。読めない・書けないときは何もしない
    /// （画面の更新を止めない）。
    pub fn ensure_on(&mut self) -> bool {
        let Ok(cur) = read_u32(&self.brightness) else { return false };
        if cur > 0 {
            // ユーザーや APPLaunch が明るさを変えたら、それに合わせる
            self.level = cur.min(self.max);
            return false;
        }
        if fs::write(&self.brightness, format!("{}\n", self.level)).is_ok() {
            self.restores += 1;
            true
        } else {
            false
        }
    }
}

/// 点ける明るさを決める（純粋な関数。テスト用に分けてある）。
pub fn pick_level(requested: Option<u32>, initial: u32, max: u32) -> u32 {
    let max = max.max(1);
    match requested {
        Some(v) => v.clamp(1, max),
        None if initial > 0 => initial.min(max),
        None => (max * DEFAULT_PERCENT / 100).max(1),
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
        assert_eq!(pick_level(None, 0, 100), 60); // 眠っていた → 既定
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
        assert_eq!(fs::read_to_string(dir.join("brightness")).unwrap().trim(), "60");
        assert!(!bl.ensure_on()); // 点いているので何もしない
        fs::write(dir.join("brightness"), "25\n").unwrap(); // 誰かが明るさを変えた
        assert!(!bl.ensure_on());
        fs::write(dir.join("brightness"), "0\n").unwrap(); // APPLaunch が眠らせた
        assert!(bl.ensure_on());
        assert_eq!(fs::read_to_string(dir.join("brightness")).unwrap().trim(), "25");
        assert_eq!(bl.restores, 2);
        fs::remove_dir_all(&dir).unwrap();
    }
}
