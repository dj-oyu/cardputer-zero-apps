//! C のジョブ（docs/stage3-design.md §4.4）。課題 B4。
//!
//! 木は、ファイルのあるフォルダの `*.c` と `*.h`（下のフォルダは無い）。`opts.entry` は
//! F5 を押したファイル。
//!
//! | 種類 | 中でやること |
//! |---|---|
//! | build | `aarch64-linux-gnu-gcc -mcpu=cortex-a53 -O2 -Wall -Wextra -fdiagnostics-format=json -o <out>/<entry の名前> *.c -lm`。**`main` を持つ .c が複数あるときは `entry` だけ**をコンパイルする（`main` の有無は `\bmain\s*\(` の簡易判定） |
//! | check | 同じ引数で `-fsyntax-only` |
//! | test | 無い（`error`: 「C の試験は未対応」） |
//!
//! 診断は gcc の JSON（`-fdiagnostics-format=json`。`locations[0].caret` の `line`・
//! `byte-column`（1 始まりのバイト）を使う）。`kind` が error / warning のものだけ。

use super::{JobCtx, JobFailure, JobOutput, JobResult};
use zc_wire::msg::FileDiags;

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    let _ = (ctx, out);
    todo!("B4")
}

/// gcc の JSON の診断（標準エラーに出る 1 つの配列）を読む。
pub fn parse_gcc_json(stderr: &str, src_root: &std::path::Path, into: &mut Vec<FileDiags>) {
    let _ = (stderr, src_root, into);
    todo!("B4")
}

// 受け入れ試験（課題 B4）: 開発コンテナの aarch64-linux-gnu-gcc で、hello → aarch64 の
// 実行ファイル、型エラーの行と桁、警告、main が 2 つあるフォルダ、ヘッダの中のエラー。
