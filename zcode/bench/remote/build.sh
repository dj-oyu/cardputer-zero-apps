#!/usr/bin/env bash
# remotebench を aarch64 向けにビルドし、bench/remote/out/ に置く（開発コンテナの中で実行）。
#   Windows: cd zcode\bench\remote; ..\..\..\container\dev.ps1 ./build.sh
# 使い方は README.md。
set -euo pipefail
cd "$(dirname "$0")"
TARGET=aarch64-unknown-linux-gnu
T="${CARGO_TARGET_DIR:-target}"
cargo build -q --release --locked --target "$TARGET"
mkdir -p out
cp "$T/$TARGET/release/remotebench" out/remotebench
# 実機の CPU (Cortex-A53, ARMv8.0) に無い命令が無条件に使われていないか（ring のアセンブリ）。
if command -v aarch64-linux-gnu-objdump >/dev/null; then
  n=$(aarch64-linux-gnu-objdump -d out/remotebench | grep -cE '\s(ldadd|swp|cas|ldapr)[a-z]*\s' || true)
  echo "remotebench: LSE / RCpc の命令 $n 個"
  [ "$n" -lt 1000 ] || { echo "A53 に無い命令が多すぎる" >&2; exit 1; }
fi
ls -l out/remotebench
