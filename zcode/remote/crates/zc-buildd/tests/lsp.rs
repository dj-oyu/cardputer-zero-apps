//! rust-analyzer の橋渡し（`zc_buildd::lsp`）を、本物の rust-analyzer で確かめる
//! （docs/lsp-design.md）。ビルド係のイメージ（cz-builder）の中で回す:
//!
//! ```text
//! wslc run --rm -v "<zcode>:/work:ro" -w /work/remote -e CARGO_TARGET_DIR=/var/lib/zc-buildd/t \
//!     -e ZC_E2E_REQUIRE=1 --entrypoint bash cz-builder:latest -c "cargo test --locked -p zc-buildd --test lsp"
//! ```
//!
//! rust-analyzer が無ければ飛ばす（`ZC_E2E_REQUIRE=1` なら落ちる）。依存の無い crate だけを
//! 使うので、ネットワークは要らない。

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use zc_buildd::lsp::{recv_until, LspHub, Query};
use zc_buildd::store::Store;
use zc_wire::msg::{ErrorCode, FileEntry, LspPhase, LspQuery, ServerMsg};
use zc_wire::tree::Manifest;

const PROJECT: &str = "00112233aabbccdd";
/// rust-analyzer が最初の索引を終えるまで待つ時間（1 vCPU でも足りるように多め）。
const READY_WAIT: Duration = Duration::from_secs(240);
const ANSWER_WAIT: Duration = Duration::from_secs(60);

fn skip() -> bool {
    let have = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("rust-analyzer").is_file()));
    if have {
        return false;
    }
    if std::env::var_os("ZC_E2E_REQUIRE").is_some_and(|v| v == "1") {
        panic!("rust-analyzer が無い（ZC_E2E_REQUIRE=1）");
    }
    eprintln!("rust-analyzer が無いので飛ばす");
    true
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zc-lsp-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 中身を置き場に入れて、目録を作る（実機が送ってくるのと同じ形）。
fn manifest(store: &Store, files: &[(&str, &str)]) -> Manifest {
    let mut entries = Vec::new();
    for (p, text) in files {
        let sha = zc_wire::tree::hex(&Sha256::digest(text.as_bytes()));
        let mut w = store.begin_blob(&sha, text.len() as u64).unwrap();
        w.write(0, text.as_bytes()).unwrap();
        w.finish().unwrap();
        entries.push(FileEntry { path: p.to_string(), size: text.len() as u64, sha256: sha, exec: false });
    }
    Manifest::new(entries).unwrap()
}

fn wait_ready(rx: &mpsc::Receiver<ServerMsg>) {
    let m = recv_until(rx, READY_WAIT, |m| matches!(m, ServerMsg::LspState { state: LspPhase::Ready | LspPhase::Failed, .. }));
    match m {
        Some(ServerMsg::LspState { state: LspPhase::Ready, .. }) => {}
        other => panic!("rust-analyzer が準備できない: {other:?}"),
    }
}

/// 問い合わせて、答え（`lsp_result`）を待つ。
fn ask(hub: &LspHub, store: &Store, m: &Manifest, rx: &mpsc::Receiver<ServerMsg>, q: Query) -> ServerMsg {
    let id = q.id;
    hub.query(store, PROJECT, m, q, &tx_of(rx));
    recv_until(rx, ANSWER_WAIT, |m| matches!(m, ServerMsg::LspResult { id: i, .. } if *i == id)).expect("答えが来ない")
}

// 試験の中で使う届け先（受け口ごとに 1 つ）。
thread_local! {
    static TX: std::cell::RefCell<Option<mpsc::Sender<ServerMsg>>> = const { std::cell::RefCell::new(None) };
}
fn tx_of(_rx: &mpsc::Receiver<ServerMsg>) -> mpsc::Sender<ServerMsg> {
    TX.with(|t| t.borrow().clone().unwrap())
}
fn channel() -> mpsc::Receiver<ServerMsg> {
    let (tx, rx) = mpsc::channel();
    TX.with(|t| *t.borrow_mut() = Some(tx));
    rx
}

fn q(id: u32, query: LspQuery, path: &str, line: u32, col: u32) -> Query {
    Query { id, query, path: path.into(), line, col, text: String::new() }
}

const CARGO_TOML: &str = "[package]\nname = \"zc-lsp-sample\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n";
const UTIL: &str = "/// 足し算（説明）。\npub fn helper(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
/// 3 行目は日本語の文字列の後ろに `helper` がある（UTF-8 と UTF-16 の桁が違う）。
const MAIN_V1: &str = "mod util;\n\nfn main() {\n    let s = \"日本語\"; let n = util::helper(1, 2);\n    let t = String::new();\n    println!(\"{s}{n}{}\", t.len());\n}\n";

fn col_of(text: &str, line: u32, needle: &str) -> u32 {
    text.split('\n').nth(line as usize).unwrap().find(needle).unwrap() as u32
}

#[test]
fn hover_definition_references_symbols_completion() {
    if skip() {
        return;
    }
    let dir = tmp("main");
    let store = Store::open(&dir).unwrap();
    let hub = LspHub::new();
    let rx = channel();
    let m1 = manifest(&store, &[("Cargo.toml", CARGO_TOML), ("src/main.rs", MAIN_V1), ("src/util.rs", UTIL)]);
    hub.warm(&store, PROJECT, &m1, "src/main.rs", 1, &tx_of(&rx));
    // すぐの状態は loading
    let first = recv_until(&rx, Duration::from_secs(5), |m| matches!(m, ServerMsg::LspState { .. })).unwrap();
    assert!(matches!(first, ServerMsg::LspState { state: LspPhase::Loading, .. }), "{first:?}");
    // 準備中の問い合わせには unavailable（待たせない）
    let early = ask(&hub, &store, &m1, &rx, q(1, LspQuery::Hover, "src/main.rs", 3, 0));
    assert!(matches!(early, ServerMsg::LspResult { error: Some(ErrorCode::Unavailable), .. }), "{early:?}");
    wait_ready(&rx);

    // hover: 日本語の後ろの helper（UTF-8 のバイト位置で頼む）
    let c = col_of(MAIN_V1, 3, "helper") + 2;
    let r = ask(&hub, &store, &m1, &rx, q(2, LspQuery::Hover, "src/main.rs", 3, c));
    let ServerMsg::LspResult { error: None, text, cpu_ms, .. } = &r else { panic!("{r:?}") };
    assert!(text.contains("pub fn helper(a: i32, b: i32) -> i32"), "{text}");
    assert!(text.contains("足し算"), "{text}");
    assert!(*cpu_ms > 0, "CPU を数えている");

    // definition: プロジェクトの中（util.rs の 1 行目の helper）
    let r = ask(&hub, &store, &m1, &rx, q(3, LspQuery::Definition, "src/main.rs", 3, c));
    let ServerMsg::LspResult { error: None, items, .. } = &r else { panic!("{r:?}") };
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!((items[0].path.as_str(), items[0].line, items[0].col), ("src/util.rs", 1, 7));
    assert_eq!(items[0].detail, "pub fn helper(a: i32, b: i32) -> i32 {");

    // definition: プロジェクトの外（String は alloc の中）
    let r = ask(&hub, &store, &m1, &rx, q(4, LspQuery::Definition, "src/main.rs", 4, col_of(MAIN_V1, 4, "String") + 1));
    let ServerMsg::LspResult { error: None, items, .. } = &r else { panic!("{r:?}") };
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].path.is_empty() && items[0].ext.starts_with("alloc/src/string"), "{items:?}");

    // references: 定義と使っている所
    let r = ask(&hub, &store, &m1, &rx, q(5, LspQuery::References, "src/util.rs", 1, 9));
    let ServerMsg::LspResult { error: None, items, .. } = &r else { panic!("{r:?}") };
    let mut got: Vec<(String, u32, u32)> = items.iter().map(|i| (i.path.clone(), i.line, i.col)).collect();
    got.sort();
    assert_eq!(got, [("src/main.rs".to_string(), 3, col_of(MAIN_V1, 3, "helper")), ("src/util.rs".to_string(), 1, 7)]);

    // workspace/symbol
    let r = ask(&hub, &store, &m1, &rx, Query { id: 6, query: LspQuery::Symbols, path: String::new(), line: 0, col: 0, text: "helper".into() });
    let ServerMsg::LspResult { error: None, items, .. } = &r else { panic!("{r:?}") };
    assert!(items.iter().any(|i| i.name == "helper" && i.path == "src/util.rs" && i.line == 1), "{items:?}");

    // completion: t.le|
    let main_c = MAIN_V1.replace("t.len()", "t.le");
    let m2 = manifest(&store, &[("Cargo.toml", CARGO_TOML), ("src/main.rs", &main_c), ("src/util.rs", UTIL)]);
    let cc = col_of(&main_c, 5, "t.le") + 4;
    let r = ask(&hub, &store, &m2, &rx, q(7, LspQuery::Completion, "src/main.rs", 5, cc));
    let ServerMsg::LspResult { error: None, items, .. } = &r else { panic!("{r:?}") };
    let len = items.iter().find(|i| i.name.starts_with("len")).unwrap_or_else(|| panic!("len が無い: {items:?}"));
    assert_eq!((len.line, len.col, len.insert.as_str()), (5, cc - 2, "len"));

    // 未保存の中身: helper の型を変えた版を重ねる → hover が新しい型を返す（ディスクの見張りに頼らない）
    let util2 = UTIL.replace("a: i32, b: i32) -> i32", "a: u8, b: u8) -> u8");
    let m3 = manifest(&store, &[("Cargo.toml", CARGO_TOML), ("src/main.rs", MAIN_V1), ("src/util.rs", &util2)]);
    let r = ask(&hub, &store, &m3, &rx, q(8, LspQuery::Hover, "src/main.rs", 3, c));
    let ServerMsg::LspResult { error: None, text, .. } = &r else { panic!("{r:?}") };
    assert!(text.contains("pub fn helper(a: u8, b: u8) -> u8"), "{text}");

    // 木に無いファイル
    let r = ask(&hub, &store, &m3, &rx, q(9, LspQuery::Hover, "src/none.rs", 0, 0));
    assert!(matches!(r, ServerMsg::LspResult { error: Some(ErrorCode::BadTree), .. }), "{r:?}");

    // CPU の通算は減らない
    let a = hub.cpu_ms();
    assert!(a >= *cpu_ms && hub.cpu_ms() >= a);
    hub.shutdown();
    assert!(matches!(hub.state(), None));
    // 止めた後の問い合わせは起動し直して「準備中」
    let r = ask(&hub, &store, &m3, &rx, q(10, LspQuery::Hover, "src/main.rs", 3, c));
    assert!(matches!(r, ServerMsg::LspResult { error: Some(ErrorCode::Unavailable), .. }), "{r:?}");
    hub.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Cargo.toml の無い「1 ファイルだけ」でも答える（仮の crate を linkedProjects で渡す）。
#[test]
fn single_file_without_cargo_toml() {
    if skip() {
        return;
    }
    let dir = tmp("single");
    let store = Store::open(&dir).unwrap();
    let hub = LspHub::new();
    let rx = channel();
    let text = "fn twice(x: u32) -> u32 { x * 2 }\nfn main() { println!(\"{}\", twice(3)); }\n";
    let m = manifest(&store, &[("main.rs", text)]);
    hub.warm(&store, PROJECT, &m, "main.rs", 1, &tx_of(&rx));
    wait_ready(&rx);
    let r = ask(&hub, &store, &m, &rx, q(1, LspQuery::Definition, "main.rs", 1, col_of(text, 1, "twice") + 1));
    let ServerMsg::LspResult { error: None, items, .. } = &r else { panic!("{r:?}") };
    assert_eq!(items.iter().map(|i| (i.path.as_str(), i.line, i.col)).collect::<Vec<_>>(), [("main.rs", 0, 3)]);
    hub.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
