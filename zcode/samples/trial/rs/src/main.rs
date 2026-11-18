//! ビルド係で aarch64 向けに作って実機で動かす（F5）。F6 で試験、Ctrl+H で型、Ctrl+D で定義へ。

mod led;

use led::{Blinker, Pattern};

fn main() {
    let mut b = Blinker::new(Pattern::Heartbeat);
    for tick in 0..8 {
        // 点灯なら ●、消灯なら ○
        print!("{}", if b.step() { "●" } else { "○" });
        if tick == 7 {
            println!();
        }
    }
    println!("点灯した回数: {}", b.lit());
}
