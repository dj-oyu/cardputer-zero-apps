#!/usr/bin/env bash
# ベンチマークを aarch64 向けにビルドし、bench/out/ に置く (開発コンテナの中で実行)。
#   Windows: cd zcode\bench; ..\..\container\dev.ps1 ./build.sh [名前...]
#   名前を省くと全部。出来たバイナリは scp で実機の /tmp に送って実行し、終わったら消す。
set -euo pipefail
cd "$(dirname "$0")"
TARGET=aarch64-unknown-linux-gnu
OUT_DIR="${CARGO_TARGET_DIR:-target}/$TARGET/release"
names=("$@")
[ ${#names[@]} -eq 0 ] && names=(mcbench glyphbench hlbench dfabench)
mkdir -p out
for n in "${names[@]}"; do
    (cd "$n" && cargo build -q --release --locked --target "$TARGET")
    cp "$OUT_DIR/$n" "out/$n"
    echo "built out/$n"
done
