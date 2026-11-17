#!/usr/bin/env bash
# zcode に同梱するコードのフォントを取ってきて out/fonts/ に置く（build.sh・CI から）。
#   英字: Cozette（MIT）。BDF を、エディタで使う範囲の字だけに減らす（tools/subset_bdf.py）
#   日本語: PixelMplus12（M+ FONT LICENSE）。PixelMplus の zip から 12px の Regular だけ
# 取ってきたものは SHA-256 で確かめる（版を上げるときは URL と値を一緒に書き換える）。
# 一度取ったものは out/fonts-src/ に残し、次からは取りに行かない。
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=out/fonts-src
OUT=out/fonts
mkdir -p "$SRC" "$OUT"

fetch() { # <url> <保存先> <sha256>
    local url=$1 dst=$2 sum=$3
    if [ ! -f "$dst" ] || ! echo "$sum  $dst" | sha256sum -c --quiet - 2>/dev/null; then
        curl -sSfL --retry 3 -o "$dst.part" "$url"
        mv "$dst.part" "$dst"
    fi
    echo "$sum  $dst" | sha256sum -c --quiet - || { echo "SHA-256 が合わない: $url" >&2; rm -f "$dst"; exit 1; }
}

fetch https://github.com/slavfox/Cozette/releases/download/v.1.30.0/cozette.bdf \
    "$SRC/cozette-1.30.0.bdf" 8d740166af3a14053773ac7a8846bf288c8818a44f73ab28d4725d91b03a5639
fetch https://raw.githubusercontent.com/slavfox/Cozette/v.1.30.0/LICENSE \
    "$SRC/cozette-1.30.0-LICENSE" 5d7f0dad37ceef2dc1e9c23e85fbf18629c4eaffe9a919fa4f5b618b46766d23
fetch https://github.com/itouhiro/PixelMplus/releases/download/v1.0.0/PixelMplus-20130602.zip \
    "$SRC/PixelMplus-20130602.zip" ca4b5eecc9cf869e85d480000084ecc0fb24c16fc8b37b4046145ecd642fae11

python3 tools/subset_bdf.py "$SRC/cozette-1.30.0.bdf" "$OUT/cozette.bdf"
cp "$SRC/cozette-1.30.0-LICENSE" "$OUT/LICENSE-Cozette"
python3 - "$SRC/PixelMplus-20130602.zip" "$OUT" <<'PY'
import sys, zipfile
z = zipfile.ZipFile(sys.argv[1])
d = 'PixelMplus-20130602/'
for name, out in [('PixelMplus12-Regular.ttf', 'PixelMplus12-Regular.ttf'),
                  ('mplus_bitmap_fonts/LICENSE_E', 'LICENSE-PixelMplus'),
                  ('mplus_bitmap_fonts/LICENSE_J', 'LICENSE-PixelMplus.ja'),
                  ('README.txt', 'README-PixelMplus.txt')]:
    open(f'{sys.argv[2]}/{out}', 'wb').write(z.read(d + name))
PY
echo "02f19467ea7cc235cc06c570b7f6c3b0a12a6f682bc8d74c43f2d323d97bcd12  $OUT/PixelMplus12-Regular.ttf" | sha256sum -c --quiet -
ls -l "$OUT"
