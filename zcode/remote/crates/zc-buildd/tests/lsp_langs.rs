//! Rust 以外の言語サーバー（`zc_buildd::lsp::servers` の表）を、本物のサーバーで確かめる
//! （docs/lsp-design.md）。ビルド係のイメージ（cz-builder）の中で回す:
//!
//! ```text
//! wslc run --rm -v "<zcode>:/work:ro" -w /work/remote -e CARGO_TARGET_DIR=/var/lib/zc-buildd/t \
//!     -e ZC_E2E_REQUIRE=1 --entrypoint bash cz-builder:latest -c "cargo test --locked -p zc-buildd --test lsp_langs -- --nocapture"
//! ```
//!
//! 言語ごとに「関数を定義して、別の所で使う」小さなプロジェクトを作り、使っている所で
//! hover（名前が出る）と定義へ（定義の行へ飛ぶ）を必ず確かめる。使っている所・名前を探す・
//! 補完は、サーバーによって 1 ファイルだけでは答えないことがあるので、結果を表に出すだけ
//! （`--nocapture` で見る）。サーバーが無ければ飛ばす（`ZC_E2E_REQUIRE=1` なら落ちる）。
//! ネットワークは要らない（依存の無いプロジェクトだけ）。

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use zc_buildd::lsp::{recv_until, spec_for, LspHub, Query};
use zc_buildd::store::Store;
use zc_wire::msg::{FileEntry, LspPhase, LspQuery, ServerMsg};
use zc_wire::tree::Manifest;

const READY_WAIT: Duration = Duration::from_secs(180);
const ANSWER_WAIT: Duration = Duration::from_secs(90);

struct Case {
    name: &'static str,
    files: &'static [(&'static str, &'static str)],
    /// 使っている所（パス・行・その行で探す字）。
    at: (&'static str, u32, &'static str),
    /// 定義の場所として受ける（パス・行）。宣言と定義のどちらに飛ぶかがサーバーで違うものは 2 つ。
    def: &'static [(&'static str, u32)],
    /// 補完を頼む所（パス・行・その行で探す字の後ろ）と、候補に出てほしい名前。
    complete: Option<(&'static str, u32, &'static str, &'static str)>,
}

const CASES: &[Case] = &[
    Case {
        name: "python",
        files: &[("util.py", "def twice(x: int) -> int:\n    \"\"\"2 倍にする。\"\"\"\n    return x * 2\n"), ("main.py", "from util import twice\n\ns = \"日本語\"; print(twice(3))\ntw\n")],
        at: ("main.py", 2, "twice"),
        def: &[("util.py", 0)],
        complete: Some(("main.py", 3, "tw", "twice")),
    },
    Case {
        name: "typescript",
        files: &[("util.ts", "/** 2 倍にする。 */\nexport function twice(x: number): number {\n  return x * 2;\n}\n"), ("main.ts", "import { twice } from \"./util\";\n\nconst s = \"日本語\"; console.log(s, twice(3));\n")],
        at: ("main.ts", 2, "twice"),
        def: &[("util.ts", 1)],
        complete: None,
    },
    Case {
        name: "javascript",
        files: &[("main.js", "function twice(x) {\n  return x * 2;\n}\nconsole.log(twice(3));\n")],
        at: ("main.js", 3, "twice"),
        def: &[("main.js", 0)],
        complete: None,
    },
    Case {
        name: "go",
        files: &[
            ("go.mod", "module example.com/s\n\ngo 1.27\n"),
            ("main.go", "package main\n\nimport \"fmt\"\n\nfunc main() {\n\ts := \"日本語\"\n\tfmt.Println(s, twice(3))\n}\n"),
            ("util.go", "package main\n\n// twice は 2 倍にする。\nfunc twice(x int) int { return x * 2 }\n"),
        ],
        at: ("main.go", 6, "twice"),
        def: &[("util.go", 3)],
        complete: None,
    },
    Case {
        name: "zig",
        files: &[("main.zig", "const std = @import(\"std\");\n/// 2 倍にする。\nfn twice(x: u32) u32 {\n    return x * 2;\n}\npub fn main() void {\n    std.debug.print(\"{}\\n\", .{twice(3)});\n}\n")],
        at: ("main.zig", 6, "twice"),
        def: &[("main.zig", 2)],
        complete: None,
    },
    Case {
        name: "c",
        files: &[
            ("util.h", "/* 2 倍にする。 */\nint twice(int x);\n"),
            ("util.c", "#include \"util.h\"\nint twice(int x) { return x * 2; }\n"),
            ("main.c", "#include <stdio.h>\n#include \"util.h\"\nint main(void) { printf(\"%d\\n\", twice(3)); return 0; }\n"),
        ],
        at: ("main.c", 2, "twice"),
        def: &[("util.h", 1), ("util.c", 1)],
        complete: None,
    },
    Case {
        name: "cpp",
        files: &[("main.cpp", "#include <cstdio>\n// 2 倍にする。\nstatic int twice(int x) { return x * 2; }\nint main() { std::printf(\"%d\\n\", twice(3)); }\n")],
        at: ("main.cpp", 3, "twice"),
        def: &[("main.cpp", 2)],
        complete: None,
    },
    Case {
        name: "shell",
        files: &[("main.sh", "#!/bin/bash\n# 2 倍にする。\ntwice() {\n  echo $(( $1 * 2 ))\n}\ntwice 3\n")],
        at: ("main.sh", 5, "twice"),
        def: &[("main.sh", 2)],
        complete: None,
    },
    Case {
        name: "ruby",
        // トップレベルの def の呼び出しは ruby-lsp が追わないので、クラスのメソッドで確かめる
        files: &[("calc.rb", "class Calc\n  # 2 倍にする。\n  def self.twice(x) = x * 2\nend\n"), ("main.rb", "require_relative \"calc\"\nputs Calc.twice(3)\n")],
        at: ("main.rb", 1, "twice"),
        def: &[("calc.rb", 2)],
        complete: None,
    },
    Case {
        name: "moonbit",
        // 別のパッケージの関数（@s.twice）を呼ぶ形。moon-lsp は木の中の `_build` を読むので、木を
        // そろえるときに消すと答えなくなる（Spec::keep。2026-11-18 に実機のサンプルで見つけた）
        files: &[
            ("moon.mod", "name = \"t/s\"\n\nversion = \"0.1.0\"\n"),
            ("moon.pkg", ""),
            ("lib.mbt", "///|\n/// 2 倍にする。\npub fn twice(x : Int) -> Int {\n  x * 2\n}\n"),
            ("cmd/main/moon.pkg", "import {\n  \"t/s\" @s,\n}\n\npkgtype(kind: \"executable\")\n"),
            ("cmd/main/main.mbt", "///|\nfn main {\n  println(@s.twice(2))\n}\n"),
        ],
        at: ("cmd/main/main.mbt", 2, "twice"),
        def: &[("lib.mbt", 2)],
        complete: None,
    },
];

fn require() -> bool {
    std::env::var_os("ZC_E2E_REQUIRE").is_some_and(|v| v == "1")
}

/// 作業フォルダ。`/tmp` の下は ruby-lsp が索引から外す（`**/tmp/**`）ので、HOME の下を使う。
fn tmp(name: &str) -> PathBuf {
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")).unwrap_or_else(std::env::temp_dir);
    let d = base.join(format!("zc-lsp-langs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

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

fn line_of<'a>(files: &[(&str, &'a str)], path: &str, line: u32) -> &'a str {
    files.iter().find(|(p, _)| *p == path).unwrap().1.split('\n').nth(line as usize).unwrap()
}

/// 問い合わせて答えを待つ。準備中（unavailable）なら少し待って何度か頼み直す。
#[allow(clippy::too_many_arguments)]
fn ask(hub: &LspHub, store: &Store, m: &Manifest, rx: &mpsc::Receiver<ServerMsg>, tx: &mpsc::Sender<ServerMsg>, id: &mut u32, query: LspQuery, path: &str, line: u32, col: u32, text: &str) -> ServerMsg {
    let mut last = None;
    for _ in 0..6 {
        *id += 1;
        let want = *id;
        hub.query(store, "aa00bb11cc22dd33", m, Query { id: want, query, path: path.into(), line, col, text: text.into() }, tx);
        let r = recv_until(rx, ANSWER_WAIT, |m| matches!(m, ServerMsg::LspResult { id: i, .. } if *i == want)).expect("答えが来ない");
        if matches!(&r, ServerMsg::LspResult { error: Some(zc_wire::msg::ErrorCode::Unavailable), .. }) {
            last = Some(r);
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }
        return r;
    }
    panic!("ずっと準備中: {last:?}");
}

/// 1 言語ぶん。返すのは表の 1 行。
fn run(case: &Case) -> Option<String> {
    let spec = spec_for(case.at.0).expect("表にある");
    let have = zc_buildd::lsp::servers::find(spec).is_some();
    if !have {
        assert!(!require(), "{} が無い（ZC_E2E_REQUIRE=1）", spec.name);
        return Some(format!("{:<11} {} が無いので飛ばす", case.name, spec.name));
    }
    let dir = tmp(case.name);
    let store = Store::open(&dir).unwrap();
    let hub = LspHub::new();
    let (tx, rx) = mpsc::channel();
    let m = manifest(&store, case.files);
    let t0 = std::time::Instant::now();
    hub.warm(&store, "aa00bb11cc22dd33", &m, case.at.0, 1, &tx);
    let st = recv_until(&rx, READY_WAIT, |m| matches!(m, ServerMsg::LspState { state: LspPhase::Ready | LspPhase::Failed, .. }));
    let Some(ServerMsg::LspState { state: LspPhase::Ready, .. }) = st else { panic!("{}: 準備できない: {st:?}", case.name) };
    let ready_ms = t0.elapsed().as_millis();
    let mut id = 0;
    let (path, line, needle) = case.at;
    let col = line_of(case.files, path, line).find(needle).unwrap() as u32 + 1;

    // hover: 名前が出る（Markdown の囲みは外れている）
    // （開いた直後は解析が済んでおらず空で返すサーバーがある: 空なら少し待って頼み直す。実機でも
    // 人がもう一度押せば出る）
    let mut tries = 0;
    let r = loop {
        let r = ask(&hub, &store, &m, &rx, &tx, &mut id, LspQuery::Hover, path, line, col, "");
        tries += 1;
        if tries >= 5 || !matches!(&r, ServerMsg::LspResult { error: None, text, .. } if text.is_empty()) {
            break r;
        }
        std::thread::sleep(Duration::from_secs(2));
    };
    let ServerMsg::LspResult { error: None, text: hover, .. } = &r else { panic!("{}: hover {r:?}", case.name) };
    if tries > 1 {
        eprintln!("{}: hover は {tries} 回目で答えた", case.name);
    }
    assert!(hover.contains("twice"), "{}: hover に名前が無い: {hover:?}", case.name);
    assert!(!hover.contains("```"), "{}: 囲みが残っている: {hover:?}", case.name);

    // 定義へ: 定義の行（プロジェクトの中）
    let r = ask(&hub, &store, &m, &rx, &tx, &mut id, LspQuery::Definition, path, line, col, "");
    let ServerMsg::LspResult { error: None, items, .. } = &r else { panic!("{}: definition {r:?}", case.name) };
    let got: Vec<(&str, u32)> = items.iter().map(|i| (i.path.as_str(), i.line)).collect();
    assert!(got.iter().any(|g| case.def.contains(g)), "{}: 定義の場所が違う: {got:?}（{:?} のどれか）", case.name, case.def);

    // 使っている所・名前を探す・補完は表に出すだけ
    let r = ask(&hub, &store, &m, &rx, &tx, &mut id, LspQuery::References, path, line, col, "");
    let refs = match &r {
        ServerMsg::LspResult { error: None, items, .. } => items.len().to_string(),
        ServerMsg::LspResult { message, .. } => format!("失敗（{message}）"),
        _ => unreachable!(),
    };
    let r = ask(&hub, &store, &m, &rx, &tx, &mut id, LspQuery::Symbols, path, 0, 0, "twice");
    let syms = match &r {
        ServerMsg::LspResult { error: None, items, .. } => items.iter().filter(|i| i.name.contains("twice")).count().to_string(),
        ServerMsg::LspResult { message, .. } => format!("失敗（{message}）"),
        _ => unreachable!(),
    };
    let comp = match case.complete {
        Some((p, l, after, want)) => {
            let c = (line_of(case.files, p, l).find(after).unwrap() + after.len()) as u32;
            match ask(&hub, &store, &m, &rx, &tx, &mut id, LspQuery::Completion, p, l, c, "") {
                ServerMsg::LspResult { error: None, items, .. } => {
                    let hit = items.iter().any(|i| i.name.starts_with(want) || i.insert.starts_with(want));
                    assert!(hit, "{}: 補完に {want} が無い: {:?}", case.name, items.iter().map(|i| &i.name).collect::<Vec<_>>());
                    format!("{} 件", items.len())
                }
                other => format!("失敗（{other:?}）"),
            }
        }
        None => "-".into(),
    };
    hub.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
    // 改行は ⏎ にして 1 行で見せる
    let first = hover.replace('\n', "⏎");
    Some(format!("{:<11} {:<21} 準備 {ready_ms:>5}ms  使用 {refs:<3} 名前 {syms:<3} 補完 {comp:<6} hover: {first}", case.name, spec.name))
}

#[test]
fn every_language_server_answers() {
    let mut table = Vec::new();
    for case in CASES {
        if let Some(row) = run(case) {
            table.push(row);
        }
    }
    eprintln!("\n言語サーバーの結果:\n{}", table.join("\n"));
}

/// 同じプロジェクトで別の言語のファイルに移ると、前のサーバーを止めて替える（同時に 1 つ）。
#[test]
fn switching_language_replaces_the_server() {
    let py = spec_for("a.py").unwrap();
    let sh = spec_for("a.sh").unwrap();
    if [py, sh].iter().any(|s| zc_buildd::lsp::servers::find(s).is_none()) {
        assert!(!require(), "pyright / bash-language-server が無い");
        return;
    }
    let dir = tmp("switch");
    let store = Store::open(&dir).unwrap();
    let hub = LspHub::new();
    let (tx, rx) = mpsc::channel();
    let m = manifest(&store, &[("a.py", "x = 1\n"), ("a.sh", "echo 1\n")]);
    hub.warm(&store, "aa00bb11cc22dd33", &m, "a.py", 1, &tx);
    assert!(recv_until(&rx, READY_WAIT, |m| matches!(m, ServerMsg::LspState { state: LspPhase::Ready, .. })).is_some());
    hub.warm(&store, "aa00bb11cc22dd33", &m, "a.sh", 1, &tx);
    assert!(recv_until(&rx, READY_WAIT, |m| matches!(m, ServerMsg::LspState { state: LspPhase::Ready, .. })).is_some());
    // パスの無い問い合わせ（名前を探す）は今のサーバー（bash）へ
    let mut id = 0;
    let r = ask(&hub, &store, &m, &rx, &tx, &mut id, LspQuery::Symbols, "", 0, 0, "x");
    assert!(matches!(r, ServerMsg::LspResult { error: None, .. }), "{r:?}");
    // 受け持つサーバーの無い種類は断る
    hub.query(&store, "aa00bb11cc22dd33", &m, Query { id: 99, query: LspQuery::Hover, path: "README.md".into(), line: 0, col: 0, text: String::new() }, &tx);
    let r = recv_until(&rx, ANSWER_WAIT, |m| matches!(m, ServerMsg::LspResult { id: 99, .. })).unwrap();
    assert!(matches!(&r, ServerMsg::LspResult { error: Some(_), message, .. } if message.contains("README.md")), "{r:?}");
    hub.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
