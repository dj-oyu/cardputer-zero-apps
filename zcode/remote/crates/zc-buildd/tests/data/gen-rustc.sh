#!/bin/bash
# runner/rust.rs の試験に使う rustc の JSON の実物を作る（開発コンテナで）。
#   cd zcode/remote/crates/zc-buildd/tests/data && bash gen-rustc.sh
# 木の根は /tmp/zc-sample/src に固定（試験はこのパスを根として読む）。
# 依存 crate（木の外）は /tmp/zc-sample/dep。
set -eu
out="$(cd "$(dirname "$0")" && pwd)"
rm -rf /tmp/zc-sample
mkdir -p /tmp/zc-sample/src/src /tmp/zc-sample/dep/src
export CARGO_TARGET_DIR=/tmp/zc-sample/target CARGO_TERM_COLOR=never

# 木の外の依存（中に型エラー）
cat > /tmp/zc-sample/dep/Cargo.toml <<'T'
[package]
name = "dep"
version = "0.1.0"
edition = "2021"
T
cat > /tmp/zc-sample/dep/src/lib.rs <<'T'
pub fn broken() -> i32 {
    "not a number"
}
T

cat > /tmp/zc-sample/src/Cargo.toml <<'T'
[package]
name = "sample"
version = "0.1.0"
edition = "2021"
T
# 型エラー・日本語のコメントの後ろのエラー・手元のマクロの中のエラー・derive・警告（lib と test で 2 回）
cat > /tmp/zc-sample/src/src/lib.rs <<'T'
pub fn typed() -> i32 {
    "s"
}

pub fn japanese() {
    let _x: i32 = /* 日本語 */ "y";
}

macro_rules! bad {
    () => {
        let _m: u8 = "inside macro";
    };
}

pub fn uses_macro() {
    bad!();
}

struct NoDebug;
#[derive(Debug)]
pub struct HasField {
    f: NoDebug,
}

pub fn warn() {
    let unused = 1;
}
T
(cd /tmp/zc-sample/src && cargo check --all-targets --message-format=json > "$out/rustc-errors.jsonl" 2>/dev/null || true)

# 依存 crate だけを指すエラー
cat > /tmp/zc-sample/src/Cargo.toml <<'T'
[package]
name = "sample"
version = "0.1.0"
edition = "2021"

[dependencies]
dep = { path = "../dep" }
T
cat > /tmp/zc-sample/src/src/lib.rs <<'T'
pub fn call() -> i32 {
    dep::broken()
}
T
(cd /tmp/zc-sample/src && cargo check --message-format=json > "$out/rustc-dep.jsonl" 2>/dev/null || true)

# 標準ライブラリのマクロ（println! の引数の型）
cat > /tmp/zc-sample/src/Cargo.toml <<'T'
[package]
name = "sample"
version = "0.1.0"
edition = "2021"
T
cat > /tmp/zc-sample/src/src/lib.rs <<'T'
struct P;
pub fn show() {
    println!("{}", P);
}
T
(cd /tmp/zc-sample/src && cargo check --message-format=json > "$out/rustc-std-macro.jsonl" 2>/dev/null || true)

# 試験: 通る・落ちる・無視
cat > /tmp/zc-sample/src/src/lib.rs <<'T'
pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    #[test]
    fn passes() { assert_eq!(super::add(1, 2), 3); }
    #[test]
    fn fails() {
        assert_eq!(super::add(1, 2), 4, "足し算が違う");
    }
    #[test]
    #[ignore]
    fn skipped() {}
}
T
(cd /tmp/zc-sample/src && cargo test --no-fail-fast --message-format=json > "$out/libtest.txt" 2>/dev/null || true)
echo done
