// 「規則をビルド時に DFA へコンパイルしてバイナリに埋め込む」方式の試算。
// Python 風のトークン規則 (文脈 2 つ: 通常 / 三重引用符の文字列) を多パターン DFA に
// して、(1) コンパイル時間 (= ビルド時にやる分)、(2) バイト列からの復元時間
// (= 実機の起動時にやる分)、(3) 300 行のトークン化時間 を測る。
use regex_automata::dfa::{dense, Automaton};
use regex_automata::{Anchored, Input};
use std::time::Instant;

const NORMAL: &[(&str, &str)] = &[
    ("comment", r"#[^\n]*"),
    ("triple_open", r#"(?:[rRbBfFuU]{0,2})(?:'''|""")"#),
    ("string", r#"(?:[rRbBfFuU]{0,2})(?:"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')"#),
    ("number", r"(?:0[xX][0-9a-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|(?:[0-9][0-9_]*)?\.?[0-9][0-9_]*(?:[eE][+-]?[0-9]+)?[jJ]?)"),
    ("keyword", r"(?:False|None|True|and|as|assert|async|await|break|class|continue|def|del|elif|else|except|finally|for|from|global|if|import|in|is|lambda|nonlocal|not|or|pass|raise|return|try|while|with|yield)(?-u:\b)"),
    ("builtin", r"(?:print|len|range|sum|min|max|open|int|float|str|list|dict|set|tuple|isinstance|enumerate|zip|map|filter|sorted|super|self)(?-u:\b)"),
    ("decorator", r"@[A-Za-z_][A-Za-z0-9_.]*"),
    ("ident", r"[A-Za-z_\p{L}][A-Za-z0-9_\p{L}\p{N}]*"),
    ("op", r"[-+*/%=<>!&|^~:.,;()\[\]{}]+"),
    ("space", r"[ \t]+"),
    ("other", r"[^\n]"),
];
const TRIPLE: &[(&str, &str)] = &[
    ("triple_close", r#"(?:'''|""")"#),
    ("string_body", r#"(?:[^'"\\\n]|\\.|'(?:[^']|$)|"(?:[^"]|$))+"#),
    ("other", r"[^\n]"),
];

const SAMPLE: &str = r#"import os, sys
from dataclasses import dataclass

@dataclass
class Point:
    x: float = 0.0
    y: float = 0.0  # 座標

    def dist(self, other: "Point") -> float:
        return ((self.x - other.x) ** 2 + (self.y - other.y) ** 2) ** 0.5

def main(argv):
    pts = [Point(i, i * 2) for i in range(10)]
    total = sum(p.dist(pts[0]) for p in pts)
    print(f"total={total:.3f} {len(argv)}", file=sys.stderr)
    s = '''multi
line'''
    return 0 if total > 1 else 1
"#;

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status").unwrap_or_default().lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0)
}

fn build(rules: &[(&str, &str)]) -> Vec<u8> {
    let pats: Vec<&str> = rules.iter().map(|r| r.1).collect();
    let dfa = dense::Builder::new()
        .configure(dense::Config::new().starts_for_each_pattern(false))
        .build_many(&pats).unwrap();
    let (bytes, pad) = dfa.to_bytes_native_endian();
    bytes[pad..].to_vec()
}

/// 先頭一致で最長のトークンを返す (パターン番号, 終端)
fn next_tok(dfa: &dense::DFA<&[u32]>, hay: &[u8], at: usize) -> Option<(usize, usize)> {
    let input = Input::new(hay).range(at..).anchored(Anchored::Yes).earliest(false);
    dfa.try_search_fwd(&input).ok().flatten().map(|m| (m.pattern().as_usize(), m.offset()))
}

fn main() {
    // (1) ビルド時に相当: コンパイル
    let t = Instant::now();
    let n_bytes = build(NORMAL);
    let t_bytes = build(TRIPLE);
    println!("(1) compile (build-time): {:7.1} ms, sizes normal={} KB triple={} KB",
             t.elapsed().as_secs_f64() * 1e3, n_bytes.len() / 1024, t_bytes.len() / 1024);

    // u32 境界に置き直す (実物では include_bytes! を整列させて置く)
    let to_u32 = |b: &[u8]| -> Vec<u32> {
        b.chunks(4).map(|c| { let mut a = [0u8; 4]; a[..c.len()].copy_from_slice(c); u32::from_ne_bytes(a) }).collect()
    };
    let nw = to_u32(&n_bytes);
    let tw = to_u32(&t_bytes);
    let r0 = rss_kb();
    // (2) 実機の起動時に相当: バイト列から復元 (ゼロコピー)
    let t = Instant::now();
    let as_bytes = |w: &Vec<u32>| unsafe { std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 4) };
    let (normal, _) = dense::DFA::from_bytes(as_bytes(&nw)).unwrap();
    let (triple, _) = dense::DFA::from_bytes(as_bytes(&tw)).unwrap();
    let normal: dense::DFA<&[u32]> = normal;
    let triple: dense::DFA<&[u32]> = triple;
    println!("(2) load from bytes:      {:7.3} ms", t.elapsed().as_secs_f64() * 1e3);

    // (3) 300 行をトークン化 (文脈は行をまたいで引き継ぐ)
    let lines: Vec<String> = SAMPLE.lines().cycle().take(300).map(|l| format!("{l}\n")).collect();
    for round in 1..=3 {
        let t = Instant::now();
        let mut in_triple = false;
        let mut toks = 0usize;
        for l in &lines {
            let hay = l.as_bytes();
            let mut at = 0;
            while at < hay.len() && hay[at] != b'\n' {
                let dfa = if in_triple { &triple } else { &normal };
                match next_tok(dfa, hay, at) {
                    Some((pid, end)) if end > at => {
                        let name = if in_triple { TRIPLE[pid].0 } else { NORMAL[pid].0 };
                        if name == "triple_open" { in_triple = true; }
                        if name == "triple_close" { in_triple = false; }
                        toks += 1;
                        at = end;
                    }
                    _ => { at += 1; }
                }
            }
        }
        let ms = t.elapsed().as_secs_f64() * 1e3;
        println!("(3) round {round}: 300 lines {:6.2} ms ({:.4} ms/line), tokens={toks}", ms, ms / 300.0);
    }
    println!("RSS after load: +{} KB (DFA bytes live in the binary in the real thing)", rss_kb() as i64 - r0 as i64);
}
