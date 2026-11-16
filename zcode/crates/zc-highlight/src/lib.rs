//! ハイライト（design.md §7）。
//!
//! - 規則は `zcode/languages/<言語>/language.toml`（テキスト。書き方は
//!   `zcode/languages/README.md`）。`build.rs` が文脈ごとに多パターンの DFA へ変換して
//!   実行ファイルに埋め込む。実行時にはコンパイルもパースもしない（ゼロコピーで読む）
//! - 外には「行 → 色付きの区間（[`Span`]）」の形でしか見せない。色は
//!   [`theme()`]（`languages/theme.toml` から作る）がトークンの種類から決める
//! - 行ごとに「行頭の文脈」（[`LineState`]）を覚え、編集した行から下は、行頭の文脈が
//!   前回と同じになったところで再計算を打ち切る
//! - 見えている行と、編集した行から下の一定量（目安 [`MAIN_BUDGET_LINES`] 行）はメインで
//!   同期して計算する。残りは [`Highlighter::compute_rest`] を batch スレッドで走らせ、
//!   結果を [`Highlighter::apply_rest`] で取り込む（版が古ければ捨てる）
//! - 言語の表は、その言語を初めて使うときに読む（`madvise(WILLNEED)` で領域をまとめて
//!   先読みする）。SD からの読み込みが起きうるので、ファイルを開く処理（batch）の中で
//!   [`Registry::prepare`] を先に呼んでおくこと（メインで初めて触らない。design.md §3）
//!
//! この crate は `zc-buffer` を知らない。テキストは `zc_core::LineSource` 越しに読む。
//!
//! 使い方（メイン）:
//!
//! ```
//! use zc_highlight::{Highlighter, Registry};
//! use std::path::Path;
//! let reg = Registry::builtin();
//! let lang = reg.detect(Path::new("a.py"), "");
//! assert!(lang.is_some());
//! struct Lines<'a>(&'a [&'a str]); // ふつうは zc-buffer のバッファかその写し
//! impl zc_core::LineSource for Lines<'_> {
//!     fn len_lines(&self) -> u32 { self.0.len_lines() }
//!     fn line(&self, i: u32, out: &mut String) { self.0.line(i, out) }
//! }
//! let text: &[&str] = &["def f():", "    return 1  # c"];
//! let mut h = Highlighter::new(lang);
//! let r = h.update(&Lines(text), zc_highlight::MAIN_BUDGET_LINES);
//! assert!(r.complete);
//! let mut spans = Vec::new();
//! h.spans(1, text[1], &mut spans);
//! assert_eq!(spans.last().unwrap().kind, zc_core::TokenKind::Comment);
//! ```

mod format;
mod lang;

#[cfg(feature = "compile")]
pub mod compile;
#[cfg(feature = "compile")]
pub mod schema;

use lang::LangRt;
use std::path::Path;
use std::sync::OnceLock;
use zc_core::{Attrs, CancelToken, Cancelled, Color, LineChange, LineSource, Span, Style, Theme, TokenKind, Version, Versioned};

/// メインで 1 回に計算する行数の目安（design.md §7.3）。超える分は batch に出す。
pub const MAIN_BUDGET_LINES: u32 = 2000;

/// 埋め込みの形の版（`format.rs`）。
pub const FORMAT_VERSION: u32 = format::layout::FORMAT_VERSION;

/// 4096 バイト境界に置いたバイト列（言語の領域をページ境界にそろえるため）。
#[repr(C, align(4096))]
pub(crate) struct AlignedBlob<B: ?Sized>(B);

mod generated {
    use super::{AlignedBlob, LanguageInfo, OutlineKind, OutlineSpec};
    include!(concat!(env!("OUT_DIR"), "/languages.rs"));
}

/// 言語の番号（[`Registry`] の中での位置）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LanguageId(pub u16);

/// 言語の設定（`language.toml` の、規則以外の部分）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageInfo {
    /// `languages/` の下のフォルダ名（`python` など。変わらない識別子）。
    pub key: &'static str,
    /// 表示名（ステータスバーに出す）。
    pub name: &'static str,
    /// 拡張子（ドットなし、小文字）。
    pub extensions: &'static [&'static str],
    /// 拡張子で決まらないファイル名（完全一致）。
    pub filenames: &'static [&'static str],
    /// 行コメントの記号（Ctrl+/ 用）。
    pub line_comment: Option<&'static str>,
    /// 字下げ 1 段ぶんの文字列。
    pub indent: &'static str,
    /// F5 で実行するコマンド（`{file}` をパスに置き換える）。
    pub run: Option<&'static str>,
    /// 保存時の構文チェックのコマンド（§10.2）。
    pub check: Option<&'static str>,
    /// チェックの出力からエラー行を拾う正規表現（zc-intel が使う）。
    /// 括弧の組 1 = 行（1 始まり）、2 = 桁（省略可）、3 = メッセージ（省略可）。
    pub check_pattern: Option<&'static str>,
    /// 単語補完に出すキーワード（`language.toml` の `keywords`）。
    pub keywords: &'static [&'static str],
    /// アウトラインの規則（`[outline]`。無ければ `None`）。
    pub outline: Option<OutlineSpec>,
}

/// アウトラインの項目の種類（zc-intel の `OutlineKind` と同じ並び）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutlineKind {
    Function,
    Class,
    Heading,
}

/// アウトラインの規則（`language.toml` の `[outline]`）。zc-intel の `OutlineRules` に
/// そのまま写せる形（横同士は依存しないので、写すのは統合側）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutlineSpec {
    /// 行の最初の語 → 項目の種類（`def` → 関数 など）。
    pub items: &'static [(&'static str, OutlineKind)],
    /// キーワードの前に付いてよい語（`async`・`pub` など）。
    pub modifiers: &'static [&'static str],
    /// 見出しの記号（Markdown の `#`）。
    pub heading_char: Option<char>,
    /// 字下げ 1 段の空白の数（省略時は `indent` の長さ。タブなら 1）。
    pub indent_width: u32,
}

/// 埋め込みの表の大きさ（報告用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageStats {
    /// 言語の領域（表と DFA）のバイト数。
    pub region_bytes: usize,
    /// 文脈の名前。
    pub contexts: &'static [&'static str],
}

struct Entry {
    info: &'static LanguageInfo,
    region: &'static [u8],
    first_line: Option<regex_automata::dfa::dense::DFA<&'static [u32]>>,
    rt: OnceLock<Option<LangRt>>,
}

/// 埋め込んだ言語の一覧。
pub struct Registry {
    langs: Vec<Entry>,
    /// 表が壊れていて読めなかったときの理由。
    error: Option<String>,
}

fn blob() -> &'static [u8] {
    &generated::BLOB.0
}

impl Registry {
    /// 実行ファイルに埋め込んだ言語。最初の呼び出しで先頭（目録と 1 行目の判定）だけ読む。
    pub fn builtin() -> &'static Registry {
        static R: OnceLock<Registry> = OnceLock::new();
        R.get_or_init(|| match Registry::from_blob(blob(), generated::BUILD_ID, &generated::INFOS) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("zc-highlight: 埋め込みのハイライト表が読めない（{e}）。ハイライトなしで続ける");
                Registry { langs: Vec::new(), error: Some(e) }
            }
        })
    }

    fn from_blob(b: &'static [u8], build_id: u64, infos: &'static [LanguageInfo]) -> Result<Registry, String> {
        use format::layout::*;
        if b.len() < HEADER_LEN || b[0..4] != MAGIC {
            return Err("マジックが違う".into());
        }
        let ver = rd_u32(b, 4).unwrap_or(0);
        if ver != FORMAT_VERSION {
            return Err(format!("形式の版が違う（{ver}、期待 {FORMAT_VERSION}）"));
        }
        let id = rd_u64(b, 8).unwrap_or(0);
        if id != build_id {
            return Err(format!("ビルド ID が違う（{id:#x}、期待 {build_id:#x}）"));
        }
        let n = rd_u32(b, 16).unwrap_or(0) as usize;
        if n != infos.len() {
            return Err("言語の数が生成コードと合わない".into());
        }
        let dir = rd_u32(b, 20).unwrap_or(0) as usize;
        let mut langs = Vec::with_capacity(n);
        for (i, info) in infos.iter().enumerate() {
            let at = dir + i * DIR_REC_LEN;
            let f = |k: usize| rd_u32(b, at + k * 4).ok_or("目録が切れている");
            let (ro, rl, fo, fl) = (f(0)? as usize, f(1)? as usize, f(2)?, f(3)?);
            let region = b.get(ro..ro + rl).ok_or("言語の領域が範囲外")?;
            langs.push(Entry { info, region, first_line: lang::load_first_line(b, fo, fl), rt: OnceLock::new() });
        }
        Ok(Registry { langs, error: None })
    }

    /// 埋め込みの表が読めなかった理由（読めていれば `None`）。
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    /// 全部の言語。
    pub fn languages(&self) -> impl Iterator<Item = LanguageId> + '_ {
        (0..self.langs.len()).map(|i| LanguageId(i as u16))
    }
    /// 言語の設定。
    pub fn info(&self, id: LanguageId) -> &LanguageInfo {
        self.langs[id.0 as usize].info
    }
    /// フォルダ名（`python` など）から。
    pub fn by_key(&self, key: &str) -> Option<LanguageId> {
        self.langs.iter().position(|e| e.info.key == key).map(|i| LanguageId(i as u16))
    }
    /// ファイル名（完全一致 → 拡張子）と 1 行目（shebang）から言語を決める。
    /// 1 行目の判定は小さな DFA だけを使い、各言語の領域には触らない。
    pub fn detect(&self, path: &Path, first_line: &str) -> Option<LanguageId> {
        let id = |i: usize| Some(LanguageId(i as u16));
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if let Some(i) = self.langs.iter().position(|e| e.info.filenames.contains(&name)) {
                return id(i);
            }
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_ascii_lowercase();
            if let Some(i) = self.langs.iter().position(|e| e.info.extensions.contains(&ext.as_str())) {
                return id(i);
            }
        }
        if !first_line.is_empty() {
            for (i, e) in self.langs.iter().enumerate() {
                if let Some(d) = &e.first_line {
                    if lang::first_line_matches(d, first_line) {
                        return id(i);
                    }
                }
            }
        }
        None
    }
    /// 言語の表を読み込む（初回だけ。領域に `madvise(WILLNEED)` をかけてから読む）。
    /// SD からの読み込みが起きうるので、**batch / assist で呼ぶ**。読めれば `true`。
    pub fn prepare(&self, id: LanguageId) -> bool {
        self.rt(id).is_some()
    }
    /// 言語の表が読み込み済みか（メインはこれが `true` の言語だけ同期で計算するとよい）。
    pub fn is_prepared(&self, id: LanguageId) -> bool {
        self.langs.get(id.0 as usize).is_some_and(|e| matches!(e.rt.get(), Some(Some(_))))
    }
    /// 埋め込みの大きさ。
    pub fn stats(&self, id: LanguageId) -> LanguageStats {
        LanguageStats { region_bytes: self.langs[id.0 as usize].region.len(), contexts: generated::CONTEXT_NAMES[id.0 as usize] }
    }
    /// 埋め込みの表全体のバイト数。
    pub fn blob_bytes() -> usize {
        generated::BLOB_LEN
    }

    fn rt(&self, id: LanguageId) -> Option<&LangRt> {
        let e = self.langs.get(id.0 as usize)?;
        e.rt.get_or_init(|| {
            will_need(e.region);
            LangRt::load(e.region, false)
        })
        .as_ref()
    }
}

/// 領域をまとめて先読みさせる（ばらばらの小さな読み出しを避ける。design.md §8.2）。
fn will_need(region: &[u8]) {
    #[cfg(unix)]
    {
        const PAGE: usize = 4096;
        let start = region.as_ptr() as usize & !(PAGE - 1);
        let end = region.as_ptr() as usize + region.len();
        // SAFETY: 実行ファイルの読み取り専用の領域に先読みの助言をするだけ（中身は変わらない）。
        unsafe {
            libc::madvise(start as *mut libc::c_void, end - start, libc::MADV_WILLNEED);
        }
    }
}

/// `languages/theme.toml` から作った配色（無い種類は `Theme::dark_plus()` のまま）。
pub fn theme() -> Theme {
    let mut t = Theme::dark_plus();
    for &(k, fg, bg, attrs) in generated::THEME_ENTRIES {
        t.set_token(TokenKind::from_u8(k), Style { fg: Color::hex(fg), bg: bg.map(Color::hex), attrs: Attrs(attrs) });
    }
    t
}

/// 行頭の文脈（文脈スタックを表す番号。言語ごと）。同じ値なら、その行以降の結果は同じ。
/// `LineState::default()`（0）は最下段の `main` だけのスタック。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct LineState(pub u32);

/// [`Highlighter::update`] の結果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateResult {
    /// 行頭の文脈が確定している行数（先頭からこの行まで）。
    pub valid_lines: u32,
    /// 末尾まで確定したか。`false` なら残りを batch に出す。
    pub complete: bool,
}

/// batch スレッドで計算した「残りの行頭の文脈」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestStates {
    /// 最初の行。
    pub from_line: u32,
    /// `from_line` 行目以降の行頭の文脈（末尾の行まで）。
    pub states: Vec<LineState>,
}

/// まだ計算していない行頭の文脈。
const UNKNOWN: u32 = u32::MAX;

/// 1 つの文書のハイライトの状態（行頭の文脈のキャッシュ）。メインスレッドが持つ。
///
/// 中身: `states[i]` は i 行目の行頭の文脈。`states[..valid]` は確定。それより先は、
/// `UNKNOWN` か、前回の値（表示の当て推量と、打ち切りの比較に使う）。
/// `dirty_end` 以降の前回の値は「互いに矛盾しない列」（ある行の値が正しければ、
/// 次の `UNKNOWN` までの値も正しい）なので、計算した値と一致したらそこで打ち切れる。
pub struct Highlighter {
    lang: Option<LanguageId>,
    states: Vec<u32>,
    valid: u32,
    dirty_end: u32,
}

impl Highlighter {
    /// `lang` が `None` なら色を付けない。
    pub fn new(lang: Option<LanguageId>) -> Highlighter {
        Highlighter { lang, states: Vec::new(), valid: 0, dirty_end: 0 }
    }
    pub fn language(&self) -> Option<LanguageId> {
        self.lang
    }
    /// 言語を替える（キャッシュは捨てる）。
    pub fn set_language(&mut self, lang: Option<LanguageId>) {
        *self = Highlighter::new(lang);
    }

    fn rt(&self) -> Option<&'static LangRt> {
        Registry::builtin().rt(self.lang?)
    }

    /// 行数を合わせる（`edited` を通らずに行数が変わったときの保険）。
    fn sync_len(&mut self, n: u32) {
        let n = n as usize;
        if self.states.len() != n {
            self.states.resize(n, UNKNOWN);
            self.valid = self.valid.min(n as u32);
            self.dirty_end = self.dirty_end.min(n as u32);
        }
        if n > 0 {
            self.states[0] = 0;
            self.valid = self.valid.max(1);
        }
    }

    /// 編集を知らせる（その行から下の文脈を無効にする）。
    pub fn edited(&mut self, change: LineChange) {
        let s = change.old.start as usize;
        let lo = s + 1;
        let old_hi = (change.old.end as usize).max(lo);
        let new_hi = (change.new.end as usize).max(lo);
        if lo <= self.states.len() {
            let rm_end = old_hi.min(self.states.len());
            self.states.splice(lo..rm_end, std::iter::repeat(UNKNOWN).take(new_hi - lo));
        }
        let shift = |x: usize, inside: usize| -> usize {
            if x <= lo {
                x
            } else if x >= old_hi {
                x + new_hi - old_hi
            } else {
                inside
            }
        };
        self.valid = self.valid.min(lo as u32);
        self.dirty_end = shift(self.dirty_end as usize, new_hi).max(new_hi).min(self.states.len().max(lo)) as u32;
    }

    /// 無効になった行頭の文脈を、最大 `budget_lines` 行ぶん計算し直す（メインで呼ぶ）。
    /// 行頭の文脈が前回と同じになったら、そこで打ち切る。
    pub fn update(&mut self, src: &dyn LineSource, budget_lines: u32) -> UpdateResult {
        let n = src.len_lines();
        self.sync_len(n);
        let Some(rt) = self.rt() else {
            // 色を付けない言語: 全部確定扱い
            self.valid = n;
            return UpdateResult { valid_lines: n, complete: true };
        };
        let mut buf = String::new();
        let mut stack: Vec<u16> = Vec::with_capacity(8);
        let mut done = 0u32;
        let mut cur_state = u32::MAX; // stack が表している番号
        while self.valid < n && done < budget_lines {
            let i = self.valid as usize - 1;
            let st = self.states[i];
            if st != cur_state {
                rt.stack_of(st, &mut stack);
            }
            src.line(i as u32, &mut buf);
            let touched = rt.tokenize(&mut stack, buf.as_bytes(), &mut |_, _, _| {});
            let end = if touched { rt.intern(&stack) } else { st };
            cur_state = end;
            done += 1;
            let j = i + 1;
            if j as u32 >= self.dirty_end && self.states[j] == end {
                // 前回と同じになった: 次の UNKNOWN までは前回の値が正しい
                let mut k = j + 1;
                while k < n as usize && self.states[k] != UNKNOWN {
                    k += 1;
                }
                self.valid = k as u32;
                cur_state = u32::MAX;
            } else {
                self.states[j] = end;
                self.valid = j as u32 + 1;
            }
        }
        UpdateResult { valid_lines: self.valid, complete: self.valid >= n }
    }

    /// `line` 行目のトークンを `out` に入れる（先に `out` を空にする）。区間は行の頭から
    /// 隙間なく並ぶ（色なしの部分は `TokenKind::Text`）。空の行なら空。
    /// その行の文脈がまだ確定していなければ前回の値で色を付け、それも無ければ
    /// 色なしの 1 区間を返す。
    pub fn spans(&self, line: u32, text: &str, out: &mut Vec<Span>) {
        out.clear();
        if text.is_empty() {
            return;
        }
        let st = self.states.get(line as usize).copied().unwrap_or(UNKNOWN);
        let rt = match self.rt() {
            Some(rt) if st != UNKNOWN => rt,
            _ => {
                out.push(Span { start: 0, end: text.len() as u32, kind: TokenKind::Text });
                return;
            }
        };
        let mut stack = Vec::with_capacity(8);
        rt.stack_of(st, &mut stack);
        rt.tokenize(&mut stack, text.as_bytes(), &mut |s, e, k| {
            let kind = TokenKind::from_u8(k);
            if let Some(last) = out.last_mut() {
                if last.kind == kind && last.end as usize == s {
                    last.end = e as u32;
                    return;
                }
            }
            out.push(Span { start: s as u32, end: e as u32, kind });
        });
    }

    /// `line` 行目の行頭の文脈（確定していれば）。
    pub fn line_state(&self, line: u32) -> Option<LineState> {
        if line < self.valid {
            Some(LineState(self.states[line as usize]))
        } else {
            None
        }
    }

    /// batch に出す依頼を作る: 行頭の文脈が分かっている最後の行と、その行頭の文脈
    /// （[`Highlighter::compute_rest`] の `from_line` / `state` にそのまま渡す）。
    /// 全部確定していれば `None`。まだ一度も計算していなければ `(0, 既定)`。
    pub fn rest_request(&self) -> Option<(u32, LineState)> {
        if self.states.is_empty() {
            return Some((0, LineState::default()));
        }
        if self.valid as usize >= self.states.len() {
            return None;
        }
        let i = self.valid.max(1) - 1;
        Some((i, LineState(self.states[i as usize])))
    }

    /// **batch スレッドで呼ぶ。** `from_line` 行目（行頭の文脈は `state`）から末尾までの
    /// 行頭の文脈を計算する。256 行ごとに `cancel` を見て、古ければやめる。
    /// 言語の表をまだ読んでいなければ、ここで読む（[`Registry::prepare`]）。
    pub fn compute_rest(
        lang: LanguageId,
        src: &dyn LineSource,
        from_line: u32,
        state: LineState,
        cancel: &CancelToken,
    ) -> Result<Versioned<RestStates>, Cancelled> {
        let n = src.len_lines();
        let mut states = Vec::with_capacity(n.saturating_sub(from_line) as usize);
        if let Some(rt) = Registry::builtin().rt(lang) {
            let mut buf = String::new();
            let mut stack = Vec::with_capacity(8);
            rt.stack_of(state.0, &mut stack);
            let mut st = state.0;
            for line in from_line..n {
                if (line - from_line) % 256 == 0 {
                    cancel.check()?;
                }
                states.push(LineState(st));
                src.line(line, &mut buf);
                if rt.tokenize(&mut stack, buf.as_bytes(), &mut |_, _, _| {}) {
                    st = rt.intern(&stack);
                }
            }
        }
        Ok(Versioned::new(cancel.version(), RestStates { from_line, states }))
    }

    /// batch の結果を取り込む。`current` と版が違えば捨てて `false`。
    pub fn apply_rest(&mut self, rest: Versioned<RestStates>, current: Version) -> bool {
        if rest.version != current {
            return false;
        }
        let RestStates { from_line, states } = rest.value;
        let from = from_line as usize;
        if from == 0 {
            if states.is_empty() {
                return false;
            }
            self.states = states.iter().map(|s| s.0).collect();
        } else {
            if from_line > self.valid || from + states.len() != self.states.len() {
                return false;
            }
            for (k, s) in states.iter().enumerate() {
                self.states[from + k] = s.0;
            }
        }
        self.valid = self.states.len() as u32;
        self.dirty_end = self.valid;
        true
    }
}

/// テスト・計測用: `lang` の表が読めていれば、文脈の数と、これまでに見た文脈スタックの数。
pub fn debug_counts(lang: LanguageId) -> Option<(usize, usize)> {
    let rt = Registry::builtin().rt(lang)?;
    Some((rt.context_count(), rt.interned()))
}

/// 1 行を、行頭の文脈 `state` からトークン化して、行末の文脈を返す（zc-intel などが
/// トークンを使い回すとき用。`out` には [`Highlighter::spans`] と同じ形で入る）。
pub fn tokenize_line(lang: LanguageId, state: LineState, text: &str, out: &mut Vec<Span>) -> LineState {
    out.clear();
    let Some(rt) = Registry::builtin().rt(lang) else {
        if !text.is_empty() {
            out.push(Span { start: 0, end: text.len() as u32, kind: TokenKind::Text });
        }
        return state;
    };
    let mut stack = Vec::with_capacity(8);
    rt.stack_of(state.0, &mut stack);
    let touched = rt.tokenize(&mut stack, text.as_bytes(), &mut |s, e, k| {
        let kind = TokenKind::from_u8(k);
        if let Some(last) = out.last_mut() {
            if last.kind == kind && last.end as usize == s {
                last.end = e as u32;
                return;
            }
        }
        out.push(Span { start: s as u32, end: e as u32, kind });
    });
    if touched {
        LineState(rt.intern(&stack))
    } else {
        state
    }
}

/// 道具・テスト用: 変換したばかりの言語の領域（[`compile::CompiledLanguage::region`]）で
/// 行の列をトークン化する。領域は検査つきで読む（壊れていれば `None`）。読み込んだ表は
/// 解放しない（道具の 1 回きりの実行で使う想定）。
#[cfg(feature = "compile")]
pub fn tokenize_with_region(region: &[u8], lines: &[&str]) -> Option<Vec<Vec<Span>>> {
    let words = region.len().div_ceil(8);
    let buf: &'static mut [u64] = Box::leak(vec![0u64; words].into_boxed_slice());
    // SAFETY: u64 の配列を同じ長さのバイト列として見るだけ（8 バイト境界になる）。
    let bytes: &'static mut [u8] = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, words * 8) };
    bytes[..region.len()].copy_from_slice(region);
    let rt: &'static LangRt = Box::leak(Box::new(LangRt::load(&bytes[..region.len()], true)?));
    let mut stack = vec![0u16];
    let mut out = Vec::with_capacity(lines.len());
    for l in lines {
        let mut v: Vec<Span> = Vec::new();
        rt.tokenize(&mut stack, l.as_bytes(), &mut |s, e, k| {
            let kind = TokenKind::from_u8(k);
            match v.last_mut() {
                Some(last) if last.kind == kind && last.end as usize == s => last.end = e as u32,
                _ => v.push(Span { start: s as u32, end: e as u32, kind }),
            }
        });
        out.push(v);
    }
    Some(out)
}

#[cfg(test)]
mod tests;
