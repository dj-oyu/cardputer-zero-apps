#!/bin/bash
# LLRT（AWS の QuickJS ベースの JS ランタイム）を CardputerZero の Cortex-A53 向けにビルドする。
#
# 配布版の aarch64 は Graviton 向け（target-cpu=neoverse-n1 + LSE）で、ARMv8.0 の A53 では
# SIGILL で落ちる。ここでは CPU の指定だけを差し替えて、公式の手順（Makefile）でビルドする。
#
# 使い捨てのコンテナの中で動かす（Windows からは build.ps1）。置き場:
#   /b    名前付きボリューム llrt-build（ソース・ツールチェーン・ビルド成果物。次回の再利用用）
#   /out  できた実行ファイルを置く場所
#
# 引数: [LLRT のタグ] [種類: no-sdk | std | full-sdk]
set -euo pipefail
LLRT_TAG=${1:-v0.10.0-beta}
VARIANT=${2:-no-sdk}
# ツールの版（上げるときは ziglang.org の SHA256 も書き換える）
ZIG_VER=0.17.1
ZIG_SHA=3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a
NODE_VER=v26.13.1

case $VARIANT in
  no-sdk) ZIP=llrt-linux-arm64-no-sdk.zip ;;
  std) ZIP=llrt-linux-arm64.zip ;;
  full-sdk) ZIP=llrt-linux-arm64-full-sdk.zip ;;
  *) echo "種類は no-sdk / std / full-sdk のどれか: $VARIANT" >&2; exit 2 ;;
esac

# ツールチェーンはボリュームに置く（CARGO_HOME はイメージのまま。rustup の代理コマンドがそこにある）
export RUSTUP_HOME=/b/rustup
export PATH=/b/tools/bin:/b/zig:/b/node/bin:$PATH
# LLRT の Makefile は ./target を前提にしている（開発イメージは別の場所を指している）
unset CARGO_TARGET_DIR
log() { echo "=== $(date +%T) $*"; }

log "道具（apt）"
need=""
for c in git make cmake zip unzip xz zstd; do command -v $c >/dev/null || need=1; done
if [ -n "$need" ]; then
  apt-get update -qq && apt-get install -y -qq git make cmake zip unzip xz-utils zstd >/dev/null
fi

mkdir -p /b && cd /b
if [ "$(/b/zig/zig version 2>/dev/null)" != "$ZIG_VER" ]; then
  log "zig $ZIG_VER"
  curl -sSfLo zig.tar.xz "https://ziglang.org/download/$ZIG_VER/zig-x86_64-linux-$ZIG_VER.tar.xz"
  echo "$ZIG_SHA  zig.tar.xz" | sha256sum -c -
  rm -rf zig && mkdir zig && tar xJf zig.tar.xz -C zig --strip-components=1 && rm zig.tar.xz
fi
if [ "$(/b/node/bin/node --version 2>/dev/null)" != "$NODE_VER" ]; then
  log "node $NODE_VER"
  f=node-$NODE_VER-linux-x64.tar.xz
  curl -sSfLo "$f" "https://nodejs.org/dist/$NODE_VER/$f"
  curl -sSfL "https://nodejs.org/dist/$NODE_VER/SHASUMS256.txt" | grep " $f\$" | sha256sum -c -
  rm -rf node && mkdir node && tar xJf "$f" -C node --strip-components=1 && rm "$f"
fi

# 別のタグのソースが残っていれば取り直す
if [ -d /b/llrt/.git ] && [ "$(git -C /b/llrt describe --tags --exact-match 2>/dev/null)" != "$LLRT_TAG" ]; then
  rm -rf /b/llrt
fi
if [ ! -d /b/llrt/.git ]; then
  log "LLRT $LLRT_TAG を取ってくる"
  git clone -q --depth 1 --branch "$LLRT_TAG" https://github.com/awslabs/llrt /b/llrt
  git -C /b/llrt submodule update --init --checkout --depth 1 zstd
fi
cd /b/llrt
git log -1 --format='%H %s'

log "CPU の指定を A53 向けに差し替える"
git checkout -- .cargo/config.toml Makefile
sed -i 's/"-Ctarget-feature=+lse,+crt-static"/"-Ctarget-feature=+crt-static"/; s/"-Ctarget-cpu=neoverse-n1"/"-Ctarget-cpu=cortex-a53"/' .cargo/config.toml
sed -i 's/zig cc -target aarch64-linux-musl /zig cc -target aarch64-linux-musl -mcpu=cortex_a53 /' Makefile
# 差し替えが効いたか（上流の書き方が変わったら、ここで止まる）
grep -q '"-Ctarget-cpu=cortex-a53"' .cargo/config.toml || { echo "config.toml の差し替えに失敗" >&2; exit 1; }
! grep -q '+lse' .cargo/config.toml || { echo "+lse が残っている" >&2; exit 1; }
grep -q 'mcpu=cortex_a53' Makefile || { echo "Makefile の差し替えに失敗" >&2; exit 1; }
# C の依存（QuickJS など）も A53 向けに
export CFLAGS_aarch64_unknown_linux_musl="-mcpu=cortex_a53"

log "Rust（Makefile が指定する nightly。build-std で std も A53 向けに作り直す）"
RV=$(sed -n 's/^RUST_VERSION = //p' Makefile)
rustup toolchain install "$RV" --profile minimal --target aarch64-unknown-linux-musl --component rust-src
command -v cargo-zigbuild >/dev/null || cargo +"$RV" install --locked --root /b/tools cargo-zigbuild

log "JS の依存（yarn.lock のとおり）"
# Node 25 以降は corepack を同梱しない
command -v corepack >/dev/null || npm install -g --ignore-scripts corepack >/dev/null
corepack enable --install-directory /b/node/bin
COREPACK_ENABLE_DOWNLOAD_PROMPT=0 yarn install --immutable 2>&1 | tail -3

log "zstd（C）"
rm -rf lib/arm64
make libs-arm64 2>&1 | tail -3

log "LLRT（$VARIANT）"
make "$ZIP" 2>&1 | tail -5

log "A53 に無い命令が残っていないか"
B=target/aarch64-unknown-linux-musl/release/llrt
aarch64-linux-gnu-objdump -d --no-show-raw-insn "$B" > /tmp/d.txt
lse=$(grep -cE '\s(ldadd|ldclr|ldeor|ldset|ldsmax|ldsmin|ldumax|ldumin|swp|cas)(a|l|al)?(b|h)?\s' /tmp/d.txt || true)
rcpc=$(grep -cE '\s(sdot|udot|ldapr|ldaprb|ldaprh)\s' /tmp/d.txt || true)
echo "LSE: $lse  RCpc/dotprod: $rcpc"
# 実行時に CPU を見る補助関数のぶん（v0.10.0-beta で 34）だけなら良い。数千あれば指定が効いていない
if [ "$lse" -gt 200 ] || [ "$rcpc" -gt 0 ]; then
  echo "A53 で動かない命令が多すぎる。CPU の指定が効いていない" >&2
  exit 1
fi

mkdir -p /out
name="llrt-${LLRT_TAG#v}-linux-a53-$VARIANT"
cp "$B" "/out/$name"
(cd /out && sha256sum "$name" > "$name.sha256" && cat "$name.sha256")
ls -l "/out/$name"
log "できた: $name"
