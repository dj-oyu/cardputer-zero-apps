//! 端末の解析の速さ（vte + 画面の更新）を測る。pty は使わない。
//!
//! ```text
//! cargo run --release -p zc-term --example termbench [MB]
//! ```
//!
//! 3 種類の入力を 8KB ずつ流し込んで MB/秒を出す:
//! - `ascii`: 英数字だけの行（ログ・`seq` の出力に近い）
//! - `mixed`: SGR の色付き（16 色・256 色・truecolor）の行と、`ls --color` 風の行、
//!   カーソル移動と行の消去（プロンプトの描き直しに近い）
//! - `cjk`: 日本語が半分ほど入った行（全角の幅の処理）
//!
//! 画面は 53 桁 × 13 行（320×170 の全画面で 6×12 画素の字）、スクロールバック 1000 行。
//! 比較のため、`vte` の解析だけ（字を数えるだけで画面を更新しない）の速さも出す。

use std::time::Instant;
use zc_term::bench::Emulator;

fn ascii(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 128);
    let mut i = 0u64;
    while v.len() < n {
        v.extend_from_slice(format!("line {i}: the quick brown fox jumps over the lazy dog\r\n").as_bytes());
        i += 1;
    }
    v
}

fn mixed(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 256);
    let mut i = 0u32;
    while v.len() < n {
        match i % 4 {
            0 => v.extend_from_slice(
                format!("\x1b[1;34mdir{i}\x1b[0m  \x1b[32mrun.sh\x1b[0m  \x1b[38;5;{}mfile_{i}.py\x1b[0m  notes.txt\r\n", i % 256)
                    .as_bytes(),
            ),
            1 => v.extend_from_slice(
                format!("\x1b[38;2;{};{};200m  File \"main.py\", line {i}, in <module>\x1b[0m\r\n", i % 256, (i * 7) % 256).as_bytes(),
            ),
            2 => v.extend_from_slice(format!("\x1b[01;32muser@zero\x1b[00m:\x1b[01;34m~/code\x1b[00m$ \x1b[K echo {i}\r\n").as_bytes()),
            _ => v.extend_from_slice(format!("\x1b[2K\r[{:>3}%] building\x1b[1A\x1b[1B\x1b[10G\x1b[7mok\x1b[27m\r\n", i % 101).as_bytes()),
        }
        i += 1;
    }
    v
}

fn cjk(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 256);
    let mut i = 0u32;
    while v.len() < n {
        v.extend_from_slice(format!("{i}: 日本語の行をたくさん流して全角の処理を測る abc\r\n").as_bytes());
        i += 1;
    }
    v
}

/// 解析だけ（画面を更新しない）の比較用。字と制御を数えるだけ。
#[derive(Default)]
struct Count(u64);

impl vte::Perform for Count {
    fn print(&mut self, _c: char) {
        self.0 += 1;
    }
    fn execute(&mut self, _b: u8) {
        self.0 += 1;
    }
    fn csi_dispatch(&mut self, _p: &vte::Params, _i: &[u8], _ig: bool, _a: char) {
        self.0 += 1;
    }
}

/// 3 回測って最短の秒数（1 回目はページフォールトなどの温めを含む）。
fn best_of(mut f: impl FnMut()) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..3 {
        let t0 = Instant::now();
        f();
        best = best.min(t0.elapsed().as_secs_f64());
    }
    best
}

fn report(name: &str, bytes: usize, secs: f64) {
    let mb = bytes as f64 / 1e6;
    println!("{name:12} {mb:5.1} MB {:8.1} ms {:7.1} MB/s  ({:.2} ms / 64KB)", secs * 1e3, mb / secs, secs * 1e3 / (mb / 0.065536));
}

fn run(name: &str, data: &[u8]) {
    let t = best_of(|| {
        let mut emu = Emulator::new(53, 13);
        for chunk in data.chunks(8192) {
            emu.feed(chunk);
        }
        assert!(emu.scrollback_len() > 0 && !emu.row(0).is_empty());
    });
    report(name, data.len(), t);
    let t = best_of(|| {
        let mut p = vte::Parser::new();
        let mut c = Count::default();
        for chunk in data.chunks(8192) {
            p.advance(&mut c, chunk);
        }
        assert!(c.0 > 0);
    });
    report(&format!("{name} (vte)"), data.len(), t);
}

fn main() {
    let mb: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(8);
    let n = mb * 1_000_000;
    run("ascii", &ascii(n));
    run("mixed", &mixed(n));
    run("cjk", &cjk(n));
}
