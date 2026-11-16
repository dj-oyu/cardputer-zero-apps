#!/usr/bin/env bash
# QR Reader の .deb を組み立てる。
#   usage: ./build.sh <version>     例: ./build.sh 3.5.1
# 出力: qrreader-lcd_<version>_arm64.deb (このディレクトリ)
# .deb は単体で完結する形 (APPLaunch の流儀)。共通部品
# ../common/zero_ui.py はビルド時に lib へコピーして同梱する。
set -euo pipefail
cd "$(dirname "$0")"
VERSION="${1:?usage: build.sh <version>}"
APP="usr/share/APPLaunch"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

# 生成アセットが無ければ作る (要: segno / Pillow)
[ -f qrreader_test_qr.png ] || python3 tools/make_test_qr.py
[ -f packaging/qrreader.png ] || python3 tools/make_icon.py

mkdir -p "$STAGE/$APP/bin" "$STAGE/$APP/lib/qrreader" \
         "$STAGE/$APP/applications" "$STAGE/$APP/share/images" \
         "$STAGE/DEBIAN"
sed "s/^Version:.*/Version: $VERSION/" packaging/control \
    > "$STAGE/DEBIAN/control"
cp packaging/postinst "$STAGE/DEBIAN/postinst"
cp packaging/qrreader-lcd "$STAGE/$APP/bin/qrreader-lcd"
cp packaging/qrreader-lcd.desktop "$STAGE/$APP/applications/"
cp packaging/qrreader.png "$STAGE/$APP/share/images/qrreader.png"
cp qrreader.py qrreader_lcd.py qrguide_worker.py qrdecode_worker.py \
   qrreader_test_qr.png "$STAGE/$APP/lib/qrreader/"
cp ../common/zero_ui.py "$STAGE/$APP/lib/qrreader/"

find "$STAGE" -type d -exec chmod 755 {} +
find "$STAGE" -type f -exec chmod 644 {} +
chmod 755 "$STAGE/DEBIAN/postinst" "$STAGE/$APP/bin/qrreader-lcd"

OUT="qrreader-lcd_${VERSION}_arm64.deb"
dpkg-deb --build "$STAGE" "$OUT"
echo "built $OUT"
