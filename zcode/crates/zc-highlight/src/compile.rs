// 言語の定義（`language.toml` を読んだもの）→ 埋め込み用のバイト列（design.md §7.2・§8.2）。
//
// `build.rs` と、ライブラリの `compile` 機能（道具 `zcode-langc`）の両方から使う。
// 実機の実行ファイルには入らない（DFA を作るコードは大きく、A53 では遅い）。
//
// 正規表現は regex-automata の多パターン dense DFA にする。設定:
// - Unicode を使わない（`\w` `\d` `\s` `\b` は ASCII。`\p{..}` は使えない）。
//   非 ASCII の文字は「識別子の一部」として扱う: `[{{nonascii}}]`（= `\x80-\xFF`）を
//   識別子の文字クラスに足して書く
// - `^` `$` は行頭・行末（multi_line）。各行は末尾に `\n` を付けて照合する
// - 後方参照・先読み・後読みは使えない（DFA にできない）。先読みの代わりに
//   規則の `followed_by` を使う

use crate::format::layout::*;
use crate::schema::{LanguageFile, Rule};
use regex_automata::dfa::{dense, StartKind};
use regex_automata::nfa::thompson;
use regex_automata::util::syntax;
use regex_automata::MatchKind;
use std::collections::BTreeMap;
use zc_core::TokenKind;

/// 変換の失敗。どのファイルのどの規則かを持つ。
#[derive(Debug, Clone)]
pub struct CompileError {
    pub origin: String,
    pub msg: String,
}

impl core::fmt::Display for CompileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.origin, self.msg)
    }
}

impl std::error::Error for CompileError {}

/// 文脈 1 つぶんの大きさ（報告用）。
#[derive(Debug, Clone)]
pub struct ContextStats {
    pub name: String,
    pub rules: usize,
    pub dfa_bytes: usize,
    /// 文字の種類の数（バイトの同値類）と、表の 1 状態の幅（2 の冪）。
    pub alphabet: usize,
    pub stride: usize,
}

/// 変換した言語 1 つ。
#[derive(Debug, Clone)]
pub struct CompiledLanguage {
    /// フォルダ名（`python` など）。
    pub key: String,
    pub file: LanguageFile,
    /// 言語の領域（[`crate::format`] の形）。
    pub region: Vec<u8>,
    /// 1 行目の判定用 DFA。
    pub first_line: Option<Vec<u8>>,
    pub contexts: Vec<ContextStats>,
    /// `followed_by` 用の DFA の合計。
    pub head_bytes: usize,
}

/// 組み込みの部品（`{{名前}}`）。
const BUILTIN_VARS: &[(&str, &str)] = &[
    // 文字クラスの中に書く: `[A-Za-z_{{nonascii}}]`
    ("nonascii", r"\x80-\xFF"),
];

struct FlatRule {
    pattern: String,
    head: Option<String>,
    token: u8,
    pop: u8,
    push: Vec<u16>,
    flags: u8,
    loc: String,
}

/// 規則の正規表現の設定（DFA にする前の構文の設定）。道具の検査でも同じものを使う。
pub fn syntax_config() -> syntax::Config {
    syntax::Config::new().unicode(false).utf8(false).multi_line(true).crlf(false)
}

fn build_dfa(patterns: &[&str], anchored: bool) -> Result<Vec<u8>, String> {
    let dfa = dense::Builder::new()
        .configure(
            dense::Config::new()
                .start_kind(if anchored { StartKind::Anchored } else { StartKind::Unanchored })
                .match_kind(MatchKind::LeftmostFirst)
                .minimize(true)
                .byte_classes(true)
                .accelerate(false)
                .specialize_start_states(false)
                .starts_for_each_pattern(false)
                .unicode_word_boundary(false),
        )
        .syntax(syntax_config())
        .thompson(thompson::Config::new().utf8(false).shrink(true))
        .build_many(patterns)
        .map_err(|e| {
            let mut s = e.to_string();
            let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
            while let Some(x) = src {
                s.push_str(": ");
                s.push_str(&x.to_string());
                src = x.source();
            }
            s
        })?;
    let (bytes, pad) = dfa.to_bytes_little_endian();
    LAST_INFO.with(|c| c.set((dfa.alphabet_len(), dfa.stride())));
    Ok(bytes[pad..].to_vec())
}

thread_local! {
    /// 直前に作った DFA の（文字の種類の数, 表の幅）。報告用。
    static LAST_INFO: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// 1 つの正規表現が規則として使えるか（部品の置き換えは済ませたもの）。道具用。
pub fn check_regex(pat: &str) -> Result<(), String> {
    if let Some(why) = unsupported_feature(pat) {
        return Err(why.to_string());
    }
    build_dfa(&[pat], true).map(|_| ())
}

/// DFA にできない書き方を探して、分かりやすい理由を返す。
pub fn unsupported_feature(pat: &str) -> Option<&'static str> {
    let b = pat.as_bytes();
    let mut i = 0;
    let mut in_class = false;
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() => {
                let c = b[i + 1];
                match c {
                    b'1'..=b'9' if !in_class => return Some("後方参照（\\1 など）は使えない"),
                    b'k' if b.get(i + 2) == Some(&b'<') => return Some("名前つきの後方参照（\\k<..>）は使えない"),
                    b'p' | b'P' => return Some("Unicode の文字クラス（\\p{..}）は使えない。非 ASCII は [{{nonascii}}] で書く"),
                    b'G' => return Some("\\G（前の一致の終わり）は使えない"),
                    b'h' | b'H' if !in_class => return Some("\\h は使えない（[0-9a-fA-F] と書く）"),
                    b'z' | b'A' => {}
                    b'Z' => return Some("\\Z は使えない（$ と書く）"),
                    _ => {}
                }
                i += 2;
                continue;
            }
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            b'(' if !in_class && b.get(i + 1) == Some(&b'?') => {
                let rest = &b[i + 2..];
                if rest.starts_with(b"=") || rest.starts_with(b"!") {
                    return Some("先読み（(?=..) / (?!..)）は使えない。後ろの条件は followed_by に書く");
                }
                if rest.starts_with(b"<=") || rest.starts_with(b"<!") {
                    return Some("後読み（(?<=..) / (?<!..)）は使えない");
                }
                if rest.starts_with(b">") {
                    return Some("アトミックグループ（(?>..)）は使えない。(?:..) で書く");
                }
            }
            b'+' if !in_class && i > 0 && matches!(b[i - 1], b'*' | b'+' | b'?' | b'}') && !is_escaped(b, i - 1) => {
                return Some("強欲な繰り返し（*+ ++ ?+）は使えない");
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn is_escaped(b: &[u8], i: usize) -> bool {
    let mut n = 0;
    let mut j = i;
    while j > 0 && b[j - 1] == b'\\' {
        n += 1;
        j -= 1;
    }
    n % 2 == 1
}

/// `{{名前}}` を置き換える（部品の中の部品も）。
pub fn expand_vars(pat: &str, vars: &BTreeMap<String, String>) -> Result<String, String> {
    fn go(pat: &str, vars: &BTreeMap<String, String>, depth: u32) -> Result<String, String> {
        if depth > 16 {
            return Err("variables の参照が深すぎる（循環していないか）".into());
        }
        let mut out = String::with_capacity(pat.len());
        let mut rest = pat;
        while let Some(i) = rest.find("{{") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 2..];
            let j = after.find("}}").ok_or_else(|| format!("閉じていない {{{{ がある: {pat}"))?;
            let name = after[..j].trim();
            let val = match vars.get(name) {
                Some(v) => go(v, vars, depth + 1)?,
                None => match BUILTIN_VARS.iter().find(|(n, _)| *n == name) {
                    Some((_, v)) => v.to_string(),
                    None => return Err(format!("知らない部品 {{{{{name}}}}}（variables に無い）")),
                },
            };
            out.push_str(&val);
            rest = &after[j + 2..];
        }
        out.push_str(rest);
        Ok(out)
    }
    go(pat, vars, 0)
}

fn token_id(name: &str) -> Result<u8, String> {
    TokenKind::from_name(name).map(|k| k as u8).ok_or_else(|| {
        let all: Vec<&str> = (0..TokenKind::COUNT as u8).map(|i| TokenKind::from_u8(i).name()).collect();
        format!("知らないトークンの種類 \"{name}\"（使えるもの: {}）", all.join(", "))
    })
}

/// 1 つの言語を変換する。`key` はフォルダ名、`origin` はエラーに出すファイル名。
pub fn compile_language(key: &str, file: &LanguageFile, origin: &str) -> Result<CompiledLanguage, CompileError> {
    let err = |msg: String| CompileError { origin: origin.to_string(), msg };
    if file.name.trim().is_empty() {
        return Err(err("name が空".into()));
    }
    if !file.contexts.contains_key("main") {
        return Err(err("contexts.main が無い（行頭の文脈の出発点）".into()));
    }
    if let Some(o) = &file.outline {
        for (k, kind) in &o.items {
            if !crate::schema::OUTLINE_KINDS.contains(&kind.as_str()) {
                return Err(err(format!(
                    "outline.items.{k}: 知らない種類 \"{kind}\"（{} のどれか）",
                    crate::schema::OUTLINE_KINDS.join(" / ")
                )));
            }
        }
        if let Some(h) = &o.heading_char {
            if h.chars().count() != 1 {
                return Err(err(format!("outline.heading_char は 1 文字（\"{h}\"）")));
            }
        }
    }
    // 文脈の番号: main が 0、残りは名前順
    let mut names: Vec<&str> = vec!["main"];
    names.extend(file.contexts.keys().map(|s| s.as_str()).filter(|s| *s != "main"));
    if names.len() > MAX_CONTEXTS {
        return Err(err(format!("文脈が多すぎる（{} 個、上限 {MAX_CONTEXTS}）", names.len())));
    }
    let ctx_id = |n: &str| names.iter().position(|x| *x == n).map(|i| i as u16);

    for (c, t) in &file.default_token {
        if ctx_id(c).is_none() {
            return Err(err(format!("default_token.{c}: そういう文脈は無い")));
        }
        token_id(t).map_err(|m| err(format!("default_token.{c}: {m}")))?;
    }
    let default_of = |c: &str| -> u8 { file.default_token.get(c).and_then(|t| token_id(t).ok()).unwrap_or(0) };

    // 語の表（語, 種類, 積む文脈）
    let mut words: Vec<(Vec<u8>, u8, u16)> = Vec::new();
    for (kind, list) in &file.words {
        let t = token_id(kind).map_err(|m| err(format!("words.{kind}: {m}")))?;
        for w in list {
            if w.is_empty() || w.len() > u16::MAX as usize {
                return Err(err(format!("words.{kind}: 空か長すぎる語がある")));
            }
            let push = match file.word_push.get(w) {
                Some(c) => ctx_id(c).ok_or_else(|| err(format!("word_push.{w}: そういう文脈は無い: {c}")))?,
                None => NO_FALLBACK,
            };
            words.push((w.as_bytes().to_vec(), t, push));
        }
    }
    words.sort();
    for p in words.windows(2) {
        if p[0].0 == p[1].0 {
            return Err(err(format!("words: \"{}\" が 2 回出てくる", String::from_utf8_lossy(&p[0].0))));
        }
    }
    for w in file.word_push.keys() {
        if !words.iter().any(|x| x.0 == w.as_bytes()) {
            return Err(err(format!("word_push.{w}: [words] に無い語")));
        }
    }

    // include を展開して、文脈ごとの規則の列にする。文脈の最後の規則が include なら、
    // それは展開せず fallback にする（DFA を複製せずに共有する。結果は同じ）
    fn flatten(
        file: &LanguageFile,
        ctx: &str,
        top: bool,
        stack: &mut Vec<String>,
        out: &mut Vec<FlatRule>,
        ctx_id: &dyn Fn(&str) -> Option<u16>,
    ) -> Result<Option<u16>, String> {
        if stack.iter().any(|s| s == ctx) {
            return Err(format!("include が循環している: {} → {ctx}", stack.join(" → ")));
        }
        stack.push(ctx.to_string());
        let rules = file.contexts.get(ctx).ok_or_else(|| format!("そういう文脈は無い: {ctx}"))?;
        let mut fallback = None;
        for (i, r) in rules.iter().enumerate() {
            let loc = format!("contexts.{ctx}[{i}]");
            if let Some(inc) = &r.include {
                if *r != (Rule { include: Some(inc.clone()), ..Rule::default() }) {
                    return Err(format!("{loc}: include の規則にはほかの項目を書けない"));
                }
                let id = ctx_id(inc).ok_or_else(|| format!("{loc}: そういう文脈は無い: {inc}"))?;
                if top && i + 1 == rules.len() {
                    // 循環の検査のためだけに展開してみる
                    flatten(file, inc, false, stack, &mut Vec::new(), ctx_id).map_err(|m| format!("{loc}: {m}"))?;
                    fallback = Some(id);
                } else {
                    flatten(file, inc, false, stack, out, ctx_id).map_err(|m| format!("{loc}: {m}"))?;
                }
                continue;
            }
            let m = r.match_.as_ref().ok_or_else(|| format!("{loc}: match も include も無い"))?;
            if r.set.is_some() && (r.push.is_some() || r.pop.is_some()) {
                return Err(format!("{loc}: set は push / pop と一緒に書けない"));
            }
            let (pop, push_names) = if let Some(s) = &r.set {
                (1u8, s.to_vec())
            } else {
                (r.pop.as_ref().map(|p| p.count()).unwrap_or(0), r.push.as_ref().map(|p| p.to_vec()).unwrap_or_default())
            };
            let mut push = Vec::new();
            for p in &push_names {
                push.push(ctx_id(p).ok_or_else(|| format!("{loc}: push / set の先の文脈が無い: {p}"))?);
            }
            if m.is_empty() && pop == 0 && push.is_empty() {
                return Err(format!("{loc}: 空の match は文脈を変える（pop / push / set）ときだけ使える"));
            }
            let token = match &r.token {
                Some(t) => token_id(t).map_err(|e| format!("{loc}: {e}"))?,
                None => TOKEN_INHERIT,
            };
            let flags = if r.words == Some(true) { RULE_WORDS } else { 0 };
            out.push(FlatRule { pattern: m.clone(), head: r.followed_by.clone(), token, pop, push, flags, loc });
        }
        stack.pop();
        Ok(fallback)
    }

    let mut ctx_rules: Vec<Vec<FlatRule>> = Vec::new();
    let mut fallbacks: Vec<Option<u16>> = Vec::new();
    for n in &names {
        let mut out = Vec::new();
        let fb = flatten(file, n, true, &mut Vec::new(), &mut out, &ctx_id).map_err(err)?;
        if out.len() > u16::MAX as usize {
            return Err(err(format!("contexts.{n}: 規則が多すぎる")));
        }
        if out.iter().any(|r| r.flags & RULE_WORDS != 0) && words.is_empty() {
            return Err(err(format!("contexts.{n}: words = true の規則があるのに [words] が無い")));
        }
        ctx_rules.push(out);
        fallbacks.push(fb);
    }
    // スタックに載りうる文脈（main と push / set の先）と、その fallback の先だけを DFA に
    // する。include で取り込まれるだけの文脈（部品）は DFA を持たない
    let mut reach = vec![false; names.len()];
    reach[0] = true;
    let mut work = vec![0usize];
    // word_push の先（どの文脈の words 規則からでも積まれうる）
    for w in &words {
        if w.2 != NO_FALLBACK && !reach[w.2 as usize] {
            reach[w.2 as usize] = true;
            work.push(w.2 as usize);
        }
    }
    while let Some(c) = work.pop() {
        let mut next: Vec<u16> = ctx_rules[c].iter().flat_map(|r| r.push.iter().copied()).collect();
        next.extend(fallbacks[c]);
        for p in next {
            if !reach[p as usize] {
                reach[p as usize] = true;
                work.push(p as usize);
            }
        }
    }
    for (ci, rules) in ctx_rules.iter_mut().enumerate() {
        if !reach[ci] {
            rules.clear();
            fallbacks[ci] = None;
        } else if rules.is_empty() && fallbacks[ci].is_none() {
            return Err(err(format!("contexts.{}: 規則が 1 つも無い", names[ci])));
        }
    }

    // 正規表現を組み立てて、文脈ごとに DFA にする
    let mut ctx_dfas: Vec<Vec<u8>> = Vec::new();
    let mut heads: Vec<Vec<Option<Vec<u8>>>> = Vec::new();
    let mut stats = Vec::new();
    let mut head_bytes = 0;
    for (ci, rules) in ctx_rules.iter().enumerate() {
        if rules.is_empty() {
            stats.push(ContextStats { name: names[ci].to_string(), rules: 0, dfa_bytes: 0, alphabet: 0, stride: 0 });
            ctx_dfas.push(Vec::new());
            heads.push(Vec::new());
            continue;
        }
        let mut pats = Vec::new();
        let mut hs = Vec::new();
        for r in rules {
            let check = |p: &str, what: &str| -> Result<String, CompileError> {
                let p = expand_vars(p, &file.variables).map_err(|m| err(format!("{} ({what}): {m}", r.loc)))?;
                if let Some(why) = unsupported_feature(&p) {
                    return Err(err(format!("{} ({what} = '{p}'): {why}", r.loc)));
                }
                // 1 つずつ試して、どの規則が悪いかを示す
                build_dfa(&[p.as_str()], true).map_err(|m| err(format!("{} ({what} = '{p}'): DFA にできない: {m}", r.loc)))?;
                Ok(p)
            };
            let m = check(&r.pattern, "match")?;
            match &r.head {
                Some(f) => {
                    let f = check(f, "followed_by")?;
                    pats.push(format!("(?:{m})(?:{f})"));
                    let h = build_dfa(&[m.as_str()], true).map_err(|m| err(format!("{}: {m}", r.loc)))?;
                    head_bytes += h.len();
                    hs.push(Some(h));
                }
                None => {
                    pats.push(m);
                    hs.push(None);
                }
            }
        }
        let refs: Vec<&str> = pats.iter().map(|s| s.as_str()).collect();
        let dfa = build_dfa(&refs, true).map_err(|m| err(format!("contexts.{}: DFA にできない: {m}", names[ci])))?;
        let (alphabet, stride) = LAST_INFO.with(|c| c.get());
        stats.push(ContextStats { name: names[ci].to_string(), rules: rules.len(), dfa_bytes: dfa.len(), alphabet, stride });
        ctx_dfas.push(dfa);
        heads.push(hs);
    }

    let first_line = match &file.first_line {
        Some(p) => {
            let p = expand_vars(p, &file.variables).map_err(|m| err(format!("first_line: {m}")))?;
            if let Some(why) = unsupported_feature(&p) {
                return Err(err(format!("first_line = '{p}': {why}")));
            }
            Some(build_dfa(&[p.as_str()], false).map_err(|m| err(format!("first_line = '{p}': DFA にできない: {m}")))?)
        }
        None => None,
    };

    // 領域に並べる
    let n_ctx = names.len();
    let n_rules: usize = ctx_rules.iter().map(|r| r.len()).sum();
    let n_push: usize = ctx_rules.iter().flatten().map(|r| r.push.len()).sum();
    let ctx_off = REGION_HEADER_LEN;
    let rule_off = ctx_off + n_ctx * CTX_REC_LEN;
    let push_off = rule_off + n_rules * RULE_REC_LEN;
    let words_off = (push_off + n_push * 2).div_ceil(4) * 4;
    let wstr_off = words_off + words.len() * WORD_REC_LEN;
    let wstr_len: usize = words.iter().map(|w| w.0.len()).sum();
    let data_off = (wstr_off + wstr_len).div_ceil(DFA_ALIGN) * DFA_ALIGN;
    // DFA の置き場所を決める
    let mut blobs: Vec<u8> = Vec::new();
    let mut place = |b: &[u8]| -> (u32, u32) {
        pad_to(&mut blobs, DFA_ALIGN);
        let off = data_off + blobs.len();
        blobs.extend_from_slice(b);
        (off as u32, b.len() as u32)
    };
    let ctx_places: Vec<(u32, u32)> = ctx_dfas.iter().map(|d| if d.is_empty() { (0, 0) } else { place(d) }).collect();
    let head_places: Vec<Vec<Option<(u32, u32)>>> =
        heads.iter().map(|hs| hs.iter().map(|h| h.as_ref().map(|h| place(h))).collect()).collect();

    let mut v = Vec::with_capacity(data_off + blobs.len());
    v.extend_from_slice(&REGION_MAGIC);
    wr_u32(&mut v, n_ctx as u32);
    wr_u32(&mut v, n_rules as u32);
    wr_u32(&mut v, n_push as u32);
    wr_u32(&mut v, ctx_off as u32);
    wr_u32(&mut v, rule_off as u32);
    wr_u32(&mut v, push_off as u32);
    wr_u32(&mut v, words_off as u32);
    wr_u32(&mut v, words.len() as u32);
    wr_u32(&mut v, 0);
    let mut rule_base = 0u32;
    for (ci, rules) in ctx_rules.iter().enumerate() {
        wr_u32(&mut v, ctx_places[ci].0);
        wr_u32(&mut v, ctx_places[ci].1);
        wr_u32(&mut v, rule_base);
        wr_u16(&mut v, rules.len() as u16);
        v.push(default_of(names[ci]));
        v.push(0);
        wr_u16(&mut v, fallbacks[ci].unwrap_or(NO_FALLBACK));
        wr_u16(&mut v, 0);
        rule_base += rules.len() as u32;
    }
    let mut push_idx = 0u32;
    for (ci, rules) in ctx_rules.iter().enumerate() {
        for (ri, r) in rules.iter().enumerate() {
            v.push(r.token);
            v.push(r.pop);
            v.push(r.push.len().min(255) as u8);
            v.push(r.flags);
            wr_u32(&mut v, push_idx);
            let (ho, hl) = head_places[ci][ri].unwrap_or((0, 0));
            wr_u32(&mut v, ho);
            wr_u32(&mut v, hl);
            push_idx += r.push.len() as u32;
        }
    }
    for r in ctx_rules.iter().flatten() {
        for &p in &r.push {
            wr_u16(&mut v, p);
        }
    }
    pad_to(&mut v, 4);
    debug_assert_eq!(v.len(), words_off);
    let mut s_at = wstr_off;
    for (w, t, push) in &words {
        wr_u32(&mut v, s_at as u32);
        wr_u16(&mut v, w.len() as u16);
        v.push(*t);
        v.push(0);
        wr_u16(&mut v, *push);
        wr_u16(&mut v, 0);
        s_at += w.len();
    }
    for (w, _, _) in &words {
        v.extend_from_slice(w);
    }
    pad_to(&mut v, DFA_ALIGN);
    debug_assert_eq!(v.len(), data_off);
    v.extend_from_slice(&blobs);

    Ok(CompiledLanguage { key: key.to_string(), file: file.clone(), region: v, first_line, contexts: stats, head_bytes })
}

/// 全部の言語を 1 つのバイト列にまとめる。
pub fn build_blob(langs: &[CompiledLanguage], build_id: u64) -> Vec<u8> {
    let n = langs.len();
    let dir_off = HEADER_LEN;
    let mut v = Vec::new();
    v.extend_from_slice(&MAGIC);
    wr_u32(&mut v, FORMAT_VERSION);
    wr_u32(&mut v, build_id as u32);
    wr_u32(&mut v, (build_id >> 32) as u32);
    wr_u32(&mut v, n as u32);
    wr_u32(&mut v, dir_off as u32);
    wr_u32(&mut v, 0);
    wr_u32(&mut v, 0);
    // 目録は後で埋める
    v.resize(dir_off + n * DIR_REC_LEN, 0);
    let mut first = Vec::new();
    for l in langs {
        match &l.first_line {
            Some(d) => {
                pad_to(&mut v, DFA_ALIGN);
                first.push((v.len() as u32, d.len() as u32));
                v.extend_from_slice(d);
            }
            None => first.push((0, 0)),
        }
    }
    let mut regions = Vec::new();
    for l in langs {
        pad_to(&mut v, REGION_ALIGN);
        regions.push((v.len() as u32, l.region.len() as u32));
        v.extend_from_slice(&l.region);
    }
    for i in 0..n {
        let at = dir_off + i * DIR_REC_LEN;
        let rec = [regions[i].0, regions[i].1, first[i].0, first[i].1];
        for (k, x) in rec.iter().enumerate() {
            v[at + k * 4..at + k * 4 + 4].copy_from_slice(&x.to_le_bytes());
        }
    }
    v
}

/// `language.toml` の文字列を読む（`toml` が要る。build.rs と道具だけ）。
pub fn parse_language_toml(text: &str) -> Result<LanguageFile, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}
