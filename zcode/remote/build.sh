#!/usr/bin/env bash
# 外部ビルドの実行ファイルを作る（開発コンテナの中で実行）。
#   Windows: cd zcode\remote; ..\..\container\dev.ps1 ./build.sh
#
#   out/zc-remote   実機用 (aarch64)。~/.local/bin/zc-remote に置く（docs/stage3-design.md §12）
#   out/zc-buildd   ビルド係の受付 (x86_64)。コンテナのイメージに入れる
set -euo pipefail
cd "$(dirname "$0")"
T="${CARGO_TARGET_DIR:-target}"
DEV=aarch64-unknown-linux-gnu
cargo build -q --release --locked --target "$DEV" -p zc-remote
cargo build -q --release --locked -p zc-buildd
mkdir -p out
cp "$T/$DEV/release/zc-remote" out/zc-remote
cp "$T/release/zc-buildd" out/zc-buildd
# 実機の CPU (Cortex-A53, ARMv8.0) に無い命令が入っていないか（ring のアセンブリ）。
# 数十〜数百は実行時に CPU を見て切り替える作りなので可。数千あれば無条件に使っている。
if command -v aarch64-linux-gnu-objdump >/dev/null; then
  n=$(aarch64-linux-gnu-objdump -d out/zc-remote | grep -cE '\s(ldadd|swp|cas|ldapr)[a-z]*\s' || true)
  echo "zc-remote: LSE / RCpc の命令 $n 個"
  [ "$n" -lt 1000 ] || { echo "A53 に無い命令が多すぎる" >&2; exit 1; }
fi
ls -l out/zc-remote out/zc-buildd
