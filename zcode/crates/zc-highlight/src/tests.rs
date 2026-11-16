use super::*;
use crate::compile::{compile_language, parse_language_toml};
use crate::lang::LangRt;
use zc_core::{LineRange, VersionCounter};

/// 文字列の配列を LineSource にする。
struct L<'a>(&'a [&'a str]);
impl LineSource for L<'_> {
    fn len_lines(&self) -> u32 {
        self.0.len_lines()
    }
    fn line(&self, line: u32, out: &mut String) {
        self.0.line(line, out)
    }
}

fn py() -> LanguageId {
    Registry::builtin().by_key("python").expect("python が埋め込まれていない")
}

/// 行ごとの（種類, 文字列）。Text は省く。
fn toks(lines: &[&str]) -> Vec<Vec<(TokenKind, String)>> {
    let mut h = Highlighter::new(Some(py()));
    assert!(h.update(&L(lines), 10_000).complete);
    let mut out = Vec::new();
    let mut sp = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        h.spans(i as u32, l, &mut sp);
        // 隙間なく並ぶこと
        let mut at = 0;
        for s in &sp {
            assert_eq!(s.start, at, "{l:?}: {sp:?}");
            assert!(s.end > s.start);
            assert!(l.is_char_boundary(s.end as usize));
            at = s.end;
        }
        assert_eq!(at as usize, l.len());
        out.push(sp.iter().filter(|s| s.kind != TokenKind::Text).map(|s| (s.kind, l[s.start as usize..s.end as usize].to_string())).collect());
    }
    out
}

fn has(line: &[(TokenKind, String)], kind: TokenKind, text: &str) -> bool {
    line.iter().any(|(k, t)| *k == kind && t == text)
}

#[test]
fn registry_detect() {
    let r = Registry::builtin();
    assert!(r.error().is_none());
    let p = py();
    assert_eq!(r.info(p).name, "Python");
    assert_eq!(r.info(p).line_comment, Some("#"));
    assert_eq!(r.detect(Path::new("x/a.py"), ""), Some(p));
    assert_eq!(r.detect(Path::new("A.PYW"), ""), Some(p));
    assert_eq!(r.detect(Path::new("script"), "#!/usr/bin/env python3"), Some(p));
    assert_eq!(r.detect(Path::new("script"), "#!/bin/sh"), None);
    assert_eq!(r.detect(Path::new("a.unknown"), ""), None);
    let o = r.info(p).outline.expect("outline");
    assert!(o.items.contains(&("def", OutlineKind::Function)));
    assert_eq!(o.indent_width, 4);
    assert!(r.info(p).keywords.contains(&"lambda"));
    assert!(r.prepare(p));
    assert!(r.is_prepared(p));
}

#[test]
fn theme_from_toml() {
    let t = theme();
    assert_eq!(t.token(TokenKind::Keyword).fg, zc_core::palette::KEYWORD);
    assert_eq!(t.token(TokenKind::Control).fg, zc_core::palette::CONTROL);
    assert!(t.token(TokenKind::Heading).attrs.contains(Attrs::BOLD));
}

#[test]
fn python_tokens() {
    let t = toks(&[
        "@dataclass",
        "class Point(Base):",
        "    def dist(self, other: \"Point\" = None) -> float:",
        "        return f\"{self.x!r:>10} {{x}}\" + 'a\\n' # 注釈",
        "x = 0x1F + 1.5e3j if 漢字 else print(len(s))",
        "s = '''start",
        "middle \"quoted\" ",
        "end''' + r\"\\d\"",
    ]);
    assert!(has(&t[0], TokenKind::Attribute, "@dataclass"));
    assert!(has(&t[1], TokenKind::Keyword, "class"));
    assert!(has(&t[1], TokenKind::Type, "Point"));
    assert!(has(&t[2], TokenKind::Function, "dist"));
    assert!(has(&t[2], TokenKind::Constant, "self"));
    assert!(has(&t[2], TokenKind::Variable, "other"));
    assert!(has(&t[2], TokenKind::Type, "float"));
    assert!(has(&t[2], TokenKind::String, "\"Point\""));
    assert!(has(&t[3], TokenKind::Control, "return"));
    // `{` と `self` はどちらも定数の色なので 1 区間にまとまる
    assert!(has(&t[3], TokenKind::Constant, "{self"));
    assert!(has(&t[3], TokenKind::Constant, "!r:>10}"));
    assert!(has(&t[3], TokenKind::Escape, "{{"));
    assert!(has(&t[3], TokenKind::Escape, "\\n"));
    assert!(has(&t[3], TokenKind::Comment, "# 注釈"));
    assert!(has(&t[4], TokenKind::Number, "0x1F"));
    assert!(has(&t[4], TokenKind::Number, "1.5e3j"));
    assert!(has(&t[4], TokenKind::Function, "print"));
    assert!(!t[4].iter().any(|(_, s)| s.contains("漢字")), "非 ASCII の識別子は地の文: {:?}", t[4]);
    assert!(has(&t[6], TokenKind::String, "middle \"quoted\" "));
    assert!(has(&t[7], TokenKind::String, "end'''"));
    assert!(has(&t[7], TokenKind::String, "r\"\\d\""));
}

#[test]
fn keyword_followed_by_non_ascii_is_not_keyword() {
    let t = toks(&["if文 = 1", "if x: pass"]);
    assert!(!t[0].iter().any(|(k, _)| *k == TokenKind::Control), "{:?}", t[0]);
    assert!(has(&t[1], TokenKind::Control, "if"));
}

#[test]
fn unterminated_single_quote_ends_at_eol() {
    let t = toks(&["s = 'abc", "x = 1"]);
    assert!(has(&t[1], TokenKind::Number, "1"));
}

/// 簡単な擬似乱数（テストを再現できるように固定の種）。
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn full_states(lines: &[String]) -> Vec<u32> {
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    let mut h = Highlighter::new(Some(py()));
    assert!(h.update(&L(&refs), u32::MAX).complete);
    h.states.clone()
}

#[test]
fn incremental_equals_full_recompute() {
    let pieces = [
        "x = 1", "\"\"\"", "'''", "def f(a,", "    b=(1,", "    2)):", "s = f\"{a}\"", "# c", "", "    return a", "r'''",
        "class C:", "y = '\\", "}", ")", "(",
    ];
    let mut rng = Lcg(42);
    let mut lines: Vec<String> = (0..60).map(|_| pieces[rng.below(pieces.len() as u64) as usize].to_string()).collect();
    let mut h = Highlighter::new(Some(py()));
    for round in 0..400 {
        // 編集: 置き換え・挿入・削除
        let n = lines.len() as u32;
        let s = rng.below(n as u64) as u32;
        let old_len = (rng.below(3) as u32).min(n - s).max(if n - s > 0 { 1 } else { 0 });
        let new_len = rng.below(3) as u32 + if old_len == 0 { 1 } else { 0 };
        let new: Vec<String> = (0..new_len).map(|_| pieces[rng.below(pieces.len() as u64) as usize].to_string()).collect();
        lines.splice(s as usize..(s + old_len) as usize, new);
        if lines.is_empty() {
            lines.push(String::new());
        }
        h.edited(LineChange { old: LineRange::new(s, s + old_len), new: LineRange::new(s, s + new_len) });
        // ときどき予算を小さくして、途中で止まった状態に次の編集を重ねる
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let budget = if round % 3 == 0 { 1 + rng.below(5) as u32 } else { u32::MAX };
        let r = h.update(&L(&refs), budget);
        if r.complete {
            assert_eq!(h.states, full_states(&lines), "round {round}");
        } else {
            // 確定している部分は正しいこと
            let full = full_states(&lines);
            assert_eq!(h.states[..r.valid_lines as usize], full[..r.valid_lines as usize], "round {round}");
        }
    }
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    assert!(h.update(&L(&refs), u32::MAX).complete);
    assert_eq!(h.states, full_states(&lines));
}

#[test]
fn early_stop_after_local_edit() {
    let mut lines: Vec<String> = (0..5000).map(|i| format!("x{i} = {i}  # c")).collect();
    let mut h = Highlighter::new(Some(py()));
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    let r = h.update(&L(&refs), MAIN_BUDGET_LINES);
    assert!(!r.complete);
    assert_eq!(r.valid_lines, MAIN_BUDGET_LINES + 1);
    assert!(h.update(&L(&refs), u32::MAX).complete);
    // 1 行だけ変えても、行頭の文脈が同じになったところで止まる（予算 3 行で済む）
    lines[100] = "y = 2".into();
    h.edited(LineChange { old: LineRange::new(100, 101), new: LineRange::new(100, 101) });
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    assert!(h.update(&L(&refs), 3).complete);
    // 三重引用符を開くと下が全部変わる
    lines[100] = "y = \"\"\"".into();
    h.edited(LineChange { old: LineRange::new(100, 101), new: LineRange::new(100, 101) });
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    let r = h.update(&L(&refs), 50);
    assert!(!r.complete);
    let mut sp = Vec::new();
    h.spans(120, &lines[120], &mut sp);
    assert_eq!(sp, vec![Span { start: 0, end: lines[120].len() as u32, kind: TokenKind::String }]);
}

#[test]
fn rest_on_batch_with_versions() {
    let lines: Vec<String> = (0..3000).map(|i| if i == 10 { "'''".to_string() } else { format!("v = {i}") }).collect();
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    let counter = VersionCounter::new();
    let mut h = Highlighter::new(Some(py()));
    let r = h.update(&L(&refs), 100);
    assert!(!r.complete);
    let (from, st) = h.rest_request().unwrap();
    let rest = Highlighter::compute_rest(py(), &L(&refs), from, st, &counter.token()).unwrap();
    assert!(h.apply_rest(rest.clone(), counter.current()));
    assert!(h.rest_request().is_none());
    assert_eq!(h.states, full_states(&lines));
    // 古い版は捨てる
    counter.bump();
    assert!(!h.apply_rest(rest, counter.current()));
    // 取り消し
    let t = counter.token();
    counter.bump();
    assert!(Highlighter::compute_rest(py(), &L(&refs), 0, LineState::default(), &t).is_err());
    // 開くときの流れ: batch で全部計算 → メインの新しい Highlighter に取り込む
    let mut h2 = Highlighter::new(Some(py()));
    let (from, st) = h2.rest_request().unwrap();
    assert_eq!((from, st), (0, LineState::default()));
    let rest = Highlighter::compute_rest(py(), &L(&refs), from, st, &counter.token()).unwrap();
    assert!(h2.apply_rest(rest, counter.current()));
    assert_eq!(h2.states, full_states(&lines));
    assert!(h2.update(&L(&refs), 0).complete);
}

#[test]
fn no_language_is_plain() {
    let mut h = Highlighter::new(None);
    let src: &[&str] = &["abc", ""];
    assert!(h.update(&L(src), 1).complete);
    let mut sp = Vec::new();
    h.spans(0, "abc", &mut sp);
    assert_eq!(sp, vec![Span { start: 0, end: 3, kind: TokenKind::Text }]);
    h.spans(1, "", &mut sp);
    assert!(sp.is_empty());
}

#[test]
fn tokenize_line_helper() {
    let mut sp = Vec::new();
    let st = tokenize_line(py(), LineState::default(), "s = \"\"\"abc", &mut sp);
    assert_ne!(st, LineState::default());
    let st2 = tokenize_line(py(), st, "def\"\"\" + 1", &mut sp);
    assert_eq!(st2, LineState::default());
    assert_eq!(sp[0].kind, TokenKind::String);
}

// ---- 自前の規則（compile を実行時に使う） ----

fn rt_from(toml: &str) -> &'static LangRt {
    let f = parse_language_toml(toml).unwrap();
    let c = compile_language("t", &f, "t.toml").unwrap_or_else(|e| panic!("{e}"));
    // 8 バイト境界に置く
    let words = c.region.len().div_ceil(8);
    let mut v = vec![0u64; words];
    let bytes: &'static mut [u8] = unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, words * 8) };
    std::mem::forget(v);
    bytes[..c.region.len()].copy_from_slice(&c.region);
    Box::leak(Box::new(LangRt::load(bytes, true).expect("load")))
}

fn run(rt: &LangRt, lines: &[&str]) -> Vec<Vec<(u8, String)>> {
    let mut stack = vec![0u16];
    lines
        .iter()
        .map(|l| {
            let mut v: Vec<(u8, String)> = Vec::new();
            rt.tokenize(&mut stack, l.as_bytes(), &mut |s, e, k| v.push((k, l[s..e].to_string())));
            v
        })
        .collect()
}

fn compile_err(toml: &str) -> String {
    let f = parse_language_toml(toml).unwrap();
    compile_language("t", &f, "t.toml").err().map(|e| e.to_string()).unwrap_or_default()
}

const K: u8 = TokenKind::Keyword as u8;
const S: u8 = TokenKind::String as u8;
const F: u8 = TokenKind::Function as u8;
const T: u8 = TokenKind::Text as u8;

#[test]
fn engine_followed_by_and_zero_width() {
    let rt = rt_from(
        r#"
name = "t"
[[contexts.main]]
match = '[a-z]+'
followed_by = '\('
token = "function"
[[contexts.main]]
match = '<'
token = "keyword"
push = "loop"
[[contexts.loop]]
match = ''
push = "loop"
"#,
    );
    let r = run(rt, &["ab(c", "<xyz"]);
    assert_eq!(r[0][0], (F, "ab".into()));
    assert_eq!(r[0][1], (T, "(".into()));
    // 空の一致で push し続けても止まる（深さの上限と、同じ位置での空の一致の上限）
    assert_eq!(r[1][0], (K, "<".into()));
    assert_eq!(r[1].iter().map(|x| x.1.as_str()).collect::<String>(), "<xyz");
}

#[test]
fn engine_pop_count_set_and_newline() {
    let rt = rt_from(
        r#"
name = "t"
[default_token]
a = "string"
b = "string"
[[contexts.main]]
match = '\['
token = "keyword"
push = ["a", "b"]
[[contexts.a]]
match = '\]'
token = "keyword"
pop = true
[[contexts.b]]
match = '\]\]'
token = "keyword"
pop = 2
[[contexts.b]]
match = '!'
set = "a"
[[contexts.b]]
match = '\n'
pop = true
"#,
    );
    let r = run(rt, &["[x]]y", "[x!z]]w", "[q", "r]s"]);
    assert_eq!(r[0], vec![(K, "[".into()), (S, "x".into()), (K, "]]".into()), (T, "y".into())]);
    // set: b を降ろして a を積む（main, a, a）→ ] で 1 段ずつ降りる
    assert_eq!(
        r[1],
        vec![(K, "[".into()), (S, "x".into()), (S, "!".into()), (S, "z".into()), (K, "]".into()), (K, "]".into()), (T, "w".into())]
    );
    // 行末の \n で b を降ろし、次の行は a の中から
    assert_eq!(r[3], vec![(S, "r".into()), (K, "]".into()), (T, "s".into())]);
}

#[test]
fn engine_utf8_boundaries() {
    let rt = rt_from(
        r#"
name = "t"
[[contexts.main]]
match = '\\.'
token = "escape"
[[contexts.main]]
match = '[a-z]'
token = "keyword"
"#,
    );
    let r = run(rt, &["\\日a本"]);
    assert_eq!(r[0][0].1, "\\日");
    assert_eq!(r[0][1], (K, "a".into()));
    assert_eq!(r[0][2], (T, "本".into()));
}

#[test]
fn compile_errors_name_the_rule() {
    let base = "name = \"t\"\n[[contexts.main]]\nmatch = 'a'\n";
    let e = compile_err(&format!("{base}[[contexts.main]]\nmatch = '(?=x)y'\n"));
    assert!(e.contains("t.toml") && e.contains("contexts.main[1]") && e.contains("先読み"), "{e}");
    let e = compile_err(&format!("{base}[[contexts.main]]\nmatch = '(a)\\1'\n"));
    assert!(e.contains("後方参照"), "{e}");
    let e = compile_err(&format!("{base}[[contexts.main]]\nmatch = '\\p{{L}}'\n"));
    assert!(e.contains("Unicode"), "{e}");
    let e = compile_err(&format!("{base}[[contexts.main]]\nmatch = '(?<=a)b'\n"));
    assert!(e.contains("後読み"), "{e}");
    let e = compile_err(&format!("{base}[[contexts.main]]\nmatch = 'a'\ntoken = \"nope\"\n"));
    assert!(e.contains("nope"), "{e}");
    let e = compile_err(&format!("{base}[[contexts.main]]\nmatch = 'a'\npush = \"zz\"\n"));
    assert!(e.contains("zz"), "{e}");
    let e = compile_err(&format!("{base}[[contexts.main]]\ninclude = \"x\"\n[[contexts.x]]\ninclude = \"main\"\n"));
    assert!(e.contains("循環"), "{e}");
    let e = compile_err(&format!("{base}[[contexts.main]]\nmatch = ''\n"));
    assert!(e.contains("空の match"), "{e}");
    let e = compile_err(&format!("{base}[[contexts.main]]\nmatch = '{{{{nope}}}}'\n"));
    assert!(e.contains("nope"), "{e}");
    let e = compile_err("name = \"t\"\n[[contexts.other]]\nmatch = 'a'\n");
    assert!(e.contains("main"), "{e}");
    let e = compile_err(&format!("{base}[[contexts.main]]\nmatch = 'a'\nwords = true\n"));
    assert!(e.contains("[words]"), "{e}");
    let e = compile_err(&format!("{base}[words]\nkeyword = [\"if\"]\ncontrol = [\"if\"]\n"));
    assert!(e.contains("2 回"), "{e}");
}

#[test]
fn non_ascii_literals_match_as_utf8() {
    // 非 ASCII の文字そのもの（UTF-8 のバイト列として照合される）は書ける
    let rt = rt_from("name = \"t\"\n[[contexts.main]]\nmatch = '「.*?」'\ntoken = \"string\"\n");
    let r = run(rt, &["a「か」b"]);
    assert_eq!(r[0], vec![(T, "a".into()), (S, "「か」".into()), (T, "b".into())]);
}

#[test]
fn engine_words_and_word_push() {
    let rt = rt_from(
        r#"
name = "t"
[words]
keyword = ["fn"]
type = ["Int"]
[word_push]
fn = "name"
[[contexts.main]]
match = '[A-Za-z]+'
words = true
[[contexts.main]]
match = ' +'
[[contexts.name]]
match = ' +'
[[contexts.name]]
match = '[a-z]+'
token = "function"
pop = true
"#,
    );
    let r = run(rt, &["fn go Int x"]);
    assert_eq!(r[0], vec![(K, "fn".into()), (T, " ".into()), (F, "go".into()), (T, " ".into()), (TokenKind::Type as u8, "Int".into()), (T, " ".into()), (T, "x".into())]);
    let e = compile_err("name = \"t\"\n[words]\nkeyword = [\"a\"]\n[word_push]\nb = \"main\"\n[[contexts.main]]\nmatch = 'a'\n");
    assert!(e.contains("word_push.b"), "{e}");
}

#[test]
fn fallback_include_equals_inline_include() {
    // 最後の include（fallback で共有）と途中の include（展開）で結果が同じ
    let a = rt_from(
        "name = \"t\"\n[[contexts.main]]\nmatch = 'x'\ntoken = \"keyword\"\n[[contexts.main]]\ninclude = \"c\"\n[[contexts.c]]\nmatch = '[a-z]+'\ntoken = \"string\"\n[[contexts.c]]\nmatch = 'xy'\ntoken = \"number\"\n",
    );
    let b = rt_from(
        "name = \"t\"\n[[contexts.main]]\nmatch = 'x'\ntoken = \"keyword\"\n[[contexts.main]]\ninclude = \"c\"\n[[contexts.main]]\nmatch = '\\n'\n[[contexts.c]]\nmatch = '[a-z]+'\ntoken = \"string\"\n[[contexts.c]]\nmatch = 'xy'\ntoken = \"number\"\n",
    );
    let lines = ["xyz ab x", "yx"];
    assert_eq!(run(a, &lines), run(b, &lines));
}

#[test]
fn builtin_sizes_within_budget() {
    let r = Registry::builtin();
    for id in r.languages() {
        let s = r.stats(id);
        assert!(s.region_bytes <= 100 * 1024, "{}: {} bytes", r.info(id).key, s.region_bytes);
    }
}
