//! Markdown の解析の速さを測る（実機で、全体の解析をメインで済ませてよい大きさを決める）。
//!
//! ```text
//! cargo run -p zc-markdown --release --example parsebench
//! ```
//!
//! 約 4KB / 32KB / 128KB の文書について、全体の解析（ms）、1 行ぶんの `line_into`（µs）、
//! 文字列が違う行のその場の解析（µs）、1 行の編集の `edited`（µs）を出す。

use std::time::Instant;
use zc_core::{CancelToken, LineChange, LineRange, LineSource};
use zc_markdown::{MdDocument, MdLine};

struct Doc(Vec<String>);

impl LineSource for Doc {
    fn len_lines(&self) -> u32 {
        self.0.len().max(1) as u32
    }
    fn line(&self, line: u32, out: &mut String) {
        out.clear();
        if let Some(s) = self.0.get(line as usize) {
            out.push_str(s);
        }
    }
}

/// ふつうのメモに近い文書（見出し・段落・箇条書き・引用・コード・表）を `bytes` くらい作る。
fn generate(bytes: usize) -> Doc {
    let section = [
        "## 作業メモ {n}",
        "",
        "CMA は **64MB** しかない。*カメラ*と同時に使うときは注意する。詳細は [development.md](../development.md) を参照。",
        "The quick brown fox jumps over the lazy dog, with `inline code` and a [link](https://example.com/{n}).",
        "",
        "- [x] 基板を発注する",
        "- [ ] ケースを **3D 印刷** する",
        "- [ ] `zcode` を実機で試す",
        "  - 入れ子の項目 {n}",
        "  - もう 1 つ",
        "",
        "> 引用の行。**太字**と*斜体*を含む。",
        "> 2 行目。",
        "",
        "```python",
        "import zero_ui as ui",
        "ui.font(12)",
        "print(\"section {n}\")",
        "```",
        "",
        "1. 番号つき",
        "2. の箇条書き",
        "",
        "| 列 A | 列 B |",
        "|------|------|",
        "| 1    | 2    |",
        "",
    ];
    let mut lines = vec!["# 買い物と作業メモ".to_string(), String::new()];
    let mut size = 0;
    let mut n = 0;
    while size < bytes {
        n += 1;
        for l in section {
            let l = l.replace("{n}", &n.to_string());
            size += l.len() + 1;
            lines.push(l);
        }
    }
    Doc(lines)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    println!("{:>8} {:>6} | {:>9} {:>9} | {:>8} {:>8} | {:>8} {:>8} {:>5}", "bytes", "lines", "parse ms", "(min)", "line µs", "1行 µs", "edit µs", "(max)", "true");
    for target in [4 * 1024, 32 * 1024, 128 * 1024] {
        let mut doc = generate(target);
        let bytes: usize = doc.0.iter().map(|l| l.len() + 1).sum();
        let cancel = CancelToken::never();

        // 全体の解析
        let runs = if target > 64 * 1024 { 7 } else { 21 };
        let mut times = Vec::new();
        let mut md = MdDocument::new();
        for _ in 0..runs {
            let t = Instant::now();
            let parsed = MdDocument::parse(&doc, &cancel).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1e3);
            md.replace(parsed, cancel.version());
        }
        let parse_min = times.iter().cloned().fold(f64::MAX, f64::min);
        let parse_med = median(times);

        // 1 行ぶんの line_into（覚えている結果をそのまま返す）
        let mut out = MdLine::new();
        let mut sink = 0usize;
        let reps = (200_000 / doc.0.len()).max(1);
        let t = Instant::now();
        for _ in 0..reps {
            for (i, l) in doc.0.iter().enumerate() {
                md.line_into(i as u32, l, None, &mut out);
                sink += out.runs.len();
            }
        }
        let line_us = t.elapsed().as_secs_f64() * 1e6 / (reps * doc.0.len()) as f64;

        // 文字列が違う行（未確定文字列を挟んだ行）: その場で 1 行だけ解析する
        let mut alt = Vec::new();
        for l in doc.0.iter().take(200) {
            alt.push(format!("{l}▽てん"));
        }
        let t = Instant::now();
        for _ in 0..20 {
            for (i, l) in alt.iter().enumerate() {
                md.line_into(i as u32, l, Some(i as u32), &mut out);
                sink += out.runs.len();
            }
        }
        let one_us = t.elapsed().as_secs_f64() * 1e6 / (20 * alt.len()) as f64;

        // 1 行の編集（行末に 1 字足す）
        let mut edits = Vec::new();
        let mut asked = 0;
        let step = (doc.0.len() / 300).max(1);
        for i in (0..doc.0.len()).step_by(step) {
            doc.0[i].push('x');
            let c = LineChange { old: LineRange::new(i as u32, i as u32 + 1), new: LineRange::new(i as u32, i as u32 + 1) };
            let t = Instant::now();
            let need = md.edited(&doc, c);
            edits.push(t.elapsed().as_secs_f64() * 1e6);
            if need {
                asked += 1;
                let parsed = MdDocument::parse(&doc, &cancel).unwrap();
                md.replace(parsed, cancel.version());
            }
        }
        let edit_max = edits.iter().cloned().fold(0.0, f64::max);
        let n_edits = edits.len();
        let edit_med = median(edits);

        println!(
            "{:>8} {:>6} | {:>9.3} {:>9.3} | {:>8.3} {:>8.2} | {:>8.2} {:>8.2} {:>2}/{}",
            bytes,
            doc.0.len(),
            parse_med,
            parse_min,
            line_us,
            one_us,
            edit_med,
            edit_max,
            asked,
            n_edits
        );
        std::hint::black_box(sink);
    }
    println!("parse: 全体の解析（中央値と最小）。line: 覚えている行の line_into。1行: 文字列が違う行のその場の解析。");
    println!("edit: 行末に 1 字足したときの edited（中央値と最大）。true: 全体の再解析を頼んだ回数。");
}
