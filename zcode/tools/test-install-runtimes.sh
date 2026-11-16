#!/usr/bin/env bash
# install-runtimes.sh の試験（開発コンテナで。実機には触らない・ダウンロードもしない）。
# 偽の $HOME に、偽物の実行ファイル・tgz・tar と、それに合わせた偽の lock で入れて確かめる。
#
#   cd zcode && ../container/dev.ps1 bash tools/test-install-runtimes.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
INST="$here/install-runtimes.sh"
W=$(mktemp -d "${TMPDIR:-/tmp}/zc-rt-test.XXXXXX")
trap 'rm -rf "$W"' EXIT
export HOME="$W/home"
mkdir -p "$HOME"
RT="$HOME/.local/share/zcode/runtimes"
fails=0 n=0

ok() { n=$((n + 1)); echo "ok $n - $*"; }
ng() { n=$((n + 1)); fails=$((fails + 1)); echo "NG $n - $*" >&2; }
check() { local what=$1; shift; if "$@"; then ok "$what"; else ng "$what"; fi; }
# 失敗するはずのコマンド（終了コードが 0 でなければ良い）
fails_ok() { local what=$1; shift; if "$@" >"$W/out" 2>&1; then ng "$what（成功してしまった）"; cat "$W/out" >&2; else ok "$what"; fi; }
runs() { local what=$1; shift; if "$@" >"$W/out" 2>&1; then ok "$what"; else ng "$what"; cat "$W/out" >&2; fi; }
# 置き場の様子（パス・種類・リンク先・mtime・inode）。2 回目に何も変えていないかを見る
snapshot() { find "$HOME" -printf '%p %y %l %T@ %i\n' | LC_ALL=C sort; }
# 断ったときの比較用: フォルダの mtime は見ない（作りかけを置き場の中に作って消すので変わる）
snapshot_files() { find "$HOME" \( -type d -printf '%p %y\n' \) -o -printf '%p %y %l %T@ %i\n' | LC_ALL=C sort; }
sha() { sha256sum "$1" | cut -d' ' -f1; }

# 偽物を作る: 版 $1 の llrt・tsc（tgz）・llrt-types（tar）を $2 に
make_set() {
  local v=$1 d=$2
  mkdir -p "$d" "$W/build-$v/package/lib" "$W/build-$v/types/fs"
  printf '#!/bin/sh\necho "LLRT v%s"\n' "$v" >"$d/llrt-$v-linux-a53-no-sdk"
  chmod 644 "$d/llrt-$v-linux-a53-no-sdk" # 実行権限が無くても入れたら付く
  printf '#!/bin/sh\necho "Version %s"\n' "$v" >"$W/build-$v/package/lib/tsc"
  chmod 755 "$W/build-$v/package/lib/tsc"
  echo "// lib $v" >"$W/build-$v/package/lib/lib.es2022.d.ts"
  echo '{"name":"@typescript/typescript-linux-arm64"}' >"$W/build-$v/package/package.json"
  tar -czf "$d/typescript-linux-arm64-$v.tgz" -C "$W/build-$v" package
  echo "/// <reference types=\"./fs.d.ts\" />  // $v" >"$W/build-$v/types/index.d.ts"
  echo "declare const x: number;" >"$W/build-$v/types/fs.d.ts"
  echo "export {};" >"$W/build-$v/types/fs/promises.d.ts"
  (cd "$W/build-$v/types" && tar -cf "$d/llrt-types-$v.tar" index.d.ts fs.d.ts fs/promises.d.ts)
}

# 偽の lock（版 $1 の 3 行）を $2 に
make_lock() {
  local v=$1 d=$2 lock=$3
  {
    echo "# 試験用"
    echo "llrt        $v  $(sha "$d/llrt-$v-linux-a53-no-sdk")  llrt-$v-linux-a53-no-sdk  -"
    echo "llrt-types  $v  $(sha "$d/llrt-types-$v.tar")  llrt-types-$v.tar  -"
    echo "tsc         $v  $(sha "$d/typescript-linux-arm64-$v.tgz")  typescript-linux-arm64-$v.tgz  -"
  } >"$lock"
}

link_is() { [ "$(readlink "$1")" = "$2" ]; }

# --- 1. 入れる -------------------------------------------------------------
make_set 1.0.0 "$W/v1"
make_lock 1.0.0 "$W/v1" "$W/lock1"
runs "全部入れる" "$INST" --lock "$W/lock1" --from "$W/v1"
check "llrt のリンク" link_is "$HOME/.local/bin/llrt" "$RT/llrt-1.0.0/llrt"
check "tsc のリンク" link_is "$HOME/.local/bin/tsc" "$RT/typescript-1.0.0/lib/tsc"
check "型定義のリンク" link_is "$HOME/.local/share/zcode/types/llrt" "$RT/llrt-types-1.0.0"
check "llrt が動く" test "$("$HOME/.local/bin/llrt" --version)" = "LLRT v1.0.0"
check "tsc が動く" test "$("$HOME/.local/bin/tsc" --version)" = "Version 1.0.0"
check "tsc の隣に lib" test -f "$RT/typescript-1.0.0/lib/lib.es2022.d.ts"
check "型定義の index.d.ts（入れ子も）" test -f "$HOME/.local/share/zcode/types/llrt/index.d.ts" -a -f "$RT/llrt-types-1.0.0/fs/promises.d.ts"
check "作りかけが残っていない" test -z "$(find "$RT" -maxdepth 1 -name '.stage-*')"
check "1 つ前の版の記録は無い" test ! -e "$RT/llrt.prev"

# --- 2. 2 回入れても同じ -----------------------------------------------------
before=$(snapshot)
runs "2 回目" "$INST" --lock "$W/lock1" --from "$W/v1"
check "2 回目は何も変えない（mtime・inode も）" test "$before" = "$(snapshot)"

# --- 3. SHA-256 が合わないものは入れない --------------------------------------
mkdir -p "$W/bad"
cp "$W/v1/"* "$W/bad/"
echo "# 改ざん" >>"$W/bad/llrt-1.0.0-linux-a53-no-sdk"
rm -rf "$RT/llrt-1.0.0" # 置き場に無い状態から
rm "$HOME/.local/bin/llrt"
before=$(snapshot)
fails_ok "SHA-256 の違うファイルを断る" "$INST" --lock "$W/lock1" --from "$W/bad" llrt
check "断ったときは何も置かない" test "$before" = "$(snapshot)"
check "断った理由に SHA-256" grep -q 'SHA-256 が runtimes.lock と合わない' "$W/out"
runs "入れ直す" "$INST" --lock "$W/lock1" --from "$W/v1" llrt
check "入れ直した llrt のリンク" link_is "$HOME/.local/bin/llrt" "$RT/llrt-1.0.0/llrt"

# 置き場のフォルダが壊れていたら（印が合わない）作り直す
echo broken >"$RT/llrt-1.0.0/.zc-sha256"
runs "壊れた置き場を作り直す" "$INST" --lock "$W/lock1" --from "$W/v1" llrt
check "作り直した印" test "$(cat "$RT/llrt-1.0.0/.zc-sha256")" = "$(sha "$W/v1/llrt-1.0.0-linux-a53-no-sdk")"

# --- 4. lock の不備 -----------------------------------------------------------
sed 's/^\(llrt  *[^ ]*  *\)[0-9a-f]\{64\}/\1TODO/' "$W/lock1" >"$W/lock-todo"
fails_ok "SHA-256 が決まっていない行を断る" "$INST" --lock "$W/lock-todo" --from "$W/v1" llrt
fails_ok "lock に無い名前を断る（zc-remote）" "$INST" --lock "$W/lock1" --from "$W/v1" zc-remote
fails_ok "知らない名前を断る" "$INST" --lock "$W/lock1" --from "$W/v1" node
fails_ok "--from が無い" "$INST" --lock "$W/lock1"
mkdir -p "$W/empty"
fails_ok "フォルダに何も無い" "$INST" --lock "$W/lock1" --from "$W/empty"
fails_ok "名前を指定したのにファイルが無い" "$INST" --lock "$W/lock1" --from "$W/empty" tsc

# --- 5. 版を上げる → 張り替え・1 つ前を残す・--rollback -----------------------
make_set 2.0.0 "$W/v2"
make_lock 2.0.0 "$W/v2" "$W/lock2"
runs "版 2 を入れる" "$INST" --lock "$W/lock2" --from "$W/v2"
check "llrt が版 2 に" test "$("$HOME/.local/bin/llrt" --version)" = "LLRT v2.0.0"
check "型定義が版 2 に" link_is "$HOME/.local/share/zcode/types/llrt" "$RT/llrt-types-2.0.0"
check "版 1 は 1 つ前として残る" test -d "$RT/llrt-1.0.0" -a -d "$RT/typescript-1.0.0" -a -d "$RT/llrt-types-1.0.0"
check "1 つ前の記録" link_is "$RT/llrt.prev" "$RT/llrt-1.0.0"
runs "一覧" "$INST" --lock "$W/lock2" --list
check "一覧に今と前" grep -q 'llrt-2.0.0' "$W/out"

runs "llrt を戻す" "$INST" --rollback llrt
check "llrt が版 1 に戻る" test "$("$HOME/.local/bin/llrt" --version)" = "LLRT v1.0.0"
check "戻すと前の記録が入れ替わる" link_is "$RT/llrt.prev" "$RT/llrt-2.0.0"
check "ほかの名前は動かない" test "$("$HOME/.local/bin/tsc" --version)" = "Version 2.0.0"
runs "もう一度戻す（元に戻る）" "$INST" --rollback llrt
check "llrt が版 2 に" test "$("$HOME/.local/bin/llrt" --version)" = "LLRT v2.0.0"

# 版 3 を入れると、版 1 は消える（今 = 3、前 = 2）。llrt と llrt-types を取り違えない
make_set 3.0.0 "$W/v3"
make_lock 3.0.0 "$W/v3" "$W/lock3"
runs "版 3 を入れる" "$INST" --lock "$W/lock3" --from "$W/v3"
check "版 1 は消える" test ! -e "$RT/llrt-1.0.0" -a ! -e "$RT/typescript-1.0.0" -a ! -e "$RT/llrt-types-1.0.0"
check "版 2 は 1 つ前として残る" test -d "$RT/llrt-2.0.0" -a -d "$RT/llrt-types-2.0.0" -a -d "$RT/typescript-2.0.0"
check "llrt-types は llrt の古い版として消されない" test -d "$RT/llrt-types-3.0.0"
check "tsc が版 3 に" test "$("$HOME/.local/bin/tsc" --version)" = "Version 3.0.0"

# --- 6. 動かない実行ファイル（SIGILL）は入れない --------------------------------
make_set 4.0.0 "$W/v4"
printf '#!/bin/sh\nkill -s ILL $$\n' >"$W/v4/llrt-4.0.0-linux-a53-no-sdk"
make_lock 4.0.0 "$W/v4" "$W/lock4"
before=$(snapshot_files)
fails_ok "SIGILL で落ちるものを断る" "$INST" --lock "$W/lock4" --from "$W/v4" llrt
check "断った理由に SIGILL" grep -q 'SIGILL' "$W/out"
check "SIGILL のときは何も変えない" test "$before" = "$(snapshot_files)"
printf '\x7fELF\x02\x01\x01\0garbage' >"$W/v4/llrt-4.0.0-linux-a53-no-sdk"
make_lock 4.0.0 "$W/v4" "$W/lock4"
fails_ok "実行できないもの（別の CPU 向け）を断る" "$INST" --lock "$W/lock4" --from "$W/v4" llrt
check "実行できないときも何も変えない" test "$before" = "$(snapshot_files)"

# --- 7. 自分で置いたファイルは上書きしない -----------------------------------
runs "zc-remote の無い lock でも全部入れは通る" "$INST" --lock "$W/lock3" --from "$W/v3"
rm "$HOME/.local/bin/tsc"
echo "my tsc" >"$HOME/.local/bin/tsc"
fails_ok "リンクでない ~/.local/bin/tsc を上書きしない" "$INST" --lock "$W/lock2" --from "$W/v2" tsc
check "自分のファイルはそのまま" test "$(cat "$HOME/.local/bin/tsc")" = "my tsc"
rm "$HOME/.local/bin/tsc"

# --- 8. --remove ------------------------------------------------------------
runs "llrt を消す" "$INST" --remove llrt
check "llrt のリンクと版が消える" test ! -e "$HOME/.local/bin/llrt" -a ! -L "$HOME/.local/bin/llrt" -a -z "$(find "$RT" -maxdepth 1 -name 'llrt-[0-9]*')" -a ! -L "$RT/llrt.prev"
check "llrt-types は残る" test -d "$RT/llrt-types-3.0.0" -a -L "$HOME/.local/share/zcode/types/llrt"
fails_ok "消したあと --rollback できない" "$INST" --rollback llrt

# --- 9. 名前が空白を含む $HOME でも動く --------------------------------------
export HOME="$W/home with space"
mkdir -p "$HOME"
RT="$HOME/.local/share/zcode/runtimes"
runs "空白入りの HOME" "$INST" --lock "$W/lock1" --from "$W/v1"
check "空白入りの HOME で llrt が動く" test "$("$HOME/.local/bin/llrt" --version)" = "LLRT v1.0.0"

# --- 10. 本物の runtimes.lock が読める（ファイルは無いので、形だけ） ------------------
for name in llrt llrt-types tsc; do
  check "runtimes.lock の $name の行" grep -Eq "^$name +[0-9][^ ]* +[0-9a-f]{64} +[^ ]+ +[^ ]+$" "$here/runtimes.lock"
done

echo
if [ "$fails" -eq 0 ]; then
  echo "全部通った（$n 件）"
else
  echo "$fails / $n 件が失敗" >&2
  exit 1
fi
