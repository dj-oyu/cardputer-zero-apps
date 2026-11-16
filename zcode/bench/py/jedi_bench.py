#!/usr/bin/env python3
"""実機の jedi (python-lsp-server の中身) で補完と構文チェックの重さを測る。

キャッシュは /tmp に書かせ、最後に消す (SD に書かない)。

実機で: python3 jedi_bench.py [見本にする .py ファイル]
既定の見本は、インストール済みの QR Reader 本体 (約 1200 行)。
"""
import resource
import shutil
import sys
import time

CACHE = "/tmp/jedi-bench"
DEFAULT_SAMPLE = "/usr/share/APPLaunch/lib/qrreader/qrreader_lcd.py"


def rss_mb() -> int:
    for line in open("/proc/self/status"):
        if line.startswith("VmRSS"):
            return int(line.split()[1]) // 1024
    return 0


def main() -> None:
    sample = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_SAMPLE
    t = time.perf_counter()
    import jedi
    jedi.settings.cache_directory = CACHE
    print(f"import jedi: {(time.perf_counter() - t) * 1000:.0f}ms rss={rss_mb()}MB")
    src = open(sample).read()
    cases = [("os.pa", "import os\nos.pa"),
             ("np.ar", "import numpy as np\nnp.ar"),
             ("own-file", src + "\nui.")]
    try:
        for name, code in cases:
            for i in (1, 2):
                t = time.perf_counter()
                script = jedi.Script(code, path="/tmp/x.py")
                line = code.count("\n") + 1
                col = len(code.split("\n")[-1])
                comps = script.complete(line, col)
                print(f"{name:10s} run{i}: {(time.perf_counter() - t) * 1000:7.0f}ms "
                      f"n={len(comps):4d} rss={rss_mb()}MB")
        t = time.perf_counter()
        errs = jedi.Script(src, path="/tmp/y.py").get_syntax_errors()
        print(f"syntax errors ({src.count(chr(10))} lines): "
              f"{(time.perf_counter() - t) * 1000:.0f}ms n={len(errs)}")
        print("peak rss MB:", resource.getrusage(resource.RUSAGE_SELF).ru_maxrss // 1024)
    finally:
        shutil.rmtree(CACHE, ignore_errors=True)


if __name__ == "__main__":
    main()
