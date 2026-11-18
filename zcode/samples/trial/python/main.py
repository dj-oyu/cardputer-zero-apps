"""LED の点滅をまねる（F5 で実行。Ctrl+O でアウトライン）。"""
import time

from stats import Stats


def blink(n: int = 6, dt: float = 0.2) -> Stats:
    s = Stats()
    for i in range(n):
        on = i % 2 == 0
        print("●" if on else "○", i, flush=True)
        s.add(i)
        time.sleep(dt)
    return s


if __name__ == "__main__":
    s = blink()
    print(f"合計 {s.total}、平均 {s.mean():.2f}")
