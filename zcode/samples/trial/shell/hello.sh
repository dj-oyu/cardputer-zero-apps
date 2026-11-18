#!/bin/bash
# 実機の情報を出す（F5 で実行）。
set -euo pipefail

greet() {
    local name=$1
    printf 'こんにちは、%s\n' "$name"
}

greet "$(whoami)"
echo "カーネル: $(uname -r)"
for f in /sys/class/thermal/thermal_zone*/temp; do
    [ -r "$f" ] && echo "温度: $(( $(cat "$f") / 1000 ))℃"
done
free -m | awk 'NR == 2 { print "空きメモリ: " $7 " MB" }'
