# zcode のビルド係のイメージ（Cloudflare Containers と PC で同じもの）。linux/amd64。
# docs/stage3-design.md §6.4。雛形: 課題 C3 が、版とチェックサムを固定して仕上げる。
#
# ビルドの文脈は zcode/（zcode/.dockerignore で、要るものだけに絞ってある）:
#   docker build -f remote/container/Dockerfile -t cz-builder:latest .      （zcode/ で）
#   wslc build  -f remote/container/Dockerfile -t cz-builder:latest .
#
# 中身は開発コンテナ（container/Dockerfile）の Rust の部分と同じ（Debian 13・rustup の stable・
# aarch64 のクロス）に、Node・npm・esbuild・TypeScript 7 の tsc・zstd・受付のプログラムを足したもの。
# Python・フォントは入れない（イメージを小さく保つ）。
# **秘密を入れない**（build.rs・postinstall・試験といった任意のコードがこの中で動く）。

# ---- 受付のプログラム（zc-buildd）を作る ----
FROM debian:trixie-slim AS build
ENV DEBIAN_FRONTEND=noninteractive RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl gcc libc6-dev \
 && rm -rf /var/lib/apt/lists/*
# TODO(C3): rustup-init をチェックサムつきで取り、Rust の版を固定する（ARG RUST_VERSION）
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
WORKDIR /src
COPY . .
RUN cd remote && cargo build --release --locked -p zc-buildd \
 && cp target/release/zc-buildd /usr/local/bin/zc-buildd

# ---- 実行用 ----
FROM debian:trixie-slim
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/node/bin:$PATH \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl git xz-utils zstd file \
      gcc libc6-dev gcc-aarch64-linux-gnu libc6-dev-arm64-cross binutils-aarch64-linux-gnu \
 && rm -rf /var/lib/apt/lists/*
# Rust（ホスト用と aarch64 用。試験は x86_64 のまま動かす）
# TODO(C3): 版を固定する
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
      --default-toolchain stable --target aarch64-unknown-linux-gnu \
 && chmod -R a+rX /opt/cargo /opt/rustup
# Node・npm（nodejs.org の配布物。SHASUMS256 で確かめる）、esbuild、TypeScript 7（Go 版の tsc）
# TODO(C3): ARG NODE_VERSION / ESBUILD_VERSION / TYPESCRIPT_VERSION と SHA-256 を固定して入れる
#   - /opt/node（node・npm）
#   - npm i -g --ignore-scripts esbuild@<版> @typescript/typescript-linux-x64@<版>（tsc を PATH に）
# TODO(C3・キャッシュ): sccache を入れるか（docs/stage3-design.md §6.5）

COPY --from=build /usr/local/bin/zc-buildd /usr/local/bin/zc-buildd

# 道具は root で動かさない
RUN useradd --create-home --uid 1000 builder \
 && mkdir -p /var/lib/zc-buildd && chown builder:builder /var/lib/zc-buildd
USER builder
ENV HOME=/home/builder
EXPOSE 7411
# SIGTERM で、走っているジョブを止めてから終わる（Cloudflare は SIGTERM の最大 15 分後に SIGKILL）
ENTRYPOINT ["/usr/local/bin/zc-buildd", "--listen", "0.0.0.0:7411", "--data", "/var/lib/zc-buildd"]
