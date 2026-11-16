#!/usr/bin/env python3
"""zcode のランチャー用アイコン（100x100 PNG）を作る。要: Pillow、DejaVu Sans Mono Bold。

  python3 tools/make_icon.py            → packaging/zcode.png（正式版）
  python3 tools/make_icon.py --dev OUT  → OUT（タイトルバーの右に DEV の札。開発用の起動口）

生成物なので git には入れない（ほかのアプリと同じ流儀）。

見た目: Y2K 風のツヤのある、Mac 風ターミナルのウィンドウ。銀色のタイトルバーに
赤・黄・緑の丸いボタン、黒い画面にうっすら走査線、燐光の緑で `</>` と
`zcode▌`（ブロックカーソル）。全体に上半分の光沢。4 倍の大きさで描いてから縮小して
縁をなめらかにする。
"""

import os
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

SIZE = 100
S = 4  # 描くときの倍率
W = SIZE * S
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"

INK = (4, 6, 8)                 # 縁と影
SCREEN_TOP = (44, 48, 52)       # 画面（上）
SCREEN_BOTTOM = (2, 4, 4)       # 画面（下）
BAR_TOP = (246, 246, 250)       # タイトルバー（銀）
BAR_BOTTOM = (168, 170, 180)
GREEN_TOP = (210, 255, 214)     # 燐光の緑
GREEN_BOTTOM = (40, 220, 90)
GREEN_GLOW = (60, 255, 120)
GREEN_DARK = (0, 40, 14)
WHITE = (255, 255, 255)
LIGHTS = [((255, 120, 110), (214, 40, 40)),   # 閉じる
          ((255, 222, 110), (226, 160, 20)),  # しまう
          ((140, 240, 120), (30, 170, 50))]   # 広げる


def vgrad(size, top, bottom):
    """縦のグラデーション（RGBA）。"""
    w, h = size
    g = Image.new("RGBA", (1, h))
    for y in range(h):
        t = y / max(h - 1, 1)
        g.putpixel((0, y), tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3)) + (255,))
    return g.resize((w, h))


def rounded_mask(size, box, radius, corners=None):
    m = Image.new("L", size, 0)
    ImageDraw.Draw(m).rounded_rectangle(box, radius=radius, fill=255, corners=corners)
    return m


def paste_grad(img, box, top, bottom, mask):
    layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
    layer.paste(vgrad((box[2] - box[0], box[3] - box[1]), top, bottom), (box[0], box[1]))
    layer.putalpha(ImageChops.multiply(layer.getchannel("A"), mask))
    img.alpha_composite(layer)


def gloss(img, box, radius, strength=150):
    """box の上半分に、上ほど明るい白い光沢を重ねる（下端は泡のように丸く切る）。"""
    x0, y0, x1, y1 = box
    h = (y1 - y0) // 2
    alpha = Image.new("L", img.size, 0)
    ad = ImageDraw.Draw(alpha)
    for i in range(h):
        a = int(strength * (0.35 + 0.65 * (1 - i / h) ** 1.2))
        ad.line((x0, y0 + i, x1, y0 + i), fill=a)
    shape = Image.new("L", img.size, 0)
    sd = ImageDraw.Draw(shape)
    inset = max((x1 - x0) // 30, 1)
    sd.rounded_rectangle((x0 + inset, y0 + inset, x1 - inset, y0 + h + h // 3), radius=radius, fill=255)
    sd.ellipse((x0 - (x1 - x0) // 4, y0 + h // 2, x1 + (x1 - x0) // 4, y0 + h * 2), fill=0)
    white = Image.new("RGBA", img.size, WHITE + (0,))
    white.putalpha(ImageChops.multiply(alpha, shape))
    img.alpha_composite(white)


def glow_text(img, xy, text, font, top, bottom, outline, glow=None, stroke=0):
    """光る（グロー）・縁取りつきの、縦グラデーションの文字。"""
    x, y = xy
    if glow:
        g = Image.new("RGBA", img.size, (0, 0, 0, 0))
        ImageDraw.Draw(g).text((x, y), text, font=font, fill=glow + (200,),
                               stroke_width=stroke + 2 * S, stroke_fill=glow + (200,))
        img.alpha_composite(g.filter(ImageFilter.GaussianBlur(3 * S)))
    if stroke:
        ImageDraw.Draw(img).text((x, y), text, font=font, fill=outline, stroke_width=stroke, stroke_fill=outline)
    mask = Image.new("L", img.size, 0)
    ImageDraw.Draw(mask).text((x, y), text, font=font, fill=255)
    bbox = mask.getbbox()
    if bbox:
        paste_grad(img, (0, bbox[1], img.size[0], bbox[3]), top, bottom, mask)


def glossy_dot(img, cx, cy, r, top, bottom):
    """ツヤのある丸いボタン（信号機）。"""
    d = ImageDraw.Draw(img)
    d.ellipse((cx - r - S, cy - r - S, cx + r + S, cy + r + S), fill=(70, 70, 76, 230))
    m = Image.new("L", img.size, 0)
    ImageDraw.Draw(m).ellipse((cx - r, cy - r, cx + r, cy + r), fill=255)
    paste_grad(img, (cx - r, cy - r, cx + r, cy + r), top, bottom, m)
    hl = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(hl).ellipse((cx - r * 0.6, cy - r * 0.85, cx + r * 0.6, cy - r * 0.05), fill=WHITE + (190,))
    img.alpha_composite(hl.filter(ImageFilter.GaussianBlur(S * 0.6)))


def sparkle(img, cx, cy, r):
    s = Image.new("RGBA", img.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(s)
    w = r // 5
    d.polygon([(cx, cy - r), (cx + w, cy - w), (cx + r, cy), (cx + w, cy + w),
               (cx, cy + r), (cx - w, cy + w), (cx - r, cy), (cx - w, cy - w)], fill=WHITE + (235,))
    img.alpha_composite(s.filter(ImageFilter.GaussianBlur(r // 4)))
    img.alpha_composite(s)


def pill(img, box, top, bottom, radius):
    ImageDraw.Draw(img).rounded_rectangle((box[0] - S, box[1] - S, box[2] + S, box[3] + S),
                                          radius=radius + S, fill=(60, 20, 30, 230))
    paste_grad(img, box, top, bottom, rounded_mask(img.size, box, radius))
    gloss(img, box, radius, strength=170)


def make(dev: bool) -> Image.Image:
    img = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    box = (3 * S, 3 * S, W - 4 * S, W - 4 * S)
    radius = 18 * S

    # 落ち影
    sh = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(sh).rounded_rectangle((box[0], box[1] + 3 * S, box[2], box[3] + 3 * S),
                                         radius=radius, fill=(0, 0, 0, 140))
    img.alpha_composite(sh.filter(ImageFilter.GaussianBlur(2 * S)))

    # 窓の縁
    ImageDraw.Draw(img).rounded_rectangle(box, radius=radius, fill=INK)
    inner = (box[0] + 2 * S, box[1] + 2 * S, box[2] - 2 * S, box[3] - 2 * S)
    r_in = radius - 2 * S
    bar_h = 22 * S
    bar = (inner[0], inner[1], inner[2], inner[1] + bar_h)
    screen = (inner[0], bar[3], inner[2], inner[3])

    # タイトルバー（銀、上の角だけ丸い）と、画面との境目の線
    paste_grad(img, bar, BAR_TOP, BAR_BOTTOM,
               rounded_mask(img.size, bar, r_in, corners=(True, True, False, False)))
    ImageDraw.Draw(img).line((bar[0], bar[3] - S, bar[2], bar[3] - S), fill=(90, 92, 100, 255), width=S)

    # 黒い画面（下の角だけ丸い）
    smask = rounded_mask(img.size, screen, r_in, corners=(False, False, True, True))
    paste_grad(img, screen, SCREEN_TOP, SCREEN_BOTTOM, smask)
    # 走査線（うっすら）
    scan = Image.new("RGBA", img.size, (0, 0, 0, 0))
    sd = ImageDraw.Draw(scan)
    for y in range(screen[1], screen[3], 3 * S):
        sd.line((screen[0], y, screen[2], y), fill=(0, 0, 0, 70), width=S)
    scan.putalpha(ImageChops.multiply(scan.getchannel("A"), smask))
    img.alpha_composite(scan)
    # 画面の緑のにじみ（中央を少しだけ明るく）
    haze = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(haze).ellipse((screen[0] + 8 * S, screen[1] + 6 * S, screen[2] - 8 * S, screen[3] - 4 * S),
                                 fill=(30, 120, 60, 70))
    haze = haze.filter(ImageFilter.GaussianBlur(8 * S))
    haze.putalpha(ImageChops.multiply(haze.getchannel("A"), smask))
    img.alpha_composite(haze)

    # 信号機（閉じる / しまう / 広げる）
    cy = bar[1] + bar_h // 2
    for i, (top, bottom) in enumerate(LIGHTS):
        glossy_dot(img, bar[0] + 11 * S + i * 12 * S, cy, 4 * S, top, bottom)

    # 中央の </> と、プロンプト風の名前
    d = ImageDraw.Draw(img)
    big = ImageFont.truetype(FONT, 30 * S)
    t = "</>"
    tw = d.textlength(t, font=big)
    glow_text(img, ((W - tw) / 2, screen[1] + 5 * S), t, big, GREEN_TOP, GREEN_BOTTOM, GREEN_DARK,
              glow=GREEN_GLOW, stroke=2 * S)
    small = ImageFont.truetype(FONT, 14 * S)
    n = "zcode"
    nw = d.textlength(n, font=small)
    cursor_w = 7 * S
    nx = (W - nw - cursor_w - S) / 2
    ny = screen[1] + 43 * S
    glow_text(img, (nx, ny), n, small, GREEN_TOP, GREEN_BOTTOM, GREEN_DARK, glow=GREEN_GLOW)
    # ブロックカーソル
    cx0 = int(nx + nw + S)
    cur = (cx0, int(ny + 3 * S), cx0 + cursor_w, int(ny + 17 * S))
    gl = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(gl).rectangle(cur, fill=GREEN_GLOW + (180,))
    img.alpha_composite(gl.filter(ImageFilter.GaussianBlur(2 * S)))
    paste_grad(img, cur, GREEN_TOP, GREEN_BOTTOM, rounded_mask(img.size, cur, S))

    # 全体の光沢と、内側の光の縁
    gloss(img, inner, r_in, strength=150)
    ImageDraw.Draw(img).rounded_rectangle(inner, radius=r_in, outline=(255, 255, 255, 80), width=S)

    # きらめき
    sparkle(img, screen[2] - 9 * S, screen[1] + 9 * S, 5 * S)

    if dev:
        b = (bar[2] - 34 * S, bar[1] + 5 * S, bar[2] - 6 * S, bar[3] - 5 * S)
        pill(img, b, (255, 170, 70), (255, 60, 130), 6 * S)
        tag = ImageFont.truetype(FONT, 10 * S)
        tw = d.textlength("DEV", font=tag)
        glow_text(img, (b[0] + (b[2] - b[0] - tw) / 2, b[1] + 0.5 * S), "DEV", tag,
                  WHITE, (255, 230, 240), (90, 10, 40), stroke=S)

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
