//! zc-highlight の計測（measurements.md §6 の dfabench の続き）。
//!
//! ```sh
//! cargo run --release -p zc-highlight --example hlbench [ファイル] [行数]
//! # 実機: aarch64 でビルドして /tmp に置き、timeout を付けて動かす
//! ```
//!
//! 出すもの: 埋め込みの表の読み込み（Registry の初回）、言語の初回の読み込み（prepare）、
//! 全行の行頭の文脈の計算（update）、見えている 13 行の区間（spans）、1 行の編集の
//! 再計算、batch の compute_rest、トークン数。

use std::path::Path;
use std::time::Instant;
use zc_core::{CancelToken, LineChange, LineRange, LineSource};
use zc_highlight::{Highlighter, Registry};

struct Lines(Vec<String>);
impl LineSource for Lines {
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

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().cloned().unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../../languages/python/sample.py").to_string());
    let want: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3000);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let base: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    let lines: Vec<String> = base.iter().cycle().take(want).cloned().collect();
    let src = Lines(lines);
    let n = src.0.len();

    let t = Instant::now();
    let reg = Registry::builtin();
    let t_reg = ms(t);
    let id = reg.detect(Path::new(&path), &base[0]).expect("言語が決まらない");
    let t = Instant::now();
    assert!(reg.prepare(id));
    let t_prep = ms(t);
    println!("blob {} B, {} region {} B", Registry::blob_bytes(), reg.info(id).key, reg.stats(id).region_bytes);
    println!("first use: Registry::builtin {:.3} ms, prepare({}) {:.3} ms", t_reg, reg.info(id).key, t_prep);

    for round in 1..=3 {
        let mut h = Highlighter::new(Some(id));
        let t = Instant::now();
        let r = h.update(&src, u32::MAX);
        let el = ms(t);
        assert!(r.complete);
        // トークン数
        let mut spans = Vec::new();
        let mut toks = 0usize;
        let t2 = Instant::now();
        for (i, l) in src.0.iter().enumerate() {
            h.spans(i as u32, l, &mut spans);
            toks += spans.len();
        }
        let el2 = ms(t2);
        println!(
            "round {round}: update {n} lines {:.2} ms ({:.4} ms/line); spans all {:.2} ms ({:.4} ms/line, {:.1} tokens/line, {:.0} ns/token)",
            el,
            el / n as f64,
            el2,
            el2 / n as f64,
            toks as f64 / n as f64,
            el2 * 1e6 / toks as f64
        );
    }

    let mut h = Highlighter::new(Some(id));
    h.update(&src, u32::MAX);
    let mut spans = Vec::new();
    let t = Instant::now();
    for i in 100..113 {
        h.spans(i, &src.0[i as usize], &mut spans);
    }
    println!("visible 13 lines spans: {:.4} ms", ms(t));

    // 1 行の編集（文脈が変わらない）→ 打ち切り
    let mut src2 = Lines(src.0.clone());
    src2.0[50] = "x = 1  # edited".into();
    h.edited(LineChange { old: LineRange::new(50, 51), new: LineRange::new(50, 51) });
    let t = Instant::now();
    let r = h.update(&src2, zc_highlight::MAIN_BUDGET_LINES);
    println!("edit line 50 (no state change): update {:.4} ms, complete={}", ms(t), r.complete);
    // 三重引用符を開く → 下が全部変わる。メインは予算まで、残りは batch
    src2.0[50] = "x = '''".into();
    h.edited(LineChange { old: LineRange::new(50, 51), new: LineRange::new(50, 51) });
    let t = Instant::now();
    let r = h.update(&src2, zc_highlight::MAIN_BUDGET_LINES);
    println!("edit line 50 (opens '''): main budget {} lines {:.2} ms, valid={} complete={}", zc_highlight::MAIN_BUDGET_LINES, ms(t), r.valid_lines, r.complete);
    if let Some((from, st)) = h.rest_request() {
        let t = Instant::now();
        let rest = Highlighter::compute_rest(id, &src2, from, st, &CancelToken::never()).unwrap();
        println!("compute_rest from {from}: {} lines {:.2} ms", rest.value.states.len(), ms(t));
    }
    println!("context stacks seen: {:?}", zc_highlight::debug_counts(id));
}
