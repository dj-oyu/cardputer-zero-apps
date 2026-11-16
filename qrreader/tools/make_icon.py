#!/usr/bin/env python3
"""APPLaunch 用アイコン (packaging/qrreader.png) を生成する。

生成物なので git には入れない。PIL だけで描く簡素な QR 風アイコン。
"""

import os

from PIL import Image, ImageDraw

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "..", "packaging", "qrreader.png")
ACCENT = (61, 255, 122)


def main() -> int:
    img = Image.new("RGBA", (96, 96), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([4, 4, 92, 92], radius=18, fill=(14, 18, 16, 255),
                        outline=ACCENT, width=3)

    def finder(x, y):
        d.rectangle([x, y, x + 22, y + 22], outline=(255, 255, 255), width=4)
        d.rectangle([x + 7, y + 7, x + 15, y + 15], fill=(255, 255, 255))

    finder(16, 16)
    finder(58, 16)
    finder(16, 58)
    # 右下はデータモジュール風のドット
    for (x, y) in ((58, 58), (72, 58), (58, 72), (80, 72), (66, 80)):
        d.rectangle([x, y, x + 8, y + 8], fill=ACCENT)
    img.save(OUT)
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
