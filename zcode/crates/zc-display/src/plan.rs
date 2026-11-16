//! フラッシュ計画（design.md §5.2）。
//!
//! 液晶のドライバは、変更されたページを含む **1 つの行範囲** にまとめて転送する。
//! 離れた 2 か所（編集中の行とステータスバー）を同時に変えると間の行も全部送られ、
//! 転送開始までの待ちも行数に比例して伸びる。そこで flush スレッドは、変更行を
//! 「まとまり」に分け、1 つずつ書いて fsync する:
//!
//! - 間が [`MERGE_GAP_ROWS`] 行未満の変更は 1 つのまとまりにする
//! - それ以上離れていれば別のまとまりにして、fsync を分ける
//! - 優先度の高いまとまり（カーソル行 → 編集範囲 → その他 → ステータスバー）から送る
//! - flush が忙しい間にメインが同じ行を書き換えたら、表の上で併合される
//!   （行ごとの表なので、重なった範囲は自然に 1 つになる）

use zc_core::{Priority, RowRange};

/// これ未満の行数しか離れていない変更は、1 回の fsync にまとめる。
pub const MERGE_GAP_ROWS: u16 = 20;

const CLEAN: u8 = u8::MAX;

/// まだ液晶に送っていない行の表（1 行 1 バイト: 優先度、または「変更なし」）。
#[derive(Clone, Debug)]
pub struct PendingRows {
    rows: Vec<u8>,
    dirty: usize,
}

/// 1 回の fsync で送るまとまり。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlushGroup {
    /// まとまり全体の行範囲（ドライバが転送する範囲の目安）。
    pub bounds: RowRange,
    /// その中で実際に変わっている行の並び（上から順。mmap にはここだけ書く）。
    pub runs: Vec<RowRange>,
    /// まとまりの中でいちばん高い優先度。
    pub priority: Priority,
}

impl FlushGroup {
    /// 実際に書く行数。
    pub fn rows_written(&self) -> u16 {
        self.runs.iter().map(|r| r.len()).sum()
    }
}

impl PendingRows {
    /// `height` 行ぶんの空の表。
    pub fn new(height: u16) -> PendingRows {
        PendingRows { rows: vec![CLEAN; height as usize], dirty: 0 }
    }
    pub fn is_empty(&self) -> bool {
        self.dirty == 0
    }
    /// 送っていない行の数。
    pub fn dirty_rows(&self) -> usize {
        self.dirty
    }
    /// 1 行を「変更あり」にする。すでに変更ありなら、優先度は高い方を残す。
    pub fn mark_row(&mut self, y: u16, p: Priority) {
        let Some(slot) = self.rows.get_mut(y as usize) else { return };
        if *slot == CLEAN {
            self.dirty += 1;
        }
        *slot = (*slot).min(p as u8);
    }
    /// 行範囲を「変更あり」にする。
    pub fn mark(&mut self, r: RowRange, p: Priority) {
        for y in r.start..r.end.min(self.rows.len() as u16) {
            self.mark_row(y, p);
        }
    }

    /// 変更行を、間が `merge_gap` 行未満ならまとめながら、まとまりに分ける（上から順）。
    pub fn groups(&self, merge_gap: u16) -> Vec<FlushGroup> {
        let mut out: Vec<FlushGroup> = Vec::new();
        let n = self.rows.len();
        let mut y = 0;
        while y < n {
            if self.rows[y] == CLEAN {
                y += 1;
                continue;
            }
            let start = y;
            let mut pri = CLEAN;
            while y < n && self.rows[y] != CLEAN {
                pri = pri.min(self.rows[y]);
                y += 1;
            }
            let run = RowRange::new(start as u16, y as u16);
            let p = Priority::from_u8(pri);
            match out.last_mut() {
                Some(g) if run.start - g.bounds.end < merge_gap => {
                    g.bounds.end = run.end;
                    g.runs.push(run);
                    g.priority = g.priority.min(p);
                }
                _ => out.push(FlushGroup { bounds: run, runs: vec![run], priority: p }),
            }
        }
        out
    }

    /// 次に送るまとまりを取り出し、表から消す。優先度が高いものが先、同じなら上が先。
    pub fn take_next(&mut self, merge_gap: u16) -> Option<FlushGroup> {
        let g = self.groups(merge_gap).into_iter().min_by_key(|g| (g.priority, g.bounds.start))?;
        for r in &g.runs {
            for y in r.start..r.end {
                self.rows[y as usize] = CLEAN;
                self.dirty -= 1;
            }
        }
        Some(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rr(a: u16, b: u16) -> RowRange {
        RowRange::new(a, b)
    }

    #[test]
    fn distant_ranges_are_split() {
        // 編集中の行 (13..24) とステータスバー (159..170): 間が 135 行 → 別々に送る
        let mut p = PendingRows::new(170);
        p.mark(rr(159, 170), Priority::Status);
        p.mark(rr(13, 24), Priority::Cursor);
        let g = p.groups(MERGE_GAP_ROWS);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].bounds, rr(13, 24));
        assert_eq!(g[1].bounds, rr(159, 170));
        // カーソル行が先、ステータスバーが後
        let first = p.take_next(MERGE_GAP_ROWS).unwrap();
        assert_eq!((first.bounds, first.priority), (rr(13, 24), Priority::Cursor));
        let second = p.take_next(MERGE_GAP_ROWS).unwrap();
        assert_eq!((second.bounds, second.priority), (rr(159, 170), Priority::Status));
        assert!(p.take_next(MERGE_GAP_ROWS).is_none());
        assert!(p.is_empty());
    }

    #[test]
    fn near_ranges_are_merged_into_one_group() {
        // 間が 19 行 → 1 回にまとめる。書くのは変わった行だけ
        let mut p = PendingRows::new(170);
        p.mark(rr(10, 12), Priority::Normal);
        p.mark(rr(31, 40), Priority::Edit);
        let g = p.take_next(MERGE_GAP_ROWS).unwrap();
        assert_eq!(g.bounds, rr(10, 40));
        assert_eq!(g.runs, vec![rr(10, 12), rr(31, 40)]);
        assert_eq!(g.priority, Priority::Edit);
        assert_eq!(g.rows_written(), 11);
        assert!(p.is_empty());
        // 間がちょうど 20 行 → 分ける
        let mut p = PendingRows::new(170);
        p.mark(rr(10, 12), Priority::Normal);
        p.mark(rr(32, 40), Priority::Normal);
        assert_eq!(p.groups(MERGE_GAP_ROWS).len(), 2);
    }

    #[test]
    fn overlapping_marks_merge_and_keep_best_priority() {
        // flush が忙しい間に同じ行をもう一度書き換えた
        let mut p = PendingRows::new(170);
        p.mark(rr(20, 40), Priority::Normal);
        p.mark(rr(30, 50), Priority::Status);
        p.mark(rr(35, 36), Priority::Cursor);
        assert_eq!(p.dirty_rows(), 30);
        let g = p.take_next(MERGE_GAP_ROWS).unwrap();
        assert_eq!(g.runs, vec![rr(20, 50)]);
        assert_eq!(g.priority, Priority::Cursor);
    }

    #[test]
    fn priority_order_then_top_first() {
        let mut p = PendingRows::new(170);
        p.mark(rr(0, 5), Priority::Normal);
        p.mark(rr(60, 65), Priority::Edit);
        p.mark(rr(120, 125), Priority::Normal);
        p.mark(rr(165, 170), Priority::Status);
        let order: Vec<u16> = std::iter::from_fn(|| p.take_next(MERGE_GAP_ROWS)).map(|g| g.bounds.start).collect();
        assert_eq!(order, vec![60, 0, 120, 165]);
    }

    #[test]
    fn out_of_range_is_ignored() {
        let mut p = PendingRows::new(10);
        p.mark(rr(8, 300), Priority::Normal);
        p.mark_row(500, Priority::Normal);
        assert_eq!(p.dirty_rows(), 2);
        assert_eq!(p.take_next(0).unwrap().bounds, rr(8, 10));
    }
}
