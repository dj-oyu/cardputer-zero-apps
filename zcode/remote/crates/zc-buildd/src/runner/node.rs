//! JavaScript / TypeScript（npm の依存あり）のジョブ（docs/stage3-design.md §4.4）。課題 B4。
//!
//! 木の根は `package.json` のあるフォルダ。`opts.entry` は F5 を押したファイル。
//!
//! | 種類 | 中でやること |
//! |---|---|
//! | （共通） | `package-lock.json` があれば `npm ci --ignore-scripts --no-audit --no-fund`、無ければ `npm install --ignore-scripts --package-lock-only` で作ってから `npm ci`（作った lock は書き戻す）。`node_modules` は `<project_dir>/node_modules` に置き、lock の SHA-256 が前回と同じなら飛ばす |
//! | check | `tsc --noEmit --pretty false`（`tsconfig.json` があればそれ、無ければ実機と同じ既定: `--strict --target es2022 --module esnext --moduleResolution bundler --skipLibCheck`、JS は `--allowJs --checkJs`） |
//! | build | check と同じ型検査 → 通ったら `esbuild <entry> --bundle --platform=node --format=esm --target=es2022 --outfile=<out>/<名前>.js`（LLRT が持っている `node:*` は外に残す）。成果物の種類は `Js` |
//! | test | `npm test`（`package.json` に `scripts.test` があるとき。**`--ignore-scripts` は install だけ**で、試験は明示して頼まれたものなので動かす）。結果は終了コードと出力だけ（件数は 0 / 1） |
//!
//! `postinstall` などのインストールスクリプトは動かさない。動かさないと使えない依存
//! （ネイティブのアドオン）は、どのみち LLRT で動かない。
//!
//! 診断は tsc の出力 `path(行,桁): error TS2322: …`（行・桁は 1 始まり。**桁は UTF-16 の
//! 単位**なので、その行を読んで UTF-8 のバイト位置に直す）。

use super::{JobCtx, JobFailure, JobOutput, JobResult};
use zc_wire::msg::FileDiags;

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    let _ = (ctx, out);
    todo!("B4")
}

/// tsc の `--pretty false` の出力を読む。
pub fn parse_tsc(output: &str, src_root: &std::path::Path, into: &mut Vec<FileDiags>) {
    let _ = (output, src_root, into);
    todo!("B4")
}

/// 1 始まりの UTF-16 の桁を、その行の中の UTF-8 のバイト位置（0 始まり）に直す。
pub fn utf16_col_to_byte(line_text: &str, col_1based_utf16: u32) -> u32 {
    let _ = (line_text, col_1based_utf16);
    todo!("B4")
}

// 受け入れ試験（課題 B4）: parse_tsc と桁の変換は正解の文字列で。通しの試験は道具
// （node・npm・tsc・esbuild）があるときだけ（ビルド係のイメージの中。無ければ飛ばす）:
// 依存 1 つのプロジェクトの check（型エラーの位置）、build → 1 ファイルの JS、lock の書き戻し。
