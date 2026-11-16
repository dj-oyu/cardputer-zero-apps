//! 自前のキーリピート（design.md §6.2）。
//!
//! 実機のキーボードはリピートを送ってこない（EV_REP 無し）ので、押下と離上を見て
//! 自分で作る。時刻はすべて引数で受け取る（時計に依存しないのでテストできる）。
//!
//! - 押下から `delay_us`（既定 400ms）後に最初のリピート、以後 `interval_us`
//!   （30 回/秒）ごと。画面がスクロールする操作は `scroll_interval_us`（20 回/秒。
//!   全画面の転送が毎回 約 27ms かかるため。§5.3）
//! - リピートするのは最後に押したキー 1 つだけ
//! - 処理が遅れて間隔を過ぎていたら、溜まった分は出さずに飛ばす（1 回だけ出す）

/// リピートの速さの区分。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepeatClass {
    /// リピートしない（Ctrl の組み合わせ、Enter、Esc、F キーなど）。
    None,
    /// 文字・BackSpace・Delete・左右の矢印: 30 回/秒。
    Normal,
    /// 上下の矢印・PgUp・PgDn（画面がスクロールしうる）: 20 回/秒。
    Scroll,
}

/// リピートの設定（設定ファイルで変えられる値）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepeatConfig {
    /// 押下から最初のリピートまで（µs）。
    pub delay_us: u64,
    /// [`RepeatClass::Normal`] の間隔（µs）。
    pub interval_us: u64,
    /// [`RepeatClass::Scroll`] の間隔（µs）。
    pub scroll_interval_us: u64,
}

impl Default for RepeatConfig {
    fn default() -> Self {
        RepeatConfig { delay_us: 400_000, interval_us: 1_000_000 / 30, scroll_interval_us: 1_000_000 / 20 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Active {
    code: u16,
    interval_us: u64,
    next_at: u64,
    count: u32,
}

/// リピートの状態機械。
#[derive(Clone, Debug)]
pub struct Repeater {
    cfg: RepeatConfig,
    active: Option<Active>,
}

impl Repeater {
    pub fn new(cfg: RepeatConfig) -> Repeater {
        Repeater { cfg, active: None }
    }
    pub fn config(&self) -> RepeatConfig {
        self.cfg
    }
    pub fn set_config(&mut self, cfg: RepeatConfig) {
        self.cfg = cfg;
        self.active = None;
    }

    /// キーが押された。前のリピートは止まり、`class` が `None` でなければ
    /// このキーのリピートを仕掛ける。
    pub fn press(&mut self, code: u16, class: RepeatClass, now_us: u64) {
        let interval_us = match class {
            RepeatClass::None => {
                self.active = None;
                return;
            }
            RepeatClass::Normal => self.cfg.interval_us,
            RepeatClass::Scroll => self.cfg.scroll_interval_us,
        };
        self.active = Some(Active { code, interval_us, next_at: now_us + self.cfg.delay_us, count: 0 });
    }

    /// キーが離された。リピート中のキーなら止める。
    pub fn release(&mut self, code: u16) {
        if self.active.is_some_and(|a| a.code == code) {
            self.active = None;
        }
    }

    /// リピートを止める（モディファイアが変わった、フォーカスが移った、など）。
    pub fn cancel(&mut self) {
        self.active = None;
    }

    /// リピート中のキーコード。
    pub fn active_code(&self) -> Option<u16> {
        self.active.map(|a| a.code)
    }

    /// 次にリピートを出す時刻（timerfd に仕掛ける値）。
    pub fn next_deadline(&self) -> Option<u64> {
        self.active.map(|a| a.next_at)
    }

    /// 時刻 `now_us` になった。リピートを出す時刻を過ぎていれば、そのキーコードと
    /// 「このキーで何回目のリピートか」（1 から）を返す。1 回の呼び出しで出すのは
    /// 1 回だけで、遅れた分は飛ばす。
    pub fn fire(&mut self, now_us: u64) -> Option<(u16, u32)> {
        let a = self.active.as_mut()?;
        if now_us < a.next_at {
            return None;
        }
        a.count += 1;
        a.next_at += a.interval_us;
        if a.next_at <= now_us {
            a.next_at = now_us + a.interval_us;
        }
        Some((a.code, a.count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1000;

    #[test]
    fn delay_then_30_per_second() {
        let mut r = Repeater::new(RepeatConfig::default());
        r.press(30, RepeatClass::Normal, 1_000 * MS);
        assert_eq!(r.next_deadline(), Some(1_400 * MS));
        assert_eq!(r.fire(1_399 * MS), None);
        assert_eq!(r.fire(1_400 * MS), Some((30, 1)));
        // 以後 33.3ms ごと。1 秒間 (1.4s〜2.4s) に 30 回
        let mut n = 0;
        let mut t = 1_400 * MS;
        while t < 2_400 * MS {
            t = r.next_deadline().unwrap();
            if t < 2_400 * MS {
                assert!(r.fire(t).is_some());
                n += 1;
            }
        }
        assert_eq!(n, 30); // 33.333ms ごと: 1 秒に 30 回
    }

    #[test]
    fn scroll_class_is_20_per_second() {
        let mut r = Repeater::new(RepeatConfig::default());
        r.press(109, RepeatClass::Scroll, 0);
        assert_eq!(r.fire(400 * MS), Some((109, 1)));
        assert_eq!(r.next_deadline(), Some(450 * MS));
    }

    #[test]
    fn no_repeat_class_and_release_stop() {
        let mut r = Repeater::new(RepeatConfig::default());
        r.press(30, RepeatClass::Normal, 0);
        r.press(31, RepeatClass::None, 100 * MS); // Ctrl+S など: 前のリピートも止まる
        assert_eq!(r.next_deadline(), None);
        assert_eq!(r.fire(10_000 * MS), None);
        r.press(30, RepeatClass::Normal, 0);
        r.release(31); // 別のキーの離上では止まらない
        assert_eq!(r.active_code(), Some(30));
        r.release(30);
        assert_eq!(r.active_code(), None);
    }

    #[test]
    fn latest_key_wins() {
        let mut r = Repeater::new(RepeatConfig::default());
        r.press(30, RepeatClass::Normal, 0);
        r.press(31, RepeatClass::Normal, 300 * MS);
        assert_eq!(r.fire(400 * MS), None); // a のリピートは出ない
        assert_eq!(r.fire(700 * MS), Some((31, 1)));
    }

    #[test]
    fn falling_behind_skips_instead_of_bursting() {
        let mut r = Repeater::new(RepeatConfig::default());
        r.press(30, RepeatClass::Normal, 0);
        assert_eq!(r.fire(400 * MS), Some((30, 1)));
        // 200ms 止まっていた: 溜まった 6 回分は出さず、1 回だけ
        assert_eq!(r.fire(600 * MS), Some((30, 2)));
        assert_eq!(r.fire(600 * MS), None);
        assert_eq!(r.next_deadline(), Some(600 * MS + 33_333));
    }
}
