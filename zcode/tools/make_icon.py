#!/usr/bin/env python3
"""zcode のランチャー用アイコン（100x100 PNG）を作る。要: Pillow、DejaVu Sans Mono Bold。

  python3 tools/make_icon.py            → packaging/zcode.png（正式版）
  python3 tools/make_icon.py --dev OUT  → OUT（タイトルバーの右に DEV の札。開発用の起動口）

生成物なので git には入れない（ほかのアプリと同じ流儀）。

見た目: Y2K 風のツヤのある、Mac 風ターミナルのウィンドウ。黒いガラスの本体（走査線は
窓全体に通す）に、スモークガラスのタイトルバーを半透明で重ね、赤・黄・緑の丸い
ボタンを置く（照りはタイトルバーの中だけ）。燐光の緑の `</>` の背後だけを魚眼で膨らませ（走査線がレンズ状にたわむ）、
`</>` 自体はまっすぐ描く。下に
`zcode▌`（ブロックカーソル）。4 倍の大きさで描いてから縮小して縁をなめらかにする。
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


# 文字の光（ブルーム）は弱めにして、明るさの段階を減らす（わずかに縞が出る程度）。
# なめらかで強い光は文字の輪郭をぼかして読みにくくした
GLOW_ALPHA = 120
GLOW_LEVELS = 6


def banded_glow(layer, blur):
    """layer をぼかしてから、透明度を GLOW_LEVELS 段に減らす。"""
    g = layer.filter(ImageFilter.GaussianBlur(blur))
    step = 255 / GLOW_LEVELS
    a = g.getchannel("A").point(lambda v: int(round(v / step) * step) if v >= step / 2 else 0)
    g.putalpha(a)
    return g


def glow_text(img, xy, text, font, top, bottom, outline, glow=None, stroke=0):
    """光る（グロー）・縁取りつきの、縦グラデーションの文字。"""
    x, y = xy
    if glow:
        g = Image.new("RGBA", img.size, (0, 0, 0, 0))
        ImageDraw.Draw(g).text((x, y), text, font=font, fill=glow + (GLOW_ALPHA,),
                               stroke_width=stroke + S, stroke_fill=glow + (GLOW_ALPHA,))
        img.alpha_composite(banded_glow(g, 2 * S))
    if stroke:
        ImageDraw.Draw(img).text((x, y), text, font=font, fill=outline, stroke_width=stroke, stroke_fill=outline)
    # 中身のマスクも stroke_width を付けて描く（縁は 0、中身は 255）。Pillow は
    # stroke の有無で文字の置き位置がずれるので、縁取りなしで描くと中身が片側に
    # 寄り、縁取りの太さが左右で変わって見える
    mask = Image.new("L", img.size, 0)
    ImageDraw.Draw(mask).text((x, y), text, font=font, fill=255, stroke_width=stroke, stroke_fill=0)
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


def bulge(img, center, radius, strength=0.4):
    """center を中心に、半径 radius の円の中を膨らませる（魚眼）。
    円の縁では歪みがゼロになるので、境目は出ない。strength は 0〜1。"""
    import numpy as np

    a = np.asarray(img).astype(np.float32)
    h, w = a.shape[:2]
    cx, cy = center
    y0, y1 = max(cy - radius, 0), min(cy + radius, h)
    x0, x1 = max(cx - radius, 0), min(cx + radius, w)
    yy, xx = np.mgrid[y0:y1, x0:x1].astype(np.float32)
    dx, dy = xx - cx, yy - cy
    r = np.sqrt(dx * dx + dy * dy) / radius
    inside = r < 1.0
    # 出力の半径 r に、元の半径 r' = r (1 - k (1 - r^2)) を対応させる。中心の拡大率は
    # 1/(1-k) で頭打ち、縁 (r=1) で等倍に戻り、その間はなめらかに変わる
    # （r^(1+k) のような式は中心で拡大率が発散し、膨らみが急になる）
    k = strength
    scale = np.where(inside, 1.0 - k * (1.0 - r * r), 1.0)
    sx = cx + dx * scale
    sy = cy + dy * scale
    # 双一次補間
    sx0 = np.clip(np.floor(sx).astype(int), 0, w - 2)
    sy0 = np.clip(np.floor(sy).astype(int), 0, h - 2)
    fx = (sx - sx0)[..., None]
    fy = (sy - sy0)[..., None]
    p00 = a[sy0, sx0]
    p01 = a[sy0, sx0 + 1]
    p10 = a[sy0 + 1, sx0]
    p11 = a[sy0 + 1, sx0 + 1]
    out = (p00 * (1 - fx) * (1 - fy) + p01 * fx * (1 - fy) + p10 * (1 - fx) * fy + p11 * fx * fy)
    region = a[y0:y1, x0:x1]
    region[inside] = out[inside]
    a[y0:y1, x0:x1] = region
    return Image.fromarray(np.clip(a, 0, 255).astype(np.uint8), "RGBA")


def code_glyph(img, center, half_w, half_h, width, outline):
    """`</>` を図形として描く。<・/・> を同じ太さの先の丸い線で描き、縁取りはそれより
    太い暗い線を先に描いて、どこでも同じ太さにする（フォントの斜線は縮小すると縁の
    濃さが場所で変わり、棒が波打って見えた）。光（グロー）は縁取りより下に描く。"""
    cx, cy = center
    gap = half_w * 0.18                      # 記号どうしのすき間
    cw = half_w * 0.36                       # < と > の幅
    lt = [(cx - gap - cw * 0.0 - half_w * 0.32, cy - half_h * 0.62),
          (cx - half_w, cy),
          (cx - gap - half_w * 0.32, cy + half_h * 0.62)]
    gt = [(cx + gap + half_w * 0.32, cy - half_h * 0.62),
          (cx + half_w, cy),
          (cx + gap + half_w * 0.32, cy + half_h * 0.62)]
    sl = [(cx - half_w * 0.20, cy + half_h), (cx + half_w * 0.20, cy - half_h)]

    def strokes(draw, w, fill):
        for pts in (lt, gt):
            draw.line(pts, fill=fill, width=w, joint="curve")
            for x, y in (pts[0], pts[2], pts[1]):
                draw.ellipse((x - w / 2, y - w / 2, x + w / 2, y + w / 2), fill=fill)
        draw.line(sl, fill=fill, width=w)
        for x, y in sl:
            draw.ellipse((x - w / 2, y - w / 2, x + w / 2, y + w / 2), fill=fill)

    # 光
    g = Image.new("RGBA", img.size, (0, 0, 0, 0))
    strokes(ImageDraw.Draw(g), width + 2 * outline + 3 * S, GREEN_GLOW + (GLOW_ALPHA,))
    img.alpha_composite(banded_glow(g, 2 * S))
    # 縁取り（一定の太さ）
    strokes(ImageDraw.Draw(img), width + 2 * outline, GREEN_DARK + (255,))
    # 中身（縦グラデーション）
    m = Image.new("L", img.size, 0)
    strokes(ImageDraw.Draw(m), width, 255)
    bbox = m.getbbox()
    if bbox:
        paste_grad(img, (0, bbox[1], img.size[0], bbox[3]), GREEN_TOP, GREEN_BOTTOM, m)


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
    imask = rounded_mask(img.size, inner, r_in)

    # 本体: 黒いガラスを窓全体に敷き、走査線もタイトルバーの下まで通す
    # （タイトルバーは、この上に重ねる半透明のガラスにする）
    paste_grad(img, inner, SCREEN_TOP, SCREEN_BOTTOM, imask)
    scan = Image.new("RGBA", img.size, (0, 0, 0, 0))
    sd = ImageDraw.Draw(scan)
    for y in range(inner[1], inner[3], 3 * S):
        sd.line((inner[0], y, inner[2], y), fill=(0, 0, 0, 70), width=S)
    scan.putalpha(ImageChops.multiply(scan.getchannel("A"), imask))
    img.alpha_composite(scan)
    # 画面の緑のにじみ（中央を少しだけ明るく）
    smask = rounded_mask(img.size, screen, r_in, corners=(False, False, True, True))
    haze = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(haze).ellipse((screen[0] + 8 * S, screen[1] + 6 * S, screen[2] - 8 * S, screen[3] - 4 * S),
                                 fill=(30, 120, 60, 80))
    haze = haze.filter(ImageFilter.GaussianBlur(8 * S))
    haze.putalpha(ImageChops.multiply(haze.getchannel("A"), smask))
    img.alpha_composite(haze)

    # レンズ: 背景（走査線と緑のにじみ）だけを魚眼で膨らませる。</> 自体は歪ませない
    # （文字ごと歪ませると、中心を通る斜線が中央で太くたわんで見えた）
    d = ImageDraw.Draw(img)
    ty = screen[1] + 3 * S
    lens_c = (W // 2, int(ty + 22 * S))
    lens_r = 34 * S
    img = bulge(img, lens_c, lens_r, strength=0.25)
    # レンズの縁を少し暗くして、ふくらみの奥行きを出す
    vig = Image.new("L", img.size, 0)
    ImageDraw.Draw(vig).ellipse((lens_c[0] - lens_r, lens_c[1] - lens_r, lens_c[0] + lens_r, lens_c[1] + lens_r),
                                outline=90, width=6 * S)
    vig = vig.filter(ImageFilter.GaussianBlur(4 * S))
    shade = Image.new("RGBA", img.size, (0, 0, 0, 0))
    shade.putalpha(ImageChops.multiply(vig, smask))
    img.alpha_composite(shade)
    # </> は図形でまっすぐ描く（太さと縁取りが一定）
    code_glyph(img, lens_c, half_w=29 * S, half_h=14 * S, width=int(4.5 * S), outline=int(1.75 * S))
    # レンズの照り（左上の細い円弧の光）
    lens = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(lens).arc((lens_c[0] - lens_r + 4 * S, lens_c[1] - lens_r + 4 * S,
                              lens_c[0] + lens_r - 4 * S, lens_c[1] + lens_r - 4 * S),
                             start=195, end=290, fill=WHITE + (80,), width=2 * S)
    lens = lens.filter(ImageFilter.GaussianBlur(S))
    lens.putalpha(ImageChops.multiply(lens.getchannel("A"), smask))
    img.alpha_composite(lens)

    # 下の名前とブロックカーソル（歪ませない）
    d = ImageDraw.Draw(img)
    small = ImageFont.truetype(FONT, 14 * S)
    n = "zcode"
    nw = d.textlength(n, font=small)
    cursor_w = 7 * S
    nx = (W - nw - cursor_w - S) / 2
    ny = screen[1] + 45 * S
    glow_text(img, (nx, ny), n, small, GREEN_TOP, GREEN_BOTTOM, GREEN_DARK, glow=GREEN_GLOW)
    cx0 = int(nx + nw + S)
    # 文字の実際の上端（d の縦棒）とベースライン（z の下端）にそろえる
    top = ny + small.getbbox("d")[1]
    base = ny + small.getbbox("z")[3]
    cur = (cx0, int(top), cx0 + cursor_w, int(base))
    gl = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(gl).rectangle(cur, fill=GREEN_GLOW + (GLOW_ALPHA,))
    img.alpha_composite(banded_glow(gl, 2 * S))
    paste_grad(img, cur, GREEN_TOP, GREEN_BOTTOM, rounded_mask(img.size, cur, S))

    # タイトルバー: 本体の上に重ねるスモークガラス（下の黒と走査線が透ける）
    bmask = rounded_mask(img.size, bar, r_in, corners=(True, True, False, False))
    glass = Image.new("RGBA", img.size, (0, 0, 0, 0))
    gd = ImageDraw.Draw(glass)
    for i in range(bar_h):
        a = int(150 - 95 * i / bar_h)          # 上ほど白っぽい
        c = int(235 - 70 * i / bar_h)
        gd.line((bar[0], bar[1] + i, bar[2], bar[1] + i), fill=(c, c, c + 8, a))
    glass.putalpha(ImageChops.multiply(glass.getchannel("A"), bmask))
    img.alpha_composite(glass)
    # 境目は硬い線にせず、上に細い光、下にごく薄い影
    edge = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ed = ImageDraw.Draw(edge)
    ed.line((bar[0], bar[3] - S, bar[2], bar[3] - S), fill=(0, 0, 0, 90), width=S)
    ed.line((bar[0], bar[3], bar[2], bar[3]), fill=(255, 255, 255, 30), width=S)
    edge.putalpha(ImageChops.multiply(edge.getchannel("A"), imask))
    img.alpha_composite(edge)

    # 信号機（閉じる / しまう / 広げる）
    cy = bar[1] + bar_h // 2
    for i, (top, bottom) in enumerate(LIGHTS):
        glossy_dot(img, bar[0] + 11 * S + i * 12 * S, cy, 4 * S, top, bottom)

    if dev:
        b = (bar[2] - 34 * S, bar[1] + 5 * S, bar[2] - 6 * S, bar[3] - 5 * S)
        pill(img, b, (255, 170, 70), (255, 60, 130), 6 * S)
        tag = ImageFont.truetype(FONT, 10 * S)
        tw = d.textlength("DEV", font=tag)
        glow_text(img, (b[0] + (b[2] - b[0] - tw) / 2, b[1] + 0.5 * S), "DEV", tag,
                  WHITE, (255, 230, 240), (90, 10, 40), stroke=S)

    # 最後に、タイトルバーの照り（バーの中だけ。上ほど明るく、バーの中ほどで消える）
    shine = Image.new("L", img.size, 0)
    sd2 = ImageDraw.Draw(shine)
    for i in range(bar_h):
        sd2.line((bar[0], bar[1] + i, bar[2], bar[1] + i), fill=int(150 * max(0.0, 1 - i / (bar_h * 0.6)) ** 1.3))
    shine = ImageChops.multiply(shine, rounded_mask(img.size, (bar[0] + S, bar[1] + S, bar[2] - S, bar[3]),
                                                    r_in - S, corners=(True, True, False, False)))
    white = Image.new("RGBA", img.size, WHITE + (0,))
    white.putalpha(shine)
    img.alpha_composite(white)
    ImageDraw.Draw(img).rounded_rectangle(inner, radius=r_in, outline=(255, 255, 255, 70), width=S)
    sparkle(img, screen[2] - 9 * S, screen[1] + 9 * S, 5 * S)

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
