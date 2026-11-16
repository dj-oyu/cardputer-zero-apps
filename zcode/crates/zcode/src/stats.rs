//! 実行中の計測値。終了時に標準エラーへ出す（`--stats`）。段階 1 の実機計測と、
//! 以後の「打鍵遅延が目標内か」の確認に使う。

use std::collections::VecDeque;
use std::fmt::Write as _;
use zc_core::time::now_us;
use zc_display::FlushRecord;
use zc_font::FontStats;

/// 値の集まり（µs）から中央値などを出す。
#[derive(Clone, Debug, Default)]
pub struct Samples(Vec<u32>);

impl Samples {
    const CAP: usize = 20_000;

    pub fn push(&mut self, v: u64) {
        if self.0.len() < Samples::CAP {
            self.0.push(v.min(u32::MAX as u64) as u32);
        }
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// 分位点（0.0〜1.0）。空なら 0。
    pub fn quantile(&self, q: f64) -> u32 {
        if self.0.is_empty() {
            return 0;
        }
        let mut v = self.0.clone();
        v.sort_unstable();
        v[((v.len() - 1) as f64 * q).round() as usize]
    }
    /// `n=.. p50=.. p95=.. max=..`（µs）。
    pub fn summary(&self) -> String {
        format!(
            "n={} p50={} p95={} max={} us",
            self.len(),
            self.quantile(0.5),
            self.quantile(0.95),
            self.quantile(1.0)
        )
    }
}

/// 1 回の実行の計測値。
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// プロセスが `main` に入った時刻。
    pub t_start_us: u64,
    /// 最初のフレームを flush スレッドに渡し終えた時刻。
    pub first_frame_us: u64,
    /// 最初の `fsync(fb0)` を呼んだ時刻。
    pub first_fsync_us: u64,
    /// 起床 → 描画開始（入力の処理）。
    pub input: Samples,
    /// 描画（部品の `render`）。
    pub render: Samples,
    /// 行差分 + コピー + flush スレッドへの受け渡し。
    pub present: Samples,
    /// 起床 → flush スレッドへ渡し終えるまで（メインの持ち時間の合計）。
    pub wake_to_handoff: Samples,
    /// 渡し終えてから `fsync` を呼ぶまで（flush スレッドの起床 + mmap への書き込み）。
    pub handoff_to_fsync: Samples,
    /// 起床 → `fsync` を呼ぶまで。打鍵の遅延のうち、こちらで決まる部分。
    pub wake_to_fsync: Samples,
    /// `fsync` の所要（続けて呼ぶと止まる。止まるのは flush スレッドだけ）。
    pub fsync: Samples,
    /// 1 フレームで変わった行数。
    pub rows: Samples,
    pub frames: u32,
    pub flush_count: u32,
    pub keys: u32,
    pub key_batches: u32,
    pub syn_dropped: u32,
    /// fb0 の読み戻し: 調べた回数 / 上書きされていた回数 / 違っていた行数の最大。
    pub verify_checks: u32,
    pub verify_mismatch: u32,
    pub verify_max_rows: u16,
    pub fonts: FontStats,
    pub t_end_us: u64,
    pub rss_kb: u32,
    pub rss_peak_kb: u32,
    pub threads: u32,
    /// フレームの通し番号 → (起床時刻, 渡し終えた時刻)。flush の記録と突き合わせる。
    recent: VecDeque<(u64, u64, u64)>,
}

impl Stats {
    pub fn new(t_start_us: u64) -> Stats {
        Stats { t_start_us, ..Stats::default() }
    }

    /// 1 フレームぶん。`wake`: 起床、`t0`: 描画開始、`t1`: 描画終了、`handoff`: 渡し終えた時刻。
    pub fn frame(&mut self, seq: u64, wake: u64, t0: u64, t1: u64, handoff: u64, rows: u16) {
        self.frames += 1;
        self.input.push(t0.saturating_sub(wake));
        self.render.push(t1.saturating_sub(t0));
        self.present.push(handoff.saturating_sub(t1));
        self.wake_to_handoff.push(handoff.saturating_sub(wake));
        self.rows.push(rows as u64);
        if self.recent.len() >= 256 {
            self.recent.pop_front();
        }
        self.recent.push_back((seq, wake, handoff));
    }

    /// flush の記録を取り込む。
    pub fn flushes(&mut self, recs: &[FlushRecord]) {
        for r in recs {
            self.flush_count += 1;
            if self.first_fsync_us == 0 {
                self.first_fsync_us = r.t_fsync_us;
            }
            self.fsync.push(r.t_done_us.saturating_sub(r.t_fsync_us));
            if let Some(&(_, wake, handoff)) = self.recent.iter().find(|&&(s, _, _)| s == r.seq) {
                self.handoff_to_fsync.push(r.t_fsync_us.saturating_sub(handoff));
                self.wake_to_fsync.push(r.t_fsync_us.saturating_sub(wake));
            }
        }
    }

    pub fn verify(&mut self, mismatched_rows: u16) {
        self.verify_checks += 1;
        if mismatched_rows > 0 {
            self.verify_mismatch += 1;
            self.verify_max_rows = self.verify_max_rows.max(mismatched_rows);
        }
    }

    /// 終了時刻を記録する。
    pub fn finish(&mut self) {
        self.t_end_us = now_us();
    }

    /// `/proc/self/status` から RSS などを読む。
    pub fn read_proc(&mut self) {
        let Ok(s) = std::fs::read_to_string("/proc/self/status") else { return };
        let num = |key: &str| -> u32 {
            s.lines()
                .find_map(|l| l.strip_prefix(key))
                .and_then(|v| v.split_whitespace().next())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        };
        self.rss_kb = num("VmRSS:");
        self.rss_peak_kb = num("VmHWM:");
        self.threads = num("Threads:");
    }

    /// 人が読む形の報告。
    pub fn report(&self) -> String {
        let mut o = String::new();
        let ms = |a: u64, b: u64| (a.saturating_sub(b)) as f64 / 1000.0;
        let _ = writeln!(o, "zcode stats");
        let _ = writeln!(
            o,
            "  startup: first frame handed off {:.1} ms, first fsync {:.1} ms after start; ran {:.1} s",
            ms(self.first_frame_us, self.t_start_us),
            ms(self.first_fsync_us, self.t_start_us),
            ms(self.t_end_us, self.t_start_us) / 1000.0
        );
        let _ = writeln!(o, "  frames={} flushes={} keys={} key_batches={} syn_dropped={}", self.frames, self.flush_count, self.keys, self.key_batches, self.syn_dropped);
        let _ = writeln!(o, "  rows/frame       : n={} p50={} max={}", self.rows.len(), self.rows.quantile(0.5), self.rows.quantile(1.0));
        let _ = writeln!(o, "  input handling   : {}", self.input.summary());
        let _ = writeln!(o, "  render           : {}", self.render.summary());
        let _ = writeln!(o, "  diff+handoff     : {}", self.present.summary());
        let _ = writeln!(o, "  wake->handoff    : {}", self.wake_to_handoff.summary());
        let _ = writeln!(o, "  handoff->fsync   : {}", self.handoff_to_fsync.summary());
        let _ = writeln!(o, "  wake->fsync call : {}", self.wake_to_fsync.summary());
        let _ = writeln!(o, "  fsync duration   : {}", self.fsync.summary());
        let _ = writeln!(
            o,
            "  fb readback      : checks={} overwritten={} max_rows={}",
            self.verify_checks, self.verify_mismatch, self.verify_max_rows
        );
        let _ = writeln!(
            o,
            "  fonts            : rasterized={} late={} cached={} cache_bytes={}",
            self.fonts.rasterized, self.fonts.late, self.fonts.cached, self.fonts.cache_bytes
        );
        let _ = writeln!(o, "  memory           : rss={} kB peak={} kB threads={}", self.rss_kb, self.rss_peak_kb, self.threads);
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::RowRange;

    #[test]
    fn quantiles() {
        let mut s = Samples::default();
        assert_eq!(s.quantile(0.5), 0);
        for v in [5, 1, 9, 3, 7] {
            s.push(v);
        }
        assert_eq!((s.quantile(0.0), s.quantile(0.5), s.quantile(1.0)), (1, 5, 9));
    }

    #[test]
    fn frame_and_flush_are_matched_by_seq() {
        let mut st = Stats::new(1000);
        st.frame(1, 2000, 2010, 2300, 2350, 11);
        st.frame(2, 5000, 5005, 5100, 5120, 170);
        let rec = |seq, t| FlushRecord { seq, bounds: RowRange::new(0, 1), rows: 1, t_start_us: t, t_fsync_us: t + 40, t_done_us: t + 100, last: true };
        st.flushes(&[rec(1, 2400), rec(2, 5200), rec(9, 9000)]);
        assert_eq!(st.first_fsync_us, 2440);
        assert_eq!(st.flush_count, 3);
        assert_eq!(st.handoff_to_fsync.0, vec![90, 120]);
        assert_eq!(st.wake_to_fsync.0, vec![440, 240]);
        assert_eq!(st.fsync.quantile(0.5), 60);
        assert_eq!(st.render.quantile(1.0), 290);
        assert!(st.report().contains("frames=2"));
    }
}
