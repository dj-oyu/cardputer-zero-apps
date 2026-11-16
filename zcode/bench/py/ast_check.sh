#!/bin/sh
# 保存時の構文チェック (ast.parse を使い捨てのプロセスで実行) の重さを測る。
# 実機で: sh ast_check.sh [見本にする .py ファイル]
f="${1:-/usr/share/APPLaunch/lib/qrreader/qrreader_lcd.py}"
for i in 1 2 3; do
    s=$(date +%s%N)
    python3 -I -S -c 'import ast,sys,resource; ast.parse(open(sys.argv[1]).read()); print("maxrss", resource.getrusage(resource.RUSAGE_SELF).ru_maxrss, "KB")' "$f"
    e=$(date +%s%N)
    echo "ast.parse incl. startup: $(( (e - s) / 1000000 )) ms"
done
s=$(date +%s%N); python3 -I -S -c pass; e=$(date +%s%N)
echo "python3 startup only: $(( (e - s) / 1000000 )) ms"
