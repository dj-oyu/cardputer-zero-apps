#!/usr/bin/env bash
# 実機に、zcode の F5 と型検査で使う実行環境を入れる（sudo は要らない）。
# docs/stage3-design.md §10.3・§12.3。
#
#   実機で:  ./install-runtimes.sh --from <ファイルを置いたフォルダ> [名前…]
#            （名前を省くと、フォルダにあるものを全部）
#            ./install-runtimes.sh --list              入っているものと版
#            ./install-runtimes.sh --rollback <名前>   1 つ前の版に戻す（もう一度で元に戻る）
#            ./install-runtimes.sh --remove <名前>     消す（リンクと、残している版を全部）
#   どれにも  --lock <ファイル>  で runtimes.lock を差し替えられる（既定はこのスクリプトの隣）
#
# 入れるもの（版と SHA-256 は runtimes.lock。**合わないファイルは入れない**）:
#
#   名前        元のファイル                                   置き場
#   llrt        runtimes/llrt/build.ps1 が作った A53 版         ~/.local/share/zcode/runtimes/llrt-<版>/llrt
#   tsc         npm の @typescript/typescript-linux-arm64 の tgz ~/.local/share/zcode/runtimes/typescript-<版>/
#   llrt-types  runtimes/llrt/build.ps1 が作った型定義の tar     ~/.local/share/zcode/runtimes/llrt-types-<版>/
#   zc-remote   zcode/remote/build.sh が作った out/zc-remote    ~/.local/share/zcode/runtimes/zc-remote-<版>/zc-remote
#
# 入れたら、版によらない名前を張り替える（シンボリックリンク。張り替えは ln -sfn → mv -T で一度に）:
#
#   ~/.local/bin/llrt  ~/.local/bin/tsc  ~/.local/bin/zc-remote
#   ~/.local/share/zcode/types/llrt        → llrt-types-<版>（language.toml の check が読む）
#
# - 実機からはダウンロードしない（PC で取って scp で /tmp に置き、--from で渡す。/tmp は RAM
#   なので、終わったら消す）。SD に書くのは置き場への 1 回だけ。同じ版がもう入っていれば
#   何も書かない（2 回目は何も変えない）
# - 他所で作られた実行ファイルは、入れる前に実機の CPU（Cortex-A53・ARMv8.0）で動くことを
#   確かめる: `<実行ファイル> --version` を timeout 10 で動かし、SIGILL（終了コード 132）・
#   実行できない（126 / 127。別の CPU 向けなど）・時間切れ（124）なら入れずにやめる
# - 古い版は 1 つだけ残す（--rollback 用。~/.local/share/zcode/runtimes/<名前>.prev が指す）。
#   それより古い版のフォルダは、張り替えが済んでから消す
# - zcode は起動時に ~/.local/bin を PATH の先頭に足す（APPLaunch から起動したときの PATH には
#   入っていないため）
# - 試験は隣の test-install-runtimes.sh（開発コンテナで、偽の $HOME と偽物のファイルで回す）
set -euo pipefail

prog=${0##*/}
here=$(cd "$(dirname "$0")" && pwd)
RT="$HOME/.local/share/zcode/runtimes"
BIN="$HOME/.local/bin"
TYPES="$HOME/.local/share/zcode/types"
NAMES=(llrt tsc llrt-types zc-remote)

die() { echo "$prog: $*" >&2; exit 1; }
say() { echo "$prog: $*"; }

usage() {
  sed -n '4,10p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

# 名前 → 置き場のフォルダ名の頭（<頭>-<版>）
prefix_of() {
  case $1 in
    llrt) echo llrt ;;
    tsc) echo typescript ;;
    llrt-types) echo llrt-types ;;
    zc-remote) echo zc-remote ;;
    *) die "知らない名前: $1（${NAMES[*]} のどれか）" ;;
  esac
}

# 名前 → 版によらない名前（リンク）
link_of() {
  case $1 in
    llrt) echo "$BIN/llrt" ;;
    tsc) echo "$BIN/tsc" ;;
    llrt-types) echo "$TYPES/llrt" ;;
    zc-remote) echo "$BIN/zc-remote" ;;
  esac
}

# 名前 → 版のフォルダの中の、リンクが指すもの（空ならフォルダそのもの）
entry_of() {
  case $1 in
    llrt) echo llrt ;;
    tsc) echo lib/tsc ;;
    llrt-types) echo "" ;;
    zc-remote) echo zc-remote ;;
  esac
}

# 名前 → 動くか確かめる実行ファイル（空なら確かめない）
exe_of() {
  case $1 in
    llrt-types) echo "" ;;
    *) entry_of "$1" ;;
  esac
}

# runtimes.lock から 1 行を読む。LOCK_VER・LOCK_SHA・LOCK_FILE に入れる
read_lock() {
  local name=$1 n v s f rest found=""
  [ -f "$LOCK" ] || die "lock が無い: $LOCK"
  while read -r n v s f rest; do
    case $n in '' | '#'*) continue ;; esac
    [ "$n" = "$name" ] || continue
    [ -z "$found" ] || die "$LOCK: $name の行が 2 つある"
    found=1
    [[ $v =~ ^[0-9][0-9A-Za-z.+-]*$ ]] || die "$LOCK: $name の版が不正: $v（数字で始まる英数字と . + -）"
    [[ $s =~ ^[0-9a-f]{64}$ ]] || die "$LOCK: $name の SHA-256 が不正: $s（16 進 64 桁。決まっていない版は入れられない）"
    [[ -n $f && $f != */* && $f != . && $f != .. ]] || die "$LOCK: $name のファイル名が不正: $f"
    LOCK_VER=$v LOCK_SHA=$s LOCK_FILE=$f
  done <"$LOCK"
  [ -n "$found" ] || die "$LOCK に $name の行が無い"
}

# リンクが指す版のフォルダ（無ければ空）
current_dir() {
  local name=$1 link t entry
  link=$(link_of "$name")
  [ -L "$link" ] || return 0
  t=$(readlink "$link")
  entry=$(entry_of "$name")
  [ -z "$entry" ] || t=${t%/"$entry"}
  echo "$t"
}

prev_dir() {
  local p="$RT/$(prefix_of "$1").prev"
  [ -L "$p" ] && readlink "$p" || true
}

# リンクを一度に張り替える（張り替えの途中で名前が消える瞬間を作らない）
swap_link() {
  local target=$1 link=$2
  [ "$(readlink "$link" 2>/dev/null || true)" = "$target" ] && return 0
  if [ -e "$link" ] && [ ! -L "$link" ]; then
    die "$link がリンクではない（自分で置いたもの？）。消すか名前を変えてから、やり直す"
  fi
  mkdir -p "$(dirname "$link")"
  ln -sfn "$target" "$link.zc-new"
  mv -Tf "$link.zc-new" "$link"
}

set_prev() {
  local name=$1 dir=$2 p
  p="$RT/$(prefix_of "$name").prev"
  if [ -z "$dir" ]; then
    rm -f "$p"
  else
    swap_link "$dir" "$p"
  fi
}

# 今の版と 1 つ前の版のほかを消す
prune() {
  local name=$1 pre cur prev d
  pre=$(prefix_of "$name")
  cur=$(current_dir "$name")
  prev=$(prev_dir "$name")
  for d in "$RT/$pre"-[0-9]*; do
    [ -d "$d" ] || continue
    [ "$d" = "$cur" ] || [ "$d" = "$prev" ] && continue
    say "古い版を消す: ${d##*/}"
    rm -rf -- "$d"
  done
}

# 実行ファイルが実機の CPU で動くか（SIGILL・実行できない・固まる、なら失敗）
probe() {
  local exe=$1 out code=0
  out=$(timeout 10 "$exe" --version 2>&1 </dev/null) || code=$?
  case $code in
    0) say "  ${out%%$'\n'*}" ;;
    132) echo "$prog: $exe が SIGILL で落ちた（この CPU に無い命令を使っている。A53 向けに作り直す）" >&2; return 1 ;;
    124) echo "$prog: $exe --version が 10 秒で終わらない" >&2; return 1 ;;
    126 | 127) echo "$prog: $exe を実行できない（別の CPU 向け？）: $out" >&2; return 1 ;;
    *) say "  （--version が終了コード $code。動きはしたので続ける）${out%%$'\n'*}" ;;
  esac
}

# 中身を置く（stage は空のフォルダ）
unpack() {
  local name=$1 src=$2 stage=$3
  case $name in
    llrt | zc-remote)
      cp -- "$src" "$stage/$name"
      chmod 755 "$stage/$name"
      ;;
    tsc)
      # npm の tgz は package/ の下に入っている
      tar -xzf "$src" -C "$stage" --strip-components=1 --no-same-owner --no-same-permissions
      chmod 755 "$stage/lib/tsc"
      ;;
    llrt-types)
      tar -xf "$src" -C "$stage" --no-same-owner --no-same-permissions
      ;;
  esac
  local entry
  entry=$(entry_of "$name")
  [ -z "$entry" ] || [ -f "$stage/$entry" ] || die "$src の中に $entry が無い"
  [ "$name" != llrt-types ] || [ -f "$stage/index.d.ts" ] || die "$src の中に index.d.ts が無い"
}

install_one() {
  local name=$1 pre dir src sum cur entry exe
  read_lock "$name"
  pre=$(prefix_of "$name")
  dir="$RT/$pre-$LOCK_VER"
  src="$FROM/$LOCK_FILE"
  [ -f "$src" ] || die "$src が無い（$name $LOCK_VER）"
  sum=$(sha256sum -- "$src")
  sum=${sum%% *}
  if [ "$sum" != "$LOCK_SHA" ]; then
    die "$name: SHA-256 が runtimes.lock と合わないので入れない
  ファイル: $src
  lock:     $LOCK_SHA
  実物:     $sum"
  fi
  say "$name $LOCK_VER（SHA-256 一致）"

  entry=$(entry_of "$name")
  exe=$(exe_of "$name")
  if [ -d "$dir" ] && [ "$(cat "$dir/.zc-sha256" 2>/dev/null || true)" = "$LOCK_SHA" ]; then
    say "  もう置いてある: $dir"
  else
    mkdir -p "$RT"
    # 途中で止まったら（die・Ctrl+C）作りかけを消す
    STAGE=$(mktemp -d "$RT/.stage-$pre-XXXXXX")
    trap '[ -z "$STAGE" ] || rm -rf -- "$STAGE"' EXIT
    unpack "$name" "$src" "$STAGE"
    if [ -n "$exe" ] && ! probe "$STAGE/$exe"; then
      die "$name を入れずにやめた（今の版はそのまま）"
    fi
    echo "$LOCK_SHA" >"$STAGE/.zc-sha256"
    chmod 755 "$STAGE"
    # SD に書ききってから名前を付ける（電源断で半端なフォルダが「入っている」ことにならない）
    sync -f "$STAGE" 2>/dev/null || sync
    rm -rf -- "$dir"
    mv -T -- "$STAGE" "$dir"
    STAGE=""
  fi

  cur=$(current_dir "$name")
  if [ "$cur" != "$dir" ]; then
    swap_link "$dir${entry:+/$entry}" "$(link_of "$name")"
    if [ -n "$cur" ] && [ -d "$cur" ]; then
      set_prev "$name" "$cur"
    fi
    say "  張り替えた: $(link_of "$name") → $dir${entry:+/$entry}"
  else
    say "  張り替え不要: $(link_of "$name")"
  fi
  # 1 つ前の版が今の版と同じなら、記録を消す
  [ "$(prev_dir "$name")" != "$dir" ] || set_prev "$name" ""
  prune "$name"
}

do_install() {
  [ -n "$FROM" ] || die "--from <フォルダ> が要る"
  [ -d "$FROM" ] || die "フォルダが無い: $FROM"
  FROM=$(cd "$FROM" && pwd)
  local todo=("$@") n
  if [ ${#todo[@]} -eq 0 ]; then
    # フォルダにあるものを全部（lock に行があって、ファイルが置いてあるもの）
    for n in "${NAMES[@]}"; do
      if (read_lock "$n") >/dev/null 2>&1; then
        read_lock "$n"
        [ ! -f "$FROM/$LOCK_FILE" ] || todo+=("$n")
      fi
    done
    [ ${#todo[@]} -gt 0 ] || die "$FROM に、runtimes.lock にあるファイルが 1 つも無い"
  fi
  for n in "${todo[@]}"; do prefix_of "$n" >/dev/null; done
  for n in "${todo[@]}"; do install_one "$n"; done
  sync -f "$RT" 2>/dev/null || sync
  case ":$PATH:" in
    *":$BIN:"*) ;;
    *) say "注意: $BIN が PATH に無い（zcode は起動時に足す。シェルで使うなら ~/.profile に）" ;;
  esac
}

do_list() {
  local n cur prev pre d
  for n in "${NAMES[@]}"; do
    pre=$(prefix_of "$n")
    cur=$(current_dir "$n")
    prev=$(prev_dir "$n")
    printf '%-11s 今: %s\n' "$n" "${cur:+${cur##*/}}${cur:-（入っていない）}"
    [ -z "$prev" ] || printf '%-11s 前: %s\n' "" "${prev##*/}"
    for d in "$RT/$pre"-[0-9]*; do
      [ -d "$d" ] || continue
      [ "$d" = "$cur" ] || [ "$d" = "$prev" ] || printf '%-11s 他: %s\n' "" "${d##*/}"
    done
  done
}

do_rollback() {
  local name=$1 cur prev entry
  prefix_of "$name" >/dev/null
  cur=$(current_dir "$name")
  prev=$(prev_dir "$name")
  [ -n "$prev" ] && [ -d "$prev" ] || die "$name: 戻す先（1 つ前の版）が無い"
  entry=$(entry_of "$name")
  swap_link "$prev${entry:+/$entry}" "$(link_of "$name")"
  if [ -n "$cur" ] && [ -d "$cur" ]; then set_prev "$name" "$cur"; else set_prev "$name" ""; fi
  say "$name: ${cur##*/} → ${prev##*/} に戻した"
}

do_remove() {
  local name=$1 pre link d
  pre=$(prefix_of "$name")
  link=$(link_of "$name")
  if [ -L "$link" ]; then rm -f -- "$link"; elif [ -e "$link" ]; then die "$link がリンクではない。消さない"; fi
  rm -f -- "$RT/$pre.prev"
  for d in "$RT/$pre"-[0-9]*; do
    [ -d "$d" ] || continue
    rm -rf -- "$d"
    say "消した: ${d##*/}"
  done
  say "$name を消した"
}

# 引数
FROM="" LOCK="$here/runtimes.lock" ACTION=install ARG="" STAGE=""
names=()
while [ $# -gt 0 ]; do
  case $1 in
    --from) [ $# -ge 2 ] || usage; FROM=$2; shift 2 ;;
    --lock) [ $# -ge 2 ] || usage; LOCK=$2; shift 2 ;;
    --list) ACTION=list; shift ;;
    --rollback | --remove) [ $# -ge 2 ] || usage; ACTION=${1#--}; ARG=$2; shift 2 ;;
    -h | --help) usage ;;
    -*) echo "$prog: 知らないオプション: $1" >&2; usage ;;
    *) names+=("$1"); shift ;;
  esac
done

case $ACTION in
  install) do_install ${names[@]+"${names[@]}"} ;;
  list) do_list ;;
  rollback) do_rollback "$ARG" ;;
  remove) do_remove "$ARG" ;;
esac
