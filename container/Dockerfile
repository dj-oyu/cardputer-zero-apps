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

WORKDIR /work
CMD ["bash"]
