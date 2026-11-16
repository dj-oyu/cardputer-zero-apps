#!/bin/sh
# ランチャーの「zcode (dev)」から起動される開発用の入口（実機の ~/.local/bin/zcode-dev）。
# 何を動かすかは ~/.config/zcode-dev/args の 1 行目（引数）で決める。無ければ --keytest。
# 標準エラーは ~/.cache/zcode-dev/last.log に残す（SSH から読む）。
# ここはユーザーの書ける場所なので、差し替えに sudo は要らない（dev/README.md）。
dir="$HOME/.local/lib/zcode-dev"
args="--keytest"
[ -f "$HOME/.config/zcode-dev/args" ] && args=$(head -n 1 "$HOME/.config/zcode-dev/args")
mkdir -p "$HOME/.cache/zcode-dev"
# 引数は空白で分ける（クォートは使えない。開発用なのでこれで足りる）
# shellcheck disable=SC2086
exec "$dir/zcode" $args 2>"$HOME/.cache/zcode-dev/last.log"
