//! 補完・診断・アウトライン（design.md §10）。最初の版。
//!
//! ほかの crate に見せるのは 3 つの形だけ: 補完候補（[`Completion`]）、診断
//! （[`Diagnostic`]）、アウトライン（[`OutlineItem`]）。最初の版（単語補完・保存時の
//! 構文チェック・トークンからのアウトライン）と、後の版の LSP クライアントは、
//! この形の裏で差し替える。
//!
//! この crate は `zc-buffer` も `zc-highlight` も知らない。テキストは
//! `zc_core::LineSource`、トークンは `zc_core::Span` で受け取る。
//!
//! | 機能 | 入口 | 動くスレッド |
//! |---|---|---|
//! | 単語補完 | [`WordIndex`]（[`WordIndex::build`] / [`WordIndex::edited`]）、[`prefix_at`]、[`complete`] | メイン（`build` だけ batch でもよい） |
//! | 構文チェック | [`run_check`] | batch |
//! | アウトライン | [`outline_item`] / [`outline`] と [`OutlineRules`] | メイン（行ごと）/ batch（全体） |

mod check;
mod minire;
mod words;

pub use check::{parse_diagnostics, run_check, run_check_with, split_command, CheckError, DEFAULT_TIMEOUT};
pub use words::{complete, prefix_at, WordIndex, MAX_WORD_BYTES, MIN_WORD_LEN};

use zc_core::{LineSource, Span, TokenKind};

/// 補完候補の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    /// 開いているファイルに出てくる単語。
    Word,
    /// 言語のキーワード。
    Keyword,
}

/// 補完候補。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub kind: CompletionKind,
}

/// 診断の重さ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// 診断（波線とステータスバーに出す）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// 0 始まりの行。
    pub line: u32,
    /// 桁（分かれば。0 始まり）。
    pub col: Option<u32>,
    pub severity: Severity,
    pub message: String,
}

/// アウトラインの項目の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutlineKind {
    Function,
    Class,
    Heading,
}

/// アウトラインの 1 項目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    pub line: u32,
    /// 入れ子の深さ（字下げ・見出しのレベルから。0 始まり）。
    pub depth: u8,
    pub kind: OutlineKind,
    pub name: String,
}

// ---------------------------------------------------------------------------
// アウトライン
// ---------------------------------------------------------------------------

/// 「この単語で始まる行は、この種類の項目」という規則 1 つ。
#[derive(Clone, Copy, Debug)]
pub struct OutlineRule<'a> {
    pub keyword: &'a str,
    pub kind: OutlineKind,
}

/// アウトラインの規則（言語ごとに呼ぶ側が持つ。この crate は言語を知らない）。
#[derive(Clone, Copy, Debug)]
pub struct OutlineRules<'a> {
    /// 項目になるキーワード（`def` / `class` / `fn` / `struct` …）。
    pub items: &'a [OutlineRule<'a>],
    /// キーワードの前に付いてよい語（`pub` / `async` / `export` …）。`pub(crate)` のような
    /// 括弧つきも読み飛ばす。
    pub modifiers: &'a [&'a str],
    /// 見出しの記号（Markdown の `#`）。`None` なら見出しは無い。
    pub heading_char: Option<char>,
    /// 字下げ 1 段の空白の数（深さの計算。タブは 1 つで 1 段）。
    pub indent_width: u32,
}

/// Python: `def` / `class`（`async def` も）。
pub const PYTHON: OutlineRules<'static> = OutlineRules {
    items: &[OutlineRule { keyword: "def", kind: OutlineKind::Function }, OutlineRule { keyword: "class", kind: OutlineKind::Class }],
    modifiers: &["async"],
    heading_char: None,
    indent_width: 4,
};

/// Rust: `fn` / `struct` / `enum` / `trait` / `mod`。
pub const RUST: OutlineRules<'static> = OutlineRules {
    items: &[
        OutlineRule { keyword: "fn", kind: OutlineKind::Function },
        OutlineRule { keyword: "struct", kind: OutlineKind::Class },
        OutlineRule { keyword: "enum", kind: OutlineKind::Class },
        OutlineRule { keyword: "trait", kind: OutlineKind::Class },
        OutlineRule { keyword: "mod", kind: OutlineKind::Class },
    ],
    modifiers: &["pub", "async", "unsafe", "const", "extern"],
    heading_char: None,
    indent_width: 4,
};

/// JavaScript / TypeScript: `function` / `class`。
pub const JAVASCRIPT: OutlineRules<'static> = OutlineRules {
    items: &[OutlineRule { keyword: "function", kind: OutlineKind::Function }, OutlineRule { keyword: "class", kind: OutlineKind::Class }],
    modifiers: &["export", "default", "async", "abstract"],
    heading_char: None,
    indent_width: 2,
};

/// Markdown: `#` の見出し。
pub const MARKDOWN: OutlineRules<'static> =
    OutlineRules { items: &[], modifiers: &[], heading_char: Some('#'), indent_width: 4 };

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 語の終わり（バイト位置）。
fn word_end(text: &str, from: usize) -> usize {
    text[from..].find(|c: char| !is_ident(c)).map_or(text.len(), |i| from + i)
}

fn skip_spaces(text: &str, from: usize) -> usize {
    from + (text.len() - from) - text[from..].trim_start().len()
}

/// `byte` がコメントか文字列の中か。
fn in_comment_or_string(spans: &[Span], byte: usize) -> bool {
    spans.iter().any(|s| (s.start as usize) <= byte && byte < s.end as usize && matches!(s.kind, TokenKind::Comment | TokenKind::String))
}

/// 1 行（`text` とそのトークン `spans`）から、アウトラインの項目になるか調べる。
/// 行の最初の語（`rules.modifiers` を読み飛ばしたあと）が `rules.items` のキーワードで、
/// そのあとに名前が続く行が項目になる。コメントや文字列の中は除く。見出しは
/// `rules.heading_char` の連続のあとに空白がある行。`spans` が空でも動く。
pub fn outline_item(line: u32, text: &str, spans: &[Span], rules: &OutlineRules) -> Option<OutlineItem> {
    let rest = text.trim_start();
    let indent_bytes = text.len() - rest.len();
    if in_comment_or_string(spans, indent_bytes) {
        return None;
    }
    if let Some(h) = rules.heading_char {
        if rest.starts_with(h) {
            let n = rest.chars().take_while(|c| *c == h).count();
            let after = &rest[n * h.len_utf8()..];
            if !(1..=6).contains(&n) || !after.starts_with([' ', '\t']) {
                return None;
            }
            let name = after.trim().trim_end_matches(h).trim();
            if name.is_empty() {
                return None;
            }
            return Some(OutlineItem { line, depth: (n - 1) as u8, kind: OutlineKind::Heading, name: name.to_string() });
        }
    }
    let mut pos = indent_bytes;
    loop {
        let end = word_end(text, pos);
        if end == pos {
            return None;
        }
        let word = &text[pos..end];
        if let Some(rule) = rules.items.iter().find(|r| r.keyword == word) {
            let name_start = skip_spaces(text, end);
            if name_start == end {
                return None; // `fn(` や `def:` は項目ではない
            }
            let name_end = word_end(text, name_start);
            if name_end == name_start {
                return None;
            }
            return Some(OutlineItem {
                line,
                depth: depth_of(&text[..indent_bytes], rules.indent_width),
                kind: rule.kind,
                name: text[name_start..name_end].to_string(),
            });
        }
        if !rules.modifiers.contains(&word) {
            return None;
        }
        pos = skip_spaces(text, end);
        // `pub(crate)` のような括弧つきの修飾
        if text[pos..].starts_with('(') && word == "pub" {
            let close = text[pos..].find(')')?;
            pos = skip_spaces(text, pos + close + 1);
        }
    }
}

fn depth_of(indent: &str, width: u32) -> u8 {
    let tabs = indent.matches('\t').count() as u32;
    let spaces = indent.matches(' ').count() as u32;
    (tabs + spaces / width.max(1)).min(255) as u8
}

/// 全行のアウトライン。`spans_of(行, テキスト, 出力)` は、その行のトークンを `out` に入れる
/// （`zc-highlight` から取る。トークンを持たない呼び方なら何もしない関数でよい）。
/// 全行を 1 回なめるので、2,000 行でも batch に出すのが無難。
pub fn outline(
    src: &dyn LineSource,
    rules: &OutlineRules,
    spans_of: &mut dyn FnMut(u32, &str, &mut Vec<Span>),
) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    let mut text = String::new();
    let mut spans = Vec::new();
    for l in 0..src.len_lines() {
        src.line(l, &mut text);
        spans.clear();
        spans_of(l, &text, &mut spans);
        if let Some(it) = outline_item(l, &text, &spans, rules) {
            items.push(it);
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, rules: &OutlineRules) -> Option<(OutlineKind, u8, String)> {
        outline_item(3, text, &[], rules).map(|i| (i.kind, i.depth, i.name))
    }

    #[test]
    fn python_outline() {
        assert_eq!(item("def foo(a, b):", &PYTHON), Some((OutlineKind::Function, 0, "foo".into())));
        assert_eq!(item("    def 日本語_関数(self):", &PYTHON), Some((OutlineKind::Function, 1, "日本語_関数".into())));
        assert_eq!(item("        async def run(self):", &PYTHON), Some((OutlineKind::Function, 2, "run".into())));
        assert_eq!(item("class Foo(Base):", &PYTHON), Some((OutlineKind::Class, 0, "Foo".into())));
        assert_eq!(item("\tclass Inner:", &PYTHON), Some((OutlineKind::Class, 1, "Inner".into())));
        assert_eq!(item("define = 1", &PYTHON), None);
        assert_eq!(item("x = def_value", &PYTHON), None);
        assert_eq!(item("def", &PYTHON), None);
        assert_eq!(item("def (x)", &PYTHON), None);
        assert_eq!(item("", &PYTHON), None);
        assert_eq!(item("    ", &PYTHON), None);
        assert_eq!(item("async = 3", &PYTHON), None);
    }

    #[test]
    fn rust_outline() {
        assert_eq!(item("pub fn new() -> Self {", &RUST), Some((OutlineKind::Function, 0, "new".into())));
        assert_eq!(item("    pub(crate) async fn go() {", &RUST), Some((OutlineKind::Function, 1, "go".into())));
        assert_eq!(item("pub unsafe extern fn x();", &RUST), Some((OutlineKind::Function, 0, "x".into())));
        assert_eq!(item("pub struct Buffer {", &RUST), Some((OutlineKind::Class, 0, "Buffer".into())));
        assert_eq!(item("enum Kind {", &RUST), Some((OutlineKind::Class, 0, "Kind".into())));
        assert_eq!(item("let fn_ptr = 1;", &RUST), None);
        assert_eq!(item("pub(crate", &RUST), None);
        assert_eq!(item("// fn commented()", &RUST), None);
    }

    #[test]
    fn comments_and_strings_are_skipped_via_spans() {
        let text = "def foo():";
        let spans = [Span { start: 0, end: 10, kind: TokenKind::Comment }];
        assert_eq!(outline_item(0, text, &spans, &PYTHON), None);
        let spans = [Span { start: 0, end: 3, kind: TokenKind::Keyword }];
        assert!(outline_item(0, text, &spans, &PYTHON).is_some());
        // 複数行文字列の中の def
        let spans = [Span { start: 0, end: 20, kind: TokenKind::String }];
        assert_eq!(outline_item(0, "    def inside_doc():", &spans, &PYTHON), None);
    }

    #[test]
    fn markdown_headings() {
        let m = |t| item(t, &MARKDOWN);
        assert_eq!(m("# タイトル"), Some((OutlineKind::Heading, 0, "タイトル".into())));
        assert_eq!(m("### 小見出し ###"), Some((OutlineKind::Heading, 2, "小見出し".into())));
        assert_eq!(m("#nospace"), None);
        assert_eq!(m("####### too deep"), None);
        assert_eq!(m("# "), None);
        assert_eq!(m("text # not heading"), None);
    }

    #[test]
    fn javascript_outline() {
        assert_eq!(item("export default async function main() {", &JAVASCRIPT), Some((OutlineKind::Function, 0, "main".into())));
        assert_eq!(item("  class A extends B {", &JAVASCRIPT), Some((OutlineKind::Class, 1, "A".into())));
        assert_eq!(item("function () {}", &JAVASCRIPT), None);
    }

    #[test]
    fn whole_document_outline() {
        let lines: Vec<&str> = vec!["import os", "", "class A:", "    def f(self):", "        pass", "# def not_real", "def g():"];
        struct L<'a>(&'a [&'a str]);
        impl LineSource for L<'_> {
            fn len_lines(&self) -> u32 {
                self.0.len_lines()
            }
            fn line(&self, line: u32, out: &mut String) {
                self.0.line(line, out)
            }
        }
        // 行 5 はコメントのトークンを返す
        let mut spans_of = |l: u32, t: &str, out: &mut Vec<Span>| {
            if l == 5 {
                out.push(Span { start: 0, end: t.len() as u32, kind: TokenKind::Comment });
            }
        };
        let items = outline(&L(&lines), &PYTHON, &mut spans_of);
        let got: Vec<(u32, u8, &str)> = items.iter().map(|i| (i.line, i.depth, i.name.as_str())).collect();
        assert_eq!(got, [(2, 0, "A"), (3, 1, "f"), (6, 0, "g")]);
    }
}
