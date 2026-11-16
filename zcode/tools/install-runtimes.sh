#!/usr/bin/env bash
# 実機に、zcode の F5 と型検査で使う実行環境を入れる（sudo は要らない）。
# docs/stage3-design.md §10.3・§12.3。雛形: 中身は課題 R1。
#
#   実機で:  ./install-runtimes.sh --from <ファイルを置いたフォルダ> [llrt] [tsc] [llrt-types] [zc-remote]
#            （名前を省くと、フォルダにあるものを全部）
#            ./install-runtimes.sh --list        入っているものと版
#            ./install-runtimes.sh --remove <名前>
#
# 入れるもの（版と SHA-256 は隣の runtimes.lock。**合わないファイルは入れない**）:
#
#   名前        元のファイル                                   置き場
#   llrt        runtimes/llrt/build.ps1 が作った A53 版         ~/.local/share/zcode/runtimes/llrt-<版>/llrt
#   tsc         npm の @typescript/typescript-linux-arm64 の tgz ~/.local/share/zcode/runtimes/typescript-<版>/
#   llrt-types  LLRT のリポジトリの types/*.d.ts をまとめた tar  ~/.local/share/zcode/runtimes/llrt-types-<版>/
#   zc-remote   zcode/remote/build.sh が作った out/zc-remote    ~/.local/share/zcode/runtimes/zc-remote-<版>/zc-remote
#
# 入れたら、版によらない名前を張り替える（シンボリックリンク。張り替えは ln -sfn → mv で一度に）:
#
#   ~/.local/bin/llrt  ~/.local/bin/tsc  ~/.local/bin/zc-remote
#   ~/.local/share/zcode/types/llrt        → llrt-types-<版>（language.toml の check が読む）
#
# - 実機からはダウンロードしない（PC で取って scp で /tmp に置き、--from で渡す。/tmp は RAM
#   なので、終わったら消す）。SD に書くのは置き場への 1 回だけ
# - 他所で作られた実行ファイルは、入れる前に実機の CPU（Cortex-A53・ARMv8.0）で動くことを
#   確かめる: `<実行ファイル> --version` を timeout 10 で動かし、SIGILL（終了コード 132）なら
#   入れずにやめる
# - 古い版のフォルダは、張り替えが済んでから消す
# - zcode は起動時に ~/.local/bin を PATH の先頭に足す（APPLaunch から起動したときの PATH には
#   入っていないため）
set -euo pipefail
echo "install-runtimes.sh: まだ実装していない（docs/stage3-design.md の課題 R1）" >&2
exit 1
