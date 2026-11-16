//! Rust のジョブ（docs/stage3-design.md §4.4）。課題 B3。
//!
//! | 種類 | 中でやること |
//! |---|---|
//! | build | `cargo build --target aarch64-unknown-linux-gnu --profile zc-f5 --message-format=json-diagnostic-rendered-ansi`（`release` のときは `--release`）。実行ファイルが 1 つならそれ、複数なら `opts.entry` か `default-run`、決まらなければ診断「実行ファイルが複数ある」 |
//! | check | `cargo check --target aarch64-unknown-linux-gnu --all-targets --message-format=json` |
//! | test | `cargo test --no-fail-fast --message-format=json`（**x86_64 のまま**）+ libtest の出力を読む |
//!
//! - **F5 用のプロファイル `zc-f5`**: `inherits = "dev"`・`opt-level = 2`・`debug = 0`・
//!   `strip = true`・`lto = false`・`incremental = true`・`codegen-units = 16`。プロジェクトの
//!   `Cargo.toml` は書き換えず、`--config 'profile.zc-f5.…'` で渡す
//! - `RUSTFLAGS` 相当も `--config target.aarch64-unknown-linux-gnu.rustflags=["-Ctarget-cpu=cortex-a53"]`
//! - `CARGO_TARGET_DIR` は `<project_dir>/target`。`--locked` は付けない: `Cargo.lock` が
//!   無い・古いときは cargo が作り直すので、**変わっていたら [`super::JobResult::write_back`]
//!   に入れて実機へ返す**（無いと job id が安定しない）
//! - `Cargo.toml` の無い「1 ファイルだけ」（`opts.entry` = `main.rs`）: `<project_dir>/single/`
//!   に、`[package] name = <ファイル名>`・`edition = "2021"`・`[[bin]] path = <src>/<entry>` の
//!   `Cargo.toml` を作って、同じ手順で回す
//! - ネットワーク: crates.io から依存を取る（コンテナから外へ出られること）。`build.rs` と
//!   手続きマクロはこのコンテナの中で動く
//!
//! # 診断（rustc の JSON → [`zc_wire::msg::Diag`]）
//!
//! - `reason = "compiler-message"` の `message` を読む。`level` が error / warning のものだけ
//!   （note・help は親のメッセージの後ろに 1 行で足す。合計 400 バイトまで）
//! - 位置は `is_primary` の span。**行は 1 始まり → 0 始まり。桁は 1 始まりの「文字数」→
//!   0 始まりの「行の中の UTF-8 のバイト位置」**（span の `text[].text` がその行の中身なので、
//!   そこから数え直す。日本語のコメントの後ろでずれないように）
//! - マクロの展開先・依存 crate の中を指す span は、`expansion` をたどって**手元の
//!   呼び出し元**の位置に寄せる。作業の木の外にしか位置が無ければ `path = ""`（位置なし）
//! - パスは木の根からの相対・`/` 区切りに直す（cargo は crate の根からの相対で出す）
//! - 同じ（パス・行・桁・メッセージ）は 1 つにする（lib と test で 2 回出る）
//! - 上限 [`zc_wire::limits::MAX_DIAGNOSTICS`]（error を先に残す）

use super::{JobCtx, JobFailure, JobOutput, JobResult, TestResults};
use std::path::Path;
use zc_wire::msg::FileDiags;

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    let _ = (ctx, out);
    todo!("B3")
}

/// cargo の `--message-format=json` の 1 行を読んで、診断を `into` に足す。
/// `crate_root_hint` は、相対パスを木の根からに直すための crate の根（`manifest_path` から）。
pub fn parse_cargo_message(line: &str, src_root: &Path, into: &mut Vec<FileDiags>) {
    let _ = (line, src_root, into);
    todo!("B3")
}

/// 1 始まりの文字数の桁を、その行の中の UTF-8 のバイト位置（0 始まり）に直す。
/// 行より先を指していたら行末。
pub fn char_col_to_byte(line_text: &str, col_1based_chars: u32) -> u32 {
    let _ = (line_text, col_1based_chars);
    todo!("B3")
}

/// libtest の出力（`test foo::bar ... ok` / `FAILED` / `ignored`、`---- name stdout ----`、
/// `panicked at src/x.rs:12:5:`）から結果を作る。
pub fn parse_libtest(output: &str, src_root: &Path) -> TestResults {
    let _ = (output, src_root);
    todo!("B3")
}

#[cfg(test)]
mod tests {
    use super::*;

    // 受け入れ試験（課題 B3）。tests/data/ に rustc の JSON の実物を置いて突き合わせる:
    // 型エラー（E0308）、日本語のコメントの後ろのエラー（桁がバイト位置）、マクロの中の
    // エラー（呼び出し元に寄る）、依存 crate だけを指す（位置なし）、警告、lib と test の重複。
    // 通しの試験（開発コンテナに cargo と aarch64 のリンカがある。依存なしの crate なので
    // ネットワークは要らない）: build → `file` で aarch64 の実行ファイル、check でエラーの
    // 行と桁、test で通った・落ちた数と落ちた場所、1 ファイルだけ、Cargo.lock の書き戻し。
    #[test]
    #[ignore = "B3"]
    fn columns_are_utf8_byte_offsets() {
        // 「日本語」は 3 文字 9 バイト。8 文字目（1 始まり）の x は 13 バイト目（0 始まり）
        assert_eq!(char_col_to_byte("// 日本語 x", 8), 13);
        assert_eq!(char_col_to_byte("let x = 1;", 5), 4);
        assert_eq!(char_col_to_byte("ab", 99), 2);
    }
}
