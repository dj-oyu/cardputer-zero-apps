# CardputerZero アプリの開発用コンテナ (Windows の WSL コンテナー / Docker 共通)
#
# 実機と同じ Debian 13 (trixie) / Python 3.13 で、実機なしで回せる作業をする:
#   - .deb のビルド (各アプリの build.sh。dpkg-deb は Windows に無い)
#   - 描画確認 (--render-test。Noto CJK フォント込み)
#   - qrreader の自己テスト (--selftest。zxing-cpp)
# カメラ・fb0・キーボード・BlueZ は無いので、実際の動作確認は実機で行う。
# ホストは x86_64 だが、アプリは pure Python で .deb は arm64 ラベルを
# 付けるだけなので、ここで作った .deb をそのまま実機に入れられる。
#
# 使い方は container/README.md (Windows では container/dev.ps1 経由)。
FROM debian:trixie-slim

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONDONTWRITEBYTECODE=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1

# 実機の Raspberry Pi OS に入っている/アプリが Depends で要求する
# python3 パッケージと、描画に使う CJK フォント
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      python3 python3-pip \
      python3-pil python3-numpy python3-dbus python3-gi \
      fonts-noto-cjk \
      file \
 && rm -rf /var/lib/apt/lists/*

# 実機と同じく pip で入れるもの (qrreader の README の手順に合わせる)
#   zxing-cpp: デコードエンジン / segno: テスト用 QR の生成
RUN pip3 install --no-cache-dir --break-system-packages zxing-cpp segno

# Rust アプリ用: rustup の stable と aarch64 クロスリンカ。
# 実機と同じ trixie の glibc でリンクするので、出来たバイナリはそのまま動く。
# --render-test 用に x86_64 のネイティブビルドもできる。
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl gcc libc6-dev \
      gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
      fonts-dejavu-core \
 && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:$PATH \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
      --default-toolchain stable --target aarch64-unknown-linux-gnu \
 && rustup component add clippy rustfmt \
 && chmod -R a+rwX /opt/cargo /opt/rustup
# crate のキャッシュとビルド成果物は、dev.ps1 が名前付きボリュームとして
# /opt/cargo/registry と /cargo-target にマウントする (Windows 側の
# マウント越しにビルドすると遅いため)
ENV CARGO_TARGET_DIR=/cargo-target

WORKDIR /work
CMD ["bash"]
