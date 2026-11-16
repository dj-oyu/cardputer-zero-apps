#!/usr/bin/env python3
"""静止画デコードワーカー (v3.5)。

アプリ本体とは別プロセスで実行される。全解像度 (3280x2464) の
zxing デコードはノイズ場面で1回数十秒かかり、しかも GIL を
長く握るため、本体内で回すと捕捉・ウォッチドッグまで飢える
(2026-11-16 実機テスト4で確認)。そのため分離した。

使い方: qrdecode_worker.py <出力 .json> <JPEG パス> [JPEG パス...]
出力: {"results": [{"text": str, "bbox": [x0, y0, x1, y1]}...],
       "path": 結果が得られた JPEG, "size": [W, H]}
bbox は元画像 (全解像度) の画素座標。複数コードがある場合は
読めた全件を左→右順で返す (「どれが読まれたか」の表示・切替用)。
試行順: 1640 幅に縮小 → 全解像度 → 中央 1600x1600 切り出し。
最初に結果が得られた試行の全件を採用する。
"""

from __future__ import annotations

import json
import os
import sys

import numpy as np
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import qrreader as core


def decode_still(path: str) -> list[dict]:
    img = np.asarray(Image.open(path).convert("L"))
    # rpicam の画素は 180 度回転して出るが、回転はデコードに無害。
    # bbox も画素座標のまま返し、表示側で回転を戻す。
    h, w = img.shape
    cx0, cy0 = (w - 1600) // 2, (h - 1600) // 2
    small = np.asarray(Image.fromarray(img).resize((1640, 1232)))
    sx, sy = w / 1640.0, h / 1232.0
    attempts = (
        (small, lambda b: (b[0] * sx, b[1] * sy, b[2] * sx, b[3] * sy)),
        (img, lambda b: b),
        (img[cy0:cy0 + 1600, cx0:cx0 + 1600],
         lambda b: (b[0] + cx0, b[1] + cy0, b[2] + cx0, b[3] + cy0)),
    )
    for cand, conv in attempts:
        try:
            hits = core.decode_all(cand)
        except Exception:
            hits = []
        if not hits:
            continue
        seen: set[str] = set()
        results: list[dict] = []
        for text, bbox in hits:
            if text in seen:
                continue
            seen.add(text)
            b = conv(bbox)
            results.append({"text": text,
                            "bbox": [round(float(v), 1) for v in b]})
        results.sort(key=lambda r: (r["bbox"][0] + r["bbox"][2]) / 2)
        return results
    return []


def main() -> int:
    outp, paths = sys.argv[1], sys.argv[2:]
    payload: dict = {"results": [], "path": None, "size": None}
    for path in paths:
        try:
            results = decode_still(path)
            with Image.open(path) as im:
                size = list(im.size)
        except Exception:
            results, size = [], None
        if results:
            payload = {"results": results, "path": path, "size": size}
            break
    with open(outp, "w") as f:
        json.dump(payload, f)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
