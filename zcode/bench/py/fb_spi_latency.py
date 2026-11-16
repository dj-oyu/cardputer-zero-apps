#!/usr/bin/env python3
"""fb0 に書いてから SPI 転送が始まるまでの時間を、fsync の有無で比べる。

SPI コントローラの送信バイト数 (sysfs の統計) を 0.3ms ごとに読み、書き込みから
カウンタが動き始めるまでの時間を測る。fb0 には読んだ内容と同じバイトを書き戻すので、
画面の見た目は変わらない。

注意: APPLaunch がランチャーを表示している間は、APPLaunch 自身の再描画でも
カウンタが動く (下の idle bytes/s)。転送の「終わり」はこのため測れない。
転送の終わりと連続 fsync の挙動は mcbench の fb / fb2 で測る。

実機で: python3 fb_spi_latency.py
"""
import mmap
import os
import statistics as st
import time

STATS = "/sys/class/spi_master/spi0/statistics/bytes_tx"
W, H = 320, 170
ROW = W * 2


def tx() -> int:
    with open(STATS) as f:
        return int(f.read())


def main() -> None:
    fd = os.open("/dev/fb0", os.O_RDWR)
    m = mmap.mmap(fd, W * H * 2)
    orig = m[:]
    b0 = tx()
    time.sleep(1)
    print("idle bytes/s (APPLaunch など):", tx() - b0)

    def run(r0: int, r1: int, label: str, use_fsync: bool, n: int = 12) -> None:
        starts = []
        for _ in range(n):
            time.sleep(0.25)
            base = tx()
            t0 = time.perf_counter()
            m[r0 * ROW:r1 * ROW] = orig[r0 * ROW:r1 * ROW]
            if use_fsync:
                os.fsync(fd)
            while time.perf_counter() - t0 < 0.25:
                if tx() != base:
                    starts.append((time.perf_counter() - t0) * 1000)
                    break
                time.sleep(0.0003)
        if starts:
            print(f"{label:28s} start median {st.median(starts):5.1f}ms "
                  f"(min {min(starts):.1f} max {max(starts):.1f}) n={len(starts)}")
        else:
            print(label, "no transfer seen")

    try:
        run(80, 91, "1 text row (11px), no fsync", False)
        run(80, 91, "1 text row (11px), fsync", True)
        run(0, H, "full screen, no fsync", False)
        run(0, H, "full screen, fsync", True)
    finally:
        m[:] = orig
        os.fsync(fd)


if __name__ == "__main__":
    main()
