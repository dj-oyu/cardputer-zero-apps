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
    qr.save(OUT, scale=10, border=4)
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
