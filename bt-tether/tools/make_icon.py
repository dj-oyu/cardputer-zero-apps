#!/usr/bin/env python3
"""APPLaunch 用アイコン (packaging/bt-tether.png) を生成する。

生成物なので git には入れない。Bluetooth ルーンを緑で描く。
"""

import os

from PIL import Image, ImageDraw

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "..", "packaging", "bt-tether.png")
ACCENT = (61, 255, 122)


def main() -> int:
    img = Image.new("RGBA", (96, 96), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([4, 4, 92, 92], radius=18, fill=(14, 18, 16, 255),
                        outline=ACCENT, width=3)
    # Bluetooth ルーン (縦棒 + 上下の三角)
    cx = 48
    d.line([(cx, 18), (cx, 78)], fill=(255, 255, 255), width=5)
    d.line([(cx, 18), (70, 35), (cx, 52)], fill=(255, 255, 255), width=5)
    d.line([(cx, 44), (70, 61), (cx, 78)], fill=(255, 255, 255), width=5)
    img.save(OUT)
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
