//! 単語補完（design.md §10.2 最初の版）。
//!
//! 開いている文書の識別子らしい単語（英数字と `_`、3 文字以上）の索引を持ち、
//! 前方一致で候補を出す。索引は行ごとの単語の列と、単語ごとの出現数で、編集の通知
//! （[`LineChange`]）で変わった行だけ入れ替える。候補の並びは、カーソルに近い行に
//! 出てくる単語を先に、次に出現数の多いもの。

use crate::{Completion, CompletionKind};
use std::collections::HashMap;
use std::sync::Arc;
use zc_core::{CancelToken, Cancelled, LineChange, LineSource, Pos, VersionCounter};

/// 索引に入れる単語の最小の長さ（文字数）。
pub const MIN_WORD_LEN: usize = 3;
/// 索引に入れる単語の最大の長さ（バイト）。
pub const MAX_WORD_BYTES: usize = 64;

const NOT_MATCHED: u32 = u32::MAX;
const MATCHED: u32 = u32::MAX - 1;

/// 単語の一部になる文字。かな・漢字の連なりは「単語」にならない（補完の役に立たない）。
fn is_word_char(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphanumeric() || c == '_'
    } else {
        c.is_alphanumeric() && !is_cjk(c)
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x9FFF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFF66..=0xFF9F | 0x20000..=0x3FFFF)
}

/// `line` の中の単語を順に渡す（数字で始まるもの・短いもの・長いものは飛ばす）。
fn for_each_word(line: &str, mut f: impl FnMut(&str)) {
    let mut start: Option<usize> = None;
    let mut chars = 0usize;
    let emit = |s: usize, e: usize, n: usize, f: &mut dyn FnMut(&str)| {
        let w = &line[s..e];
        if n >= MIN_WORD_LEN && w.len() <= MAX_WORD_BYTES && !w.as_bytes()[0].is_ascii_digit() {
            f(w);
        }
    };
    for (i, c) in line.char_indices() {
        if is_word_char(c) {
            if start.is_none() {
                start = Some(i);
                chars = 0;
            }
            chars += 1;
        } else if let Some(s) = start.take() {
            emit(s, i, chars, &mut f);
        }
    }
    if let Some(s) = start {
        emit(s, line.len(), chars, &mut f);
    }
}

struct Word {
    text: Arc<str>,
    /// 文書全体での出現数。0 なら空き（再利用できる）。
    count: u32,
}

/// 単語の索引（開いている文書 1 つぶん）。メインスレッドが持つ。
/// `Send` なので、batch で [`WordIndex::build`] して渡せる。
#[derive(Default)]
pub struct WordIndex {
    words: Vec<Word>,
    map: HashMap<Arc<str>, u32>,
    free: Vec<u32>,
    /// 行ごとの単語（`words` の添字）。
    lines: Vec<Vec<u32>>,
}

impl WordIndex {
    pub fn new() -> WordIndex {
        WordIndex::default()
    }

    /// 全体から作り直す。**batch スレッドで呼べる**（256 行ごとに `cancel` を見る）。
    pub fn build(src: &dyn LineSource, cancel: &CancelToken) -> Result<WordIndex, Cancelled> {
        let mut idx = WordIndex::new();
        let n = src.len_lines();
        let mut buf = String::new();
        idx.lines.reserve(n as usize);
        for l in 0..n {
            if l % 256 == 0 {
                cancel.check()?;
            }
            src.line(l, &mut buf);
            let ids = idx.line_words(&buf);
            idx.lines.push(ids);
        }
        Ok(idx)
    }

    fn intern(&mut self, w: &str) -> u32 {
        if let Some(&id) = self.map.get(w) {
            self.words[id as usize].count += 1;
            return id;
        }
        let text: Arc<str> = Arc::from(w);
        let id = if let Some(id) = self.free.pop() {
            self.words[id as usize] = Word { text: text.clone(), count: 1 };
            id
        } else {
            self.words.push(Word { text: text.clone(), count: 1 });
            (self.words.len() - 1) as u32
        };
        self.map.insert(text, id);
        id
    }

    fn line_words(&mut self, line: &str) -> Vec<u32> {
        let mut ids = Vec::new();
        // for_each_word は &mut self を借りられないので、いったん切り出す
        let mut found: Vec<(usize, usize)> = Vec::new();
        let base = line.as_ptr() as usize;
        for_each_word(line, |w| found.push((w.as_ptr() as usize - base, w.len())));
        for (s, l) in found {
            ids.push(self.intern(&line[s..s + l]));
        }
        ids
    }

    fn release(&mut self, id: u32) {
        let w = &mut self.words[id as usize];
        w.count -= 1;
        if w.count == 0 {
            self.map.remove(&w.text);
            self.free.push(id);
        }
    }

    /// 編集を知らせる（変わった行の単語を入れ替える）。`src` は編集後のテキスト。
    /// 行数が合わなくなっていたら（通知の取りこぼし）、全体を作り直す。
    pub fn edited(&mut self, src: &dyn LineSource, change: LineChange) {
        let n = src.len_lines() as usize;
        let old_start = (change.old.start as usize).min(self.lines.len());
        let old_end = (change.old.end as usize).min(self.lines.len()).max(old_start);
        let expected = self.lines.len() - (old_end - old_start) + change.new.len() as usize;
        if expected != n || self.lines.is_empty() {
            self.rebuild(src);
            return;
        }
        for l in old_start..old_end {
            for id in std::mem::take(&mut self.lines[l]) {
                self.release(id);
            }
        }
        let mut buf = String::new();
        let mut new_lines = Vec::with_capacity(change.new.len() as usize);
        for l in change.new.start..change.new.end {
            src.line(l, &mut buf);
            new_lines.push(self.line_words(&buf));
        }
        self.lines.splice(old_start..old_end, new_lines);
    }

    fn rebuild(&mut self, src: &dyn LineSource) {
        let never = VersionCounter::new().token();
        *self = WordIndex::build(src, &never).expect("取り消されない");
    }

    /// 索引にある（出現数が 1 以上の）異なる単語の数。
    pub fn distinct_words(&self) -> usize {
        self.map.len()
    }
    /// 索引している行数。
    pub fn lines(&self) -> usize {
        self.lines.len()
    }
}

/// カーソル位置の単語の先頭部分（補完の対象）。単語の途中でなければ `None`。
/// 数字で始まるものは単語ではないので `None`。
pub fn prefix_at(src: &dyn LineSource, pos: Pos) -> Option<String> {
    let mut line = String::new();
    src.line(pos.line, &mut line);
    let before: Vec<char> = line.chars().take(pos.col as usize).collect();
    let n = before.iter().rev().take_while(|c| is_word_char(**c)).count();
    if n == 0 {
        return None;
    }
    let word: String = before[before.len() - n..].iter().collect();
    if word.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(word)
}

/// `prefix` が `w` の先頭に一致するか。ASCII は大文字小文字を無視する。
fn prefix_matches(w: &str, prefix: &str) -> bool {
    let (mut a, mut b) = (w.chars(), prefix.chars());
    loop {
        match (a.next(), b.next()) {
            (_, None) => return true,
            (None, Some(_)) => return false,
            (Some(x), Some(y)) => {
                if !(x == y || (x.is_ascii() && y.is_ascii() && x.eq_ignore_ascii_case(&y))) {
                    return false;
                }
            }
        }
    }
}

/// 補完候補を出す。`keywords` は言語のキーワード。近い行の単語を先に、最大 `limit` 件。
///
/// 並び: 文書の単語（大文字小文字まで一致するものが先、次に `near` からの行の距離が
/// 近い順、出現数の多い順、辞書順）、そのあとにキーワード。`prefix` と同じ単語は
/// 候補に出さない。キーワードと同じ単語は、キーワードとしてだけ出す。
pub fn complete(index: &WordIndex, keywords: &[&str], prefix: &str, near: Pos, limit: usize) -> Vec<Completion> {
    if prefix.is_empty() || limit == 0 {
        return Vec::new();
    }
    // 1. 前方一致する単語に印を付ける（異なる単語の数だけ。行は見ない）
    let mut dist: Vec<u32> = vec![NOT_MATCHED; index.words.len()];
    let mut matched = 0usize;
    for (id, w) in index.words.iter().enumerate() {
        if w.count > 0 && &*w.text != prefix && prefix_matches(&w.text, prefix) {
            dist[id] = MATCHED;
            matched += 1;
        }
    }
    let mut out: Vec<Completion> = Vec::new();
    if matched > 0 {
        // 2. 行を 1 回なめて、単語ごとにカーソルからいちばん近い行の距離を求める
        for (l, ids) in index.lines.iter().enumerate() {
            let d = (l as i64 - near.line as i64).unsigned_abs().min((MATCHED - 1) as u64) as u32;
            for &id in ids {
                let slot = &mut dist[id as usize];
                if *slot == MATCHED || (*slot != NOT_MATCHED && d < *slot) {
                    *slot = d;
                }
            }
        }
        let mut cands: Vec<(bool, u32, u32, &Arc<str>)> = Vec::with_capacity(matched);
        for (id, w) in index.words.iter().enumerate() {
            if dist[id] != NOT_MATCHED && w.count > 0 && !keywords.contains(&&*w.text) {
                cands.push((!w.text.starts_with(prefix), dist[id], u32::MAX - w.count, &w.text));
            }
        }
        cands.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)).then_with(|| a.3.cmp(b.3)));
        out.extend(
            cands
                .into_iter()
                .take(limit)
                .map(|c| Completion { label: c.3.to_string(), kind: CompletionKind::Word }),
        );
    }
    for k in keywords {
        if out.len() >= limit {
            break;
        }
        if *k != prefix && k.starts_with(prefix) {
            out.push(Completion { label: k.to_string(), kind: CompletionKind::Keyword });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `&[&str]` を `&dyn LineSource` として渡すための包み。
    struct L<'a>(&'a [&'a str]);
    impl LineSource for L<'_> {
        fn len_lines(&self) -> u32 {
            self.0.len_lines()
        }
        fn line(&self, line: u32, out: &mut String) {
            self.0.line(line, out)
        }
    }

    fn index(text: &[&str]) -> WordIndex {
        WordIndex::build(&L(text), &VersionCounter::new().token()).unwrap()
    }
    fn labels(v: &[Completion]) -> Vec<&str> {
        v.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn tokenizes_identifiers_only() {
        let mut got = Vec::new();
        for_each_word("def foo_bar(x1, 12abc): # 日本語 ab café", |w| got.push(w.to_string()));
        assert_eq!(got, ["def", "foo_bar", "café"]);
    }

    #[test]
    fn prefix_at_cursor() {
        let src: &[&str] = &["    result = fo_bar(日本語wor", "123"];
        assert_eq!(prefix_at(&L(src), Pos::new(0, 7)), Some("res".into()));
        assert_eq!(prefix_at(&L(src), Pos::new(0, 4)), None);
        assert_eq!(prefix_at(&L(src), Pos::new(0, 0)), None);
        assert_eq!(prefix_at(&L(src), Pos::new(0, 28)), Some("wor".into()));
        assert_eq!(prefix_at(&L(src), Pos::new(0, 23)), None);
        // 数字で始まるものは単語ではない
        assert_eq!(prefix_at(&L(src), Pos::new(1, 3)), None);
        // 範囲外
        assert_eq!(prefix_at(&L(src), Pos::new(9, 3)), None);
    }

    #[test]
    fn ranks_by_proximity_then_frequency() {
        let src: &[&str] = &[
            "alpha_far alpha_far alpha_far",
            "",
            "",
            "alpha_mid",
            "",
            "alpha_near_a",
            "alpha_near_b alpha_near_b",
            "alpha",
        ];
        let idx = index(src);
        let r = complete(&idx, &[], "alp", Pos::new(7, 3), 10);
        // near は 7 行目（"alpha" は距離 0）。
        assert_eq!(labels(&r), ["alpha", "alpha_near_b", "alpha_near_a", "alpha_mid", "alpha_far"]);
        // 同じ距離なら出現数の多いほうが先（alpha_near_b は 2 回）
        let r = complete(&idx, &[], "alpha_near", Pos::new(6, 0), 10);
        assert_eq!(labels(&r), ["alpha_near_b", "alpha_near_a"]);
        let r = complete(&idx, &[], "alp", Pos::new(7, 3), 2);
        assert_eq!(r.len(), 2);
        assert!(complete(&idx, &[], "", Pos::new(0, 0), 10).is_empty());
        assert!(complete(&idx, &[], "zzz", Pos::new(0, 0), 10).is_empty());
    }

    #[test]
    fn case_handling_and_keywords() {
        let idx = index(&["Print_all print_one", "pretty"]);
        let r = complete(&idx, &["print", "pass", "def"], "pr", Pos::new(0, 0), 10);
        // 大文字小文字まで合うものが先、キーワードは最後
        assert_eq!(labels(&r), ["print_one", "pretty", "Print_all", "print"]);
        assert_eq!(r[3].kind, CompletionKind::Keyword);
        assert_eq!(r[0].kind, CompletionKind::Word);
        // 文書にもあるキーワードは、キーワードとして 1 回だけ
        let idx = index(&["return value", "return other"]);
        let r = complete(&idx, &["return"], "ret", Pos::new(0, 0), 10);
        assert_eq!(labels(&r), ["return"]);
        assert_eq!(r[0].kind, CompletionKind::Keyword);
        // limit はキーワードにも効く
        let r = complete(&idx, &["return", "retry"], "ret", Pos::new(0, 0), 1);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn incremental_update_matches_rebuild() {
        let mut text = vec!["one_word", "two_word", "three_word"];
        let mut idx = index(&text);
        assert_eq!(idx.lines(), 3);
        // 1 行の書き換え
        text[1] = "other_word";
        idx.edited(&L(text.as_slice()), LineChange { old: LineRange::new(1, 2), new: LineRange::new(1, 2) });
        let r = complete(&idx, &[], "tw", Pos::new(0, 0), 10);
        assert!(r.is_empty());
        let r = complete(&idx, &[], "oth", Pos::new(0, 0), 10);
        assert_eq!(labels(&r), ["other_word"]);
        assert!(complete(&idx, &[], "two", Pos::new(0, 0), 10).is_empty());
        // 行の挿入（1 行 → 2 行）
        text.insert(2, "inserted_line");
        idx.edited(&L(text.as_slice()), LineChange { old: LineRange::new(1, 2), new: LineRange::new(1, 3) });
        assert_eq!(idx.lines(), 4);
        assert_eq!(labels(&complete(&idx, &[], "ins", Pos::new(0, 0), 5)), ["inserted_line"]);
        // 行の削除
        text.drain(1..3);
        idx.edited(&L(text.as_slice()), LineChange { old: LineRange::new(1, 4), new: LineRange::new(1, 2) });
        assert_eq!(idx.lines(), 2);
        assert!(complete(&idx, &[], "ins", Pos::new(0, 0), 5).is_empty());
        let fresh = index(&text);
        assert_eq!(idx.distinct_words(), fresh.distinct_words());
        // 通知の取りこぼしは作り直しで直る
        text.push("brand_new");
        idx.edited(&L(text.as_slice()), LineChange { old: LineRange::new(0, 0), new: LineRange::new(0, 0) });
        assert_eq!(labels(&complete(&idx, &[], "bra", Pos::new(0, 0), 5)), ["brand_new"]);
        // 空き番号の再利用で別の単語に化けない
        let mut t2 = vec!["aaa_one", "bbb_two"];
        let mut i2 = index(&t2);
        t2[0] = "ccc_three";
        i2.edited(&L(t2.as_slice()), LineChange { old: LineRange::new(0, 1), new: LineRange::new(0, 1) });
        assert!(complete(&i2, &[], "aaa", Pos::new(0, 0), 5).is_empty());
        assert_eq!(labels(&complete(&i2, &[], "ccc", Pos::new(0, 0), 5)), ["ccc_three"]);
        assert_eq!(labels(&complete(&i2, &[], "bbb", Pos::new(0, 0), 5)), ["bbb_two"]);
    }

    #[test]
    fn build_can_be_cancelled() {
        let text: Vec<&str> = vec!["word_one"; 1000];
        let vc = VersionCounter::new();
        let tok = vc.token();
        vc.bump();
        assert!(WordIndex::build(&L(text.as_slice()), &tok).is_err());
    }

    #[test]
    fn index_is_send() {
        fn send<T: Send>() {}
        send::<WordIndex>();
    }

    use zc_core::LineRange;

    /// 2000 行の文書での補完の所要時間（`cargo test -p zc-intel --release -- --nocapture bench`）。
    #[test]
    fn bench_2000_lines() {
        let mut lines: Vec<String> = Vec::new();
        for i in 0..2000 {
            lines.push(format!(
                "    result_{} = self.compute_value_{}(argument_{}, other_{}) + helper_function_{}(index)  # コメント {}",
                i % 971,
                i % 53,
                i % 31,
                i % 1531,
                i % 17,
                i
            ));
        }
        let src: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let tok = VersionCounter::new().token();
        let t = std::time::Instant::now();
        let idx = WordIndex::build(&L(src.as_slice()), &tok).unwrap();
        let build = t.elapsed();
        let kw = ["def", "class", "return", "import", "if", "else", "for", "while", "self", "None"];
        let t = std::time::Instant::now();
        let n = 200;
        let mut total = 0;
        for i in 0..n {
            let r = complete(&idx, &kw, if i % 2 == 0 { "res" } else { "comp" }, Pos::new(1000, 5), 8);
            total += r.len();
        }
        let comp = t.elapsed() / n;
        let t = std::time::Instant::now();
        let mut text = src.clone();
        let edited = "    result_1 = brand_new_name(x)";
        text[1000] = edited;
        let mut idx2 = idx;
        for _ in 0..100 {
            idx2.edited(&L(text.as_slice()), LineChange { old: LineRange::new(1000, 1001), new: LineRange::new(1000, 1001) });
        }
        let edit = t.elapsed() / 100;
        eprintln!(
            "[bench] 2000 行: build {:?}, distinct {}, complete {:?}/回 (候補平均 {}), edited {:?}/回",
            build,
            idx2.distinct_words(),
            comp,
            total / n as usize,
            edit
        );
        assert!(comp.as_millis() < 50);
    }
}
