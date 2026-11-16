//! 診断の置き場と、届くまでの編集に合わせたずらし（docs/stage3-design.md §9）。
//!
//! 雛形（段階 3 の設計）。中身は担当（課題 Z1）が書く。公開の形は変えない。
//!
//! - [`ShiftLog`]: タブ（文書）ごとに 1 つ。編集のたびに行の増減を記録する。上限つき。
//!   ビルド係の結果は数秒遅れて届くので、依頼時の版から今までの変化をここから引いて、
//!   診断の行をずらしてから付ける
//! - [`DiagStore`]: プロジェクトに 1 つ。出どころ × ファイルごとに診断を持つ。置き換えは
//!   出どころごと。開いていないファイルの分も持つ（開いたときに付ける。ツリーの赤い印・
//!   問題一覧にも使う）
//!
//! どちらも I/O を持たず、メインスレッドで使う（1 回の呼び出しは µs 単位に収める）。

use std::path::Path;
use std::sync::Arc;
use zc_core::{DiagSource, Diagnostic, DiagnosticSet, LineChange, Version};

/// [`ShiftLog`] が覚える変化の数の上限。あふれたら古い方から捨てる。
pub const SHIFT_LOG_CAP: usize = 256;

/// [`ShiftLog::shift`] の結果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShiftOutcome {
    /// 依頼時の版からの変化が全部残っていて、行を正しくずらせた。
    Exact,
    /// 記録があふれていた。行は残っている分だけずらし、**桁と範囲の終わりは捨てた**
    /// （波線は行全体になる）。
    Approximate,
}

/// 1 つの文書の、行の増減の記録（上限 [`SHIFT_LOG_CAP`]）。
///
/// 編集のたびに [`ShiftLog::record`] を呼ぶ（`TextDoc` の変更通知と同じ場所）。記録は
/// 「その編集の**後**の版」と [`LineChange`] の組。
#[derive(Clone, Debug, Default)]
pub struct ShiftLog {
    // 担当が決める（VecDeque<(Version, LineChange)> を想定）。
    _private: (),
}

impl ShiftLog {
    pub fn new() -> ShiftLog {
        ShiftLog::default()
    }

    /// 編集を 1 つ記録する。`after` はその編集を反映した後の版。
    pub fn record(&mut self, after: Version, change: LineChange) {
        let _ = (after, change);
        todo!("Z1")
    }

    /// いちばん古い記録より前の版（これより前を基準にした診断は正確にずらせない）。
    /// 記録が空なら `None`。
    pub fn oldest(&self) -> Option<Version> {
        todo!("Z1")
    }

    /// `base` の版の中身に対する診断を、今の中身の位置へずらす。
    ///
    /// - 変わった範囲の中にあった診断は、範囲の中に残す（`zcode` の今の
    ///   `shift_diagnostics` と同じ規則）。その行が編集されたら桁と範囲の終わりは捨てる
    /// - 行が消えて、ずらし先がファイルの外になる診断は最後の行へ寄せる（`len_lines`）
    pub fn shift(&self, base: Version, len_lines: u32, diags: &mut [Diagnostic]) -> ShiftOutcome {
        let _ = (base, len_lines, diags);
        todo!("Z1")
    }

    /// 文書を閉じた・読み直したときに空にする。
    pub fn clear(&mut self) {
        todo!("Z1")
    }
}

/// 件数（ステータスバーの `⊗ n`、タブ名・ツリーの赤い印）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiagCounts {
    pub errors: u32,
    pub warnings: u32,
}

/// プロジェクト全体の診断の置き場。
///
/// 鍵は（出どころ, パス）。パスは [`zc_core::FileDiagnostics::path`] の約束のとおり
/// （プロジェクト相対・`/` 区切り）。統合側は、タブの絶対パスを相対に直してから引く。
#[derive(Debug, Default)]
pub struct DiagStore {
    _private: (),
}

impl DiagStore {
    pub fn new() -> DiagStore {
        DiagStore::default()
    }

    /// 結果を 1 つ取り込む。
    ///
    /// - `set.files` にあるファイルは、その出どころの分を丸ごと置き換える
    /// - `set.complete` のときだけ、同じ出どころで今回出てこなかったファイルの分を消す
    /// - 位置のずらしは呼ぶ側の仕事: 開いているファイルの分は、取り込む**前に**
    ///   [`ShiftLog::shift`] を掛けておく（`base` が `None` ならそのまま）
    ///
    /// 戻り値は、中身が変わったファイルのパス（波線・タブ名・ツリーを描き直す範囲）。
    pub fn apply(&mut self, set: DiagnosticSet) -> Vec<Arc<Path>> {
        let _ = set;
        todo!("Z1")
    }

    /// 1 ファイルの診断を、全部の出どころを合わせて行の順に返す。同じ行・同じ
    /// メッセージが 2 つの出どころから出ていたら 1 つにする（[`DiagSource::Remote`] を残す）。
    pub fn file(&self, path: &Path, out: &mut Vec<Diagnostic>) {
        let _ = (path, out);
        todo!("Z1")
    }

    /// 編集に合わせて、1 ファイルの全部の出どころの診断をずらす（打鍵ごとに呼ぶ）。
    pub fn shift_file(&mut self, path: &Path, change: LineChange) {
        let _ = (path, change);
        todo!("Z1")
    }

    /// 1 ファイルの件数。
    pub fn counts(&self, path: &Path) -> DiagCounts {
        let _ = path;
        todo!("Z1")
    }

    /// プロジェクト全体の件数。
    pub fn total(&self) -> DiagCounts {
        todo!("Z1")
    }

    /// 診断のあるファイル（パスの順）と件数。問題一覧・ツリーの印用。
    pub fn files(&self) -> Vec<(Arc<Path>, DiagCounts)> {
        todo!("Z1")
    }

    /// ある出どころの分を全部消す（プロジェクトを切り替えた、ビルド係の設定を消した）。
    pub fn clear_source(&mut self, source: DiagSource) {
        let _ = source;
        todo!("Z1")
    }

    /// ファイルの名前が変わった・消えたとき、そのファイルの分を消す。
    pub fn forget(&mut self, path: &Path) {
        let _ = path;
        todo!("Z1")
    }
}

#[cfg(test)]
mod tests {
    // 受け入れ試験（課題 Z1。`#[ignore]` を外して通す）。docs/stage3-design.md §9.4 の表。
    use super::*;
    use std::path::PathBuf;
    use zc_core::{FileDiagnostics, LineRange, Severity};

    fn d(line: u32, col: Option<u32>) -> Diagnostic {
        Diagnostic { line, col, end: None, severity: Severity::Error, code: None, message: format!("e{line}") }
    }
    fn set(source: DiagSource, complete: bool, files: &[(&str, &[u32])]) -> DiagnosticSet {
        let files = files
            .iter()
            .map(|(p, lines)| FileDiagnostics { path: Arc::from(PathBuf::from(p)), base: None, diags: lines.iter().map(|&l| d(l, Some(2))).collect() })
            .collect();
        DiagnosticSet { source, complete, files }
    }
    fn lines(s: &DiagStore, p: &str) -> Vec<u32> {
        let mut v = Vec::new();
        s.file(Path::new(p), &mut v);
        v.iter().map(|d| d.line).collect()
    }

    #[test]
    #[ignore = "Z1"]
    fn sources_do_not_erase_each_other() {
        let mut s = DiagStore::new();
        s.apply(set(DiagSource::Syntax, true, &[("src/main.rs", &[3])]));
        s.apply(set(DiagSource::Remote, true, &[("src/main.rs", &[7]), ("src/lib.rs", &[1])]));
        assert_eq!(lines(&s, "src/main.rs"), [3, 7]);
        // 構文チェックが空で返っても、ビルド係の分は残る
        s.apply(set(DiagSource::Syntax, true, &[("src/main.rs", &[])]));
        assert_eq!(lines(&s, "src/main.rs"), [7]);
        assert_eq!(s.total(), DiagCounts { errors: 2, warnings: 0 });
    }

    #[test]
    #[ignore = "Z1"]
    fn incomplete_result_keeps_missing_files() {
        let mut s = DiagStore::new();
        s.apply(set(DiagSource::Remote, true, &[("a.rs", &[1]), ("b.rs", &[2])]));
        // 途中で切れた結果: b.rs が出てこなくても消さない
        let changed = s.apply(set(DiagSource::Remote, false, &[("a.rs", &[5])]));
        assert_eq!(changed.len(), 1);
        assert_eq!(lines(&s, "a.rs"), [5]);
        assert_eq!(lines(&s, "b.rs"), [2]);
        // 完全な結果: 出てこなかった b.rs は消える
        s.apply(set(DiagSource::Remote, true, &[("a.rs", &[5])]));
        assert!(lines(&s, "b.rs").is_empty());
    }

    #[test]
    #[ignore = "Z1"]
    fn shift_log_moves_late_results() {
        let mut log = ShiftLog::new();
        // 版 10 で依頼。そのあと 3 行目（0 始まり 2）に 2 行足し（版 11）、先頭の 1 行を消した（版 12）
        log.record(Version(11), LineChange { old: LineRange::new(2, 3), new: LineRange::new(2, 5) });
        log.record(Version(12), LineChange { old: LineRange::new(0, 1), new: LineRange::new(0, 0) });
        let mut ds = vec![d(1, Some(4)), d(9, Some(4))];
        assert_eq!(log.shift(Version(10), 100, &mut ds), ShiftOutcome::Exact);
        assert_eq!(ds.iter().map(|d| (d.line, d.col)).collect::<Vec<_>>(), [(0, Some(4)), (10, Some(4))]);
        // 版 11 を基準にした結果には、版 12 の変化だけを掛ける
        let mut ds = vec![d(9, Some(4))];
        assert_eq!(log.shift(Version(11), 100, &mut ds), ShiftOutcome::Exact);
        assert_eq!(ds[0].line, 8);
    }

    #[test]
    #[ignore = "Z1"]
    fn overflowed_log_drops_columns() {
        let mut log = ShiftLog::new();
        for v in 0..(SHIFT_LOG_CAP as u64 + 10) {
            log.record(Version(v + 1), LineChange { old: LineRange::new(0, 0), new: LineRange::new(0, 1) });
        }
        let mut ds = vec![d(5, Some(4))];
        assert_eq!(log.shift(Version(0), 10_000, &mut ds), ShiftOutcome::Approximate);
        assert_eq!(ds[0].col, None);
        assert_eq!(ds[0].line, 5 + SHIFT_LOG_CAP as u32);
    }
}
