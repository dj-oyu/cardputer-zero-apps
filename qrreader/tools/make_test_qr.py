#!/usr/bin/env python3
"""テスト用 QR 画像 (qrreader_test_qr.png) を生成する。

中身は https://example.com/v2-selftest 。selftest と描画テストが
使うフィクスチャで、生成物なので git には入れない (要 segno)。
"""

import os

import segno

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "..", "qrreader_test_qr.png")


def main() -> int:
    qr = segno.make("https://example.com/v2-selftest", error="m")
    # border=2 は selftest の前提 (ファインダ中心 = 貼付位置 + (2 + 3.5)
    # モジュール、較正レジームも 330px の画像で決めた)。4 にすると
    # 較正系の項目が NG になる
    qr.save(OUT, scale=10, border=2)
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
