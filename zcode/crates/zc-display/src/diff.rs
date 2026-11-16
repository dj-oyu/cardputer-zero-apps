//! 行差分（design.md §5.1 の 2）。
//!
//! 裏バッファ（`Canvas`）と提示用バッファを行ごとに比べ、違う行だけを提示用
//! バッファにコピーして、変更行の表（[`PendingRows`]）に足す。全画面でも約 0.1ms。

use crate::plan::PendingRows;
use zc_core::{Canvas, Damage, Priority, RowRange};

/// `damage` の範囲の行を比べ、違う行を `front` にコピーして `pending` に印を付ける。
/// 変わっていた行の数を返す。
///
/// `damage` は「部品が描き変えたと申告した範囲」。申告されていない行は比べない
/// （部品が申告を漏らすと画面に出ないので、申告は広めでよい。中身が同じ行は
/// ここで落とされ、転送されない）。
pub fn diff_rows(back: &Canvas, front: &mut [u16], damage: &Damage, pending: &mut PendingRows) -> u16 {
    let w = back.width() as usize;
    debug_assert_eq!(front.len(), back.pixels().len());
    let mut changed = 0;
    // 優先度の高い範囲から処理する（重なった行は先に高い優先度で印が付く）
    let mut items: Vec<(RowRange, Priority)> = damage.iter().collect();
    items.sort_by_key(|&(r, p)| (p, r.start));
    for (r, p) in items {
        for y in r.start..r.end.min(back.height()) {
            let s = y as usize * w;
            let src = back.row(y);
            let dst = &mut front[s..s + w];
            if src != dst {
                dst.copy_from_slice(src);
                pending.mark_row(y, p);
                changed += 1;
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::{Color, Rect};

    #[test]
    fn only_changed_rows_are_copied_and_marked() {
        let mut back = Canvas::with_size(8, 20);
        let mut front = vec![0u16; 8 * 20];
        let mut pending = PendingRows::new(20);
        back.fill_rect(Rect::new(0, 5, 8, 2), Color(1));
        back.fill_rect(Rect::new(3, 12, 1, 1), Color(2));
        // 申告は広め (0..20)。変わった行は 3 行だけ
        let n = diff_rows(&back, &mut front, &Damage::rows(0, 20), &mut pending);
        assert_eq!(n, 3);
        assert_eq!(pending.dirty_rows(), 3);
        assert_eq!(&front[..], back.pixels());
        let g = pending.groups(3);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].bounds, RowRange::new(5, 7));
        assert_eq!(g[1].bounds, RowRange::new(12, 13));
        // もう一度比べても、何も変わっていない
        let mut pending = PendingRows::new(20);
        assert_eq!(diff_rows(&back, &mut front, &Damage::full(), &mut pending), 0);
        assert!(pending.is_empty());
    }

    #[test]
    fn rows_outside_damage_are_not_compared() {
        let mut back = Canvas::with_size(4, 10);
        let mut front = vec![0u16; 40];
        let mut pending = PendingRows::new(10);
        back.clear(Color(5));
        assert_eq!(diff_rows(&back, &mut front, &Damage::rows(2, 4), &mut pending), 2);
        assert_eq!(front[0], 0);
        assert_eq!(front[2 * 4], 5);
    }

    #[test]
    fn priority_comes_from_damage() {
        let mut back = Canvas::with_size(4, 30);
        let mut front = vec![0u16; 120];
        let mut pending = PendingRows::new(30);
        back.clear(Color(5));
        let d = Damage::rows(0, 30) | Damage::rows(10, 12).with_priority(Priority::Cursor);
        diff_rows(&back, &mut front, &d, &mut pending);
        let g = pending.take_next(0).unwrap();
        assert_eq!(g.priority, Priority::Cursor);
        assert_eq!(g.bounds, RowRange::new(0, 30));
    }
}
