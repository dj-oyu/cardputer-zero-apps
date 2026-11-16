#!/usr/bin/env bash
# zcode を実機 (aarch64) 向けにビルドし、out/zcode に置く (開発コンテナの中で実行)。
#   Windows: cd zcode; ..\container\dev.ps1 ./build.sh
# .deb にまとめるのは段階 6。それまでは out/zcode を scp で実機の /tmp に送って試す:
#   scp out/zcode <実機>:/tmp/ && ssh <実機> 'timeout -s INT 30 /tmp/zcode --fonttest --auto 3000 --stats; rm -f /tmp/zcode'
# 実機で動かすときは必ず timeout を付ける (固まってもキーボードと画面を握ったままにしない)。
set -euo pipefail
cd "$(dirname "$0")"
TARGET=aarch64-unknown-linux-gnu
OUT_DIR="${CARGO_TARGET_DIR:-target}/$TARGET/release"
cargo build -q --release --locked --target "$TARGET" -p zcode
mkdir -p out
cp "$OUT_DIR/zcode" out/zcode
ls -l out/zcode
