#!/usr/bin/env python3
"""ハイライトの見本（tests/golden/highlight/python.txt と比べる）。

複数行の docstring: {placeholder} と %s と \n。
"""
import os, sys
from dataclasses import dataclass, field


@dataclass(frozen=True)
class Point(object):
    x: float = 0.0
    y: float = 0.0  # 座標（日本語のコメント）

    def dist(self, other: "Point" = None, *args, scale=1_000, **kw) -> float:
        if other is None:
            return 0
        return ((self.x - other.x) ** 2 + (self.y - other.y) ** 2) ** 0.5


async def fetch(url: str, retries: int = 3,
                timeout=(1.5, 2e-3)) -> bytes:
    for i in range(retries):
        try:
            await something(url)
        except (ValueError, OSError) as e:
            print(f"retry {i!r:>3} {e} {{literal}}", file=sys.stderr)
            continue
        finally:
            pass
    raise TimeoutError('gave up: %d tries' % retries)


def main(argv):
    pts = [Point(i, i * 2) for i in range(10)]
    total = sum(p.dist(pts[0]) for p in pts)
    path = r"C:\Users\name\file.txt"
    data = b'\x00\xff' + rb"\d+"
    名前 = "日本語の文字列 \u3042"
    flags = 0x1F | 0b1010 | 0o17
    s = '''multi
line "quoted" {x}
end'''
    fs = f"""total={total:.3f} {len(argv)}
    {pts[0].x}"""
    matrix = a @ b
    lam = lambda v: not v and True or False
    return 0 if total > 1 else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
