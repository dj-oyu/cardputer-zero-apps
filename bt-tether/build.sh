#!/usr/bin/env bash
# BT Tether の .deb を組み立てる。
#   usage: ./build.sh <version>     例: ./build.sh 0.1
# 出力: bt-tether-lcd_<version>_arm64.deb (このディレクトリ)
# .deb は単体で完結する形。共通部品 ../common/zero_ui.py は
# ビルド時に lib へコピーして同梱する。
set -euo pipefail
cd "$(dirname "$0")"
VERSION="${1:?usage: build.sh <version>}"
APP="usr/share/APPLaunch"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

# 生成アセットが無ければ作る (要: Pillow)
[ -f packaging/bt-tether.png ] || python3 tools/make_icon.py

mkdir -p "$STAGE/$APP/bin" "$STAGE/$APP/lib/bt-tether" \
         "$STAGE/$APP/applications" "$STAGE/$APP/share/images" \
         "$STAGE/DEBIAN" "$STAGE/etc/sudoers.d"
sed "s/^Version:.*/Version: $VERSION/" packaging/control \
    > "$STAGE/DEBIAN/control"
cp packaging/postinst "$STAGE/DEBIAN/postinst"
cp packaging/bt-tether.sudoers "$STAGE/etc/sudoers.d/bt-tether"
cp packaging/bt-tether-lcd "$STAGE/$APP/bin/bt-tether-lcd"
cp packaging/bt-tether-lcd.desktop "$STAGE/$APP/applications/"
cp packaging/bt-tether.png "$STAGE/$APP/share/images/bt-tether.png"
cp bt_tether.py bt_tether_lcd.py "$STAGE/$APP/lib/bt-tether/"
cp ../common/zero_ui.py "$STAGE/$APP/lib/bt-tether/"

find "$STAGE" -type d -exec chmod 755 {} +
find "$STAGE" -type f -exec chmod 644 {} +
chmod 755 "$STAGE/DEBIAN/postinst" "$STAGE/$APP/bin/bt-tether-lcd"
chmod 440 "$STAGE/etc/sudoers.d/bt-tether"

OUT="bt-tether-lcd_${VERSION}_arm64.deb"
dpkg-deb --build "$STAGE" "$OUT"
echo "built $OUT"
