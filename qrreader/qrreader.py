#!/usr/bin/env python3
"""CardputerZero QRリーダー アプリ v3.0

v3.0 (2026-11-16): 「中心部から最小限の情報」への組み替え。
較正実験 (exp_acc_res.py、65モジュールQR) で、16枚平均後の
デコード下限は実効 約1.5 px/module と判明した。これを受けて:
- lores を 640x480 → 320x240 に (走査は元々 [::2] の 320x240 で
  行っていたので情報は不変。速読み・トーン合成もこの解像度で完結)
- バーストスタックの累積を適応解像度 (256/384/512) に: ファインダ
  3点の脚長からコード寸法を測り、想定80モジュールで実効
  2 px/module になる最小の累積サイズを選ぶ。位置合わせも
  累積解像度で直接行う。累積メモリは 3.7MB → 0.26-1.0MB。
- Wiener のメモリゲートを 70MB → 50MB に (累積縮小で一時
  メモリも 1/6 級になったため)

v1 (Tab5/P4 設計の移植) への追加:
- バーストスタック: 固定焦点のボケと暗所ノイズへのソフトウェア対策。
  main 中央 crop を位相相関で位置合わせしながらアキュムレータへ
  ストリーミング加算し (フレームは保持しない)、4/8/16/32 枚の節目で
  デコードを試す。最終段では Wiener デコンボリューションも試す。
  模擬検証 (較正済み劣化モデル) では単写×・デコンボ単独×・
  16枚スタック○。主因はノイズ低減だった。
- 露光は短め (1/100s 起点のラダー) + ゲイン自動。手ブレを減らし、
  増えたノイズはスタックに食わせる。
- 注意の可視化 (漫画トーン表現): ファインダパターン候補
  (1:1:3:1:1 走査 + 縦クロスチェック) の検出マップを「関心マップ」
  として保持し、時間減衰つきで蓄積する。関心の強さをスクリーン
  トーンの濃さ (ベイヤー網点) で、3点が幾何的に揃った領域には
  カケアミで描く。3点にはファインダ型の太い四角マーカー。
  偽候補は薄い点のまま拡散して消える = 機械がどこに気を取られて
  いるかが正直に見える。
- 読み取り経路は YUV420 の Y 面のみ (変換なし・省メモリ)。

キー: Q / Esc = 終了, F = フルスクリーン切替
自己テスト: python3 qrreader.py --selftest
"""

from __future__ import annotations

import base64
import json
import math
import os
import subprocess
import sys
import threading
import time
import wave

import numpy as np
import zxingcpp

MAIN_W, MAIN_H = 1640, 1232
LORES_W, LORES_H = 320, 240
CROP = 960  # main 中央 crop の一辺 (スタック対象)
ACC_SIZES = (256, 384, 512)  # 累積解像度の候補 (適応選択)
ACC_DEFAULT = 384
ACC_MODULES = 80     # 累積解像度の設計想定 (最密クラスのモジュール数)
ACC_TARGET_PPM = 2.0  # 実効 px/module の目標 (下限 1.5 への余裕込み)
COOLDOWN = 6.0  # 同じコードの再読み抑制 (秒)
BEEP_PATH = "/tmp/qrreader-beep.wav"
TEST_QR = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                       "qrreader_test_qr.png")
MILESTONES = (4, 8, 16, 32)
STACK_DECODE = True  # 診断用: False で節目のデコード/Wiener を止める
MAX_FRAMES = 48
EXPOSURE_LADDER = (6600, 10000, 16667, 25000, 33333)


# ---------------------------------------------------------------- MQJSP1

def mqjsp1_summary(text: str) -> str | None:
    """MQJSP1 ペイロードなら伏せ字要約を返す。違えば None。"""
    if not text.startswith("MQJSP1:"):
        return None
    body = text[len("MQJSP1:"):]
    try:
        raw = base64.b64decode(body + "=" * (-len(body) % 4),
                               altchars=b"-_", validate=True)
        payload = json.loads(raw.decode("utf-8"))
        if not isinstance(payload, dict) or payload.get("v") != 1:
            return "MQJSP1: 形式エラー"
    except Exception:
        return "MQJSP1: 形式エラー"
    parts = ["MQJSP1 プロビジョニング"]
    if payload.get("device"):
        parts.append(f"device={payload['device']}")
    if "wifi" in payload:
        parts.append(f"Wi-Fi ssid={payload['wifi'].get('ssid', '?')} (password 有)")
    if "tailscale" in payload:
        parts.append("Tailscale authKey 有")
    if "exp" in payload:
        left = payload["exp"] - int(time.time())
        parts.append("期限切れ" if left < 0 else f"有効期限まで {left // 60} 分")
    return " / ".join(parts)


def mask(text: str) -> str:
    if len(text) <= 8:
        return f"({len(text)} 文字)"
    return f"{text[:4]}…{text[-4:]} ({len(text)} 文字)"


# ---------------------------------------------------------------- 雑多な道具

def make_beep() -> None:
    if os.path.exists(BEEP_PATH):
        return
    rate, dur, freq = 44100, 0.12, 880
    n = int(rate * dur)
    frames = b"".join(
        int(12000 * math.sin(2 * math.pi * freq * i / rate)).to_bytes(
            2, "little", signed=True)
        for i in range(n))
    with wave.open(BEEP_PATH, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(frames)


def beep() -> None:
    env = dict(os.environ, XDG_RUNTIME_DIR="/run/user/1000")
    subprocess.Popen(["pw-play", BEEP_PATH], env=env,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def set_clipboard(text: str) -> bool:
    """xsel 経路 (v0.2 で実証済み)。環境変数は継承したまま使うこと。"""
    env = dict(os.environ, DISPLAY=os.environ.get("DISPLAY", ":0"))
    try:
        p = subprocess.run(["xsel", "--clipboard", "--input"],
                           input=text.encode(), env=env, timeout=5)
        return p.returncode == 0
    except Exception:
        return False


def decode_gray(gray: np.ndarray) -> str | None:
    hits = decode_all(gray)
    return hits[0][0] if hits else None


def decode_all(gray: np.ndarray) -> list[tuple[str, tuple[int, int, int, int]]]:
    """読めた全件を (文字列, 外接矩形 (x0, y0, x1, y1)) で返す。

    v3.5: 「どれが読まれたか」を表示するため位置も返す。
    矩形は入力画像の画素座標 (zxing の position の外接)。
    """
    if gray.dtype != np.uint8:
        gray = np.clip(gray, 0, 255).astype(np.uint8)
    try:
        results = zxingcpp.read_barcodes(np.ascontiguousarray(gray),
                                         try_harder=True)
    except TypeError:
        results = zxingcpp.read_barcodes(np.ascontiguousarray(gray))
    out: list[tuple[str, tuple[int, int, int, int]]] = []
    for r in results:
        if not r.text:
            continue
        p = r.position
        xs = (p.top_left.x, p.top_right.x, p.bottom_left.x, p.bottom_right.x)
        ys = (p.top_left.y, p.top_right.y, p.bottom_left.y, p.bottom_right.y)
        out.append((r.text, (min(xs), min(ys), max(xs), max(ys))))
    return out


def y_plane(arr: np.ndarray, w: int, h: int) -> np.ndarray:
    """YUV420 capture_array から Y 面 (w x h) を取り出す (stride 込み)。"""
    return arr[:h, :w]


def lap_var(a: np.ndarray) -> float:
    a = a.astype(np.float32)
    lap = (-4 * a[1:-1, 1:-1] + a[:-2, 1:-1] + a[2:, 1:-1]
           + a[1:-1, :-2] + a[1:-1, 2:])
    return float(lap.var())


# ------------------------------------------------------- ファインダ走査

def _runs(line: np.ndarray) -> list[tuple[int, int, bool]]:
    runs = []
    start = 0
    val = bool(line[0])
    for i in range(1, len(line)):
        if bool(line[i]) != val:
            runs.append((start, i - start, val))
            start = i
            val = bool(line[i])
    runs.append((start, len(line) - start, val))
    return runs


def _match5(lens: list[int]) -> float | None:
    total = sum(lens)
    if total < 10:
        return None
    u = total / 7.0
    exp = (u, u, 3 * u, u, u)
    if all(abs(lens[j] - exp[j]) <= 0.55 * exp[j] + 1 for j in range(5)):
        return u
    return None


def scan_finders(lores_y: np.ndarray) -> list[list]:
    """lores Y 面 (320x240) からファインダパターン候補を探す。

    戻り値: [x, y, unit, score] のリスト (lores 座標, score は裏付け数)。
    """
    small = lores_y.astype(np.float32)
    # 局所平均で二値化 (積分画像)
    k = 41
    integ = np.pad(small, ((1, 0), (1, 0))).cumsum(0).cumsum(1)
    h, w = small.shape
    ys = np.arange(h)
    xs = np.arange(w)
    y0 = np.clip(ys - k // 2, 0, h)
    y1 = np.clip(ys + k // 2 + 1, 0, h)
    x0 = np.clip(xs - k // 2, 0, w)
    x1 = np.clip(xs + k // 2 + 1, 0, w)
    area = (y1 - y0)[:, None] * (x1 - x0)[None, :]
    s = (integ[np.ix_(y1, x1)] - integ[np.ix_(y0, x1)]
         - integ[np.ix_(y1, x0)] + integ[np.ix_(y0, x0)])
    black = small < (s / area - 5.0)

    raw: list[list] = []
    for yy in range(0, h, 2):
        runs = _runs(black[yy])
        for i in range(len(runs) - 4):
            five = runs[i:i + 5]
            if not five[0][2]:
                continue
            u = _match5([r[1] for r in five])
            if u:
                cx = five[0][0] + five[0][1] + five[1][1] + five[2][1] / 2
                raw.append([cx, float(yy), u])
    conf: list[list] = []
    for cx, cy, u in raw:
        x = int(round(cx))
        if not (0 <= x < w):
            continue
        runs = _runs(black[:, x])
        for i in range(len(runs) - 4):
            five = runs[i:i + 5]
            if not five[0][2]:
                continue
            top = five[0][0]
            bot = top + sum(r[1] for r in five)
            if top <= cy <= bot:
                u2 = _match5([r[1] for r in five])
                if u2 and abs(u2 - u) < 0.45 * u:
                    conf.append([cx, cy, u, 2])
                    break
    clusters: list[list] = []
    for c in conf:
        for cl in clusters:
            if (abs(c[0] - cl[0]) < max(8, c[2] * 2)
                    and abs(c[1] - cl[1]) < max(8, c[2] * 2)):
                cl[3] += c[3]
                cl[0] = (cl[0] + c[0]) / 2
                cl[1] = (cl[1] + c[1]) / 2
                break
        else:
            clusters.append(c[:])
    clusters.sort(key=lambda c: -c[3])
    return [c[:] for c in clusters[:12]]


def find_triple(clusters: list[list]) -> dict | None:
    """候補から幾何的に正しい3点組 (直角二等辺) を探す。

    戻り値: {"corner": (x,y), "p2": (x,y), "p3": (x,y),
             "p4": (x,y), "unit": u} または None。
    """
    pts = [(c[0], c[1], c[2]) for c in clusters[:7] if c[3] >= 4]
    n = len(pts)
    for i in range(n):
        for j in range(n):
            if j == i:
                continue
            for k in range(n):
                if k in (i, j):
                    continue
                a, b, c = pts[i], pts[j], pts[k]
                dab = math.hypot(a[0] - b[0], a[1] - b[1])
                dac = math.hypot(a[0] - c[0], a[1] - c[1])
                dbc = math.hypot(b[0] - c[0], b[1] - c[1])
                if dab < 20 or dac < 20:
                    continue
                legs = sorted((dab, dac))
                if legs[0] < 1:
                    continue
                if not (0.65 <= legs[1] / legs[0] <= 1.5):
                    continue
                if not (1.25 <= dbc / ((legs[0] + legs[1]) / 2) <= 1.62):
                    continue
                us = (a[2], b[2], c[2])
                if max(us) / max(min(us), 0.1) > 1.5:
                    continue
                p4 = (b[0] + c[0] - a[0], b[1] + c[1] - a[1])
                return {"corner": (a[0], a[1]), "p2": (b[0], b[1]),
                        "p3": (c[0], c[1]), "p4": p4,
                        "unit": sum(us) / 3.0}
    return None


# ------------------------------------------------------- トーン合成

_BAYER8 = np.array([
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21]], dtype=np.float32) / 64.0

ATT_W, ATT_H = 160, 120  # 関心マップ (lores の 1/2)


def _box_blur(a: np.ndarray, r: int = 2) -> np.ndarray:
    k = 2 * r + 1
    for axis in (0, 1):
        pw = [(0, 0), (0, 0)]
        pw[axis] = (r, r)
        p = np.pad(a, pw, mode="edge")
        zw = [(0, 0), (0, 0)]
        zw[axis] = (1, 0)
        c = np.cumsum(np.pad(p, zw, mode="constant"), axis=axis)
        hi = np.take(c, np.arange(k, c.shape[axis]), axis=axis)
        lo = np.take(c, np.arange(0, c.shape[axis] - k), axis=axis)
        a = (hi - lo) / k
    return a


def tone_composite(lores_y: np.ndarray, att: np.ndarray) -> np.ndarray:
    """Y 面 + 関心マップ → 漫画トーン風 RGB (320x240x3 uint8)。"""
    base = lores_y.astype(np.float32) * 0.60 + 95.0
    up = np.repeat(np.repeat(_box_blur(att), 2, axis=0), 2, axis=1)
    up = up[:LORES_H, :LORES_W]
    bayer = np.tile(_BAYER8, (LORES_H // 8 + 1, LORES_W // 8 + 1))
    bayer = bayer[:LORES_H, :LORES_W]
    dots = up * 1.12 > bayer + 0.06
    yy, xx = np.mgrid[0:LORES_H, 0:LORES_W]
    hatch = (((xx + yy) % 9) < 2) | (((xx - yy) % 9) < 2)
    ink = dots | (hatch & (up > 0.55))
    rgb = np.dstack([base, base, base])
    rgb[ink] = 25.0
    return np.clip(rgb, 0, 255).astype(np.uint8)


_SPLAT = None


def splat(att: np.ndarray, x: float, y: float, amp: float) -> None:
    """関心マップへ放射状の blob を加える (マップ座標は lores の 1/2)。"""
    global _SPLAT
    if _SPLAT is None:
        r = 7
        yy, xx = np.mgrid[-r:r + 1, -r:r + 1]
        d = np.sqrt(xx * xx + yy * yy) / r
        _SPLAT = np.clip(1.0 - d, 0, 1) ** 1.5
    r = 7
    cx, cy = int(x / 2), int(y / 2)
    x0, x1 = max(0, cx - r), min(ATT_W, cx + r + 1)
    y0, y1 = max(0, cy - r), min(ATT_H, cy + r + 1)
    if x1 <= x0 or y1 <= y0:
        return
    att[y0:y1, x0:x1] += _SPLAT[y0 - (cy - r):y1 - (cy - r),
                                x0 - (cx - r):x1 - (cx - r)] * amp


# ------------------------------------------------------- バーストスタック

_HANN: dict = {}


def _phase_shift(ref: np.ndarray, img: np.ndarray,
                 ds: int = 1) -> tuple[float, float, float]:
    """位相相関 (放物線補間でサブピクセル)。

    戻り値 (dx, dy) は img を ref に重ねるための補正量 (元解像度換算)。
    符号は実測で確認済み: 内容を (+s) 動かした画像では相関ピークが
    (-s) に出るので、ピーク座標が補正量になる。

    ds=2 では半解像度で相関を取る。CM0 (512MB) では 960x960 の FFT を
    毎フレーム回すとメモリ圧迫でシステム全体が数秒止まり、カメラの
    dequeue タイマ切れ→前端タイムアウトで映像が死ぬ (2026-11-15 実証:
    スタッカー無効時は 250s 以上安定)。半解像度で一時メモリ 1/4。
    """
    if ds > 1:
        ref = ref[::ds, ::ds]
        img = img[::ds, ::ds]
    win = _HANN.get(ref.shape)
    if win is None:
        win = np.outer(np.hanning(ref.shape[0]), np.hanning(ref.shape[1]))
        win = win.astype(np.float32)
        _HANN[ref.shape] = win
    f1 = np.fft.fft2((ref - ref.mean()) * win)
    f2 = np.fft.fft2((img - img.mean()) * win)
    r = f1 * np.conj(f2)
    r /= (np.abs(r) + 1e-9)
    corr = np.real(np.fft.ifft2(r))
    h, w = corr.shape
    y, x = np.unravel_index(int(np.argmax(corr)), corr.shape)
    peak = float(corr[y, x])

    def parab(c: float, cm: float, cp: float) -> float:
        d = cm - 2 * c + cp
        return 0.5 * (cm - cp) / d if d != 0 else 0.0

    ox = parab(peak, float(corr[y, (x - 1) % w]), float(corr[y, (x + 1) % w]))
    oy = parab(peak, float(corr[(y - 1) % h, x]), float(corr[(y + 1) % h, x]))
    xf, yf = x + ox, y + oy
    if yf > h // 2:
        yf -= h
    if xf > w // 2:
        xf -= w
    return float(xf * ds), float(yf * ds), peak


def _shift_fft(img: np.ndarray, dx: float, dy: float) -> np.ndarray:
    """内容を (+dx, +dy) へサブピクセル移動した画像を返す。"""
    fy = np.fft.fftfreq(img.shape[0])[:, None]
    fx = np.fft.fftfreq(img.shape[1])[None, :]
    return np.real(np.fft.ifft2(
        np.fft.fft2(img) * np.exp(-2j * np.pi * (fx * dx + fy * dy))))


def _mem_available_mb() -> float:
    try:
        with open("/proc/meminfo") as fh:
            for line in fh:
                if line.startswith("MemAvailable:"):
                    return int(line.split()[1]) / 1024.0
    except Exception:
        pass
    return 9999.0


def _wiener(img: np.ndarray, r: float, nsr: float) -> np.ndarray:
    # 全て float32/complex64 で通す。旧実装は mgrid の int64 2枚 +
    # float64 カーネル + complex128 の一時配列で 960x960 に対して
    # 60-90MB の一時メモリを使い、空き ~50MB の CM0 をスラッシングに
    # 落として本体ごと固めた (2026-11-15 実証済み)。
    h, w = img.shape
    yy = (np.arange(h, dtype=np.float32) - h // 2) ** 2
    xx = (np.arange(w, dtype=np.float32) - w // 2) ** 2
    k = (yy[:, None] + xx[None, :] <= r * r).astype(np.float32)
    k /= k.sum()
    hf = np.fft.fft2(np.fft.ifftshift(k))
    del k
    f = np.fft.fft2(img.astype(np.float32, copy=False))
    return np.real(np.fft.ifft2(np.conj(hf) * f / (np.abs(hf) ** 2 + nsr)))


def _downscale_box(img_u8: np.ndarray, size: int) -> np.ndarray:
    """正方形画像を size x size へ面積平均で縮小する (float32 で返す)。"""
    if img_u8.shape[0] == size:
        return img_u8.astype(np.float32)
    from PIL import Image
    small = Image.fromarray(img_u8).resize((size, size), Image.BOX)
    return np.asarray(small, dtype=np.float32)


def acc_size_for_leg(leg_lores: float) -> int:
    """ファインダ3点の脚長 (lores px) から累積解像度を選ぶ。

    脚長を main crop 座標へ写像 (lores は main の全幅を見ており、
    レティクルの実測係数 0.84 を掛ける) し、想定 ACC_MODULES
    モジュールで実効 ACC_TARGET_PPM px/module を満たす最小の
    累積サイズを返す。較正実験の下限 (実効 約1.5) への余裕込み。
    """
    leg_crop = leg_lores * (MAIN_W / LORES_W) * 0.84
    if leg_crop <= 0:
        return ACC_DEFAULT
    need = ACC_TARGET_PPM * CROP * ACC_MODULES / leg_crop
    for size in ACC_SIZES:
        if size >= need:
            return size
    return ACC_SIZES[-1]


class BurstStacker:
    """crop 画像列を位置合わせしながら加算し、節目でデコードを試す。

    v3: フレームはまず適応解像度 (acc_size) へ面積平均で落としてから
    累積する。位置合わせも累積解像度で直接行う。デコーダが必要な
    のはモジュール格子だけであり、原生解像度で積む理由はない
    (較正実験 exp_acc_res.py: 16枚平均後の下限は実効 約1.5
    px/module。960x960 float32 = 3.7MB あった累積は最大でも
    512x512 = 1.0MB になる)。
    """

    def __init__(self) -> None:
        self.acc: np.ndarray | None = None
        self.ref: np.ndarray | None = None
        self.count = 0
        self.sharps: list[float] = []
        self.acc_target = ACC_DEFAULT
        self.acc_size: int | None = None
        # add (stack_loop) と set_target (attention/guidance) が
        # 別スレッドから同時に触るため必須 (2026-11-16 実機で
        # acc_size=None のまま // される競合を観測)。
        self._lock = threading.RLock()
        # v3.5: 直近ヒットの (文字列, 累積座標での外接矩形)。
        # 「どれが読まれたか」の枠表示用。
        self.last_hit: tuple[str, tuple[int, int, int, int]] | None = None

    def set_target(self, size: int) -> None:
        """累積解像度の目標を変える。現在の累積と違う場合は積み直す。"""
        with self._lock:
            if size == self.acc_target:
                return
            self.acc_target = size
            if self.acc is not None and size != self.acc_size:
                self.reset()

    def reset(self) -> None:
        with self._lock:
            self.acc = None
            self.ref = None
            self.count = 0
            self.sharps = []
            self.acc_size = None
            self.last_hit = None

    def add(self, crop_u8: np.ndarray) -> str | None:
        with self._lock:
            return self._add_locked(crop_u8)

    def _add_locked(self, crop_u8: np.ndarray) -> str | None:
        if self.acc_size is None:
            self.acc_size = min(self.acc_target, crop_u8.shape[0])
        crop = _downscale_box(crop_u8, self.acc_size)
        sharp = lap_var(crop[::4, ::4])
        if self.acc is None:
            self.acc = crop.copy()
            self.ref = crop.copy()
            self.count = 1
            self.sharps = [sharp]
            return None
        # 鮮明でないフレーム (手ブレ) は混ぜない
        med = float(np.median(self.sharps[-8:]))
        if sharp < 0.45 * med and self.count >= 4:
            return None
        # 位置合わせは累積解像度で直接 (ds=1)。整数シフトは np.roll
        # で十分 (主効果はノイズ平均、位置合わせは ±0.5px で足りる)。
        dx, dy, peak = _phase_shift(self.ref, crop, ds=1)
        limit = max(10, self.acc_size // 24)
        if peak < 0.05 or abs(dx) > limit or abs(dy) > limit:
            # 場面が変わった: このフレームから積み直す
            self.acc = crop.copy()
            self.ref = crop.copy()
            self.count = 1
            self.sharps = [sharp]
            return None
        self.acc += np.roll(crop, (int(round(dy)), int(round(dx))),
                            axis=(0, 1))
        self.count += 1
        self.sharps.append(sharp)
        if self.count in MILESTONES and STACK_DECODE:
            avg = self.acc / self.count
            hits = decode_all(avg)
            if hits:
                self.last_hit = hits[0]
                return hits[0][0]
            if self.count == MILESTONES[-1]:
                # Wiener は半径1.5の1回だけ + 空きメモリに余裕がある
                # 時だけ。累積解像度では一時メモリも小さい (512でも
                # 約13MB) が、ゲート自体は残す (旧教訓: 960 解像度で
                # 3半径連続実行し CM0 を固めた)。
                if _mem_available_mb() > 50:
                    base = avg.astype(np.float32) / 255.0
                    hits = decode_all(_wiener(base, 1.5, 0.006) * 255.0)
                    if hits:
                        self.last_hit = hits[0]
                        return hits[0][0]
        if self.count >= MAX_FRAMES:
            self.reset()
        return None


# ---------------------------------------------------------------- 自己テスト

def _test_scene() -> tuple[np.ndarray, tuple[int, int, int]]:
    """QR を貼った 320x240 のテスト場面と、QR の (x, y, size) を返す。"""
    from PIL import Image
    qr = Image.open(TEST_QR).convert("L")
    size = 150
    qr_r = qr.resize((size, size), Image.NEAREST)
    scene = np.full((LORES_H, LORES_W), 205, np.uint8)
    rng = np.random.default_rng(7)
    # 文字っぽいノイズ (偽候補の発生源として)
    for _ in range(26):
        x = int(rng.integers(10, 270))
        y = int(rng.integers(10, 225))
        w = int(rng.integers(14, 44))
        scene[y:y + 9, x:x + w] = 60
    x0, y0 = 150, 55
    scene[y0:y0 + size, x0:x0 + size] = np.asarray(qr_r)
    return scene, (x0, y0, size)


def selftest() -> int:
    ok = True

    def check(name: str, cond: bool, extra: str = "") -> None:
        nonlocal ok
        print(f"[{'OK' if cond else 'NG'}] {name} {extra}")
        ok = ok and cond

    # 1) 単体デコードと MQJSP1 伏せ字 (v1 互換)
    from PIL import Image
    qr_img = np.asarray(Image.open(TEST_QR).convert("L"))
    check("decode plain QR", decode_gray(qr_img) is not None)
    noise = np.random.default_rng(1).integers(0, 255, (480, 640), dtype=np.uint8)
    check("decode noise -> none", decode_gray(noise) is None)
    dummy = {"v": 1, "id": "selftest", "device": "zero-2",
             "wifi": {"ssid": "home-wifi", "password": "dummy"},
             "tailscale": {"authKey": "tskey-auth-dummy"}}
    body = base64.urlsafe_b64encode(
        json.dumps(dummy, separators=(",", ":"), sort_keys=True).encode()
    ).rstrip(b"=").decode()
    s = mqjsp1_summary("MQJSP1:" + body)
    check("MQJSP1 redaction",
          s is not None and "dummy" not in s and "tskey" not in s, f"({s})")

    # 2) ファインダ走査 + 3点ロック
    scene, (qx, qy, qsize) = _test_scene()
    t0 = time.perf_counter()
    clusters = scan_finders(scene)
    scan_ms = (time.perf_counter() - t0) * 1000
    triple = find_triple(clusters)
    check("finder scan finds triple", triple is not None,
          f"({len(clusters)} clusters, {scan_ms:.0f}ms)")
    if triple:
        cx, cy = triple["corner"]
        # 左上ファインダ中心の期待値: 貼付位置 + (border2 + 3.5) モジュール
        modules = Image.open(TEST_QR).width // 10
        unit = qsize / modules
        ex, ey = qx + 5.5 * unit, qy + 5.5 * unit
        err = math.hypot(cx - ex, cy - ey)
        check("triple corner near QR", err < 34, f"(err {err:.0f}px)")

    # 3) トーン合成の煙テスト
    att = np.zeros((ATT_H, ATT_W), np.float32)
    for c in clusters:
        splat(att, c[0], c[1], min(1.0, c[3] / 12.0))
    comp = tone_composite(scene, att)
    check("tone composite shape",
          comp.shape == (LORES_H, LORES_W, 3) and comp.dtype == np.uint8)

    # 4) バーストスタック: 単写で読めない劣化から積んで読む
    # 較正条件: ボケ+ノイズの単写は読めず、ボケだけなら読める r を探す
    # (実環境の「ノイズが最後の一押し」になっている状態の再現。
    #  実測で確定したレジーム: QR 85px / 300px キャンバス /
    #  コントラスト 0.82 / σ7.65 / ジッタ ±1.5px)
    S = 300
    small_canvas = np.full((S, S), 205, np.uint8)
    qr_small = Image.open(TEST_QR).convert("L").resize((85, 85), Image.NEAREST)
    off = (S - 85) // 2
    small_canvas[off:off + 85, off:off + 85] = np.asarray(qr_small)
    base = small_canvas.astype(np.float32) * 0.82 + 23.0
    rng = np.random.default_rng(3)
    used_r = None
    for r in (1.2, 1.6, 2.0):
        degraded = _disk_blur(base, r)
        noisy = np.clip(degraded + rng.normal(0, 7.65, degraded.shape), 0, 255)
        if decode_gray(noisy) is None and decode_gray(degraded) is not None:
            used_r = r
            break
    check("degraded single unreadable", used_r is not None, f"(r={used_r})")
    if used_r is not None:
        st = BurstStacker()
        got = None
        blurred = _disk_blur(base, used_r)
        for _ in range(16):
            dx, dy = rng.uniform(-1.5, 1.5, 2)
            fr = _shift_fft(blurred, float(dx), float(dy))
            fr = np.clip(fr + rng.normal(0, 7.65, fr.shape), 0, 255)
            got = st.add(fr.astype(np.uint8)) or got
        check("burst stack decodes", got is not None,
              f"({mask(got) if got else ''})")

    # 5) 適応累積 (v3): 480px フレームを 256 へ落として積んでも読める
    # (配管の確認。本命の 65 モジュール下限は exp_acc_res.py で較正済み)
    S5 = 480
    canvas5 = np.full((S5, S5), 205, np.uint8)
    qr5 = Image.open(TEST_QR).convert("L").resize((200, 200), Image.NEAREST)
    off5 = (S5 - 200) // 2
    canvas5[off5:off5 + 200, off5:off5 + 200] = np.asarray(qr5)
    base5 = canvas5.astype(np.float32) * 0.82 + 23.0
    rng5 = np.random.default_rng(5)
    used5 = None
    sigma5 = 7.65
    for r in (1.2, 1.6, 2.0):
        degraded = _disk_blur(base5, r)
        if decode_gray(degraded) is None:
            continue
        for sg in (7.65, 14.0, 22.0, 30.0):
            noisy = np.clip(degraded + rng5.normal(0, sg, degraded.shape),
                            0, 255)
            if decode_gray(noisy) is None:
                used5, sigma5 = r, sg
                break
        if used5 is not None:
            break
    check("adaptive regime found", used5 is not None,
          f"(r={used5}, sigma={sigma5})")
    if used5 is not None:
        st5 = BurstStacker()
        st5.set_target(256)
        got5 = None
        blurred5 = _disk_blur(base5, used5)
        for _ in range(16):
            dx, dy = rng5.uniform(-1.5, 1.5, 2)
            fr = _shift_fft(blurred5, float(dx), float(dy))
            fr = np.clip(fr + rng5.normal(0, sigma5, fr.shape), 0, 255)
            got5 = st5.add(fr.astype(np.uint8)) or got5
        check("adaptive stack (256) decodes", got5 is not None,
              f"(acc_size={st5.acc_size})")
        check("acc shape is 256x256",
              st5.acc is not None and st5.acc.shape == (256, 256))
    # 脚長 → 累積サイズの写像 (単調性と範囲)
    sizes = [acc_size_for_leg(leg) for leg in (30.0, 60.0, 100.0, 160.0)]
    check("acc_size_for_leg sane",
          sizes == sorted(sizes, reverse=True)
          and all(s in ACC_SIZES for s in sizes), f"({sizes})")

    print("SELFTEST", "OK" if ok else "FAILED")
    return 0 if ok else 1


def _disk_blur(img: np.ndarray, r: float) -> np.ndarray:
    h, w = img.shape
    yy, xx = np.mgrid[0:h, 0:w]
    k = ((xx - w // 2) ** 2 + (yy - h // 2) ** 2 <= r * r).astype(np.float64)
    k /= k.sum()
    hf = np.fft.fft2(np.fft.ifftshift(k))
    return np.real(np.fft.ifft2(np.fft.fft2(img) * hf))


# ---------------------------------------------------------------- GUI 本体

def run_gui() -> int:
    from PyQt5.QtCore import Qt, QTimer, pyqtSignal, QObject
    from PyQt5.QtGui import QImage, QPixmap, QPainter, QPen, QColor
    from PyQt5.QtWidgets import (QApplication, QLabel, QMainWindow,
                                 QVBoxLayout, QWidget)
    from picamera2 import Picamera2
    import queue as queue_mod

    make_beep()

    class Signals(QObject):
        hit = pyqtSignal(str)

    signals = Signals()
    state = {
        "lores_y": None, "lock": threading.Lock(),
        "att": np.zeros((ATT_H, ATT_W), np.float32),
        "triple": None, "stack_info": "", "flash_until": 0.0,
        "stop": False, "last_hit": ("", 0.0),
        "frame_seq": 0, "last_frame_at": time.time(),
    }
    crop_queue: queue_mod.Queue = queue_mod.Queue(maxsize=2)
    stacker = BurstStacker()

    cam = Picamera2()
    # センサは Rotation=180 (上下逆実装) なので補正は 180 度回転
    # = hflip + vflip の両方が必要 (2026-11-16 実測で確定: 実写の
    # 文字を直接読んで判定。vflip のみでは左右が鏡像になった。
    # 2026-11-15 の「vflip で正しい」という結論は、逆さに置かれた
    # 箱との混同で誤っていた)
    from libcamera import Transform
    config = cam.create_video_configuration(
        main={"size": (MAIN_W, MAIN_H), "format": "YUV420"},
        lores={"size": (LORES_W, LORES_H), "format": "YUV420"},
        buffer_count=2,
        transform=Transform(hflip=1, vflip=1),
    )
    cam.configure(config)
    cam.start()
    time.sleep(1.0)
    try:
        cam.set_controls({"Sharpness": 2.0,
                          "ExposureTime": EXPOSURE_LADDER[1]})
    except Exception as exc:
        print(f"controls: {exc}", file=sys.stderr, flush=True)
    exp_idx = [1]

    def note_hit(text: str) -> None:
        last, when = state["last_hit"]
        if text == last and time.time() - when < COOLDOWN:
            return
        state["last_hit"] = (text, time.time())
        state["flash_until"] = time.time() + 0.7
        signals.hit.emit(text)

    def capture_loop() -> None:
        # picamera2 は単一スレッド前提 (v1 の教訓)。取り込みはここに一本化。
        # 重い処理 (走査) は別スレッドへ: 捕捉が ~1s 滞るとカーネルの
        # dequeue タイマが切れ、CMA 断片化と重なるとパイプラインが
        # 復帰不能になる (2026-11-15 実機 dmesg で確認)。
        frame_no = 0
        while not state["stop"]:
            arr = cam.capture_array("lores")
            ly = y_plane(arr, LORES_W, LORES_H).copy()
            with state["lock"]:
                state["lores_y"] = ly
                state["frame_seq"] += 1
                state["last_frame_at"] = time.time()
            frame_no += 1
            # 露光ラダー: 平均輝度が低ければ露光を伸ばす
            if frame_no % 90 == 0:
                mean = float(ly.mean())
                try:
                    if mean < 30 and exp_idx[0] < len(EXPOSURE_LADDER) - 1:
                        exp_idx[0] += 1
                        cam.set_controls(
                            {"ExposureTime": EXPOSURE_LADDER[exp_idx[0]]})
                    elif mean > 95 and exp_idx[0] > 0:
                        exp_idx[0] -= 1
                        cam.set_controls(
                            {"ExposureTime": EXPOSURE_LADDER[exp_idx[0]]})
                except Exception:
                    pass
            # main は交互に取り、中央 crop をスタックへ
            if frame_no % 2 == 0:
                arr = cam.capture_array("main")
                my = y_plane(arr, MAIN_W, MAIN_H)
                x0 = (MAIN_W - CROP) // 2
                y0 = (MAIN_H - CROP) // 2
                crop = my[y0:y0 + CROP, x0:x0 + CROP].copy()
                try:
                    crop_queue.put_nowait(crop)
                except queue_mod.Full:
                    pass
            time.sleep(0.02)

    def attention_loop() -> None:
        # 関心マップの維持 (捕捉済みフレームのコピーに対して走査する)
        seen = 0
        while not state["stop"]:
            with state["lock"]:
                seq = state["frame_seq"]
                frame = state["lores_y"]
            if frame is None or seq == seen:
                time.sleep(0.05)
                continue
            seen = seq
            if seq % 3 != 0:
                continue
            try:
                clusters = scan_finders(frame)
                triple = find_triple(clusters)
                att = state["att"]
                att *= 0.80
                for c in clusters:
                    splat(att, c[0], c[1], min(1.0, c[3] / 12.0))
                if triple:
                    gx = (triple["corner"][0] + triple["p4"][0]) / 2
                    gy = (triple["corner"][1] + triple["p4"][1]) / 2
                    splat(att, gx, gy, 0.30)
                    # コード寸法が分かったら累積解像度を適応させる
                    leg = math.hypot(
                        triple["corner"][0] - triple["p2"][0],
                        triple["corner"][1] - triple["p2"][1])
                    stacker.set_target(acc_size_for_leg(leg))
                np.clip(att, 0, 1, out=att)
                state["triple"] = triple
            except Exception as exc:
                print(f"scan error: {exc}", file=sys.stderr, flush=True)

    def decode_loop() -> None:
        # lores の速い経路 (大きなコード用、v1 互換)
        while not state["stop"]:
            with state["lock"]:
                frame = state["lores_y"]
            if frame is not None:
                try:
                    text = decode_gray(frame)
                except Exception as exc:
                    print(f"decode error: {exc}", file=sys.stderr, flush=True)
                    text = None
                if text:
                    note_hit(text)
            time.sleep(0.08)

    def stack_loop() -> None:
        while not state["stop"]:
            try:
                crop = crop_queue.get(timeout=0.3)
            except queue_mod.Empty:
                continue
            try:
                text = stacker.add(crop)
            except Exception as exc:
                print(f"stack error: {exc}", file=sys.stderr, flush=True)
                text = None
            state["stack_info"] = (
                f"スタック {stacker.count} 枚 (蓄積 {stacker.acc_size}px)"
                if stacker.count > 1 else "")
            if text:
                stacker.reset()
                note_hit(text)
            time.sleep(0.25)  # FFT の連続実行で帯域を独占しない

    class Window(QMainWindow):
        def __init__(self) -> None:
            super().__init__()
            self.setWindowTitle("QRリーダー v3")
            self.resize(880, 660)
            root = QWidget()
            layout = QVBoxLayout(root)
            self.view = QLabel()
            self.view.setAlignment(Qt.AlignCenter)
            self.view.setMinimumSize(320, 240)
            self.view.setStyleSheet("background:#111;")
            self.status = QLabel("QRコードを枠の中に入れてください")
            from PyQt5.QtGui import QFont
            self.status.setFont(QFont("", 15))
            self.status.setWordWrap(True)
            self.detail = QLabel("")
            self.detail.setFont(QFont("", 12))
            self.detail.setWordWrap(True)
            layout.addWidget(self.view, 1)
            layout.addWidget(self.status)
            layout.addWidget(self.detail)
            self.setCentralWidget(root)
            self.timer = QTimer(self)
            self.timer.timeout.connect(self.refresh)
            self.timer.start(50)
            signals.hit.connect(self.on_hit)

        def refresh(self) -> None:
            # ウォッチドッグ: 一度流れ始めたフレームが 6s 止まったら
            # プロセスごと再起動する (上限 3 回)。パイプラインが
            # CMA 断片化で復帰不能になっても固まったままにしない。
            if (state["frame_seq"] > 0
                    and time.time() - state["last_frame_at"] > 6.0):
                n = int(os.environ.get("QR_WATCHDOG_N", "0"))
                if n < 3:
                    print("watchdog: camera stalled, restarting",
                          file=sys.stderr, flush=True)
                    os.environ["QR_WATCHDOG_N"] = str(n + 1)
                    state["stop"] = True
                    try:
                        cam.stop()
                        cam.close()
                    except Exception:
                        pass
                    os.execv(sys.executable, [sys.executable] + sys.argv)
            with state["lock"]:
                frame = state["lores_y"]
            if frame is None:
                return
            comp = tone_composite(frame, state["att"])
            img = QImage(comp.data, LORES_W, LORES_H, LORES_W * 3,
                         QImage.Format_RGB888).copy()
            pix = QPixmap.fromImage(img)
            p = QPainter(pix)
            # レティクル (スタック対象 = main 中央 crop を lores へ写像)
            cw = int(CROP / MAIN_W * LORES_W * 0.84)
            ch = int(CROP / MAIN_H * LORES_H * 0.84)
            x0, y0 = (LORES_W - cw) // 2, (LORES_H - ch) // 2
            leg = cw // 4
            p.setPen(QPen(QColor(15, 15, 15), 3))
            for (ax, ay, bx, by) in [
                    (x0, y0, x0 + leg, y0), (x0, y0, x0, y0 + leg),
                    (x0 + cw - leg, y0, x0 + cw, y0),
                    (x0 + cw, y0, x0 + cw, y0 + leg),
                    (x0, y0 + ch - leg, x0, y0 + ch),
                    (x0, y0 + ch, x0 + leg, y0 + ch),
                    (x0 + cw - leg, y0 + ch, x0 + cw, y0 + ch),
                    (x0 + cw, y0 + ch - leg, x0 + cw, y0 + ch)]:
                p.drawLine(ax, ay, bx, by)
            # 3点ロック: ファインダ型マーカー + 四隅の結線
            triple = state["triple"]
            if triple:
                ink = QColor(10, 10, 10)
                pts = [triple["corner"], triple["p2"], triple["p3"]]
                u = triple["unit"]
                for (fx, fy) in pts:
                    r1 = int(u * 3.5)
                    r2 = int(u * 1.7)
                    p.setPen(QPen(ink, 3))
                    p.drawRect(int(fx) - r1, int(fy) - r1, r1 * 2, r1 * 2)
                    p.setBrush(ink)
                    p.drawRect(int(fx) - r2, int(fy) - r2, r2 * 2, r2 * 2)
                    p.setBrush(Qt.NoBrush)
                p.setPen(QPen(ink, 2))
                quad = pts + [triple["p4"]]
                order = [0, 1, 3, 2]  # corner -> p2 -> p4 -> p3
                for a, b in zip(order, order[1:] + order[:1]):
                    p.drawLine(int(quad[a][0]), int(quad[a][1]),
                               int(quad[b][0]), int(quad[b][1]))
            # 読み取りフラッシュ (白抜き)
            remain = state["flash_until"] - time.time()
            if remain > 0:
                alpha = int(200 * remain / 0.7)
                p.fillRect(0, 0, LORES_W, LORES_H,
                           QColor(255, 255, 255, max(0, min(200, alpha))))
            p.end()
            self.view.setPixmap(pix.scaled(
                self.view.size(), Qt.KeepAspectRatio,
                Qt.SmoothTransformation))
            info = state["stack_info"]
            if info and self.detail.text() != info:
                self.detail.setText(info)

        def on_hit(self, text: str) -> None:
            beep()
            QApplication.clipboard().setText(text)
            copied = set_clipboard(text) or True
            summary = mqjsp1_summary(text)
            if summary:
                self.status.setText("読み取りました: " + summary)
                self.detail.setText(
                    "全文はクリップボードへ" + ("コピー済み" if copied else
                                             "コピー失敗"))
            else:
                self.status.setText("読み取りました: " + mask(text))
                self.detail.setText(
                    "クリップボードへ" + ("コピーしました" if copied else
                                         "コピーに失敗しました"))
            QTimer.singleShot(5000, lambda: self.status.setText(
                "QRコードを枠の中に入れてください"))

        def keyPressEvent(self, e) -> None:  # noqa: N802 (Qt naming)
            if e.key() in (Qt.Key_Q, Qt.Key_Escape):
                self.close()
            elif e.key() == Qt.Key_F:
                if self.isFullScreen():
                    self.showNormal()
                else:
                    self.showFullScreen()
            else:
                super().keyPressEvent(e)

        def closeEvent(self, e) -> None:  # noqa: N802
            state["stop"] = True
            super().closeEvent(e)

    app = QApplication(sys.argv)
    win = Window()
    win.show()
    threads = [threading.Thread(target=capture_loop, daemon=True),
               threading.Thread(target=attention_loop, daemon=True),
               threading.Thread(target=decode_loop, daemon=True),
               threading.Thread(target=stack_loop, daemon=True)]
    for t in threads:
        t.start()
    rc = app.exec_()
    state["stop"] = True
    try:
        cam.stop()
        cam.close()
    except Exception:
        pass
    return rc


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] == "--selftest":
        return selftest()
    return run_gui()


if __name__ == "__main__":
    raise SystemExit(main())
