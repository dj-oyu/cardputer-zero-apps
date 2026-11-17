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
# zc-buildd を足したもの。2026-11-17 に、pyright（Python の型検査。Node で動く）・ShellCheck・
# aarch64 の g++・Go・Ruby・Zig・MoonBit を足した。Python の実行系・フォント・git は入れない。
# 大きさは zcode/AGENTS.md（言語を足す前は展開後 1.52GB、送る形で約 500MB）。
#
# 版はここの ARG で固定し、取ってきたものは SHA-256 で確かめる:
#   - ベースは日付つきのタグ + digest
#   - rustup-init は static.rust-lang.org の .sha256、Rust はその版の rustup の検査に任せる
#   - Node は nodejs.org の SHASUMS256.txt の値
#   - esbuild・TypeScript・pyright は remote/container/tools/package-lock.json の integrity（npm ci）
#   - ShellCheck・Go・Zig・MoonBit は配布物の SHA-256（ARG）。g++ と ruby は Debian の apt
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
ENV PATH=/opt/cargo/bin:/opt/node/bin:/opt/tools/node_modules/.bin:/opt/go/bin:/opt/zig:/opt/moon/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

# ---- 2026-11-17 に足した言語（docs/stage3-design.md §4.4・§6.4） ----
# C++（aarch64 の g++）と Ruby（ruby -wc。Debian の 3.3、版はベースの digest で決まる）
RUN apt-get update \
 && apt-get install -y --no-install-recommends g++-aarch64-linux-gnu ruby \
 && rm -rf /var/lib/apt/lists/* \
 && aarch64-linux-gnu-g++ --version | head -1 && ruby --version
# ShellCheck（静的リンクの実行ファイル。GitHub の Release の .tar.xz の SHA-256）
ARG SHELLCHECK_VERSION=0.11.0
ARG SHELLCHECK_SHA256=8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198
RUN curl -fsSLo /tmp/sc.tar.xz \
      "https://github.com/koalaman/shellcheck/releases/download/v${SHELLCHECK_VERSION}/shellcheck-v${SHELLCHECK_VERSION}.linux.x86_64.tar.xz" \
 && echo "${SHELLCHECK_SHA256}  /tmp/sc.tar.xz" | sha256sum -c - \
 && tar -xJf /tmp/sc.tar.xz -C /tmp \
 && install -m 0755 "/tmp/shellcheck-v${SHELLCHECK_VERSION}/shellcheck" /usr/local/bin/shellcheck \
 && rm -rf /tmp/sc.tar.xz "/tmp/shellcheck-v${SHELLCHECK_VERSION}" \
 && shellcheck --version
# Go（go.dev の配布物。SHA-256 は https://go.dev/dl/?mode=json の値）。試験・文書・道具のソースは消す
# （`go build`・`go vet`・`go test` は pkg/tool の作り済みのもので動く。2026-11-17 に確かめた）
ARG GO_VERSION=1.27.4
ARG GO_SHA256=7226736c2339719cda28fbf3a54d71aa63bcefe376e98cef00034268dab24840
RUN curl -fsSLo /tmp/go.tar.gz "https://go.dev/dl/go${GO_VERSION}.linux-amd64.tar.gz" \
 && echo "${GO_SHA256}  /tmp/go.tar.gz" | sha256sum -c - \
 && mkdir -p /opt/go \
 && tar -xzf /tmp/go.tar.gz -C /opt/go --strip-components=1 --no-same-owner \
 && rm -rf /tmp/go.tar.gz /opt/go/test /opt/go/api /opt/go/doc /opt/go/misc /opt/go/src/cmd \
 && GOTOOLCHAIN=local go version
# Zig（ziglang.org の配布物。SHA-256 は https://ziglang.org/download/index.json の値）。libc の
# ヘッダは Linux の aarch64・x86_64 の分だけ残す（Windows・macOS・BSD・WASI などは消す）
ARG ZIG_VERSION=0.17.1
ARG ZIG_SHA256=3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a
RUN curl -fsSLo /tmp/zig.tar.xz "https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz" \
 && echo "${ZIG_SHA256}  /tmp/zig.tar.xz" | sha256sum -c - \
 && mkdir -p /opt/zig \
 && tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1 --no-same-owner \
 && rm -rf /tmp/zig.tar.xz /opt/zig/doc /opt/zig/lib/libtsan \
      /opt/zig/lib/libc/mingw /opt/zig/lib/libc/darwin /opt/zig/lib/libc/freebsd /opt/zig/lib/libc/netbsd \
      /opt/zig/lib/libc/openbsd /opt/zig/lib/libc/wasi \
 && cd /opt/zig/lib/libc/include \
 && for d in *; do case "$d" in aarch64-linux-*|x86_64-linux-*|x86-linux-*|any-linux-any|generic-glibc|generic-musl) ;; *) rm -rf "$d" ;; esac; done \
 && zig version
# MoonBit（cli.moonbitlang.com の版つきの配布物と core。SHA-256 は取ってきて確かめた値。
# 版の名前の + は %2B）。検査・ビルド・試験に要るもの（moon・moonc・mooncake・tcc。moonrun は
# native では使わないが、無いと moon の一部のコマンドが落ちるので残す）だけ残し、
# core は native の分だけ束ねる。ライセンス: 道具は無償の配布物（CREDITS.md）、core は Apache-2.0
ARG MOONBIT_VERSION=0.10.20%2B65efd5b93
ARG MOONBIT_SHA256=80d987523308fa490eba9f1baf98df996a5a7891e118357553b7c2608c0597c5
ARG MOONBIT_CORE_SHA256=8ec2a2c01335501d703867bb4eef5f92d3cd7e1ae1ee5f7f14d54d16d2c7c2b5
RUN curl -fsSLo /tmp/moon.tar.gz "https://cli.moonbitlang.com/binaries/${MOONBIT_VERSION}/moonbit-linux-x86_64.tar.gz" \
 && curl -fsSLo /tmp/core.tar.gz "https://cli.moonbitlang.com/cores/core-${MOONBIT_VERSION}.tar.gz" \
 && echo "${MOONBIT_SHA256}  /tmp/moon.tar.gz" | sha256sum -c - \
 && echo "${MOONBIT_CORE_SHA256}  /tmp/core.tar.gz" | sha256sum -c - \
 && mkdir -p /opt/moon \
 && tar -xzf /tmp/moon.tar.gz -C /opt/moon --no-same-owner \
 && tar -xzf /tmp/core.tar.gz -C /opt/moon/lib --no-same-owner \
 && rm -rf /tmp/moon.tar.gz /tmp/core.tar.gz /opt/moon/share /opt/moon/lib/prelude_proof /opt/moon/lib/prelude_proof_machine_int \
 && cd /opt/moon/bin \
 && chmod 0755 * internal/tcc \
 && MOON_HOME=/opt/moon ./moon -C /opt/moon/lib/core bundle --warn-list -a --target native \
 && rm -f moonc-prove moondoc moon-lsp moon-ide mooninfo moon_cove_report moon_cove.wasm moon-cram mooncram.wasm moonfmt moon-wasm-opt \
 && chmod -R a+rX,go-w /opt/moon \
 && MOON_HOME=/opt/moon moon version --all
ENV MOON_HOME=/opt/moon \
    GOTOOLCHAIN=local \
    GOTELEMETRY=off

# esbuild・TypeScript 7（Go 版の tsc。linux-x64 の実行ファイルは optionalDependencies）・pyright。
# インストールスクリプトは動かさない（esbuild の postinstall は確かめるだけで、無くても動く）
COPY remote/container/tools/package.json remote/container/tools/package-lock.json /opt/tools/
RUN cd /opt/tools \
 && npm ci --ignore-scripts --no-audit --no-fund --cache /tmp/npm-cache \
 && rm -rf /tmp/npm-cache \
 && node --version && npm --version && tsc --version && esbuild --version && pyright --version
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
    npm_config_cache=/var/lib/zc-buildd/npm-cache \
    GOPATH=/var/lib/zc-buildd/go \
    GOCACHE=/var/lib/zc-buildd/go-build \
    ZIG_GLOBAL_CACHE_DIR=/var/lib/zc-buildd/zig-global
WORKDIR /var/lib/zc-buildd
EXPOSE 7411
# SIGTERM で、走っているジョブを止めてから終わる（Cloudflare は SIGTERM の最大 15 分後に SIGKILL）
ENTRYPOINT ["/usr/local/bin/zc-buildd", "--listen", "0.0.0.0:7411", "--data", "/var/lib/zc-buildd"]
