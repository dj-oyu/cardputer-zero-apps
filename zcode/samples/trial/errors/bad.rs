// 型の誤り（Cargo.toml の無い 1 ファイル）
fn main() {
    let n: u32 = "日本";
    println!("{}", n);
}
