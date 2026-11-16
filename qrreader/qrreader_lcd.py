#!/usr/bin/env python3
"""QRリーダー LCD版 (APPLaunch アプリ) v3.5

本体液晶 (320x170, /dev/fb0 RGB565) へ直接描く版。エンジン
(ファインダ走査・関心マップ・トーン合成・バーストスタック) は
デスクトップ版 qrreader.py をそのまま import して使う。
v3.0: エンジン側の最小化に追随 (lores 320x240、適応累積)。
v3.3: メイン解像度の定期デコードを追加。従来はプレビュー
  (320x240) でファインダがロックしたときだけメイン側が
  デコードされ、自然な距離ではチラシ等のコードがプレビューで
  小さすぎてロックせず、全滅する構造だった。1.6 秒ごとに
  メイン中央クロップ (原寸) と全幅縮小を直接デコードする。
v3.4: 静止画シャッターと距離ガイダンスを追加。
  - 静止画シャッター: パネルが無いときに Enter / Space を押すと
    動画を止め、rpicam-still で全解像度 (3280x2464) を2枚撮って
    デコードし、動画に復帰する。カメラアプリの手持ち撮影で
    3枚中1枚読めたクラス (動画 1640 では物理的に不可能な
    約2 px/module 帯) をアプリに取り込む。往復の仕組みは
    2026-11-16 に実機で検証済み (1枚 約6.5秒)。
  - 距離ガイダンス: メイン解像度 (1/2 縮小) でもファインダ走査し、
    実測ピッチ (px/module) から「読める大きさ / あと少し /
    もっと近づいて」をステータスに案内する。lores で見えない
    コードもメイン側なら拾える (2026-11-16 実測でロック確認)。
    メイン側ロック時はスタッカーの目標解像度にも反映する。
v3.5: 「どれが読まれたか」の表示と、静止画フローのナビゲーション
  (モック2種をユーザーが承認, 2026-11-16):
  - 静止画で読めたら、元画像からコード周辺をクロップして背景に
    出し、読まれたコードに緑枠を付ける。複数件は ←/→ で切替
    (worker が全件+位置を返す)。動画ヒットでも位置が分かる経路
    (lores / メイン / スタック) はプレビュー上に緑枠を出す。
  - フローナビ: プレビューに案内チップ (ガイダンス連動)、
    Enter 後はステップ表示 (撮影→解析→結果) とフェーズカード
    (撮影中=枚数セグメント、解析中=撮った静止画を背景+進捗バー、
    読めず=もう一度/戻る) で一連の流れを案内する。
v3.5.1: 公開 (monorepo 化) に伴うリファクタ。LCD の土台
  (fb 描画・キー入力・パネル・フロー UI) を common/zero_ui.py
  へ切り出した。描画テストの出力が前後で完全一致することを
  確認済みで、振る舞いの変更はない。
v3.1: 結果表示をガラスパネル化 (モック v3 でユーザー承認):
  プレビューは暗くせず、読み取りと同時に半透明パネルが浮く。
  表示は一律・無編集 (種類ラベル・伏せ字・要約なし)。
  コピーは自動ではなく Enter を押したときだけ。Esc は
  コピーせず閉じる。コピー後は「いずれかのキー」で走査へ。

- プレビュー: lores Y 面 + トーン合成 (320x240) → 中央帯 320x170
  をそのまま切り出して RGB565 で fb0 へ (縮小なし)
- 終了キー: Esc / ` / BackSpace (パネル表示中は Esc = 閉じる)

描画テスト: python3 qrreader_lcd.py --render-test out.png
  (out_panel_url.png / out_panel_long.png も併せて出る)
"""

from __future__ import annotations

import math
import os
import sys
import threading
import time

import numpy as np

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
_COMMON = os.path.normpath(os.path.join(_HERE, "..", "common"))
if os.path.isdir(_COMMON):
    sys.path.insert(0, _COMMON)  # monorepo 配置 (deb 内では同居)
import qrreader as core
import zero_ui as ui
from zero_ui import (ACCENT, ENTER_KEYS, EXIT_KEYS, FB_PATH, KEY_DEV,
                     LCD_H, LCD_W, STATUS_H)

def fit_cover(img, w: int = LCD_W, h: int = LCD_H) -> np.ndarray:
    """PIL 画像を w x h に cover フィットした RGB 配列を返す。"""
    from PIL import Image
    iw, ih = img.size
    scale = max(w / iw, h / ih)
    nw, nh = max(w, round(iw * scale)), max(h, round(ih * scale))
    r = img.convert("RGB").resize((nw, nh), Image.LANCZOS)
    x0, y0 = (nw - w) // 2, (nh - h) // 2
    return np.asarray(r.crop((x0, y0, x0 + w, y0 + h)))


def still_full_bg(path: str) -> np.ndarray | None:
    """静止画 JPEG 全体を表示用背景 (320x170) にする。

    rpicam-still の画素は 180 度回転して出るので表示時に戻す。
    """
    from PIL import Image
    try:
        with Image.open(path) as im:
            return fit_cover(im.rotate(180))
    except Exception:
        return None


def still_crop_bg(path: str, bbox):
    """コード周辺のクロップ背景と、コードの相対矩形を返す。

    クロップ規則 (モックで承認): コードの外接矩形を中心に
    幅 = コード幅 x 5、高さは LCD 比。端ではクランプ。
    画素は 180 度回転して出るので、クロップを回転して戻し、
    相対矩形も鏡映する。戻り値: (背景 RGB 配列, (x, y, w, h)
    相対矩形) または None。
    """
    from PIL import Image
    try:
        img = Image.open(path).convert("RGB")
    except Exception:
        return None
    W, H = img.size
    x0, y0, x1, y1 = (float(v) for v in bbox)
    qw, qh = x1 - x0, y1 - y0
    if qw <= 0 or qh <= 0:
        return None
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    cw = qw * 5.0
    ch = cw * LCD_H / LCD_W
    if ch < qh * 2.2:
        ch = qh * 2.2
        cw = ch * LCD_W / LCD_H
    cw, ch = min(cw, float(W)), min(ch, float(H))
    ox = min(max(0.0, cx - cw / 2), W - cw)
    # コードは少し上に置く (下部に結果パネルが被るため)
    oy = min(max(0.0, cy - ch * 0.40), H - ch)
    crop = img.crop((int(ox), int(oy), int(ox + cw), int(oy + ch)))
    crop = crop.rotate(180).resize((LCD_W, LCD_H), Image.LANCZOS)
    rel = ((x0 - ox) / cw, (y0 - oy) / ch, qw / cw, qh / ch)
    disp = (1 - rel[0] - rel[2], 1 - rel[1] - rel[3], rel[2], rel[3])
    return np.asarray(crop), disp



def _cma_free_mb() -> float:
    """CMA ヒープの空き (MB)。読めなければ大きな値を返す。"""
    try:
        with open("/proc/meminfo") as f:
            for line in f:
                if line.startswith("CmaFree:"):
                    return int(line.split()[1]) / 1024.0
    except Exception:
        pass
    return 999.0




def render_frame(lores_y: np.ndarray, att: np.ndarray,
                 triple: dict | None, stack_count: int,
                 flash: float, status: str, plain: bool = False,
                 panel: dict | None = None,
                 bg: np.ndarray | None = None,
                 code_rel: tuple | None = None,
                 flow: dict | None = None,
                 now: float = 0.0) -> np.ndarray:
    """320x240 トーン合成 → 320x170 LCD フレーム (RGB)。

    v3.5: bg (静止画の背景 320x170 RGB) があればプレビュー合成の
    代わりに使い、code_rel (相対矩形) に「読まれたコード」の緑枠、
    flow (ステップ表示/チップ/フェーズカード) を重ねる。
    """
    from PIL import Image, ImageDraw
    if bg is not None:
        img = Image.fromarray(bg)
        if flow and flow.get("dim"):
            img = Image.blend(img, Image.new("RGB", img.size, (0, 0, 0)),
                              0.45)
        return ui.finish_frame(img, stack_count, status, panel,
                             code_rel, flow, now)
    if plain:  # 診断用: 合成をせず素の Y 面を出す
        comp = np.repeat(lores_y[:, :, None], 3, axis=2)
    else:
        comp = core.tone_composite(lores_y, att)
    img = Image.fromarray(comp)
    d = ImageDraw.Draw(img)
    # レティクル (デスクトップ版と同じ幾何)
    cw = int(core.CROP / core.MAIN_W * core.LORES_W * 0.84)
    ch = int(core.CROP / core.MAIN_H * core.LORES_H * 0.84)
    x0, y0 = (core.LORES_W - cw) // 2, (core.LORES_H - ch) // 2
    leg = cw // 4
    for (ax, ay, bx, by) in [
            (x0, y0, x0 + leg, y0), (x0, y0, x0, y0 + leg),
            (x0 + cw - leg, y0, x0 + cw, y0), (x0 + cw, y0, x0 + cw, y0 + leg),
            (x0, y0 + ch - leg, x0, y0 + ch), (x0, y0 + ch, x0 + leg, y0 + ch),
            (x0 + cw - leg, y0 + ch, x0 + cw, y0 + ch),
            (x0 + cw, y0 + ch - leg, x0 + cw, y0 + ch)]:
        d.line([ax, ay, bx, by], fill=(15, 15, 15), width=3)
    if triple:
        pts = [triple["corner"], triple["p2"], triple["p3"]]
        u = triple["unit"]
        for (fx, fy) in pts:
            r1, r2 = int(u * 3.5), int(u * 1.7)
            d.rectangle([fx - r1, fy - r1, fx + r1, fy + r1],
                        outline=(10, 10, 10), width=3)
            d.rectangle([fx - r2, fy - r2, fx + r2, fy + r2],
                        fill=(10, 10, 10))
        quad = pts + [triple["p4"]]
        order = [0, 1, 3, 2]
        for a, b in zip(order, order[1:] + order[:1]):
            d.line([quad[a], quad[b]], fill=(10, 10, 10), width=2)
    # 中央帯 320x170 をそのまま切り出す (v3: 合成が表示幅と同寸)
    top = (core.LORES_H - LCD_H) // 2
    img = img.crop((0, top, LCD_W, top + LCD_H))
    if flash > 0:
        white = Image.new("RGB", img.size, (255, 255, 255))
        img = Image.blend(img, white, min(0.85, flash))
    if flow and flow.get("dim"):
        img = Image.blend(img, Image.new("RGB", img.size, (0, 0, 0)),
                          0.45)
    return ui.finish_frame(img, stack_count, status, panel,
                         code_rel, flow, now)


def render_test(path: str) -> int:
    scene, _ = core._test_scene()
    clusters = core.scan_finders(scene)
    triple = core.find_triple(clusters)
    att = np.zeros((core.ATT_H, core.ATT_W), np.float32)
    for c in clusters:
        core.splat(att, c[0], c[1], min(1.0, c[3] / 12.0))
    frame = render_frame(scene, att, triple, 12, 0.0, "TEST LOCK")
    from PIL import Image
    Image.fromarray(frame).save(path)
    print(f"render-test -> {path} triple={triple is not None}")
    # パネルの見た目確認 (URL / 長文 / コピー後)
    import base64
    import json
    dummy = {"v": 1, "id": "dev-01", "device": "zero-2",
             "wifi": {"ssid": "home-wifi", "password": "dummy"},
             "tailscale": {"authKey": "tskey-auth-dummy"}}
    mqj = "MQJSP1:" + base64.urlsafe_b64encode(json.dumps(
        dummy, separators=(",", ":"), sort_keys=True).encode()
    ).rstrip(b"=").decode()
    stem = path.rsplit(".", 1)[0]
    for name, panel in (
            ("url", {"text": "https://oc-m.jp/aeropressgo100",
                     "copied": False}),
            ("long", {"text": mqj, "copied": False}),
            ("copied", {"text": "https://oc-m.jp/aeropressgo100",
                        "copied": True})):
        fr = render_frame(scene, att, triple, 12, 0.0, "TEST LOCK",
                          panel=panel)
        out = f"{stem}_panel_{name}.png"
        Image.fromarray(fr).save(out)
        print(f"render-test panel -> {out}")
    # v3.5: フロー UI と静止画結果の見た目確認
    synth = "/tmp/qr_synth_still.jpg"
    canvas = Image.new("L", (3280, 2464), 210)
    qr_big = Image.open(core.TEST_QR).convert("L").resize((620, 620))
    canvas.paste(qr_big, (1300, 900))
    canvas.convert("RGB").save(synth, quality=92)
    bbox = (1338, 938, 1884, 1482)  # worker 実測の外接矩形
    got = still_crop_bg(synth, bbox)
    full_bg = still_full_bg(synth)
    variants = []
    if got is not None:
        bg, rel = got
        variants.append(("flow_result", dict(
            bg=bg, code_rel=rel, flow={"stepper": 3},
            panel={"text": "https://example.com/v2-selftest",
                   "copied": False, "still": True, "idx": 0, "count": 2,
                   "frame_rel": rel})))
    variants.append(("flow_shoot", dict(flow={
        "stepper": 0, "dim": True,
        "card": {"big": "撮影中", "note": "そのまま動かさないで",
                 "kind": "segs", "segs": (1, 2)}})))
    variants.append(("flow_analyze", dict(bg=full_bg, flow={
        "stepper": 1, "dim": True,
        "card": {"big": "解析中", "note": "全解像度で読み取り中",
                 "kind": "bar"}})))
    variants.append(("flow_miss", dict(bg=full_bg, flow={
        "stepper": 2, "dim": True,
        "card": {"big": "読めませんでした",
                 "note": "近づくか、明るい場所でもう一度",
                 "kind": "keys"}})))
    variants.append(("flow_chip", dict(
        flow={"chip": "静止画で詳しく読む"})))
    for name, kw in variants:
        fr = render_frame(scene, att, triple, 12, 0.0, "TEST",
                          now=0.4, **kw)
        out = f"{stem}_{name}.png"
        Image.fromarray(fr).save(out)
        print(f"render-test flow -> {out}")
    return 0


def run_lcd() -> int:
    from picamera2 import Picamera2
    from libcamera import Transform
    import queue as queue_mod

    core.make_beep()
    state = {
        "lores_y": None, "lock": threading.Lock(),
        "att": np.zeros((core.ATT_H, core.ATT_W), np.float32),
        "triple": None, "stop": False, "last_hit": ("", 0.0),
        "flash_until": 0.0, "status": "SCAN", "panel": None,
        "frame_seq": 0, "last_frame_at": time.time(),
        "main_y": None, "main_at": 0.0,
        "still_req": False, "still_active": False,
        "notice": None, "guide": None, "guide_at": 0.0,
        # v3.5: 静止画フロー (shoot/analyze/result/miss) と結果表示
        "flow": None, "flow_at": 0.0, "shots_done": 0,
        "still_bg": None, "still_results": None, "still_path": None,
        "still_gen": 0,
    }
    crop_queue: queue_mod.Queue = queue_mod.Queue(maxsize=2)
    still_files_q: queue_mod.Queue = queue_mod.Queue(maxsize=2)
    stacker = core.BurstStacker()
    exp_idx = [1]

    def make_cam() -> Picamera2:
        c = Picamera2()
        config = c.create_video_configuration(
            main={"size": (core.MAIN_W, core.MAIN_H), "format": "YUV420"},
            lores={"size": (core.LORES_W, core.LORES_H), "format": "YUV420"},
            buffer_count=2,
            transform=Transform(hflip=1, vflip=1),  # 180度回転の補正
            # (2026-11-16 実測確定: vflip のみだと左右が鏡像になる)
        )
        c.configure(config)
        c.start()
        time.sleep(1.0)
        try:
            c.set_controls({"Sharpness": 2.0,
                            "ExposureTime": core.EXPOSURE_LADDER[exp_idx[0]]})
        except Exception as exc:
            print(f"controls: {exc}", file=sys.stderr, flush=True)
        return c

    cam_box = {"cam": make_cam()}

    def note_hit(text: str, frame_rel: tuple | None = None) -> None:
        # v3.1: 自動コピーはしない。パネルを出し、コピーは Enter の
        # 意図的なアクションに委ねる (モック v3 の仕様)。
        # v3.5: frame_rel (LCD 帯の相対矩形) があれば「読まれた
        # コード」に緑枠を出す。静止画パネル表示中は動画ヒットで
        # 上書きしない。
        panel = state["panel"]
        if panel is not None and panel.get("still"):
            return
        last, when = state["last_hit"]
        if text == last and time.time() - when < core.COOLDOWN:
            return
        state["last_hit"] = (text, time.time())
        state["flash_until"] = time.time() + 0.7
        core.beep()
        new_panel: dict = {"text": text, "copied": False}
        if frame_rel is not None:
            new_panel["frame_rel"] = frame_rel
        state["panel"] = new_panel

    band_top = (core.LORES_H - LCD_H) // 2

    def rel_from_lores(bbox) -> tuple | None:
        """lores 座標の外接矩形を LCD 帯の相対矩形へ (帯外は None)。"""
        x0, y0, x1, y1 = (float(v) for v in bbox)
        rx, ry = x0 / core.LORES_W, (y0 - band_top) / LCD_H
        rx1, ry1 = x1 / core.LORES_W, (y1 - band_top) / LCD_H
        if rx1 <= 0 or ry1 <= 0 or rx >= 1 or ry >= 1:
            return None
        cx, cy = max(0.0, rx), max(0.0, ry)
        return (cx, cy, min(1.0, rx1) - cx, min(1.0, ry1) - cy)

    def rel_from_main(bbox) -> tuple | None:
        """メイン座標の外接矩形を LCD 帯の相対矩形へ。"""
        kx = core.LORES_W / core.MAIN_W
        ky = core.LORES_H / core.MAIN_H
        return rel_from_lores((bbox[0] * kx, bbox[1] * ky,
                               bbox[2] * kx, bbox[3] * ky))

    def clear_still_view() -> None:
        state["flow"] = None
        state["still_bg"] = None
        state["still_results"] = None
        state["still_path"] = None

    def show_still_result(idx: int) -> None:
        """静止画の結果 idx 件目を背景+枠+パネルに反映する。"""
        results = state["still_results"]
        if not results or state["still_path"] is None:
            return
        idx %= len(results)
        got = still_crop_bg(state["still_path"], results[idx]["bbox"])
        if got is None:
            return
        bg, rel = got
        state["still_bg"] = bg
        state["panel"] = {"text": results[idx]["text"], "copied": False,
                          "still": True, "idx": idx,
                          "count": len(results), "frame_rel": rel}

    def still_sequence() -> None:
        # v3.4 静止画シャッター。capture_loop (カメラ所有スレッド) の
        # 中から呼ばれる。動画を止め → rpicam-still で全解像度を
        # 2枚続けて撮り → 動画を再生成して復帰 → その後にデコード
        # する。撮影とデコードを混ぜると2枚目がハングした実績が
        # あり (2026-11-16 実機)、単体で実績のある「続けて撮る」
        # 形に寄せる。2枚目は CMA の空きが少ないときは撮らない。
        import subprocess
        from PIL import Image as PILImage
        state["still_active"] = True
        state["still_gen"] += 1
        gen = state["still_gen"]
        state["flow"] = "shoot"
        state["flow_at"] = time.time()
        state["shots_done"] = 0
        state["still_bg"] = None
        state["still_results"] = None
        state["still_path"] = None
        state["last_frame_at"] = time.time()
        state["notice"] = ("STILL 撮影中", time.time() + 40)
        paths: list[str] = []
        try:
            try:
                cam_box["cam"].stop()
                cam_box["cam"].close()
            except Exception:
                pass
            print(f"still: CmaFree={_cma_free_mb():.0f}MB",
                  file=sys.stderr, flush=True)
            time.sleep(0.8)  # デバイス解放の完了待ち (255 対策)
            for i in range(2):
                state["notice"] = (f"STILL {i + 1}/2",
                                   time.time() + 40)
                if i == 1 and _cma_free_mb() < 12:
                    print("still: skip shot 2 (CMA low)",
                          file=sys.stderr, flush=True)
                    break
                path = f"/tmp/qr_still_{i}.jpg"
                ok = False
                for attempt in range(2):
                    try:
                        subprocess.run(
                            ["rpicam-still", "-o", path, "--width", "3280",
                             "--height", "2464", "-t", "1000",
                             "--nopreview"],
                            check=True, capture_output=True, timeout=15)
                        ok = True
                        break
                    except Exception as exc:
                        err = getattr(exc, "stderr", b"") or b""
                        if isinstance(err, bytes):
                            err = err.decode(errors="replace")
                        print(f"still capture failed (try {attempt}): "
                              f"{exc} {err[-300:]}",
                              file=sys.stderr, flush=True)
                        time.sleep(2.0)
                if not ok:
                    break
                paths.append(path)
                state["shots_done"] = len(paths)
                time.sleep(0.5)
        finally:
            try:
                cam_box["cam"] = make_cam()
            except Exception as exc:
                print(f"camera reopen failed: {exc}",
                      file=sys.stderr, flush=True)
            state["last_frame_at"] = time.time()
        # デコードは専用スレッドへ渡し、捕捉は即座に再開する
        # (全解像度デコードは数秒〜十数秒かかり、捕捉スレッドを
        # 止めるとウォッチドッグと競合する: 2026-11-16 実機)
        # 解析中の背景は撮った静止画そのもの (モックで承認)。
        state["flow"] = "analyze"
        state["flow_at"] = time.time()
        if paths:
            state["still_bg"] = still_full_bg(paths[0])
        state["notice"] = ("STILL 解析中", time.time() + 30)
        still_files_q.put((gen, paths))
        state["still_active"] = False

    def still_decode_loop() -> None:
        # デコード自体は別プロセス (qrdecode_worker.py) に出す。
        # 全解像度 zxing は GIL を長く握り、本体内では捕捉と
        # ウォッチドッグを飢えさせた (2026-11-16 実機テスト4)。
        # このスレッドは subprocess の完了を待つだけ (待機中は
        # GIL を解放する)。
        import json
        import subprocess
        worker = os.path.join(
            os.path.dirname(os.path.abspath(__file__)),
            "qrdecode_worker.py")
        outp = "/tmp/qr_still_result.json"
        while not state["stop"]:
            try:
                gen, paths = still_files_q.get(timeout=0.5)
            except queue_mod.Empty:
                continue
            payload = None
            try:
                subprocess.run([sys.executable, worker, outp, *paths],
                               capture_output=True, timeout=90)
                with open(outp) as f:
                    payload = json.load(f)
            except Exception as exc:
                print(f"still decode failed: {exc}",
                      file=sys.stderr, flush=True)
            results = (payload or {}).get("results") or []
            print(f"still decode done: {len(results)} hit(s)",
                  file=sys.stderr, flush=True)
            if gen != state["still_gen"]:
                continue  # キャンセル済み/次の撮影が始まっている
            if results:
                state["still_results"] = results
                state["still_path"] = payload.get("path")
                state["last_hit"] = (results[0]["text"], time.time())
                state["flash_until"] = time.time() + 0.7
                core.beep()
                show_still_result(0)
                state["flow"] = "result"
                state["flow_at"] = time.time()
                state["notice"] = ("STILL 読めた", time.time() + 3)
            else:
                state["flow"] = "miss"
                state["flow_at"] = time.time()
                state["notice"] = ("STILL 読めず", time.time() + 4)

    def capture_loop() -> None:
        # 捕捉専用スレッド。重い走査は attention_loop 側 (qrreader.py 参照:
        # 捕捉の滞留が dequeue タイマ切れ→CMA割当失敗の引き金になる)。
        frame_no = 0
        t_start = time.time()
        while not state["stop"]:
            if state["still_req"] and "still" not in disable:
                state["still_req"] = False
                still_sequence()
                state["last_frame_at"] = time.time()
            arr = cam_box["cam"].capture_array("lores")
            if frame_no % 150 == 0:
                print(f"hb frame={frame_no} "
                      f"t={time.time() - t_start:.0f}s", file=sys.stderr,
                      flush=True)
            ly = core.y_plane(arr, core.LORES_W, core.LORES_H).copy()
            with state["lock"]:
                state["lores_y"] = ly
                state["frame_seq"] += 1
                state["last_frame_at"] = time.time()
            frame_no += 1
            if frame_no % 90 == 0:
                mean = float(ly.mean())
                try:
                    if mean < 30 and exp_idx[0] < len(core.EXPOSURE_LADDER) - 1:
                        exp_idx[0] += 1
                        cam_box["cam"].set_controls(
                            {"ExposureTime": core.EXPOSURE_LADDER[exp_idx[0]]})
                    elif mean > 95 and exp_idx[0] > 0:
                        exp_idx[0] -= 1
                        cam_box["cam"].set_controls(
                            {"ExposureTime": core.EXPOSURE_LADDER[exp_idx[0]]})
                except Exception:
                    pass
            if frame_no % 2 == 0:
                arr = cam_box["cam"].capture_array("main")
                my = core.y_plane(arr, core.MAIN_W, core.MAIN_H)
                if time.time() - state["main_at"] > 1.0:
                    state["main_y"] = my.copy()
                    state["main_at"] = time.time()
                x0 = (core.MAIN_W - core.CROP) // 2
                y0 = (core.MAIN_H - core.CROP) // 2
                crop = my[y0:y0 + core.CROP, x0:x0 + core.CROP].copy()
                try:
                    crop_queue.put_nowait(crop)
                except queue_mod.Full:
                    pass
            time.sleep(0.02)

    def attention_loop() -> None:
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
                clusters = core.scan_finders(frame)
                triple = core.find_triple(clusters)
                att = state["att"]
                att *= 0.80
                for c in clusters:
                    core.splat(att, c[0], c[1], min(1.0, c[3] / 12.0))
                if triple:
                    gx = (triple["corner"][0] + triple["p4"][0]) / 2
                    gy = (triple["corner"][1] + triple["p4"][1]) / 2
                    core.splat(att, gx, gy, 0.30)
                    leg = math.hypot(
                        triple["corner"][0] - triple["p2"][0],
                        triple["corner"][1] - triple["p2"][1])
                    stacker.set_target(core.acc_size_for_leg(leg))
                    if state["status"] == "SCAN":
                        state["status"] = "LOCK"
                elif state["status"] == "LOCK":
                    state["status"] = "SCAN"
                np.clip(att, 0, 1, out=att)
                state["triple"] = triple
            except Exception as exc:
                print(f"scan error: {exc}", file=sys.stderr, flush=True)

    def decode_loop() -> None:
        while not state["stop"]:
            with state["lock"]:
                frame = state["lores_y"]
            if frame is not None:
                try:
                    hits = core.decode_all(frame)
                except Exception:
                    hits = []
                if hits:
                    note_hit(hits[0][0], rel_from_lores(hits[0][1]))
            time.sleep(0.08)

    def main_loop() -> None:
        # v3.3: ロックしなくてもメイン解像度を定期的に直接デコード。
        # 中央クロップ原寸 → 全幅 960 縮小の順 (どちらも背景スレッドで
        # 実行し、失敗時の全走査コストはプレビューに影響しない)。
        from PIL import Image as PILImage
        flip = False
        while not state["stop"]:
            time.sleep(1.6)
            frame = state["main_y"]
            if frame is None or state["panel"] is not None:
                continue
            x0 = (core.MAIN_W - core.CROP) // 2
            y0 = (core.MAIN_H - core.CROP) // 2
            crop = frame[y0:y0 + core.CROP, x0:x0 + core.CROP]
            small = np.asarray(PILImage.fromarray(frame).resize(
                (960, int(960 * core.MAIN_H / core.MAIN_W))))
            attempts = ((crop, (x0, y0, 1.0)), (small, (0, 0, 1640 / 960)))
            if flip:
                attempts = attempts[::-1]
            flip = not flip
            for img, (ox, oy, k) in attempts:
                if state["stop"] or state["panel"] is not None:
                    break
                try:
                    hits = core.decode_all(img)
                except Exception:
                    hits = []
                if hits:
                    b = hits[0][1]
                    main_b = (b[0] * k + ox, b[1] * k + oy,
                              b[2] * k + ox, b[3] * k + oy)
                    note_hit(hits[0][0], rel_from_main(main_b))
                    break

    def guidance_loop() -> None:
        # v3.4 距離ガイダンス: メイン解像度 (1/2 に間引き) でも
        # ファインダ走査し、実測ピッチ (px/module @1640) から
        # 読み取り可能な大きさかを判定して state["guide"] に置く。
        # 閾値は実測ベース: 動画デコードの実用域は約 3 px/module、
        # 静止画 (3280) はその2倍のピッチになるので 1.8 以上なら
        # シャッターの射程。
        # 走査は重い (820x616 で実機最悪約32秒、純 Python のため
        # GIL を塞ぐ) ので、別プロセス (qrguide_worker.py) に出し、
        # このスレッドは結果ファイルを読むだけにする。
        import json
        import subprocess
        worker = os.path.join(
            os.path.dirname(os.path.abspath(__file__)),
            "qrguide_worker.py")
        inp, outp = "/tmp/qr_guide_in.npy", "/tmp/qr_guide_out.json"
        while not state["stop"]:
            frame = state["main_y"]
            if frame is None or state["still_active"]:
                time.sleep(2.0)
                continue
            res = None
            try:
                np.save(inp, frame[::2, ::2])
                subprocess.run([sys.executable, worker, inp, outp],
                               capture_output=True, timeout=45)
                with open(outp) as f:
                    res = json.load(f)
            except Exception:
                res = None
            if res and res.get("triple"):
                pitch_main = float(res["unit"]) * 2.0
                if pitch_main >= 3.0:
                    level = 2
                elif pitch_main >= 1.8:
                    level = 1
                else:
                    level = 0
                state["guide"] = {"pitch": pitch_main, "level": level}
                state["guide_at"] = time.time()
                if state["triple"] is None:
                    leg_main = math.hypot(
                        res["corner"][0] - res["p2"][0],
                        res["corner"][1] - res["p2"][1]) * 2.0
                    stacker.set_target(core.acc_size_for_leg(
                        leg_main * core.LORES_W / core.MAIN_W))
            elif time.time() - state["guide_at"] > 3.0:
                state["guide"] = None
            time.sleep(1.0)

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
            if text:
                rel = None
                lh = stacker.last_hit
                if lh is not None and stacker.acc_size:
                    b = lh[1]
                    k = core.CROP / stacker.acc_size
                    ox = (core.MAIN_W - core.CROP) // 2
                    oy = (core.MAIN_H - core.CROP) // 2
                    rel = rel_from_main((b[0] * k + ox, b[1] * k + oy,
                                         b[2] * k + ox, b[3] * k + oy))
                stacker.reset()
                note_hit(text, rel)
            time.sleep(0.25)  # FFT の連続実行で帯域を独占しない

    def key_loop() -> None:
        try:
            reader = ui.KeyReader()
        except OSError:
            return
        while not state["stop"]:
            for code in reader.poll():
                panel = state["panel"]
                if panel is not None:
                    # パネル表示中: キーはパネルへの応答に使う
                    if panel["copied"]:
                        state["panel"] = None  # いずれかで走査へ
                        if panel.get("still"):
                            clear_still_view()
                    elif (panel.get("still")
                            and panel.get("count", 1) > 1
                            and code in (105, 106)):  # ←/→: 切替
                        show_still_result(
                            panel["idx"] + (1 if code == 106 else -1))
                    elif code in ENTER_KEYS:
                        core.set_clipboard(panel["text"])
                        panel["copied"] = True
                        core.beep()
                    elif code == 1:  # ESC: コピーせず閉じる
                        state["panel"] = None
                        if panel.get("still"):
                            clear_still_view()
                    continue
                if state["flow"] == "miss":
                    # 読めずカード: Enter=もう一度 / Esc=戻る
                    if code in ENTER_KEYS or code == 57:
                        if not state["still_active"]:
                            state["still_req"] = True
                    elif code == 1 or code in EXIT_KEYS:
                        state["still_gen"] += 1
                        clear_still_view()
                    continue
                if state["flow"] in ("shoot", "analyze"):
                    continue  # フロー中はキーを拾わない
                if code in ENTER_KEYS or code == 57:
                    # Enter / Space: 静止画シャッター (v3.4)
                    if not state["still_active"]:
                        state["still_req"] = True
                    continue
                if code in EXIT_KEYS:
                    state["stop"] = True
                    return
            time.sleep(0.05)
        reader.close()

    # 診断用スイッチ: QR_DISABLE=stack,scan,decode,tone,nodecode,maindec,still,guide
    disable = {s for s in os.environ.get("QR_DISABLE", "").split(",") if s}
    if "nodecode" in disable:
        core.STACK_DECODE = False
    if disable:
        print(f"disabled components: {sorted(disable)}",
              file=sys.stderr, flush=True)
    threads = [threading.Thread(target=capture_loop, daemon=True),
               threading.Thread(target=key_loop, daemon=True)]
    if "scan" not in disable:
        threads.append(threading.Thread(target=attention_loop, daemon=True))
    if "decode" not in disable:
        threads.append(threading.Thread(target=decode_loop, daemon=True))
    if "maindec" not in disable:
        threads.append(threading.Thread(target=main_loop, daemon=True))
    if "guide" not in disable:
        threads.append(threading.Thread(target=guidance_loop, daemon=True))
    if "still" not in disable:
        threads.append(threading.Thread(target=still_decode_loop,
                                         daemon=True))
    if "stack" not in disable:
        threads.append(threading.Thread(target=stack_loop, daemon=True))
    for t in threads:
        t.start()
    if os.environ.get("QR_STILL_TEST"):
        # 遠隔検証用: 起動 8 秒後に静止画シャッターを1回自動で切る
        def _auto_still() -> None:
            time.sleep(8)
            state["still_req"] = True
        threading.Thread(target=_auto_still, daemon=True).start()

    fb = ui.FrameBuffer()
    try:
        while not state["stop"]:
            # ウォッチドッグ (デスクトップ版と同じ: 上限3回まで自動再起動)
            if (state["frame_seq"] > 0
                    and not state["still_active"]
                    and time.time() - state["last_frame_at"] > 6.0):
                n = int(os.environ.get("QR_WATCHDOG_N", "0"))
                if n < 3:
                    print("watchdog: camera stalled, restarting",
                          file=sys.stderr, flush=True)
                    os.environ["QR_WATCHDOG_N"] = str(n + 1)
                    state["stop"] = True
                    try:
                        cam_box["cam"].stop()
                        cam_box["cam"].close()
                    except Exception:
                        pass
                    fb.close()
                    os.execv(sys.executable, [sys.executable] + sys.argv)
            with state["lock"]:
                frame = state["lores_y"]
            if frame is None:
                time.sleep(0.05)
                continue
            remain = state["flash_until"] - time.time()
            flash = max(0.0, remain / 0.7) if remain > 0 else 0.0
            now = time.time()
            notice = state["notice"]
            guide = state["guide"]
            if notice and now < notice[1]:
                disp = notice[0]
            elif guide:
                disp = {2: "QR 読める大きさ",
                        1: "QR あと少し Enter:静止画",
                        0: "QR もっと近づいて"}[guide["level"]]
            else:
                disp = state["status"]
            # v3.5: 静止画フローのナビゲーション要素を組み立てる
            flow = state["flow"]
            flow_ui = None
            bg = None
            if flow == "shoot":
                flow_ui = {"stepper": 0, "dim": True,
                           "card": {"big": "撮影中",
                                    "note": "そのまま動かさないで",
                                    "kind": "segs",
                                    "segs": (state["shots_done"], 2)}}
            elif flow == "analyze":
                bg = state["still_bg"]
                flow_ui = {"stepper": 1, "dim": True,
                           "card": {"big": "解析中",
                                    "note": "全解像度で読み取り中",
                                    "kind": "bar"}}
            elif flow == "result":
                bg = state["still_bg"]
                flow_ui = {"stepper": 3}
            elif flow == "miss":
                if now - state["flow_at"] > 4.0:
                    clear_still_view()
                    flow = None
                else:
                    bg = state["still_bg"]
                    flow_ui = {"stepper": 2, "dim": True,
                               "card": {"big": "読めませんでした",
                                        "note": "近づくか、明るい場所でもう一度",
                                        "kind": "keys"}}
            if (flow_ui is None and state["panel"] is None
                    and guide and guide["level"] == 1):
                # 案内チップは静止画の射程に入った時だけ
                flow_ui = {"chip": "静止画で詳しく読む"}
            panel = state["panel"]
            rgb = render_frame(frame, state["att"], state["triple"],
                               stacker.count, flash, disp,
                               plain=("tone" in disable),
                               panel=panel, bg=bg,
                               code_rel=(panel.get("frame_rel")
                                         if panel else None),
                               flow=flow_ui, now=now)
            fb.write(ui.to_rgb565(rgb))
            time.sleep(0.066)
    finally:
        state["stop"] = True
        fb.close()
        try:
            cam_box["cam"].stop()
            cam_box["cam"].close()
        except Exception:
            pass
    return 0


def main() -> int:
    if len(sys.argv) > 2 and sys.argv[1] == "--render-test":
        return render_test(sys.argv[2])
    return run_lcd()


if __name__ == "__main__":
    raise SystemExit(main())
