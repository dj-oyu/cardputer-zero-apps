#!/bin/bash
# コンテナの CPU の絞りが効いているかを見る（run.sh の数字を読む前に）。
# 4 本の空回しを 5 秒走らせ、cgroup の usage_usec の増え方を壁時計と比べる。
echo "cpu.max: $(cat /sys/fs/cgroup/cpu.max 2>/dev/null)  memory.max: $(cat /sys/fs/cgroup/memory.max 2>/dev/null)  nproc: $(nproc)"
u0=$(awk '/usage_usec/ {print $2}' /sys/fs/cgroup/cpu.stat)
for i in 1 2 3 4; do timeout 5 sh -c 'while :; do :; done' & done
wait
u1=$(awk '/usage_usec/ {print $2}' /sys/fs/cgroup/cpu.stat)
echo "4 本 × 5 秒で使った CPU 時間: $(( (u1 - u0) / 1000 )) ms（1 vCPU に絞れていれば約 5000）"
