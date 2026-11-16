// syntect の読み込みとハイライトのコストを測る。
//   1) 全言語入りのバイナリ (syntect 同梱の packdump) を読み込む時間と RSS
//   2) Python だけを抜き出して自前でダンプし直したものを読み込む時間
//   3) YAML (.sublime-syntax テキスト) から読み込む時間 (引数でファイルを渡したとき)
//   4) Python の見本 300 行を、初回 (正規表現のコンパイル込み) と 2 回目でハイライト
use std::time::Instant;
use syntect::dumps::{dump_binary, from_binary};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::{SyntaxDefinition, SyntaxSet, SyntaxSetBuilder};

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(0)
}

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

fn main() {
    let r0 = rss_kb();
    let t = Instant::now();
    let all = SyntaxSet::load_defaults_newlines();
    println!("1) all-languages dump load: {:6.1} ms, syntaxes={}, RSS +{} KB",
             t.elapsed().as_secs_f64() * 1e3, all.syntaxes().len(), rss_kb() - r0);

    // 2) Python だけのダンプ: 全言語セットから Python を含むセットを作り直すのは
    //    API 的に難しいので、YAML があればそれを使う。無ければ全言語ダンプのサイズだけ表示
    let all_bin = dump_binary(&all);
    println!("   all-languages dump size: {} KB (compressed)", all_bin.len() / 1024);

    if let Some(path) = std::env::args().nth(1) {
        let text = std::fs::read_to_string(&path).unwrap();
        let t = Instant::now();
        let def = SyntaxDefinition::load_from_str(&text, true, None).unwrap();
        let mut b = SyntaxSetBuilder::new();
        b.add_plain_text_syntax();
        b.add(def);
        let one = b.build();
        println!("3) YAML parse + link (1 language): {:6.1} ms ({} KB text)",
                 t.elapsed().as_secs_f64() * 1e3, text.len() / 1024);
        let bin = dump_binary(&one);
        let t = Instant::now();
        let back: SyntaxSet = from_binary(&bin);
        println!("2) binary load (same 1 language): {:6.1} ms ({} KB), syntaxes={}",
                 t.elapsed().as_secs_f64() * 1e3, bin.len() / 1024, back.syntaxes().len());
    }

    let ts = ThemeSet::load_defaults();
    let theme = &ts.themes["base16-ocean.dark"];
    let syn = all.find_syntax_by_extension("py").unwrap();
    let lines: Vec<&str> = SAMPLE.lines().cycle().take(300).collect();
    for round in 1..=2 {
        let r = rss_kb();
        let t = Instant::now();
        let mut h = HighlightLines::new(syn, theme);
        let mut first_line = 0.0;
        for (i, l) in lines.iter().enumerate() {
            let line = format!("{l}\n");
            let _ = h.highlight_line(&line, &all).unwrap();
            if i == 0 { first_line = t.elapsed().as_secs_f64() * 1e3; }
        }
        let ms = t.elapsed().as_secs_f64() * 1e3;
        println!("4) round {round}: 300 lines {:6.1} ms ({:.2} ms/line), first line {:5.1} ms, RSS +{} KB",
                 ms, ms / 300.0, first_line, rss_kb() as i64 - r as i64);
    }
    println!("final RSS: {} KB", rss_kb());
}
