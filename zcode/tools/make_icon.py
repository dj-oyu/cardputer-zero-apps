#!/usr/bin/env python3
"""zcode のランチャー用アイコン（100x100 PNG）を作る。要: Pillow、DejaVu Sans Mono Bold。

  python3 tools/make_icon.py            → packaging/zcode.png（正式版）
  python3 tools/make_icon.py --dev OUT  → OUT（右上にツヤのある DEV の札。開発用の起動口）

生成物なので git には入れない（ほかのアプリと同じ流儀）。

見た目: Y2K 風のツヤツヤしたボタン。モチーフはエディタ（上にタブの帯、中央に `</>`、
下に名前）。水色→青のグラデーションの地に、上半分の光沢、濃紺の縁と内側の光の縁、
白→水色の文字に縁取りと影、きらめきの星。4 倍の大きさで描いてから縮小して縁を
なめらかにする。
"""

import os
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

SIZE = 100
S = 4  # 描くときの倍率
W = SIZE * S
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"

NAVY = (10, 26, 92)
TOP = (120, 228, 255)     # 水色
BOTTOM = (16, 70, 235)    # 鮮やかな青
WHITE = (255, 255, 255)
TEXT_TOP = (255, 255, 255)
TEXT_BOTTOM = (150, 236, 255)


def vgrad(size, top, bottom):
    """縦のグラデーション（RGBA）。"""
    w, h = size
    g = Image.new("RGBA", (1, h))
    for y in range(h):
        t = y / max(h - 1, 1)
        g.putpixel((0, y), tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3)) + (255,))
    return g.resize((w, h))


def rounded_mask(size, box, radius):
    m = Image.new("L", size, 0)
    ImageDraw.Draw(m).rounded_rectangle(box, radius=radius, fill=255)
    return m


def gloss(img, box, radius, strength=150):
    """box の上半分に、上ほど明るい白い光沢を重ねる。"""
    x0, y0, x1, y1 = box
    h = (y1 - y0) // 2
    alpha = Image.new("L", img.size, 0)
    ad = ImageDraw.Draw(alpha)
    for i in range(h):
        a = int(strength * (0.35 + 0.65 * (1 - i / h) ** 1.2))
        ad.line((x0, y0 + i, x1, y0 + i), fill=a)
    # 光沢の形: 角の丸い四角の上半分を、下側が少し膨らんだ楕円で切る
    shape = Image.new("L", img.size, 0)
    sd = ImageDraw.Draw(shape)
    inset = (x1 - x0) // 30
    sd.rounded_rectangle((x0 + inset, y0 + inset, x1 - inset, y0 + h + h // 3), radius=radius, fill=255)
    sd.ellipse((x0 - (x1 - x0) // 4, y0 + h // 2, x1 + (x1 - x0) // 4, y0 + h * 2), fill=0)
    alpha = ImageChops.multiply(alpha, shape)
    white = Image.new("RGBA", img.size, WHITE + (0,))
    white.putalpha(alpha)
    img.alpha_composite(white)


def glossy_text(img, xy, text, font, top, bottom, stroke, shadow_offset):
    """縁取り・影つきの、縦グラデーションの文字。"""
    d = ImageDraw.Draw(img)
    x, y = xy
    # 影
    sh = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(sh).text((x, y + shadow_offset), text, font=font, fill=NAVY + (150,),
                            stroke_width=stroke, stroke_fill=NAVY + (150,))
    img.alpha_composite(sh.filter(ImageFilter.GaussianBlur(S)))
    # 縁取り
    d.text((x, y), text, font=font, fill=NAVY, stroke_width=stroke, stroke_fill=NAVY)
    # グラデーションの中身
    mask = Image.new("L", img.size, 0)
    ImageDraw.Draw(mask).text((x, y), text, font=font, fill=255)
    bbox = mask.getbbox()
    if bbox:
        g = vgrad((img.size[0], bbox[3] - bbox[1]), top, bottom)
        layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
        layer.paste(g, (0, bbox[1]))
        layer.putalpha(mask)
        img.alpha_composite(layer)


def sparkle(img, cx, cy, r):
    """4 方向に光るきらめき。"""
    s = Image.new("RGBA", img.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(s)
    w = r // 5
    d.polygon([(cx, cy - r), (cx + w, cy - w), (cx + r, cy), (cx + w, cy + w),
               (cx, cy + r), (cx - w, cy + w), (cx - r, cy), (cx - w, cy - w)], fill=WHITE + (235,))
    glow = s.filter(ImageFilter.GaussianBlur(r // 4))
    img.alpha_composite(glow)
    img.alpha_composite(s)


def pill(img, box, top, bottom, radius):
    """ツヤのあるピル形（タブ・札）。"""
    m = rounded_mask(img.size, box, radius)
    g = Image.new("RGBA", img.size, (0, 0, 0, 0))
    bw, bh = box[2] - box[0], box[3] - box[1]
    g.paste(vgrad((bw, bh), top, bottom), (box[0], box[1]))
    g.putalpha(ImageChops.multiply(g.getchannel("A"), m))
    ImageDraw.Draw(img).rounded_rectangle((box[0] - S, box[1] - S, box[2] + S, box[3] + S),
                                          radius=radius + S, fill=NAVY + (220,))
    img.alpha_composite(g)
    gloss(img, box, radius, strength=170)


def make(dev: bool) -> Image.Image:
    img = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    box = (3 * S, 3 * S, W - 4 * S, W - 4 * S)
    radius = 22 * S

    # 落ち影
    sh = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(sh).rounded_rectangle((box[0], box[1] + 3 * S, box[2], box[3] + 3 * S),
                                         radius=radius, fill=(0, 0, 40, 110))
    img.alpha_composite(sh.filter(ImageFilter.GaussianBlur(2 * S)))

    # 地: 濃紺の縁 → グラデーション
    ImageDraw.Draw(img).rounded_rectangle(box, radius=radius, fill=NAVY)
    inner = (box[0] + 2 * S, box[1] + 2 * S, box[2] - 2 * S, box[3] - 2 * S)
    base = Image.new("RGBA", img.size, (0, 0, 0, 0))
    base.paste(vgrad((inner[2] - inner[0], inner[3] - inner[1]), TOP, BOTTOM), (inner[0], inner[1]))
    base.putalpha(rounded_mask(img.size, inner, radius - 2 * S))
    img.alpha_composite(base)

    # 下の照り返し（底の方を少し明るく）
    rim = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(rim).ellipse((inner[0] + 10 * S, inner[3] - 22 * S, inner[2] - 10 * S, inner[3] + 6 * S),
                                fill=(140, 230, 255, 120))
    rim = rim.filter(ImageFilter.GaussianBlur(5 * S))
    rim.putalpha(ImageChops.multiply(rim.getchannel("A"), rounded_mask(img.size, inner, radius - 2 * S)))
    img.alpha_composite(rim)

    # 上のタブの帯（ツヤのあるピル）
    pill(img, (14 * S, 15 * S, 50 * S, 23 * S), (255, 255, 255), (170, 230, 255), 4 * S)
    pill(img, (54 * S, 15 * S, 84 * S, 23 * S), (110, 170, 255), (40, 90, 220), 4 * S)

    # 中央の </> と名前
    big = ImageFont.truetype(FONT, 34 * S)
    d = ImageDraw.Draw(img)
    t = "</>"
    tw = d.textlength(t, font=big)
    glossy_text(img, ((W - tw) / 2, 29 * S), t, big, TEXT_TOP, TEXT_BOTTOM, stroke=3 * S, shadow_offset=2 * S)
    small = ImageFont.truetype(FONT, 15 * S)
    n = "zcode"
    nw = d.textlength(n, font=small)
    glossy_text(img, ((W - nw) / 2, 69 * S), n, small, TEXT_TOP, (200, 245, 255), stroke=2 * S, shadow_offset=S)

    # 全体の光沢（上半分）と内側の光の縁
    gloss(img, inner, radius - 2 * S, strength=175)
    ImageDraw.Draw(img).rounded_rectangle(inner, radius=radius - 2 * S, outline=(255, 255, 255, 90), width=S)

    # きらめき
    sparkle(img, 17 * S, 33 * S, 7 * S)
    sparkle(img, 83 * S, 60 * S, 4 * S)

    if dev:
        b = (57 * S, 2 * S, 97 * S, 18 * S)
        pill(img, b, (255, 170, 70), (255, 60, 130), 8 * S)
        tag = ImageFont.truetype(FONT, 11 * S)
        tw = d.textlength("DEV", font=tag)
        glossy_text(img, (b[0] + (b[2] - b[0] - tw) / 2, b[1] + 1.5 * S), "DEV", tag,
                    WHITE, (255, 230, 240), stroke=S, shadow_offset=S // 2)

    return img.resize((SIZE, SIZE), Image.LANCZOS)


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
