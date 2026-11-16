//! 診断の置き場と、届くまでの編集に合わせたずらし（docs/stage3-design.md §9）。
//!
//! 公開の形は段階 3 の設計の雛形のまま（変えるときは統合側に戻す）。
//!
//! - [`ShiftLog`]: タブ（文書）ごとに 1 つ。編集のたびに行の増減を記録する。上限つき。
//!   ビルド係の結果は数秒遅れて届くので、依頼時の版から今までの変化をここから引いて、
//!   診断の行をずらしてから付ける
//! - [`DiagStore`]: プロジェクトに 1 つ。出どころ × ファイルごとに診断を持つ。置き換えは
//!   出どころごと。開いていないファイルの分も持つ（開いたときに付ける。ツリーの赤い印・
//!   問題一覧にも使う）
//!
//! どちらも I/O を持たず、メインスレッドで使う（1 回の呼び出しは µs 単位に収める）。
//!
//! # 1 つの編集で診断がどう動くか
//!
//! [`ShiftLog::shift`] と [`DiagStore::shift_file`] は同じ規則を使う。だから「依頼時の位置で
//! 取り込んで、編集のたびに `shift_file`」と「あとから `shift` でまとめてずらして取り込む」は
//! 同じ結果になる。
//!
//! - 変わった範囲より後ろの診断: 行数の増減だけ動く。桁はそのまま
//! - 変わった範囲の中の診断: 範囲の中に残す（範囲が縮んだら、残った最後の行へ）。
//!   その行は書き換わったので、**桁と範囲の終わりは捨てる**
//! - 変わった範囲より前から始まる診断: 動かない。範囲の終わりが変わった所に掛かっていたら
//!   終わりだけ捨てる（後ろにあれば終わりの行を動かす）
//!
//! # 手間と大きさ
//!
//! - `ShiftLog::record`: O(1)。メモリは 1 つの文書で最大 [`SHIFT_LOG_CAP`] 個 × 24 バイト
//!   （約 6KB）。同じ行を打ち続けるあいだは記録が増えない
//! - `ShiftLog::shift`: O(診断の数 × 依頼のあとの記録の数)。割り当てなし
//! - `DiagStore::shift_file`: O(パスの検索 + そのファイルの診断の数)。割り当てなし
//! - `DiagStore::apply`: O(結果の中の診断の数)。`complete` のときは、持っているファイルを
//!   1 回なめる

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use zc_core::{DiagSource, Diagnostic, DiagnosticSet, FileDiagnostics, LineChange, Severity, Version};

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

/// 行に増減を足す（0 より前・`u32` の外には出さない）。
fn add_lines(line: u32, delta: i64) -> u32 {
    (line as i64 + delta).clamp(0, u32::MAX as i64) as u32
}

/// 診断 1 件を、編集 1 つぶん動かす（規則はファイルの先頭）。
fn shift_one(d: &mut Diagnostic, c: LineChange) {
    let delta = c.delta();
    if d.line >= c.old.end {
        // 変わった範囲より後ろ。範囲の終わりも一緒に動く
        if delta != 0 {
            d.line = add_lines(d.line, delta);
            if let Some(end) = d.end.as_mut() {
                end.0 = add_lines(end.0, delta);
            }
        }
    } else if d.line >= c.old.start {
        // 変わった範囲の中。行は範囲の中に残し、桁は信用しない
        d.col = None;
        d.end = None;
        if d.line >= c.new.end {
            d.line = c.new.end.saturating_sub(1).max(c.new.start);
        }
    } else if let Some((end_line, end_col)) = d.end {
        // 範囲より前から始まっている。終わりだけ見る
        if end_line < c.old.start || (end_line == c.old.start && end_col == 0) {
            // 終わりは変わった所に届いていない
        } else if end_line >= c.old.end {
            d.end = Some((add_lines(end_line, delta), end_col));
        } else {
            d.end = None;
        }
    }
}

/// 1 つの文書の、行の増減の記録（上限 [`SHIFT_LOG_CAP`]）。
///
/// 編集のたびに [`ShiftLog::record`] を呼ぶ（`TextDoc` の変更通知と同じ場所）。記録は
/// 「その編集の**後**の版」と [`LineChange`] の組。
#[derive(Clone, Debug, Default)]
pub struct ShiftLog {
    /// 版の小さい順。同じ版の記録が続いてもよい（1 回の操作で変化が複数出たとき）。
    recs: VecDeque<(Version, LineChange)>,
    /// 捨てた記録のうち、いちばん新しい版。これより前の版を基準にした診断は、正確には
    /// ずらせない。何も捨てていなければ `None`。
    lost: Option<Version>,
    /// 今ある記録の最初の編集の、前の版（[`ShiftLog::oldest`] 用。何も捨てていない間だけ使う）。
    origin: Option<Version>,
}

impl ShiftLog {
    pub fn new() -> ShiftLog {
        ShiftLog::default()
    }

    /// 編集を 1 つ記録する。`after` はその編集を反映した後の版。
    pub fn record(&mut self, after: Version, change: LineChange) {
        if let Some(last) = self.recs.back_mut() {
            if after < last.0 {
                // 版が戻った（起きないはず）。順番を信用できないので、ここまでの分は
                // 「あふれた」のと同じ扱いにして捨てる
                self.lost = Some(last.0);
                self.recs.clear();
            } else if last.1 == change && change.delta() == 0 {
                // 同じ行（範囲）を、行数を変えずにもう一度書き換えた。2 回掛けても結果は
                // 同じなので、記録を増やさず版だけ進める。同じ行を打ち続けても
                // 記録があふれない
                last.0 = after;
                return;
            }
        }
        if self.recs.len() >= SHIFT_LOG_CAP {
            if let Some((v, _)) = self.recs.pop_front() {
                self.lost = Some(self.lost.map_or(v, |l| l.max(v)));
            }
        }
        if self.recs.is_empty() {
            self.origin = Some(Version(after.0.saturating_sub(1)));
        }
        self.recs.push_back((after, change));
    }

    /// いちばん古い記録より前の版（これより前を基準にした診断は正確にずらせない）。
    /// 記録が空なら `None`。
    pub fn oldest(&self) -> Option<Version> {
        if self.recs.is_empty() {
            return None;
        }
        self.lost.or(self.origin)
    }

    /// `base` の版の中身に対する診断を、今の中身の位置へずらす。
    ///
    /// - 変わった範囲の中にあった診断は、範囲の中に残す（`zcode` の今の
    ///   `shift_diagnostics` と同じ規則）。その行が編集されたら桁と範囲の終わりは捨てる
    /// - 行が消えて、ずらし先がファイルの外になる診断は最後の行へ寄せる（`len_lines`）
    pub fn shift(&self, base: Version, len_lines: u32, diags: &mut [Diagnostic]) -> ShiftOutcome {
        let exact = self.lost.is_none_or(|lost| base >= lost);
        // `base` より後の記録だけを掛ける（版の順に並んでいるので二分探索）
        let from = self.recs.partition_point(|(v, _)| *v <= base);
        let last_line = len_lines.saturating_sub(1);
        for d in diags.iter_mut() {
            if !exact {
                d.col = None;
                d.end = None;
            }
            for (_, change) in self.recs.range(from..) {
                shift_one(d, *change);
            }
            if d.line > last_line {
                d.line = last_line;
                d.col = None;
                d.end = None;
            } else if d.end.is_some_and(|(end_line, _)| end_line > last_line) {
                d.end = None;
            }
        }
        if exact {
            ShiftOutcome::Exact
        } else {
            ShiftOutcome::Approximate
        }
    }

    /// 文書を閉じた・読み直したときに空にする。
    ///
    /// 空にする前の編集をまたぐ結果（`base` が最後の記録より前）は、このあと
    /// [`ShiftOutcome::Approximate`] になる。
    pub fn clear(&mut self) {
        if let Some(&(last, _)) = self.recs.back() {
            self.lost = Some(self.lost.map_or(last, |l| l.max(last)));
        }
        self.recs.clear();
        self.origin = None;
    }
}

/// 件数（ステータスバーの `⊗ n`、タブ名・ツリーの赤い印）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiagCounts {
    pub errors: u32,
    pub warnings: u32,
}

impl DiagCounts {
    fn add(&mut self, o: DiagCounts) {
        self.errors += o.errors;
        self.warnings += o.warnings;
    }
    fn sub(&mut self, o: DiagCounts) {
        self.errors -= o.errors;
        self.warnings -= o.warnings;
    }
}

/// 出どころの数と、[`FileEntry::slots`] の中の位置。
const SOURCES: usize = 2;
const REMOTE: usize = 0;
const SYNTAX: usize = 1;

fn slot_of(source: DiagSource) -> usize {
    match source {
        DiagSource::Remote => REMOTE,
        DiagSource::Syntax => SYNTAX,
    }
}

/// 1 ファイルぶん。どちらの出どころも空になったら、表から消す。
#[derive(Debug, Default)]
struct FileEntry {
    /// 出どころごとの診断（行の順）。
    slots: [Vec<Diagnostic>; SOURCES],
    /// 全部の出どころを合わせた件数（重なりは 1 つに数える。[`DiagStore::file`] と同じ）。
    counts: DiagCounts,
    /// 最後にこのファイルを含んでいた [`DiagStore::apply`] の通し番号。
    seen: u64,
}

impl FileEntry {
    fn is_empty(&self) -> bool {
        self.slots.iter().all(|s| s.is_empty())
    }
    /// 件数を数え直す。
    fn recount(&mut self) -> DiagCounts {
        let mut c = DiagCounts::default();
        merged(&self.slots, |d| match d.severity {
            Severity::Error => c.errors += 1,
            Severity::Warning => c.warnings += 1,
        });
        self.counts = c;
        c
    }
}

/// 全部の出どころを合わせて、行の順に `f` へ渡す。同じ行ではビルド係の分が先。
/// 構文チェックの診断で、同じ行に同じメッセージのビルド係の診断があるものは飛ばす。
fn merged<'a>(slots: &'a [Vec<Diagnostic>; SOURCES], mut f: impl FnMut(&'a Diagnostic)) {
    let (remote, syntax) = (&slots[REMOTE], &slots[SYNTAX]);
    let mut ri = 0;
    for d in syntax {
        while ri < remote.len() && remote[ri].line <= d.line {
            f(&remote[ri]);
            ri += 1;
        }
        // 同じ行のビルド係の診断は、渡し終えた分の末尾に固まっている
        let dup = remote[..ri].iter().rev().take_while(|r| r.line == d.line).any(|r| r.message == d.message);
        if !dup {
            f(d);
        }
    }
    for r in &remote[ri..] {
        f(r);
    }
}

/// 行の順に並んでいなければ並べ直す（同じ行の中の順は変えない）。
fn sort_by_line(diags: &mut [Diagnostic]) {
    if diags.windows(2).any(|w| w[0].line > w[1].line) {
        diags.sort_by_key(|d| d.line);
    }
}

/// プロジェクト全体の診断の置き場。
///
/// 鍵は（出どころ, パス）。パスは [`zc_core::FileDiagnostics::path`] の約束のとおり
/// （プロジェクト相対・`/` 区切り）。統合側は、タブの絶対パスを相対に直してから引く。
#[derive(Debug, Default)]
pub struct DiagStore {
    /// パスの順。診断が 1 件も無いファイルは入れない。
    files: BTreeMap<Arc<Path>, FileEntry>,
    /// 全ファイルの件数の合計。
    total: DiagCounts,
    /// [`DiagStore::apply`] の通し番号。
    gen: u64,
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
    ///
    /// 1 つの結果に同じパスが 2 回出てきたら、2 回目は置き換えずに足す。
    pub fn apply(&mut self, set: DiagnosticSet) -> Vec<Arc<Path>> {
        let slot = slot_of(set.source);
        self.gen += 1;
        let gen = self.gen;
        let mut changed: Vec<Arc<Path>> = Vec::new();

        for FileDiagnostics { path, mut diags, .. } in set.files {
            sort_by_line(&mut diags);
            let Some(e) = self.files.get_mut(&*path) else {
                if !diags.is_empty() {
                    let mut e = FileEntry { seen: gen, ..FileEntry::default() };
                    e.slots[slot] = diags;
                    self.total.add(e.recount());
                    self.files.insert(path.clone(), e);
                    if !changed.contains(&path) {
                        changed.push(path);
                    }
                }
                continue;
            };
            let again = e.seen == gen;
            e.seen = gen;
            if again {
                if diags.is_empty() {
                    continue;
                }
                e.slots[slot].append(&mut diags);
                e.slots[slot].sort_by_key(|d| d.line);
            } else if e.slots[slot] == diags {
                continue;
            } else {
                e.slots[slot] = diags;
            }
            self.total.sub(e.counts);
            self.total.add(e.recount());
            if e.is_empty() {
                self.files.remove(&*path);
            }
            if !(again && changed.contains(&path)) {
                changed.push(path);
            }
        }

        if set.complete {
            let total = &mut self.total;
            self.files.retain(|path, e| {
                if e.seen == gen || e.slots[slot].is_empty() {
                    return true;
                }
                e.slots[slot] = Vec::new();
                total.sub(e.counts);
                total.add(e.recount());
                changed.push(path.clone());
                !e.is_empty()
            });
        }
        changed
    }

    /// 1 ファイルの診断を、全部の出どころを合わせて行の順に返す。同じ行・同じ
    /// メッセージが 2 つの出どころから出ていたら 1 つにする（[`DiagSource::Remote`] を残す）。
    ///
    /// `out` には足す（先に空にしない）。
    pub fn file(&self, path: &Path, out: &mut Vec<Diagnostic>) {
        if let Some(e) = self.files.get(path) {
            out.reserve(e.slots.iter().map(Vec::len).sum());
            merged(&e.slots, |d| out.push(d.clone()));
        }
    }

    /// 編集に合わせて、1 ファイルの全部の出どころの診断をずらす（打鍵ごとに呼ぶ）。
    pub fn shift_file(&mut self, path: &Path, change: LineChange) {
        let Some(e) = self.files.get_mut(path) else { return };
        for s in &mut e.slots {
            for d in s.iter_mut() {
                shift_one(d, change);
            }
        }
        // 出どころが 1 つだけなら件数は変わらない。2 つあるときは、行が重なって
        // 「同じ行・同じメッセージ」になる（または離れる）ことがあるので数え直す
        if e.slots.iter().all(|s| !s.is_empty()) {
            self.total.sub(e.counts);
            self.total.add(e.recount());
        }
    }

    /// 1 ファイルの件数。
    pub fn counts(&self, path: &Path) -> DiagCounts {
        self.files.get(path).map_or(DiagCounts::default(), |e| e.counts)
    }

    /// プロジェクト全体の件数。
    pub fn total(&self) -> DiagCounts {
        self.total
    }

    /// 診断のあるファイル（パスの順）と件数。問題一覧・ツリーの印用。
    pub fn files(&self) -> Vec<(Arc<Path>, DiagCounts)> {
        self.files.iter().map(|(p, e)| (p.clone(), e.counts)).collect()
    }

    /// ある出どころの分を全部消す（プロジェクトを切り替えた、ビルド係の設定を消した）。
    pub fn clear_source(&mut self, source: DiagSource) {
        let slot = slot_of(source);
        let total = &mut self.total;
        self.files.retain(|_, e| {
            if !e.slots[slot].is_empty() {
                e.slots[slot] = Vec::new();
                total.sub(e.counts);
                total.add(e.recount());
            }
            !e.is_empty()
        });
    }

    /// ファイルの名前が変わった・消えたとき、そのファイルの分を消す。
    pub fn forget(&mut self, path: &Path) {
        if let Some(e) = self.files.remove(path) {
            self.total.sub(e.counts);
        }
    }
}

#[cfg(test)]
mod tests {
    // 受け入れ試験（課題 Z1）。docs/stage3-design.md §9.4 の表。
    use super::*;
    use std::collections::HashMap;
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
    fn ch(os: u32, oe: u32, ne: u32) -> LineChange {
        LineChange { old: LineRange::new(os, oe), new: LineRange::new(os, ne) }
    }

    #[test]
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

    // ---- ここから下は、細かい場面と乱数の試験 ----

    #[test]
    fn overflow_only_affects_old_bases() {
        let mut log = ShiftLog::new();
        let n = SHIFT_LOG_CAP as u64 + 10;
        for v in 0..n {
            log.record(Version(v + 1), ch(0, 0, 1));
        }
        assert_eq!(log.recs.len(), SHIFT_LOG_CAP);
        // 捨てたのは版 1〜10 の記録。版 10 以降を基準にした結果は正確にずらせる
        assert_eq!(log.oldest(), Some(Version(10)));
        let mut ds = vec![d(5, Some(4))];
        assert_eq!(log.shift(Version(10), 10_000, &mut ds), ShiftOutcome::Exact);
        assert_eq!((ds[0].line, ds[0].col), (5 + SHIFT_LOG_CAP as u32, Some(4)));
        let mut ds = vec![d(5, Some(4))];
        assert_eq!(log.shift(Version(9), 10_000, &mut ds), ShiftOutcome::Approximate);
        // いちばん新しい版を基準にした結果は動かない
        let mut ds = vec![d(5, Some(4))];
        assert_eq!(log.shift(Version(n), 10_000, &mut ds), ShiftOutcome::Exact);
        assert_eq!((ds[0].line, ds[0].col), (5, Some(4)));
    }

    #[test]
    fn typing_on_one_line_does_not_fill_the_log() {
        let mut log = ShiftLog::new();
        log.record(Version(1), ch(3, 4, 6)); // 3 行目で改行を 2 つ
        for v in 2..2000u64 {
            log.record(Version(v), ch(4, 5, 5)); // 同じ行を打ち続ける
        }
        assert_eq!(log.recs.len(), 2);
        assert_eq!(log.oldest(), Some(Version(0)));
        let mut ds = vec![d(2, Some(1)), d(3, Some(1)), d(7, Some(1))];
        assert_eq!(log.shift(Version(0), 100, &mut ds), ShiftOutcome::Exact);
        assert_eq!(ds.iter().map(|d| (d.line, d.col)).collect::<Vec<_>>(), [(2, Some(1)), (3, None), (9, Some(1))]);
        // 打っている途中の版を基準にした結果: 行は動かず、打っている行の桁だけ捨てる
        let mut ds = vec![d(4, Some(1)), d(9, Some(1))];
        assert_eq!(log.shift(Version(500), 100, &mut ds), ShiftOutcome::Exact);
        assert_eq!(ds.iter().map(|d| (d.line, d.col)).collect::<Vec<_>>(), [(4, None), (9, Some(1))]);
    }

    #[test]
    fn edited_line_loses_column_and_end() {
        let mut log = ShiftLog::new();
        log.record(Version(1), ch(5, 6, 6));
        let mut ds = vec![d(4, Some(3)), d(5, Some(3)), d(6, Some(3))];
        for x in &mut ds {
            x.end = Some((x.line, 9));
        }
        assert_eq!(log.shift(Version(0), 100, &mut ds), ShiftOutcome::Exact);
        assert_eq!(ds.iter().map(|d| (d.line, d.col, d.end)).collect::<Vec<_>>(), [(4, Some(3), Some((4, 9))), (5, None, None), (6, Some(3), Some((6, 9)))]);
    }

    #[test]
    fn multi_line_range_end_follows_or_drops() {
        let range = |line, end| Diagnostic { end: Some(end), ..d(line, Some(1)) };
        let apply = |c: LineChange, mut x: Diagnostic| {
            shift_one(&mut x, c);
            (x.line, x.col, x.end)
        };
        // 範囲の中の行が増えた: 終わりが後ろへ動く
        assert_eq!(apply(ch(12, 13, 15), range(2, (20, 4))), (2, Some(1), Some((22, 4))));
        // 終わりの行が書き換わった: 終わりだけ捨てる
        assert_eq!(apply(ch(20, 21, 21), range(2, (20, 4))), (2, Some(1), None));
        // 終わりが「変わった行の先頭」ちょうど: 届いていないので、そのまま
        assert_eq!(apply(ch(20, 21, 25), range(2, (20, 0))), (2, Some(1), Some((20, 0))));
        // 全体が後ろにある: 始まりも終わりも動く
        assert_eq!(apply(ch(0, 3, 1), range(5, (7, 4))), (3, Some(1), Some((5, 4))));
        // 始まりの行が書き換わった: 桁も終わりも捨てる
        assert_eq!(apply(ch(2, 3, 3), range(2, (7, 4))), (2, None, None));
    }

    #[test]
    fn deleted_tail_is_clamped_to_last_line() {
        let mut log = ShiftLog::new();
        // 10 行のうち、5 行目から後ろを全部消した（残り 5 行）
        log.record(Version(1), ch(5, 10, 5));
        let mut ds = vec![d(4, Some(2)), d(8, Some(2))];
        assert_eq!(log.shift(Version(0), 5, &mut ds), ShiftOutcome::Exact);
        assert_eq!(ds.iter().map(|d| (d.line, d.col)).collect::<Vec<_>>(), [(4, Some(2)), (4, None)]);
        // 記録が無くても、ファイルの外を指す診断は最後の行へ寄せる
        let mut ds = vec![Diagnostic { end: Some((40, 1)), ..d(2, Some(2)) }, d(99, Some(2))];
        assert_eq!(ShiftLog::new().shift(Version(0), 3, &mut ds), ShiftOutcome::Exact);
        assert_eq!(ds.iter().map(|d| (d.line, d.col, d.end)).collect::<Vec<_>>(), [(2, Some(2), None), (2, None, None)]);
    }

    #[test]
    fn clear_and_version_going_back_are_treated_as_overflow() {
        let mut log = ShiftLog::new();
        assert_eq!(log.oldest(), None);
        log.record(Version(5), ch(0, 0, 1));
        assert_eq!(log.oldest(), Some(Version(4)));
        log.clear();
        assert_eq!(log.oldest(), None);
        let mut ds = vec![d(1, Some(1))];
        assert_eq!(log.shift(Version(3), 10, &mut ds), ShiftOutcome::Approximate);
        assert_eq!((ds[0].line, ds[0].col), (1, None));
        let mut ds = vec![d(1, Some(1))];
        assert_eq!(log.shift(Version(5), 10, &mut ds), ShiftOutcome::Exact);
        assert_eq!((ds[0].line, ds[0].col), (1, Some(1)));

        // 版が戻ったら、そこまでの記録は使わない
        let mut log = ShiftLog::new();
        log.record(Version(8), ch(0, 0, 1));
        log.record(Version(3), ch(0, 0, 2));
        assert_eq!(log.recs.len(), 1);
        let mut ds = vec![d(1, Some(1))];
        assert_eq!(log.shift(Version(2), 10, &mut ds), ShiftOutcome::Approximate);
        assert_eq!((ds[0].line, ds[0].col), (3, None));
    }

    #[test]
    fn same_line_same_message_is_reported_once() {
        let mut s = DiagStore::new();
        let p: Arc<Path> = Arc::from(PathBuf::from("a.ts"));
        let mk = |line, sev, msg: &str| Diagnostic { line, col: None, end: None, severity: sev, code: None, message: msg.into() };
        let one = |source, diags| DiagnosticSet { source, complete: true, files: vec![FileDiagnostics { path: p.clone(), base: None, diags }] };
        s.apply(one(DiagSource::Syntax, vec![mk(1, Severity::Error, "x"), mk(4, Severity::Error, "y"), mk(4, Severity::Warning, "z")]));
        s.apply(one(DiagSource::Remote, vec![mk(4, Severity::Error, "q"), mk(4, Severity::Error, "y"), mk(6, Severity::Warning, "w")]));
        let mut out = Vec::new();
        s.file(&p, &mut out);
        assert_eq!(out.iter().map(|d| (d.line, d.message.as_str())).collect::<Vec<_>>(), [(1, "x"), (4, "q"), (4, "y"), (4, "z"), (6, "w")]);
        assert_eq!(s.counts(&p), DiagCounts { errors: 3, warnings: 2 });
        assert_eq!(s.total(), s.counts(&p));
        // 編集で 2 つの出どころの行が重なったら、件数も 1 つに数える
        let mut s = DiagStore::new();
        s.apply(one(DiagSource::Syntax, vec![mk(2, Severity::Error, "m")]));
        s.apply(one(DiagSource::Remote, vec![mk(3, Severity::Error, "m")]));
        assert_eq!(s.total().errors, 2);
        s.shift_file(&p, ch(2, 4, 3));
        assert_eq!(s.total().errors, 1);
        assert_eq!(s.files(), vec![(p.clone(), DiagCounts { errors: 1, warnings: 0 })]);
    }

    #[test]
    fn unopened_files_are_kept_and_listed() {
        let mut s = DiagStore::new();
        let changed = s.apply(set(DiagSource::Remote, true, &[("src/z.rs", &[4]), ("src/a.rs", &[1, 2]), ("b.rs", &[])]));
        assert_eq!(changed.len(), 2);
        // パスの順。診断の無いファイルは出ない
        let names: Vec<_> = s.files().into_iter().map(|(p, c)| (p.to_str().unwrap().to_owned(), c.errors)).collect();
        assert_eq!(names, [("src/a.rs".to_owned(), 2), ("src/z.rs".to_owned(), 1)]);
        // 開いたあとは、編集に合わせてずらせる。ほかのファイルは動かない
        s.shift_file(Path::new("src/a.rs"), ch(0, 1, 3));
        assert_eq!(lines(&s, "src/a.rs"), [3, 4]);
        assert_eq!(lines(&s, "src/z.rs"), [4]);
        // 同じ結果をもう一度取り込んでも「変わった」にならない
        s.shift_file(Path::new("src/a.rs"), ch(0, 3, 1));
        let changed = s.apply(set(DiagSource::Remote, true, &[("src/z.rs", &[4])]));
        assert_eq!(changed.iter().map(|p| p.to_str().unwrap()).collect::<Vec<_>>(), ["src/a.rs"]);
        assert_eq!(s.total().errors, 1);
    }

    #[test]
    fn same_path_twice_in_one_set_is_merged() {
        let mut s = DiagStore::new();
        s.apply(set(DiagSource::Remote, true, &[("a.rs", &[9])]));
        let changed = s.apply(set(DiagSource::Remote, true, &[("a.rs", &[5]), ("b.rs", &[1]), ("a.rs", &[2])]));
        assert_eq!(lines(&s, "a.rs"), [2, 5]);
        assert_eq!(changed.len(), 2);
        assert_eq!(s.total().errors, 3);
    }

    #[test]
    fn clear_source_and_forget() {
        let mut s = DiagStore::new();
        s.apply(set(DiagSource::Syntax, true, &[("a.rs", &[1])]));
        s.apply(set(DiagSource::Remote, true, &[("a.rs", &[2]), ("b.rs", &[3]), ("c.rs", &[4])]));
        s.forget(Path::new("c.rs"));
        s.forget(Path::new("none.rs"));
        assert_eq!(s.total().errors, 3);
        s.clear_source(DiagSource::Remote);
        assert_eq!(lines(&s, "a.rs"), [1]);
        assert_eq!(s.files().len(), 1);
        assert_eq!(s.total().errors, 1);
        s.clear_source(DiagSource::Syntax);
        assert!(s.files().is_empty());
        assert_eq!(s.total(), DiagCounts::default());
    }

    /// 試験用の乱数（xorshift64*）。
    struct Rng(u64);
    impl Rng {
        fn new(seed: u64) -> Rng {
            Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 16
        }
        /// `0..n`。
        fn below(&mut self, n: u32) -> u32 {
            (self.next() % n as u64) as u32
        }
    }

    /// `len` 行の文書への編集を 1 つ作る。最後の行までを丸ごと消す編集は作らない
    /// （診断がファイルの外に出る場面は別の試験で見る）。
    fn random_edit(rng: &mut Rng, len: u32) -> LineChange {
        if rng.below(10) == 0 {
            let k = rng.below(len + 1);
            return ch(k, k, k + 1 + rng.below(3));
        }
        let start = rng.below(len);
        let old = 1 + rng.below((len - start).min(4));
        let mut new = rng.below(4);
        if new == 0 && start + old == len {
            new = 1;
        }
        ch(start, start + old, start + new)
    }

    /// 比べる相手: 行ごとに「その行に付いている診断の番号」を持つ文書。編集は行の列の
    /// 差し替えとして行い、診断は行と一緒に動く（行の番号の計算をしない）。
    struct Model {
        lines: Vec<Vec<usize>>,
        touched: Vec<bool>,
    }
    impl Model {
        fn new(len: u32) -> Model {
            Model { lines: vec![Vec::new(); len as usize], touched: Vec::new() }
        }
        /// `line` 行目に診断を付けて、番号を返す。
        fn attach(&mut self, line: u32) -> usize {
            self.touched.push(false);
            self.lines[line as usize].push(self.touched.len() - 1);
            self.touched.len() - 1
        }
        fn edit(&mut self, c: LineChange) {
            let (os, oe) = (c.old.start as usize, c.old.end as usize);
            let (ns, ne) = (c.new.start as usize, c.new.end as usize);
            let removed: Vec<Vec<usize>> = self.lines.splice(os..oe, std::iter::repeat_with(Vec::new).take(ne - ns)).collect();
            for (i, ids) in removed.into_iter().enumerate() {
                // 書き換わった行の診断は、同じ位置の新しい行へ。行が減ったら残った最後の行へ
                let target = if os + i < ne { os + i } else { ne.saturating_sub(1).max(ns) };
                for id in ids {
                    self.touched[id] = true;
                    self.lines[target].push(id);
                }
            }
        }
        /// 診断の番号 → （今の行, 桁が残っているか）。
        fn positions(&self) -> Vec<(u32, bool)> {
            let mut out = vec![(0, false); self.touched.len()];
            for (line, ids) in self.lines.iter().enumerate() {
                for &id in ids {
                    out[id] = (line as u32, !self.touched[id]);
                }
            }
            out
        }
    }

    /// 編集の列と、各時点の行数（`lens[i]` は `i` 個の編集のあと）。
    fn random_edits(rng: &mut Rng, len0: u32, n: usize) -> (Vec<LineChange>, Vec<u32>) {
        let mut len = len0;
        let (mut edits, mut lens) = (Vec::new(), vec![len0]);
        for _ in 0..n {
            let c = random_edit(rng, len);
            len = len - c.old.len() + c.new.len();
            assert!(len >= 1);
            edits.push(c);
            lens.push(len);
        }
        (edits, lens)
    }

    /// 乱数の編集列: 「依頼時の版の位置を `shift` でずらした結果」が「依頼時に付けて、
    /// 行と一緒に動かした結果」と同じ。
    #[test]
    fn random_edits_shift_matches_reattached() {
        for seed in 0..400u64 {
            let mut rng = Rng::new(seed);
            let n = if seed % 10 == 0 { 300 + rng.below(100) as usize } else { rng.below(60) as usize };
            let len0 = 1 + rng.below(40);
            let (edits, lens) = random_edits(&mut rng, len0, n);
            let mut log = ShiftLog::new();
            for (i, c) in edits.iter().enumerate() {
                log.record(Version(i as u64 + 1), *c);
            }
            for _ in 0..6 {
                // 版 j（j 個の編集のあと）の中身に対する診断
                let j = rng.below(n as u32 + 1) as usize;
                let mut at: Vec<u32> = (0..rng.below(8)).map(|_| rng.below(lens[j])).collect();
                at.sort();
                let mut model = Model::new(lens[j]);
                for &l in &at {
                    model.attach(l);
                }
                for c in &edits[j..] {
                    model.edit(*c);
                }
                let mut ds: Vec<Diagnostic> = at.iter().map(|&l| d(l, Some(3))).collect();
                match log.shift(Version(j as u64), lens[n], &mut ds) {
                    ShiftOutcome::Exact => {
                        let got: Vec<(u32, bool)> = ds.iter().map(|d| (d.line, d.col.is_some())).collect();
                        assert_eq!(got, model.positions(), "seed {seed} j {j}");
                    }
                    ShiftOutcome::Approximate => {
                        assert!(n - j > SHIFT_LOG_CAP, "seed {seed} j {j}");
                        assert!(ds.iter().all(|d| d.col.is_none() && d.line < lens[n]));
                    }
                }
                assert!(ds.windows(2).all(|w| w[0].line <= w[1].line), "行の順が崩れた: seed {seed}");
            }
        }
    }

    /// 乱数の編集列: 取り込んでから編集のたびに `shift_file` した結果も、同じ。
    #[test]
    fn random_edits_shift_file_matches_reattached() {
        for seed in 1000..1200u64 {
            let mut rng = Rng::new(seed);
            let len0 = 1 + rng.below(40);
            let n = rng.below(80) as usize;
            let (edits, _) = random_edits(&mut rng, len0, n);
            let mut model = Model::new(len0);
            let mut by_source: [Vec<Diagnostic>; 2] = Default::default();
            let mut at: Vec<u32> = (0..rng.below(12)).map(|_| rng.below(len0)).collect();
            at.sort();
            for &l in &at {
                let id = model.attach(l);
                let which = rng.below(2) as usize;
                by_source[which].push(Diagnostic { message: format!("{id}"), ..d(l, Some(1)) });
            }
            let p: Arc<Path> = Arc::from(PathBuf::from("a.rs"));
            let mut s = DiagStore::new();
            let [remote, syntax] = by_source;
            s.apply(DiagnosticSet { source: DiagSource::Remote, complete: true, files: vec![FileDiagnostics { path: p.clone(), base: None, diags: remote }] });
            s.apply(DiagnosticSet { source: DiagSource::Syntax, complete: true, files: vec![FileDiagnostics { path: p.clone(), base: None, diags: syntax }] });
            for c in &edits {
                s.shift_file(&p, *c);
                model.edit(*c);
            }
            let mut out = Vec::new();
            s.file(&p, &mut out);
            assert!(out.windows(2).all(|w| w[0].line <= w[1].line));
            let mut got = vec![(0, false); at.len()];
            for x in &out {
                got[x.message.parse::<usize>().unwrap()] = (x.line, x.col.is_some());
            }
            assert_eq!(got, model.positions(), "seed {seed}");
            assert_eq!(s.total().errors as usize, at.len());
        }
    }

    /// 乱数の取り込み: 置き換え・`complete`・重なりの扱い・件数・「変わったファイル」が、
    /// 素朴な表（出どころ × パス → 診断）と同じ。
    #[test]
    fn random_applies_match_simple_table() {
        const PATHS: [&str; 5] = ["a.rs", "b.rs", "src/c.rs", "src/d.rs", "z/e.rs"];
        let merged_ref = |table: &HashMap<(usize, &str), Vec<Diagnostic>>, p: &str| -> Vec<Diagnostic> {
            let empty = Vec::new();
            let remote = table.get(&(REMOTE, p)).unwrap_or(&empty);
            let syntax = table.get(&(SYNTAX, p)).unwrap_or(&empty);
            let mut all: Vec<(u32, usize, Diagnostic)> = remote.iter().map(|d| (d.line, 0, d.clone())).collect();
            all.extend(syntax.iter().filter(|s| !remote.iter().any(|r| r.line == s.line && r.message == s.message)).map(|d| (d.line, 1, d.clone())));
            all.sort_by_key(|x| (x.0, x.1));
            all.into_iter().map(|x| x.2).collect()
        };
        let count = |ds: &[Diagnostic]| DiagCounts {
            errors: ds.iter().filter(|d| d.severity == Severity::Error).count() as u32,
            warnings: ds.iter().filter(|d| d.severity == Severity::Warning).count() as u32,
        };
        for seed in 2000..2100u64 {
            let mut rng = Rng::new(seed);
            let mut s = DiagStore::new();
            let mut table: HashMap<(usize, &str), Vec<Diagnostic>> = HashMap::new();
            for step in 0..60 {
                let before: Vec<Vec<Diagnostic>> = PATHS.iter().map(|p| merged_ref(&table, p)).collect();
                match rng.below(12) {
                    0 => {
                        let p = PATHS[rng.below(5) as usize];
                        s.forget(Path::new(p));
                        table.remove(&(REMOTE, p));
                        table.remove(&(SYNTAX, p));
                    }
                    1 => {
                        let source = if rng.below(2) == 0 { DiagSource::Remote } else { DiagSource::Syntax };
                        s.clear_source(source);
                        table.retain(|k, _| k.0 != slot_of(source));
                    }
                    2 => {
                        let p = PATHS[rng.below(5) as usize];
                        let c = random_edit(&mut rng, 8);
                        s.shift_file(Path::new(p), c);
                        for slot in [REMOTE, SYNTAX] {
                            for d in table.get_mut(&(slot, p)).into_iter().flatten() {
                                shift_one(d, c);
                            }
                        }
                    }
                    _ => {
                        let source = if rng.below(2) == 0 { DiagSource::Remote } else { DiagSource::Syntax };
                        let complete = rng.below(2) == 0;
                        let mut files = Vec::new();
                        for p in PATHS {
                            if rng.below(3) != 0 {
                                continue;
                            }
                            let diags: Vec<Diagnostic> = (0..rng.below(4))
                                .map(|_| Diagnostic {
                                    line: rng.below(6),
                                    col: None,
                                    end: None,
                                    severity: if rng.below(3) == 0 { Severity::Warning } else { Severity::Error },
                                    code: None,
                                    message: format!("m{}", rng.below(3)),
                                })
                                .collect();
                            files.push(FileDiagnostics { path: Arc::from(PathBuf::from(p)), base: None, diags });
                        }
                        // 素朴な表: 出てきたファイルは置き換え、complete なら残りを消す
                        let slot = slot_of(source);
                        if complete {
                            table.retain(|k, _| k.0 != slot);
                        }
                        for f in &files {
                            let p = PATHS.iter().copied().find(|p| Path::new(p) == &*f.path).unwrap();
                            let mut ds = f.diags.clone();
                            ds.sort_by_key(|d| d.line);
                            table.insert((slot, p), ds);
                        }
                        let mut changed: Vec<String> = s.apply(DiagnosticSet { source, complete, files }).iter().map(|p| p.to_str().unwrap().to_owned()).collect();
                        changed.sort();
                        // 「変わった」は、出どころごとの中身が変わったファイル。合わせた結果が
                        // 変わったファイルは必ず含む
                        assert!(changed.windows(2).all(|w| w[0] != w[1]), "seed {seed} step {step}");
                        for (i, p) in PATHS.iter().enumerate() {
                            if before[i] != merged_ref(&table, p) {
                                assert!(changed.iter().any(|c| c == p), "seed {seed} step {step}: {p} が抜けた");
                            }
                        }
                    }
                }
                let mut total = DiagCounts::default();
                let mut listed = Vec::new();
                for p in PATHS {
                    let want = merged_ref(&table, p);
                    let mut got = Vec::new();
                    s.file(Path::new(p), &mut got);
                    assert_eq!(got, want, "seed {seed} step {step} {p}");
                    assert_eq!(s.counts(Path::new(p)), count(&want), "seed {seed} step {step} {p}");
                    total.add(count(&want));
                    let any = [REMOTE, SYNTAX].iter().any(|&slot| table.get(&(slot, p)).is_some_and(|v| !v.is_empty()));
                    if any {
                        listed.push((p.to_owned(), count(&want)));
                    }
                }
                assert_eq!(s.total(), total, "seed {seed} step {step}");
                let files: Vec<(String, DiagCounts)> = s.files().into_iter().map(|(p, c)| (p.to_str().unwrap().to_owned(), c)).collect();
                assert_eq!(files, listed, "seed {seed} step {step}");
            }
        }
    }
}
