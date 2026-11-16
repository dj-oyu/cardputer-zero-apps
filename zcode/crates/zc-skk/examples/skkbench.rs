//! システム辞書（SKK-JISYO.L）を開く時間と探索の時間を測る。
//!
//! ```text
//! cargo run --release -p zc-skk --example skkbench -- <辞書> [N] [--check]
//! ```
//!
//! - 冷えた状態（ページキャッシュに無い）は `posix_fadvise(DONTNEED)` で作る（root は要らない。
//!   ほかのプロセスが mmap しているページは落ちない）
//! - `open`: mmap と区画の見出し探し（送りありの区画を 1 度読む）
//! - 冷えた状態で開いた直後の 1 回目の探索（送りなし・送りあり）を数回
//! - 温まった状態で N 回（辞書から均等に選んだ読み、両方の区画）と、無い読みを N 回
//! - `prefetch`（madvise WILLNEED）を呼ぶ時間と、1 秒後の探索
//! - `--check`: 辞書の全項目を探索して、見つからないもの（並びの崩れ）を数える

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};
use zc_skk::SystemDict;

fn drop_cache(path: &Path) {
    let f = File::open(path).expect("辞書を開けない");
    // SAFETY: 有効な fd への助言だけ。
    let r = unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    if r != 0 {
        eprintln!("posix_fadvise: {r}");
    }
}

/// 辞書の全項目の読み（UTF-8, 送りありか）。
fn entries(bytes: &[u8]) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut sec = 0;
    for line in bytes.split(|&b| b == b'\n') {
        if line == b";; okuri-ari entries." {
            sec = 1;
            continue;
        }
        if line == b";; okuri-nasi entries." {
            sec = 2;
            continue;
        }
        if sec == 0 || line.is_empty() || line[0] == b';' {
            continue;
        }
        let Some(sp) = line.iter().position(|&b| b == b' ') else { continue };
        let (s, bad) = encoding_rs::EUC_JP.decode_without_bom_handling(&line[..sp]);
        if !bad {
            out.push((s.into_owned(), sec == 1));
        }
    }
    out
}

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

fn summary(name: &str, mut v: Vec<Duration>) {
    v.sort();
    let n = v.len();
    let total: Duration = v.iter().sum();
    println!(
        "{name:<28} n={n:<6} p50 {:>8.1}us  p95 {:>8.1}us  max {:>8.1}us  合計 {:>8.2}ms",
        us(v[n / 2]),
        us(v[(n * 95 / 100).min(n - 1)]),
        us(v[n - 1]),
        total.as_secs_f64() * 1e3
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = args.iter().any(|a| a == "--check");
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let Some(path) = pos.first() else {
        eprintln!("使い方: skkbench <辞書> [N] [--check]");
        std::process::exit(2);
    };
    let path = Path::new(path.as_str());
    let n: usize = pos.get(1).and_then(|s| s.parse().ok()).unwrap_or(10_000);

    let bytes = std::fs::read(path).expect("辞書を読めない");
    let all = entries(&bytes);
    let n_ari = all.iter().filter(|e| e.1).count();
    println!("{}: {} バイト、送りあり {} 項目、送りなし {} 項目", path.display(), bytes.len(), n_ari, all.len() - n_ari);
    drop(bytes);
    let step = (all.len() / n.max(1)).max(1);
    let sample: Vec<&(String, bool)> = all.iter().step_by(step).take(n).collect();
    let pick_nasi: Vec<&(String, bool)> = all.iter().filter(|e| !e.1).step_by(9_973).take(5).collect();
    let pick_ari: Vec<&(String, bool)> = all.iter().filter(|e| e.1).step_by(1_013).take(5).collect();

    // 冷えた状態で開く・最初の探索
    let mut open_t = Vec::new();
    let mut first_nasi = Vec::new();
    let mut first_ari = Vec::new();
    for i in 0..5 {
        drop_cache(path);
        let t = Instant::now();
        let d = SystemDict::open(path).expect("open");
        open_t.push(t.elapsed());
        let (r, _) = pick_nasi[i % pick_nasi.len()];
        let t = Instant::now();
        let c = d.lookup(r, false);
        first_nasi.push(t.elapsed());
        assert!(!c.is_empty() || r.is_empty(), "{r}");
        let (r, _) = pick_ari[i % pick_ari.len()];
        let t = Instant::now();
        let _ = d.lookup(r, true);
        first_ari.push(t.elapsed());
    }
    summary("open（冷えた状態）", open_t);
    summary("1 回目の探索 送りなし（冷）", first_nasi);
    summary("1 回目の探索 送りあり", first_ari);

    // 温まった状態
    let d = SystemDict::open(path).expect("open");
    for (r, o) in &sample {
        let _ = d.lookup(r, *o);
    }
    let mut hit = Vec::with_capacity(sample.len());
    let mut cands = 0usize;
    for (r, o) in &sample {
        let t = Instant::now();
        let c = d.lookup(r, *o);
        hit.push(t.elapsed());
        cands += c.len();
    }
    summary("探索（温、見つかる）", hit);
    println!("  候補の平均 {:.1} 個", cands as f64 / sample.len() as f64);
    let mut miss = Vec::with_capacity(sample.len());
    for (r, o) in &sample {
        let q = format!("{r}ゐゑ");
        let t = Instant::now();
        let c = d.lookup(&q, *o);
        miss.push(t.elapsed());
        assert!(c.is_empty());
    }
    summary("探索（温、無い読み）", miss);
    drop(d);

    // prefetch
    drop_cache(path);
    let d = SystemDict::open(path).expect("open");
    let t = Instant::now();
    d.prefetch();
    println!("prefetch の呼び出し {:.1}us", us(t.elapsed()));
    std::thread::sleep(Duration::from_secs(1));
    let mut after = Vec::new();
    for (r, o) in sample.iter().take(200) {
        let t = Instant::now();
        let _ = d.lookup(r, *o);
        after.push(t.elapsed());
    }
    summary("prefetch の 1 秒後の探索", after);

    if check {
        let t = Instant::now();
        let mut empty = Vec::new();
        for (r, o) in &all {
            if d.lookup(r, *o).is_empty() {
                empty.push(r.clone());
            }
        }
        println!(
            "全項目の確認: {} 項目を {:.0}ms、候補が空 {} 項目（見つからない、または全部が Lisp の式）",
            all.len(),
            t.elapsed().as_secs_f64() * 1e3,
            empty.len()
        );
        for r in empty.iter().take(20) {
            println!("  空: {r}");
        }
    }
}
