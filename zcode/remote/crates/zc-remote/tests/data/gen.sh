#!/usr/bin/env bash
# 通信の試験（tests/inproc/net.rs）で使う証明書を作り直す。開発コンテナの中で実行する。
#   ca.pem            試験用の認証局（鍵は残さない）
#   leaf.key          サーバの鍵（**試験専用。ほかでは使わない**）
#   leaf-good.pem     有効（2020〜2120 年）
#   leaf-expired.pem  期限切れ（2000〜2001 年）
#   leaf-future.pem   まだ有効でない（2100〜2101 年）
set -euo pipefail
cd "$(dirname "$0")"
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$t/ca.key" -out ca.pem \
  -subj "/CN=zc-remote test CA" -days 36500 \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign" 2>/dev/null
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out leaf.key 2>/dev/null
openssl req -new -key leaf.key -subj "/CN=localhost" -out "$t/leaf.csr"
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:FALSE\n' > "$t/ext"
sign() {
  openssl x509 -req -in "$t/leaf.csr" -CA ca.pem -CAkey "$t/ca.key" -CAcreateserial -CAserial "$t/ser" \
    -extfile "$t/ext" -not_before "$2" -not_after "$3" -out "$1" 2>/dev/null
}
sign leaf-good.pem 20200101000000Z 21200101000000Z
sign leaf-expired.pem 20000101000000Z 20010101000000Z
sign leaf-future.pem 21000101000000Z 21010101000000Z
ls -l ca.pem leaf.key leaf-*.pem
