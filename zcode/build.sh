#!/usr/bin/env bash
# zcode を実機 (aarch64) 向けにビルドし、out/zcode に置く (開発コンテナの中で実行)。
#   Windows: cd zcode; ..\container\dev.ps1 ./build.sh [<version>]
# 版を渡すと .deb にもまとめる: zcode_<version>_arm64.deb (このディレクトリ)
#   ..\container\dev.ps1 ./build.sh 0.1.0
# .deb を入れずに試すときは out/zcode を scp で実機の /tmp に送る:
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

VERSION="${1:-}"
[ -n "$VERSION" ] || exit 0

# --- .deb (APPLaunch の流儀: /usr/share/APPLaunch/{bin,lib/zcode,applications,share/images}) ---
APP="usr/share/APPLaunch"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
# 生成アセットが無ければ作る (要: Pillow、DejaVu Sans Mono Bold)
[ -f packaging/zcode.png ] || python3 tools/make_icon.py
mkdir -p "$STAGE/$APP/bin" "$STAGE/$APP/lib/zcode" "$STAGE/$APP/applications" "$STAGE/$APP/share/images" "$STAGE/DEBIAN"
sed "s/^Version:.*/Version: $VERSION/" packaging/control > "$STAGE/DEBIAN/control"
cp packaging/postinst "$STAGE/DEBIAN/postinst"
cp packaging/zcode "$STAGE/$APP/bin/zcode"
cp packaging/zcode.desktop "$STAGE/$APP/applications/"
cp packaging/zcode.png "$STAGE/$APP/share/images/zcode.png"
cp out/zcode "$STAGE/$APP/lib/zcode/zcode"
find "$STAGE" -type d -exec chmod 755 {} +
find "$STAGE" -type f -exec chmod 644 {} +
chmod 755 "$STAGE/DEBIAN/postinst" "$STAGE/$APP/bin/zcode" "$STAGE/$APP/lib/zcode/zcode"
DEB="zcode_${VERSION}_arm64.deb"
dpkg-deb --root-owner-group --build "$STAGE" "$DEB"
echo "built $DEB"
