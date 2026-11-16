# zcode のビルド係のイメージ（Cloudflare Containers と PC で同じもの）。linux/amd64。
# docs/stage3-design.md §6.4。
#
# ビルドの文脈は zcode/（zcode/.dockerignore で、要るものだけに絞ってある）:
#   wslc build   -f remote/container/Dockerfile -t cz-builder:latest .      （zcode/ で）
#   docker build -f remote/container/Dockerfile -t cz-builder:latest .
# PC では remote/container/run-pc.ps1 が作って動かす。Cloudflare では wrangler deploy が
# wrangler.jsonc の containers.images.builder から作る。
#
# 中身: 開発コンテナ（container/Dockerfile）の Rust の部分と同じもの（Debian 13・rustup・
# aarch64 のクロスの gcc）に、zstd・Node と npm・esbuild・TypeScript 7 の tsc・受付の
# zc-buildd を足したもの。Python・フォント・git は入れない（冷えた起動で引くので小さく保つ）。
# 大きさ（2026-11-17）: 展開後 1.52GB、レジストリに送る形（gzip の層）で約 500MB。内訳は zcode/AGENTS.md。
#
# 版はここの ARG で固定し、取ってきたものは SHA-256 で確かめる:
#   - ベースは日付つきのタグ + digest
#   - rustup-init は static.rust-lang.org の .sha256、Rust はその版の rustup の検査に任せる
#   - Node は nodejs.org の SHASUMS256.txt の値
#   - esbuild・TypeScript は remote/container/tools/package-lock.json の integrity（npm ci）
# 版を上げるときは、値を書き換えて run-pc.ps1 -Build で作り直し、中で試験を回す
# （手順は zcode/AGENTS.md「ビルド係のイメージ」）。
#
# **秘密を入れない**（build.rs・postinstall・試験といった任意のコードがこの中で動く）。

ARG DEBIAN_IMAGE=debian:trixie-20261104-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

# ---- 道具の土台（受付のビルドと実行用で共有する） ----
FROM ${DEBIAN_IMAGE} AS toolchain
ARG RUSTUP_VERSION=1.29.3
ARG RUSTUP_SHA256=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55
ARG RUST_VERSION=1.100.0
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:$PATH \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl xz-utils zstd file \
      gcc libc6-dev gcc-aarch64-linux-gnu libc6-dev-arm64-cross binutils-aarch64-linux-gnu \
 && rm -rf /var/lib/apt/lists/*
# Rust（ホスト用と aarch64 用。試験は x86_64 のまま動かす）
RUN curl -fsSLo /tmp/rustup-init \
      "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init" \
 && echo "${RUSTUP_SHA256}  /tmp/rustup-init" | sha256sum -c - \
 && chmod +x /tmp/rustup-init \
 && /tmp/rustup-init -y --no-modify-path --profile minimal \
      --default-toolchain "${RUST_VERSION}" --target aarch64-unknown-linux-gnu \
 && rm -rf /tmp/rustup-init /opt/rustup/downloads /opt/rustup/tmp /opt/cargo/registry \
 && chmod -R a+rX /opt/cargo /opt/rustup \
 && rustc --version && cargo --version

# ---- 受付のプログラム（zc-buildd）を作る ----
FROM toolchain AS build
WORKDIR /src
COPY . .
RUN cd remote \
 && CARGO_TARGET_DIR=/tmp/target cargo build --release --locked -p zc-buildd \
 && install -m 0755 /tmp/target/release/zc-buildd /usr/local/bin/zc-buildd

# ---- 実行用 ----
FROM toolchain
ARG NODE_VERSION=26.13.1
ARG NODE_SHA256=5c063e46bab0fbc1c17cc445d7ff129cbdcd0471be6ed83b6ab864f8143bc864
# Node と npm（nodejs.org の配布物）。ヘッダ・文書は消す
RUN curl -fsSLo /tmp/node.tar.xz \
      "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" \
 && echo "${NODE_SHA256}  /tmp/node.tar.xz" | sha256sum -c - \
 && mkdir -p /opt/node \
 && tar -xJf /tmp/node.tar.xz -C /opt/node --strip-components=1 --no-same-owner \
 && rm -rf /tmp/node.tar.xz /opt/node/include /opt/node/share /opt/node/*.md /opt/node/LICENSE
ENV PATH=/opt/cargo/bin:/opt/node/bin:/opt/tools/node_modules/.bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
# esbuild・TypeScript 7（Go 版の tsc。linux-x64 の実行ファイルは optionalDependencies）。
# インストールスクリプトは動かさない（esbuild の postinstall は確かめるだけで、無くても動く）
COPY remote/container/tools/package.json remote/container/tools/package-lock.json /opt/tools/
RUN cd /opt/tools \
 && npm ci --ignore-scripts --no-audit --no-fund --cache /tmp/npm-cache \
 && rm -rf /tmp/npm-cache \
 && node --version && npm --version && tsc --version && esbuild --version
# sccache（docs/stage3-design.md §6.5 の段 3）は、スナップショット（段 2）を測ってから決める。

COPY --from=build /usr/local/bin/zc-buildd /usr/local/bin/zc-buildd

# 道具は root で動かさない。crate と npm のキャッシュは置き場（--data）の中に置く
# （PC では名前付きボリュームに残り、2 回目から速い。Cloudflare では使い捨て）
RUN useradd --create-home --uid 1000 --shell /usr/sbin/nologin builder \
 && mkdir -p /var/lib/zc-buildd && chown builder:builder /var/lib/zc-buildd
USER builder
ENV HOME=/home/builder \
    LANG=C.UTF-8 \
    CARGO_HOME=/var/lib/zc-buildd/cargo-home \
    npm_config_cache=/var/lib/zc-buildd/npm-cache
WORKDIR /var/lib/zc-buildd
EXPOSE 7411
# SIGTERM で、走っているジョブを止めてから終わる（Cloudflare は SIGTERM の最大 15 分後に SIGKILL）
ENTRYPOINT ["/usr/local/bin/zc-buildd", "--listen", "0.0.0.0:7411", "--data", "/var/lib/zc-buildd"]
