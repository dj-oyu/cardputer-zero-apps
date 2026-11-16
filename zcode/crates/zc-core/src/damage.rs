//! 部品が返す「描き変えた行範囲」。
//!
//! 液晶への転送は行単位で、範囲が小さいほど早く始まる（約 50µs/行）。部品は
//! `render` で自分が描き変えた行範囲だけを返し、`zc-display` はその範囲だけを
//! 提示用バッファと比べる。離れた範囲は別々に送られる（design.md §5.2）。

use crate::geom::{Rect, RowRange};

/// 転送の優先順（小さいほど先）。design.md §5.2 の
/// 「カーソル行 → 編集範囲 → その他 → ステータスバー」。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum Priority {
    /// カーソルのある行（打鍵がいちばん早く見えてほしい所）。
    Cursor = 0,
    /// 編集で変わった範囲。
    Edit = 1,
    /// その他。
    #[default]
    Normal = 2,
    /// ステータスバーなど、遅れてよいもの。
    Status = 3,
}

impl Priority {
    pub const fn from_u8(v: u8) -> Priority {
        match v {
            0 => Priority::Cursor,
            1 => Priority::Edit,
            2 => Priority::Normal,
            _ => Priority::Status,
        }
    }
}

const CAP: usize = 6;

/// 描き変えた行範囲の集まり（優先度つき）。ヒープを使わない小さな値。
///
/// 範囲は [`CAP`] 個まで別々に持つ。あふれたら、いちばん近い 2 つをまとめる
/// （優先度は高い方に合わせる）。重なった範囲も、優先度が同じならまとめる。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Damage {
    n: u8,
    items: [(RowRange, Priority); CAP],
}

impl Damage {
    /// 何も変えていない。
    pub const NONE: Damage = Damage { n: 0, items: [(RowRange::new(0, 0), Priority::Normal); CAP] };

    /// 何も変えていない。
    pub const fn none() -> Damage {
        Damage::NONE
    }
    /// 行範囲 `y0..y1` を変えた（優先度は [`Priority::Normal`]）。
    pub fn rows(y0: u16, y1: u16) -> Damage {
        Damage::range(RowRange::new(y0, y1), Priority::Normal)
    }
    /// 行範囲を優先度つきで。
    pub fn range(r: RowRange, p: Priority) -> Damage {
        let mut d = Damage::NONE;
        d.add(r, p);
        d
    }
    /// 矩形が占める行を変えた。
    pub fn rect(r: Rect) -> Damage {
        Damage::range(r.rows(), Priority::Normal)
    }
    /// 画面全体。
    pub fn full() -> Damage {
        Damage::rect(Rect::SCREEN)
    }
    /// 全部の範囲の優先度を付け替える。
    pub fn with_priority(mut self, p: Priority) -> Damage {
        for it in &mut self.items[..self.n as usize] {
            it.1 = p;
        }
        self.normalize();
        self
    }
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
    /// 持っている範囲（順不同）。
    pub fn iter(&self) -> impl Iterator<Item = (RowRange, Priority)> + '_ {
        self.items[..self.n as usize].iter().copied()
    }
    /// 全部を含む最小の行範囲。
    pub fn bounds(&self) -> RowRange {
        self.iter().fold(RowRange::default(), |a, (r, _)| a.union(r))
    }
    /// ある行がどれかの範囲に入っていれば、その中でいちばん高い優先度。
    pub fn priority_at(&self, y: u16) -> Option<Priority> {
        self.iter().filter(|(r, _)| r.contains(y)).map(|(_, p)| p).min()
    }

    /// 範囲を足す。
    pub fn add(&mut self, r: RowRange, p: Priority) {
        if r.is_empty() {
            return;
        }
        // 同じ優先度で重なる・隣り合うものがあればまとめる
        for i in 0..self.n as usize {
            if self.items[i].1 == p && self.items[i].0.touches(r) {
                self.items[i].0 = self.items[i].0.union(r);
                self.normalize();
                return;
            }
        }
        if (self.n as usize) < CAP {
            self.items[self.n as usize] = (r, p);
            self.n += 1;
            return;
        }
        // あふれた: 新しい範囲も含めて、いちばん近い 2 つをまとめる
        let mut all = [(RowRange::default(), Priority::Normal); CAP + 1];
        all[..CAP].copy_from_slice(&self.items);
        all[CAP] = (r, p);
        let (mut bi, mut bj, mut best) = (0, 1, u16::MAX);
        for i in 0..=CAP {
            for j in i + 1..=CAP {
                let g = all[i].0.gap(all[j].0);
                if g < best {
                    (bi, bj, best) = (i, j, g);
                }
            }
        }
        all[bi] = (all[bi].0.union(all[bj].0), all[bi].1.min(all[bj].1));
        all[bj] = all[CAP];
        self.items.copy_from_slice(&all[..CAP]);
        self.normalize();
    }

    /// もう 1 つの `Damage` を足す。
    pub fn merge(&mut self, o: &Damage) {
        for (r, p) in o.iter() {
            self.add(r, p);
        }
    }
    /// 2 つを合わせたもの。
    pub fn union(mut self, o: Damage) -> Damage {
        self.merge(&o);
        self
    }

    /// 同じ優先度で重なる・隣り合う範囲をまとめ直す。
    fn normalize(&mut self) {
        let mut changed = true;
        while changed {
            changed = false;
            let n = self.n as usize;
            'outer: for i in 0..n {
                for j in i + 1..n {
                    if self.items[i].1 == self.items[j].1 && self.items[i].0.touches(self.items[j].0) {
                        self.items[i].0 = self.items[i].0.union(self.items[j].0);
                        self.items[j] = self.items[n - 1];
                        self.n -= 1;
                        changed = true;
                        break 'outer;
                    }
                }
            }
        }
    }
}

impl core::ops::BitOr for Damage {
    type Output = Damage;
    fn bitor(self, o: Damage) -> Damage {
        self.union(o)
    }
}

impl core::ops::BitOrAssign for Damage {
    fn bitor_assign(&mut self, o: Damage) {
        self.merge(&o);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(d: &Damage) -> Vec<(u16, u16, Priority)> {
        let mut v: Vec<_> = d.iter().map(|(r, p)| (r.start, r.end, p)).collect();
        v.sort();
        v
    }

    #[test]
    fn empty_and_single() {
        assert!(Damage::none().is_empty());
        assert!(Damage::rows(5, 5).is_empty());
        let d = Damage::rows(13, 24);
        assert_eq!(sorted(&d), vec![(13, 24, Priority::Normal)]);
        assert_eq!(d.bounds(), RowRange::new(13, 24));
    }

    #[test]
    fn overlapping_same_priority_merge() {
        let d = Damage::rows(10, 20) | Damage::rows(18, 30) | Damage::rows(30, 31);
        assert_eq!(sorted(&d), vec![(10, 31, Priority::Normal)]);
    }

    #[test]
    fn different_priority_kept_apart() {
        let d = Damage::rows(13, 24).with_priority(Priority::Cursor)
            | Damage::rows(159, 170).with_priority(Priority::Status)
            | Damage::rows(13, 150);
        assert_eq!(d.iter().count(), 3);
        assert_eq!(d.priority_at(15), Some(Priority::Cursor));
        assert_eq!(d.priority_at(100), Some(Priority::Normal));
        assert_eq!(d.priority_at(160), Some(Priority::Status));
        assert_eq!(d.priority_at(155), None);
    }

    #[test]
    fn overflow_merges_nearest() {
        let mut d = Damage::none();
        for i in 0..7u16 {
            d.add(RowRange::new(i * 20, i * 20 + 2), Priority::from_u8((i % 2) as u8 + 1));
        }
        assert_eq!(d.iter().count(), 6);
        // どの行も失われていない
        for i in 0..7u16 {
            assert!(d.priority_at(i * 20).is_some());
        }
        assert_eq!(d.bounds(), RowRange::new(0, 122));
    }
}
