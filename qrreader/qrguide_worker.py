#!/usr/bin/env python3
"""距離ガイダンス用ファインダ走査ワーカー (v3.4)。

アプリ本体とは別プロセスで実行される。scan_finders は純 Python
で重く (820x616 で実機最悪約32秒)、本体内で回すと GIL を塞いで
捕捉スレッドを飢えさせるため分離した。

使い方: qrguide_worker.py <入力 .npy (half-scale main Y)> <出力 .json>
出力: {"triple": bool, "unit": float, "corner": [x, y],
       "p2": [x, y], "p3": [x, y]} (座標・unit は入力フレーム基準)
"""

from __future__ import annotations

import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import qrreader as core


def main() -> int:
    inp, outp = sys.argv[1], sys.argv[2]
    res: dict = {"triple": False}
    try:
        frame = np.load(inp)
        clusters = core.scan_finders(frame)
        triple = core.find_triple(clusters)
        if triple:
            res = {"triple": True,
                   "unit": float(triple["unit"]),
                   "corner": [float(triple["corner"][0]),
                              float(triple["corner"][1])],
                   "p2": [float(triple["p2"][0]),
                          float(triple["p2"][1])],
                   "p3": [float(triple["p3"][0]),
                          float(triple["p3"][1])]}
    except Exception as exc:  # 結果ファイルは必ず書く
        res = {"triple": False, "error": str(exc)}
    with open(outp, "w") as f:
        json.dump(res, f)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
