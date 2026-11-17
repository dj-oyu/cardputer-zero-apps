#!/bin/bash
# rust-analyzer の計測をビルド係のイメージの中で回す（docs/measurements.md「rust-analyzer」）。
# Cloudflare のコンテナと同じ大きさ（1 vCPU・3GiB）に絞って、1 つの場面を 1 つのコンテナで:
#
#   wslc run --rm --memory 3g --cpus 1 -v "<zcode>/bench/lsp:/b:ro" --entrypoint bash \
#       cz-builder:lsp /b/run.sh <場面>
#
# 場面: hello | serde | ripgrep | ripgrep-noproc | ripgrep-noprime | ripgrep-build
#   （ripgrep-build は、冷えた rust-analyzer と F5 のビルドを同時に走らせる）
# memory.peak はコンテナ全体の値なので、場面ごとにコンテナを分ける。
set -eu
scene=${1:?場面}
W=/var/lib/zc-buildd/m
mkdir -p "$W"
cd "$W"
export CARGO_HOME=$W/cargo-home
B=$(dirname "$0")

# rust-analyzer の設定（ビルド係で使うもの）。checkOnSave は zc-buildd の check があるので切る。
# targetDir は zc-buildd の cargo と target/ のロックを取り合わないように別にする。
BASE='{"checkOnSave":false,"cargo":{"targetDir":true}}'
NOPROC='{"checkOnSave":false,"cargo":{"targetDir":true,"buildScripts":{"enable":false}},"procMacro":{"enable":false}}'
NOPRIME='{"checkOnSave":false,"cargo":{"targetDir":true},"cachePriming":{"enable":false}}'

hello() {
  cargo new -q --bin hello
  cat > hello/src/main.rs <<'EOF'
fn main() {
    let s = String::from("こんにちは");
    println!("{} {}", s.len(), s);
}
EOF
}
serde() {
  cargo new -q --bin serde1
  cat >> serde1/Cargo.toml <<'EOF'
regex = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
EOF
  cat > serde1/src/main.rs <<'EOF'
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Item {
    name: String,
    n: u32,
}

fn main() {
    let re = Regex::new(r"\d+").unwrap();
    let item = Item { name: "a".into(), n: 1 };
    let s = serde_json::to_string(&item).unwrap();
    println!("{} {}", re.is_match(&s), s);
}
EOF
}
ripgrep() {
  curl -fsSL https://static.crates.io/crates/ripgrep/ripgrep-14.1.1.crate | tar xz
  mv ripgrep-14.1.1 rg
}

measure() { # <名前> <フォルダ> <ファイル> <字句> <設定>
  node "$B/ra-measure.mjs" --label "$1" --root "$W/$2" --file "$3" --at "$4" --settings "$5" --timeout 1500
}

case $scene in
  hello)
    hello
    measure hello-cold hello src/main.rs "s.len" "$BASE"
    measure hello-restart hello src/main.rs "s.len" "$BASE" ;;
  serde)
    serde
    measure serde-cold serde1 src/main.rs "serde_json::to_string" "$BASE"
    measure serde-restart serde1 src/main.rs "serde_json::to_string" "$BASE" ;;
  ripgrep)
    ripgrep
    measure rg-cold rg crates/core/main.rs "anyhow::Result" "$BASE"
    measure rg-restart rg crates/core/main.rs "anyhow::Result" "$BASE" ;;
  ripgrep-noproc)
    ripgrep
    measure rg-noproc-cold rg crates/core/main.rs "anyhow::Result" "$NOPROC" ;;
  ripgrep-noprime)
    ripgrep
    measure rg-noprime-cold rg crates/core/main.rs "anyhow::Result" "$NOPRIME" ;;
  ripgrep-build)
    ripgrep
    (cd rg && cargo fetch -q)
    # zc-buildd の F5 と同じビルド（target は zc-buildd と同じく <根>/target）
    ( start=$(date +%s%N)
      cd rg && cargo build -q --target aarch64-unknown-linux-gnu --profile zc-f5 \
        --config 'profile.zc-f5.inherits="dev"' --config profile.zc-f5.opt-level=2 --config profile.zc-f5.debug=0 \
        --config profile.zc-f5.strip=true --config profile.zc-f5.lto=false --config profile.zc-f5.incremental=true \
        --config profile.zc-f5.codegen-units=16 \
        --config 'target.aarch64-unknown-linux-gnu.rustflags=["-Ctarget-cpu=cortex-a53"]' 2>/tmp/build.err
      rc=$?; echo "{\"label\":\"rg-build\",\"exit\":$rc,\"ms\":$(( ($(date +%s%N) - start) / 1000000 ))}"
    ) &
    bp=$!
    measure rg-cold-with-build rg crates/core/main.rs "anyhow::Result" "$BASE"
    wait $bp || true
    tail -3 /tmp/build.err
    echo "{\"label\":\"cgroup\",\"peak_mib\":$(( $(cat /sys/fs/cgroup/memory.peak) / 1048576 )),\"oom_kill\":\"$(grep oom_kill /sys/fs/cgroup/memory.events | tr '\n' ' ')\"}" ;;
esac
echo "{\"label\":\"cgroup-end\",\"peak_mib\":$(( $(cat /sys/fs/cgroup/memory.peak 2>/dev/null || echo 0) / 1048576 )),\"events\":\"$(tr '\n' ' ' < /sys/fs/cgroup/memory.events 2>/dev/null)\"}"
