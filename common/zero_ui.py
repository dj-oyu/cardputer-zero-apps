#!/usr/bin/env python3
"""CardputerZero 本体 LCD 用の共通 UI 部品 (zero_ui)。

APPLaunch アプリが本体液晶 (320x170, /dev/fb0 RGB565) に直接
描くための土台をまとめたもの。QR リーダーの実装から切り出した。

- 定数: 画面寸法、キーコード、アクセント色
- font(): Noto Sans CJK Bold (日本語グリフ必須。PIL のデフォルト
  フォントは日本語を持たず豆腐になる)
- to_rgb565(): RGB 配列 → fb0 へ書くバイト列
- FrameBuffer: fb0 への描画
- KeyReader: 本体キーボード (tca8418c, /dev/input/event3) の
  キー押下コードを読む
- draw_panel(): 結果表示のガラスパネル (文字列を無編集で表示、
  コピーは Enter の明示操作だけ、という表示規約も含む)
- draw_stepper() / draw_chip() / draw_card(): フローの案内要素
- finish_frame(): 320x170 の土台にステータス帯・フロー UI・
  緑枠・パネルを重ねて仕上げる

アプリ側は土台画像 (プレビュー合成や写真など) を用意し、
finish_frame に渡す。flow 辞書の形:
  {"stepper": 段階 int, "stepper_labels": ("撮影", "解析", "結果"),
   "chip": 案内チップの文言, "card": draw_card の辞書,
   "dim": 背景を暗くするか}
card 辞書: {"big": str, "note": str|None,
            "kind": "segs"|"bar"|"keys"|None, "segs": (filled, total)}
"""

from __future__ import annotations

import os
import struct

import numpy as np

FB_PATH = "/dev/fb0"
LCD_W, LCD_H = 320, 170
KEY_DEV = "/dev/input/event3"
EXIT_KEYS = {1, 14, 41}  # ESC, BACKSPACE, GRAVE
ENTER_KEYS = {28, 96}  # ENTER, KPENTER
KEY_LEFT, KEY_RIGHT, KEY_SPACE = 105, 106, 57
STATUS_H = 15  # 下部ステータス帯の高さ (px, LCD 実寸)
FONT_PATH = "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc"
ACCENT = (61, 255, 122)  # アクセント (緑)
PANEL_W = 300
PANEL_MAX_H = 132

_FONTS: dict = {}


def font(size: int):
    """Noto Sans CJK Bold を返す (サイズごとにキャッシュ)。"""
    from PIL import ImageFont
    if size not in _FONTS:
        try:
            _FONTS[size] = ImageFont.truetype(FONT_PATH, size, index=0)
        except Exception:
            _FONTS[size] = ImageFont.load_default()
    return _FONTS[size]


def to_rgb565(rgb: np.ndarray) -> bytes:
    """RGB 配列 (170x320x3) を fb0 用 RGB565 バイト列へ変換する。"""
    a = rgb.astype(np.uint16)
    v = ((a[:, :, 0] >> 3) << 11) | ((a[:, :, 1] >> 2) << 5) | (a[:, :, 2] >> 3)
    return v.astype("<u2").tobytes()


class FrameBuffer:
    """本体液晶フレームバッファ (/dev/fb0) への描画。"""

    def __init__(self, path: str = FB_PATH):
        self.fd = os.open(path, os.O_WRONLY)

    def write(self, data: bytes) -> None:
        os.lseek(self.fd, 0, os.SEEK_SET)
        os.write(self.fd, data)

    def close(self) -> None:
        os.close(self.fd)


class KeyReader:
    """本体キーボードのキー押下イベント (コード) を読む。"""

    def __init__(self, path: str = KEY_DEV):
        self.fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
        self.buf = b""

    def poll(self) -> list[int]:
        codes: list[int] = []
        try:
            data = os.read(self.fd, 256)
        except (BlockingIOError, OSError):
            data = b""
        if data:
            self.buf += data
            while len(self.buf) >= 24:
                ev, self.buf = self.buf[:24], self.buf[24:]
                _sec, _usec, etype, code, value = struct.unpack(
                    "llHHI", ev)
                if etype == 1 and value == 1:
                    codes.append(code)
        return codes

    def close(self) -> None:
        os.close(self.fd)


def wrap_text(draw, text: str, fnt, max_w: int) -> list[str]:
    """文字列をピクセル幅で折り返す (文字単位)。"""
    lines: list[str] = []
    for seg in text.split("\n"):
        cur = ""
        for ch in seg:
            if draw.textlength(cur + ch, font=fnt) <= max_w:
                cur += ch
            else:
                if cur:
                    lines.append(cur)
                cur = ch
        lines.append(cur)
    return lines


def draw_panel(img, panel: dict):
    """結果パネル (ガラス風) を 320x170 フレームに重ねる。

    panel = {"text": str, "copied": bool}。表示は一律・無編集:
    文字列をそのまま折り返して出す。長い文字列はフォントを
    落としてパネルに収める。panel["still"] が真なら下部寄せ
    (背景中央の被写体を隠さない)、"count"/"idx" があれば
    ←/→ 切替のヒントも出す。
    """
    from PIL import Image, ImageDraw
    pad = 12
    act_h = 15
    chosen = None
    for size in (17, 15, 13, 11, 9):
        fnt = font(size)
        probe = ImageDraw.Draw(img)
        lines = wrap_text(probe, panel["text"], fnt, PANEL_W - 2 * pad)
        line_h = int(size * 1.35)
        h = pad + len(lines) * line_h + 8 + act_h + pad
        chosen = (fnt, lines, line_h, h)
        if h <= PANEL_MAX_H:
            break
    fnt, lines, line_h, h = chosen
    rgba = img.convert("RGBA")
    ov = Image.new("RGBA", rgba.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(ov)
    x0 = (LCD_W - PANEL_W) // 2
    if panel.get("still"):
        # 静止画パネルは下部に寄せ、クロップ中央のコードを隠さない
        y0 = LCD_H - STATUS_H - h - 4
    else:
        y0 = (LCD_H - h) // 2
    x1, y1 = x0 + PANEL_W, y0 + h
    # 影 → ガラス本体 → 縁取り (backdrop blur の代わりに半透明+縁で近似)
    d.rounded_rectangle([x0, y0 + 3, x1, y1 + 3], radius=12,
                        fill=(0, 0, 0, 90))
    d.rounded_rectangle([x0, y0, x1, y1], radius=12,
                        fill=(14, 18, 16, 150),
                        outline=(255, 255, 255, 110), width=1)
    d.line([x0 + 10, y0 + 1, x1 - 10, y0 + 1], fill=(255, 255, 255, 50))
    rgba = Image.alpha_composite(rgba, ov)
    d = ImageDraw.Draw(rgba)
    y = y0 + pad
    for ln in lines:
        d.text((x0 + pad, y), ln, font=fnt, fill=(255, 255, 255, 255))
        y += line_h
    y += 8
    afont = font(11)
    if panel["copied"]:
        t1 = "コピーしました"
        d.text((x0 + pad, y), t1, font=afont,
               fill=ACCENT + (255,))
        x = x0 + pad + d.textlength(t1, font=afont) + 14
        d.text((x, y), "いずれかのキーで戻る", font=afont,
               fill=(200, 200, 200, 180))
    else:
        # キーキャップ風ラベル + アクション
        def keycap(x, label, color):
            w = d.textlength(label, font=afont)
            d.rounded_rectangle([x, y - 1, x + w + 8, y + act_h - 1],
                                radius=3, outline=color + (255,), width=1)
            d.text((x + 4, y), label, font=afont, fill=color + (255,))
            return x + w + 8 + 5

        x = keycap(x0 + pad, "Enter", ACCENT)
        t1 = "コピー"
        d.text((x, y), t1, font=afont, fill=ACCENT + (255,))
        x += d.textlength(t1, font=afont) + 16
        x = keycap(x, "Esc", (200, 200, 200))
        d.text((x, y), "閉じる", font=afont, fill=(200, 200, 200, 220))
        if panel.get("count", 1) > 1:
            # 複数件は ←/→ で切り替える
            x += d.textlength("閉じる", font=afont) + 16
            x = keycap(x, "←→", (200, 200, 200))
            d.text((x, y), f"{panel.get('idx', 0) + 1}/{panel['count']}",
                   font=afont, fill=(200, 200, 200, 220))
    return rgba.convert("RGB")


def draw_stepper(d, stage: int, labels: tuple = ("撮影", "解析", "結果")) -> None:
    """ステップ表示を上部中央に描く。

    stage: 現在の段階 (0 始まり)。stage >= len(labels) なら全て
    完了。完了は緑の縁+緑文字、現在は緑ベタ+黒文字、未到達は灰。
    """
    fnt = font(9)
    pad_x = 6
    widths = [d.textlength(t, font=fnt) + 2 * pad_x for t in labels]
    conn = 8
    total = sum(widths) + conn * (len(labels) - 1)
    x = (LCD_W - total) / 2
    y, hh = 4, 14
    for i, (t, w) in enumerate(zip(labels, widths)):
        if stage >= len(labels) or i < stage:
            outline, fill, fg = ACCENT, None, ACCENT
        elif i == stage:
            outline, fill, fg = ACCENT, ACCENT, (6, 19, 10)
        else:
            outline, fill, fg = (119, 119, 119), None, (170, 170, 170)
        d.rounded_rectangle([x, y, x + w, y + hh], radius=hh // 2,
                            outline=outline, fill=fill, width=1)
        d.text((x + pad_x, y + 2), t, font=fnt, fill=fg)
        x += w
        if i < len(labels) - 1:
            col = ACCENT if (stage >= len(labels) or i < stage) else (102, 102, 102)
            d.line([x + 1, y + hh // 2, x + conn - 1, y + hh // 2],
                   fill=col, width=2)
            x += conn


def draw_chip(d, text: str) -> None:
    """案内チップ (次のアクション) をステータス帯の上に描く。"""
    afont, tfont = font(10), font(11)
    label = "Enter"
    kw = d.textlength(label, font=afont) + 8
    tw = d.textlength(text, font=tfont)
    w = 10 + kw + 5 + tw + 10
    h = 20
    x0 = (LCD_W - w) / 2
    y0 = LCD_H - STATUS_H - h - 4
    d.rounded_rectangle([x0, y0, x0 + w, y0 + h], radius=h // 2,
                        fill=(8, 12, 10), outline=ACCENT, width=1)
    kx = x0 + 10
    d.rounded_rectangle([kx, y0 + 4, kx + kw, y0 + h - 4], radius=3,
                        outline=ACCENT, width=1)
    d.text((kx + 4, y0 + 4), label, font=afont, fill=ACCENT)
    d.text((kx + kw + 5, y0 + 4), text, font=tfont,
           fill=(234, 255, 241))


def draw_card(d, card: dict, now: float) -> None:
    """フェーズカード (処理中・失敗など) を中央に描く。"""
    w = 250
    big = card["big"]
    note = card.get("note")
    kind = card.get("kind")
    bfont, nfont = font(16), font(10)
    h = 16 + 24 + (16 if note else 0) + (18 if kind else 0) + 12
    x0, y0 = (LCD_W - w) / 2, (LCD_H - h) / 2
    d.rounded_rectangle([x0, y0 + 3, x0 + w, y0 + h + 3], radius=12,
                        fill=(0, 0, 0))
    d.rounded_rectangle([x0, y0, x0 + w, y0 + h], radius=12,
                        fill=(14, 18, 16), outline=(170, 255, 200),
                        width=1)
    y = y0 + 12
    tw = d.textlength(big, font=bfont)
    d.text(((LCD_W - tw) / 2, y), big, font=bfont,
           fill=(234, 255, 241))
    y += 24
    if note:
        tw = d.textlength(note, font=nfont)
        d.text(((LCD_W - tw) / 2, y), note, font=nfont,
               fill=(185, 214, 192))
        y += 16
    if kind == "segs":
        filled, total = card.get("segs", (0, 2))
        sw, gap = 28, 8
        sx = (LCD_W - (sw * total + gap * (total - 1))) / 2
        for i in range(total):
            d.rounded_rectangle(
                [sx + i * (sw + gap), y + 4, sx + i * (sw + gap) + sw,
                 y + 11], radius=2, outline=ACCENT,
                fill=ACCENT if i < filled else None, width=1)
    elif kind == "bar":
        tw_track = 150
        sx = (LCD_W - tw_track) / 2
        d.rounded_rectangle([sx, y + 5, sx + tw_track, y + 10], radius=2,
                            fill=(29, 43, 33))
        phase = (now % 1.15) / 1.15
        tri = phase * 2 if phase < 0.5 else 2 - phase * 2
        bx = sx + tri * (tw_track - 48)
        d.rounded_rectangle([bx, y + 5, bx + 48, y + 10], radius=2,
                            fill=ACCENT)
    elif kind == "keys":
        afont = font(10)
        items = (("Enter", "もう一度"), ("Esc", "戻る"))
        widths = []
        for label, t in items:
            widths.append(d.textlength(label, font=afont) + 8 + 5
                          + d.textlength(t, font=afont))
        total_w = sum(widths) + 16
        x = (LCD_W - total_w) / 2
        for (label, t), iw in zip(items, widths):
            col = ACCENT if label == "Enter" else (200, 200, 200)
            kw = d.textlength(label, font=afont) + 8
            d.rounded_rectangle([x, y + 2, x + kw, y + 16], radius=3,
                                outline=col, width=1)
            d.text((x + 4, y + 3), label, font=afont, fill=col)
            d.text((x + kw + 5, y + 3), t, font=afont, fill=col)
            x += iw + 16


def finish_frame(img, stack_count: int, status: str,
                 panel: dict | None, code_rel: tuple | None,
                 flow: dict | None, now: float):
    """320x170 の土台にステータス帯・フロー UI・緑枠・パネルを重ねる。

    stack_count > 1 のとき右端に "STK n" を出す (スタック表示を
    使わないアプリは 0 を渡す)。
    """
    from PIL import ImageDraw
    d = ImageDraw.Draw(img)
    d.rectangle([0, LCD_H - STATUS_H, LCD_W, LCD_H], fill=(0, 0, 0))
    # ステータス帯も CJK フォントで描く。PIL のデフォルトフォント
    # は日本語グリフを持たず豆腐になる (実機で確認済みの教訓)
    sfont = font(11)
    d.text((3, LCD_H - STATUS_H + 2), status, fill=(255, 255, 255),
           font=sfont)
    if stack_count > 1:
        right = f"STK {stack_count}"
        rw = d.textlength(right, font=sfont)
        d.text((LCD_W - 4 - rw, LCD_H - STATUS_H + 2), right,
               fill=(255, 255, 255), font=sfont)
    if flow:
        if flow.get("stepper") is not None:
            draw_stepper(d, flow["stepper"],
                         flow.get("stepper_labels", ("撮影", "解析", "結果")))
        if flow.get("chip"):
            draw_chip(d, flow["chip"])
        if flow.get("card"):
            draw_card(d, flow["card"], now)
    if code_rel is not None:
        rx, ry, rw, rh = code_rel
        d.rectangle([rx * LCD_W, ry * LCD_H,
                     (rx + rw) * LCD_W, (ry + rh) * LCD_H],
                    outline=ACCENT, width=2)
    if panel is not None:
        img = draw_panel(img, panel)
    return np.asarray(img)
