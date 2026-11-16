//! evdev のイベント列 → `KeyEvent`。デバイスに依存しないロジック。

use crate::keycodes::{EV_KEY, KEY_CNT};
use crate::keymap::Keymap;
use crate::repeat::{RepeatClass, RepeatConfig, Repeater};
use zc_core::{Key, KeyEvent, Mods};

/// evdev の 1 イベント（`struct input_event` の中身）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawEvent {
    /// カーネルが付けた時刻（µs）。`Keyboard` は CLOCK_MONOTONIC に切り替える。
    pub time_us: u64,
    /// `EV_KEY` など。
    pub kind: u16,
    pub code: u16,
    /// キーなら 0 = 離上、1 = 押下、2 = カーネルのリピート。
    pub value: i32,
}

/// キーごとのリピートの区分（design.md §6.2 の対象）。
pub fn repeat_class(ev: &KeyEvent) -> RepeatClass {
    if ev.mods.ctrl() || ev.mods.alt() {
        return RepeatClass::None; // Ctrl+S の連打を防ぐ
    }
    match ev.key {
        Key::Char(_) | Key::Backspace | Key::Delete | Key::Left | Key::Right => RepeatClass::Normal,
        Key::Up | Key::Down | Key::PageUp | Key::PageDown => RepeatClass::Scroll,
        _ => RepeatClass::None,
    }
}

/// 押下状態・モディファイア・リピートを追い、`KeyEvent` を作る。
#[derive(Clone, Debug)]
pub struct Translator {
    keymap: Keymap,
    down: [u64; KEY_CNT / 64],
    mods: Mods,
    repeater: Repeater,
}

impl Translator {
    pub fn new(keymap: Keymap, repeat: RepeatConfig) -> Translator {
        Translator { keymap, down: [0; KEY_CNT / 64], mods: Mods::NONE, repeater: Repeater::new(repeat) }
    }
    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }
    /// いま押されているモディファイア（こちらで追っている状態）。
    pub fn mods(&self) -> Mods {
        self.mods
    }
    pub fn is_down(&self, code: u16) -> bool {
        (code as usize) < KEY_CNT && self.down[code as usize / 64] >> (code % 64) & 1 == 1
    }
    /// いま押されているキーコード（昇順）。
    pub fn down_codes(&self) -> Vec<u16> {
        (0..KEY_CNT as u16).filter(|&c| self.is_down(c)).collect()
    }
    pub fn repeater(&self) -> &Repeater {
        &self.repeater
    }
    pub fn set_repeat_config(&mut self, cfg: RepeatConfig) {
        self.repeater.set_config(cfg);
    }
    /// リピートを止める（フォーカスが移ったときなど）。
    pub fn cancel_repeat(&mut self) {
        self.repeater.cancel();
    }

    fn set_down(&mut self, code: u16, down: bool) {
        if (code as usize) < KEY_CNT {
            let bit = 1u64 << (code % 64);
            if down {
                self.down[code as usize / 64] |= bit;
            } else {
                self.down[code as usize / 64] &= !bit;
            }
        }
    }

    fn update_mod(&mut self, m: Mods, down: bool) {
        self.mods = if down { self.mods | m } else { self.mods.without(m) };
        // モディファイアが変わったらリピートは止める（押しっぱなしの a に Ctrl を
        // 足して Ctrl+A が連発される、といったことを防ぐ）
        self.repeater.cancel();
    }

    /// evdev のイベントを 1 つ処理する。打鍵になれば返す。
    /// `EV_KEY` 以外は無視する（`SYN_DROPPED` の扱いは `Keyboard` 側）。
    pub fn feed(&mut self, ev: &RawEvent) -> Option<KeyEvent> {
        if ev.kind != EV_KEY {
            return None;
        }
        match ev.value {
            1 => {
                self.set_down(ev.code, true);
                if let Some(m) = self.keymap.modifier(ev.code) {
                    self.update_mod(m, true);
                    return None;
                }
                let key = self.keymap.translate(ev.code, self.mods);
                self.repeater.press(ev.code, repeat_class(&key), ev.time_us);
                Some(key)
            }
            0 => {
                self.set_down(ev.code, false);
                if let Some(m) = self.keymap.modifier(ev.code) {
                    self.update_mod(m, false);
                } else {
                    self.repeater.release(ev.code);
                }
                None
            }
            _ => None, // カーネルのリピート (2) は使わない
        }
    }

    /// 次にリピートを出す時刻（timerfd に仕掛ける）。
    pub fn next_deadline(&self) -> Option<u64> {
        self.repeater.next_deadline()
    }

    /// 時刻 `now_us` になった。リピートの打鍵があれば返す（`repeat = true`）。
    pub fn tick(&mut self, now_us: u64) -> Option<KeyEvent> {
        let (code, _) = self.repeater.fire(now_us)?;
        let mut key = self.keymap.translate(code, self.mods);
        key.repeat = true;
        Some(key)
    }

    /// 押下状態を取り直す（`SYN_DROPPED` のあと、起動時、grab の直後）。
    /// `bits` は `EVIOCGKEY` の結果（ビット i がキーコード i）。
    ///
    /// 取りこぼした押下・離上の打鍵は作らない（押下状態とモディファイアだけ合わせる。
    /// 勝手に文字が入るより、1 打鍵落ちる方がまし）。リピートは止める。
    pub fn resync(&mut self, bits: &[u8]) {
        self.repeater.cancel();
        self.down = [0; KEY_CNT / 64];
        self.mods = Mods::NONE;
        for code in 0..(bits.len() * 8).min(KEY_CNT) as u16 {
            if bits[code as usize / 8] >> (code % 8) & 1 == 1 {
                self.set_down(code, true);
                if let Some(m) = self.keymap.modifier(code) {
                    self.mods |= m;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1000;

    fn tr() -> Translator {
        Translator::new(Keymap::builtin(), RepeatConfig::default())
    }
    fn key(code: u16, value: i32, t_ms: u64) -> RawEvent {
        RawEvent { time_us: t_ms * MS, kind: EV_KEY, code, value }
    }

    #[test]
    fn press_release_and_shift() {
        let mut t = tr();
        let e = t.feed(&key(30, 1, 0)).unwrap();
        assert_eq!((e.key, e.text, e.repeat), (Key::Char('a'), Some('a'), false));
        assert!(t.is_down(30));
        assert_eq!(t.feed(&key(30, 0, 50)), None);
        assert!(!t.is_down(30));
        // Shift を押している間は大文字
        assert_eq!(t.feed(&key(42, 1, 100)), None);
        assert_eq!(t.mods(), Mods::SHIFT);
        assert_eq!(t.feed(&key(30, 1, 110)).unwrap().text, Some('A'));
        t.feed(&key(30, 0, 120));
        t.feed(&key(42, 0, 130));
        assert_eq!(t.mods(), Mods::NONE);
        assert_eq!(t.down_codes(), Vec::<u16>::new());
    }

    #[test]
    fn repeat_through_tick() {
        let mut t = tr();
        t.feed(&key(14, 1, 0)); // BackSpace
        assert_eq!(t.next_deadline(), Some(400 * MS));
        assert_eq!(t.tick(399 * MS), None);
        let e = t.tick(400 * MS).unwrap();
        assert_eq!((e.key, e.repeat), (Key::Backspace, true));
        t.feed(&key(14, 0, 420));
        assert_eq!(t.next_deadline(), None);
        // PgDn は 20 回/秒
        t.feed(&key(109, 1, 1000));
        t.tick(1400 * MS).unwrap();
        assert_eq!(t.next_deadline(), Some(1450 * MS));
    }

    #[test]
    fn ctrl_combo_does_not_repeat() {
        let mut t = tr();
        t.feed(&key(29, 1, 0));
        let e = t.feed(&key(31, 1, 10)).unwrap();
        assert_eq!((e.key, e.mods, e.text), (Key::Char('s'), Mods::CTRL, None));
        assert_eq!(t.next_deadline(), None);
        assert_eq!(t.tick(5_000 * MS), None);
    }

    #[test]
    fn modifier_change_cancels_repeat() {
        let mut t = tr();
        t.feed(&key(30, 1, 0));
        assert!(t.next_deadline().is_some());
        t.feed(&key(29, 1, 100)); // 押しっぱなしの a に Ctrl を足す
        assert_eq!(t.next_deadline(), None);
        // Enter・Esc・F キーはリピートしない
        for code in [28, 1, 63] {
            let mut t = tr();
            t.feed(&key(code, 1, 0));
            assert_eq!(t.next_deadline(), None, "code {code}");
        }
    }

    #[test]
    fn unknown_and_provisional_codes() {
        let mut t = tr();
        assert_eq!(t.feed(&key(600, 1, 0)).unwrap().key, Key::Unknown(600));
        let e = t.feed(&key(78, 1, 10)).unwrap(); // Sym 中の Shift の位置: 刻印なし
        assert_eq!((e.key, e.text), (Key::Unknown(78), None));
        t.feed(&key(78, 0, 15));
        let e = t.feed(&key(71, 1, 16)).unwrap(); // KEY_KP7 の枠: Sym+R の `-`
        assert_eq!((e.key, e.text), (Key::Char('-'), Some('-')));
        assert_eq!(t.feed(&key(30, 2, 20)), None); // カーネルのリピートは無視
        assert_eq!(t.feed(&RawEvent { time_us: 0, kind: 4, code: 4, value: 0x21 }), None);
    }

    #[test]
    fn resync_after_syn_dropped() {
        let mut t = tr();
        t.feed(&key(30, 1, 0)); // a を押したまま、離上を取りこぼした
        t.feed(&key(29, 1, 0));
        // 取り直すと: Shift (42) だけが押されている
        let mut bits = [0u8; KEY_CNT / 8];
        bits[42 / 8] |= 1 << (42 % 8);
        t.resync(&bits);
        assert_eq!(t.down_codes(), vec![42]);
        assert_eq!(t.mods(), Mods::SHIFT);
        assert_eq!(t.next_deadline(), None);
        assert_eq!(t.tick(10_000 * MS), None);
    }
}
