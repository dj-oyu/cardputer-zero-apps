#!/usr/bin/env python3
"""zcode のランチャー用アイコン（100x100 PNG）を作る。要: Pillow、DejaVu Sans Mono Bold。

  python3 tools/make_icon.py            → packaging/zcode.png（正式版）
  python3 tools/make_icon.py --dev OUT  → OUT（右下に橙の DEV の札。開発用の起動口）

生成物なので git には入れない（ほかのアプリと同じ流儀）。
配色は VS Code の Dark+（背景 #1e1e1e、アクセント #007acc）。
"""

import os
import sys

from PIL import Image, ImageDraw, ImageFont

SIZE = 100
BG = (30, 30, 30)
FRAME = (60, 60, 60)
BLUE = (0, 122, 204)
LIGHT_BLUE = (78, 201, 176)
ORANGE = (204, 102, 51)
WHITE = (240, 240, 240)
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"


def make(dev: bool) -> Image.Image:
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle((2, 2, SIZE - 3, SIZE - 3), radius=18, fill=BG, outline=FRAME, width=2)
    # エディタらしく、上に細いタブの帯
    d.rounded_rectangle((12, 14, 52, 22), radius=3, fill=BLUE)
    d.rounded_rectangle((56, 14, 86, 22), radius=3, fill=FRAME)
    # 中央の </>
    big = ImageFont.truetype(FONT, 34)
    text = "</>"
    w = d.textlength(text, font=big)
    d.text(((SIZE - w) / 2, 30), text, font=big, fill=LIGHT_BLUE)
    # 下に名前
    small = ImageFont.truetype(FONT, 15)
    name = "zcode"
    w = d.textlength(name, font=small)
    d.text(((SIZE - w) / 2, 70), name, font=small, fill=WHITE)
    if dev:
        tag = ImageFont.truetype(FONT, 11)
        d.rounded_rectangle((58, 2, SIZE - 3, 18), radius=6, fill=ORANGE)
        w = d.textlength("DEV", font=tag)
        d.text((58 + (SIZE - 3 - 58 - w) / 2, 3), "DEV", font=tag, fill=WHITE)
    return img


def main() -> int:
    here = os.path.dirname(os.path.abspath(__file__))
    if len(sys.argv) >= 3 and sys.argv[1] == "--dev":
        out, dev = sys.argv[2], True
    else:
        out, dev = os.path.join(here, "..", "packaging", "zcode.png"), False
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    make(dev).save(out)
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
