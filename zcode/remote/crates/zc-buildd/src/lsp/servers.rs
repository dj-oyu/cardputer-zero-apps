//! 言語サーバーの表（docs/lsp-design.md §言語の足し方）。
//!
//! 橋渡し（[`super`]）はどの言語でも同じで、言語ごとに違うところだけをここの [`Spec`] に書く:
//! 起動のしかた・受け持つ拡張子・準備ができたと見なす合図・設定（`initializationOptions` と
//! `workspace/configuration` の返事）・起動の前に木の外へ置くもの（仮の crate・`tsconfig.json`・
//! `compile_flags.txt`）・環境変数・サーバーが木の中に作るので消さないもの（`keep`）。
//!
//! **新しい言語の足し方**: (1) ここに `Spec` を 1 つ書いて [`SPECS`] に入れる、(2) ビルド係の
//! イメージ（`remote/container/Dockerfile`）にサーバーを入れる、(3) 実機側の
//! `zc_remote_ipc::RemoteLang::has_lsp` を `true` にする、(4) `tests/lsp.rs` に本物のサーバーでの
//! 試験を 1 つ足す。zcode・zc-remote・Worker・約束（zc-wire / IPC）は変えなくてよい。
//!
//! 言語は問い合わせのパスの拡張子で決める（約束に言語の欄は無い。古い zc-remote とも話せる）。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use zc_wire::tree::Manifest;

/// 準備ができたと見なす合図。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ready {
    /// rust-analyzer の `experimental/serverStatus` の `quiescent`（最初の索引が済むまで空の答えを
    /// 返すため、それまでは「準備中」と返す）。
    ServerStatus,
    /// `initialize` の返事が来たら（ほかのサーバーは読み込みが済むまで問い合わせを待たせるか、
    /// 読めた分で答える）。
    Initialized,
}

/// 起動の前の準備に渡すもの。
pub struct Setup<'a> {
    /// lsp の木（`proj/<id>/lsp`。サーバーの作業フォルダと `rootUri`）。
    pub root: &'a Path,
    /// プロジェクトの置き場（`proj/<id>`。木の外に置くものはここへ）。
    pub pdir: &'a Path,
    pub manifest: &'a Manifest,
}

/// 準備の結果。
#[derive(Default)]
pub struct Prepared {
    /// `initialize` の `initializationOptions`。
    pub init_options: Value,
    /// `workspace/configuration` で聞かれたら返す設定（`section` の点区切りでたどる）。
    pub settings: Value,
    /// サーバーに足す環境変数。
    pub env: Vec<(String, String)>,
}

/// 言語サーバー 1 種類の決めごと。
pub struct Spec {
    /// 表示とログに使う名前（実機のステータスバーにも出る）。
    pub name: &'static str,
    /// PATH で探す実行ファイル。
    pub program: &'static str,
    pub args: &'static [&'static str],
    /// 受け持つ拡張子（didOpen / didChange で中身を渡すファイル）。
    pub exts: &'static [&'static str],
    pub ready: Ready,
    pub prepare: fn(&Setup<'_>) -> Result<Prepared, String>,
    /// サーバーが木の根に作るので、木を目録にそろえるときに消さない名前（`_build` など）。
    /// 消すと問い合わせのたびに作り直しになる（moon-lsp は別のパッケージの型を読めなくなった）。
    pub keep: &'static [&'static str],
}

impl Spec {
    /// このサーバーが受け持つファイルか。
    pub fn handles(&self, rel: &str) -> bool {
        ext_of(rel).is_some_and(|e| self.exts.contains(&e))
    }
}

/// 全部の言語サーバー。拡張子が重ならないようにする（`spec_for` は最初に合ったものを返す）。
pub static SPECS: &[Spec] = &[RUST, PYTHON, TYPESCRIPT, GO, ZIG, CLANGD, BASH, RUBY, MOONBIT];

/// そのファイルを受け持つサーバー。
pub fn spec_for(rel: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.handles(rel))
}

/// 目録の中で最初に見つかる、サーバーのあるファイルのサーバー（パスの無い問い合わせのため）。
pub fn spec_for_manifest(manifest: &Manifest) -> Option<&'static Spec> {
    manifest.files().iter().find_map(|f| spec_for(&f.path))
}

fn ext_of(rel: &str) -> Option<&str> {
    let name = rel.rsplit('/').next()?;
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty()).then_some(ext)
}

/// LSP の `languageId`（拡張子から）。
pub fn language_id(rel: &str) -> &'static str {
    match ext_of(rel).unwrap_or("") {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "ts" | "mts" | "cts" => "typescript",
        "js" | "mjs" | "cjs" => "javascript",
        "go" => "go",
        "zig" | "zon" => "zig",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" => "cpp",
        "sh" | "bash" => "shellscript",
        "rb" => "ruby",
        "mbt" => "moonbit",
        _ => "plaintext",
    }
}

/// `workspace/configuration` の 1 項目の返事: `section`（`python.analysis` のような点区切り）で
/// `settings` をたどる。無ければ `null`、`section` が無ければ全部。
pub fn section_of(settings: &Value, section: Option<&str>) -> Value {
    let Some(section) = section.filter(|s| !s.is_empty()) else { return settings.clone() };
    let mut v = settings;
    for k in section.split('.') {
        match v.get(k) {
            Some(x) => v = x,
            None => return Value::Null,
        }
    }
    v.clone()
}

fn has_file(m: &Manifest, path: &str) -> bool {
    m.files().iter().any(|f| f.path == path)
}

fn write_if_changed(path: &Path, text: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(text) {
        return Ok(());
    }
    std::fs::write(path, text).map_err(|e| format!("{} を書けない: {e}", path.display()))
}

// ───────────────────────── Rust（rust-analyzer） ─────────────────────────

const RUST: Spec = Spec { name: "rust-analyzer", program: "rust-analyzer", args: &[], exts: &["rs"], ready: Ready::ServerStatus, prepare: prepare_rust, keep: &[] };

/// rust-analyzer の設定（`initializationOptions` と `workspace/configuration` の返事）。
/// 理由は docs/measurements.md §13。
pub fn ra_config(target_dir: &Path, linked: Option<&Path>) -> Value {
    let mut v = json!({
        // zc-buildd の check があるので 2 重にしない
        "checkOnSave": false,
        // build script・proc-macro の出力は別の target（ロックを取り合わない）
        "cargo": { "targetDir": target_dir },
        // 変わったファイルはこちらから知らせる（inotify を使わない）
        "files": { "watcher": "client" },
        // 診断は zc-buildd の check（cargo）が出す
        "diagnostics": { "enable": false },
        // 実機の小窓は 1 行の文字列を入れるだけ（スニペット・別の行の編集は使わない）
        "completion": { "autoimport": { "enable": false }, "callable": { "snippets": "none" }, "postfix": { "enable": false }, "limit": 100 },
    });
    if let Some(p) = linked {
        v["linkedProjects"] = json!([p]);
    }
    v
}

fn prepare_rust(s: &Setup<'_>) -> Result<Prepared, String> {
    let target = s.pdir.join("lsp-target");
    let _ = std::fs::create_dir_all(&target);
    // Cargo.toml の無い「1 ファイルだけ」: zc-buildd のジョブと同じ仮の crate を、木の外に作る
    let linked = if has_file(s.manifest, "Cargo.toml") {
        None
    } else {
        let entry = s.manifest.files().iter().find(|f| f.path.ends_with(".rs")).ok_or("Rust のファイルが無い")?;
        let dir = s.pdir.join("lsp-single");
        std::fs::create_dir_all(&dir).map_err(|e| format!("仮の crate を作れない: {e}"))?;
        let toml = format!(
            "# zc-buildd が rust-analyzer のために作った仮の crate\n[package]\nname = \"zc-single\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[[bin]]\nname = \"main\"\npath = {}\n\n[workspace]\n",
            serde_json::to_string(&s.root.join(&entry.path).to_string_lossy()).expect("JSON にできる")
        );
        let m = dir.join("Cargo.toml");
        write_if_changed(&m, &toml)?;
        Some(m)
    };
    let config = ra_config(&target, linked.as_deref());
    Ok(Prepared { init_options: config.clone(), settings: json!({ "rust-analyzer": config }), env: vec![] })
}

// ───────────────────────── Python（pyright） ─────────────────────────

const PYTHON: Spec = Spec { name: "pyright", program: "pyright-langserver", args: &["--stdio"], exts: &["py", "pyi"], ready: Ready::Initialized, prepare: prepare_python, keep: &[] };

fn prepare_python(_: &Setup<'_>) -> Result<Prepared, String> {
    // 診断は zc-buildd の check（pyright）が出すので、開いたファイルだけ見させる（木全体を解析しない）
    let settings = json!({ "python": { "analysis": { "diagnosticMode": "openFilesOnly", "autoSearchPaths": true, "useLibraryCodeForTypes": true, "typeCheckingMode": "standard" } } });
    Ok(Prepared { settings, ..Default::default() })
}

// ───────────────────────── JavaScript / TypeScript（TypeScript 7 の tsc） ─────────────────────────

/// TypeScript 7（Go 版）の `tsc` は `--lsp --stdio` で言語サーバーになる（イメージの check と同じもの）。
const TYPESCRIPT: Spec = Spec { name: "tsc", program: "tsc", args: &["--lsp", "--stdio"], exts: &["ts", "mts", "cts", "js", "mjs", "cjs"], ready: Ready::Initialized, prepare: prepare_typescript, keep: &[] };

fn prepare_typescript(s: &Setup<'_>) -> Result<Prepared, String> {
    // tsconfig.json が無ければ、check と同じ既定（runner::node::TSC_DEFAULTS）を木の外（親）に置く。
    // tsc はファイルから上へ tsconfig.json を探すので、木の中にあればそちらが先に見つかる。
    // npm の依存（`<project_dir>/node_modules`）も木の親なので、そのまま見つかる
    let p = s.pdir.join("tsconfig.json");
    if has_file(s.manifest, "tsconfig.json") || has_file(s.manifest, "jsconfig.json") {
        let _ = std::fs::remove_file(&p);
    } else {
        let cfg = json!({
            "compilerOptions": { "strict": true, "target": "es2022", "module": "esnext", "moduleResolution": "bundler", "skipLibCheck": true, "allowJs": true, "noEmit": true },
            "include": ["lsp"],
        });
        write_if_changed(&p, &serde_json::to_string_pretty(&cfg).expect("JSON にできる"))?;
    }
    Ok(Prepared::default())
}

// ───────────────────────── Go（gopls） ─────────────────────────

const GO: Spec = Spec { name: "gopls", program: "gopls", args: &["serve"], exts: &["go"], ready: Ready::Initialized, prepare: prepare_go, keep: &[] };

fn prepare_go(_: &Setup<'_>) -> Result<Prepared, String> {
    // ビルドと同じ対象（実機）で見る（build タグ・cgo の有無で見えるファイルが変わる）。
    // go.mod の無い 1 ファイルは gopls が「command-line-arguments」として扱う
    let env = [("GOOS", "linux"), ("GOARCH", "arm64"), ("CGO_ENABLED", "0"), ("GOFLAGS", "-mod=mod")].map(|(k, v)| (k.to_string(), v.to_string())).to_vec();
    let settings = json!({ "gopls": { "ui.diagnostic.staticcheck": false, "linksInHover": false, "hoverKind": "FullDocumentation" } });
    Ok(Prepared { settings, env, ..Default::default() })
}

// ───────────────────────── Zig（zls） ─────────────────────────

const ZIG: Spec = Spec { name: "zls", program: "zls", args: &[], exts: &["zig", "zon"], ready: Ready::Initialized, prepare: prepare_zig, keep: &[".zig-cache", "zig-out"] };

fn prepare_zig(_: &Setup<'_>) -> Result<Prepared, String> {
    // 診断は zc-buildd の check が出す。build.zig を走らせて診断を集める機能は使わない（重い）
    let settings = json!({ "zls": { "enable_build_on_save": false, "enable_autofix": false, "enable_snippets": false, "enable_argument_placeholders": false } });
    Ok(Prepared { init_options: settings["zls"].clone(), settings, ..Default::default() })
}

// ───────────────────────── C / C++（clangd） ─────────────────────────

const CLANGD: Spec = Spec {
    name: "clangd",
    program: "clangd",
    args: &["--log=error", "--header-insertion=never", "--function-arg-placeholders=0", "-j=1", "--pch-storage=memory"],
    exts: &["c", "h", "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx"],
    ready: Ready::Initialized,
    prepare: prepare_clangd,
    keep: &[".cache"],
};

fn prepare_clangd(s: &Setup<'_>) -> Result<Prepared, String> {
    // compile_commands.json / compile_flags.txt が木に無ければ、木の外（親）に compile_flags.txt を置く
    // （clangd はファイルから上へ探す）。言語は C++ のファイルがあれば C++、無ければ C（.h を C として読む）。
    // 対象は実機と同じ aarch64（ビルドと同じ。clang がクロスの gcc の見出しを見つける。イメージには
    // x86_64 の C++ の標準ライブラリの見出しが無く、`<cstdio>` で解析が止まるため）
    let p = s.pdir.join("compile_flags.txt");
    if has_file(s.manifest, "compile_commands.json") || has_file(s.manifest, "compile_flags.txt") {
        let _ = std::fs::remove_file(&p);
    } else {
        let cpp = s.manifest.files().iter().any(|f| matches!(ext_of(&f.path), Some("cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx")));
        let flags = if cpp { "-xc++\n-std=gnu++20\n" } else { "-xc\n-std=gnu17\n" };
        write_if_changed(&p, &format!("--target=aarch64-linux-gnu\n{flags}-I{}\n", s.root.display()))?;
    }
    Ok(Prepared::default())
}

// ───────────────────────── Shell（bash-language-server） ─────────────────────────

const BASH: Spec = Spec { name: "bash-language-server", program: "bash-language-server", args: &["start"], exts: &["sh", "bash"], ready: Ready::Initialized, prepare: prepare_bash, keep: &[] };

fn prepare_bash(_: &Setup<'_>) -> Result<Prepared, String> {
    // 診断（shellcheck）は zc-buildd の check が出すので、こちらでは回さない
    let settings = json!({ "bashIde": { "shellcheckPath": "", "backgroundAnalysisMaxFiles": 200, "enableSourceErrorDiagnostics": false } });
    Ok(Prepared { init_options: json!({}), settings, ..Default::default() })
}

// ───────────────────────── Ruby（ruby-lsp） ─────────────────────────

/// `ruby-lsp` の実行ファイルは、起動のたびに作業フォルダへ `.ruby-lsp/`（Gemfile の合成）を作って
/// `bundle exec` し直す（ネットワークと木への書き込みが要る）。それを飛ばして、本体を直接動かす
/// （イメージの gem だけを使う。プロジェクトの Gemfile の gem は読まない）。
///
/// 確かめたこと（0.26.11）: Gemfile が無いと索引を作らない（「Could not locate Gemfile」）ので、
/// 木の外に空の Gemfile を置いて `BUNDLE_GEMFILE` で渡す。クラス・モジュールの中のメソッドと定数は
/// 追えるが、トップレベルの `def` の呼び出しは hover・定義へとも答えない（ruby-lsp の作り）。
/// `tmp` という名前のフォルダの下は既定で索引から外す（置き場は `/var/lib/zc-buildd` なので当たらない）。
const RUBY: Spec = Spec { name: "ruby-lsp", program: "ruby", args: &["-rruby_lsp/internal", "-e", "$stdin.binmode; $stdout.binmode; $stdout.sync = true; RubyLsp::Server.new.start"], exts: &["rb"], ready: Ready::Initialized, prepare: prepare_ruby, keep: &[".ruby-lsp"] };

fn prepare_ruby(s: &Setup<'_>) -> Result<Prepared, String> {
    let dir = s.pdir.join("ruby-lsp");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{} を作れない: {e}", dir.display()))?;
    let gemfile = dir.join("Gemfile");
    write_if_changed(&gemfile, "# zc-buildd が ruby-lsp のために置いた空の Gemfile（依存の gem は読まない）\nsource \"https://rubygems.org\"\n")?;
    // 診断・整形は使わない（check は `ruby -wc`）
    let init = json!({ "enabledFeatures": { "diagnostics": false, "formatting": false, "onTypeFormatting": false, "codeActions": false, "semanticHighlighting": false, "inlayHint": false }, "formatter": "none", "linters": [] });
    Ok(Prepared { init_options: init, env: vec![("BUNDLE_GEMFILE".into(), gemfile.to_string_lossy().into_owned())], ..Default::default() })
}

// ───────────────────────── MoonBit（moon-lsp） ─────────────────────────

const MOONBIT: Spec = Spec { name: "moon-lsp", program: "moon-lsp", args: &[], exts: &["mbt"], ready: Ready::Initialized, prepare: |_| Ok(Prepared::default()), keep: &["_build", ".mooncakes"] };

// ───────────────────────── プロジェクトの外の場所 ─────────────────────────

/// プロジェクトの外の場所（標準ライブラリ・依存）の見せ方。実機にファイルは無いので、どこの何かが
/// 分かる短い形にする: crates.io の crate は `<名前>-<版>/…`、Rust の std は `core/src/…`、
/// Go のモジュールは `<モジュール>@<版>/…`、Go の標準は `fmt/print.go`、npm は `<パッケージ>/…`、
/// typeshed は `stdlib/os/__init__.pyi`、gem は `<名前>-<版>/…`、ほかは末尾の 3 つ。
pub fn external_label(p: &Path) -> String {
    let s = p.to_string_lossy();
    // （探す印, 印の後ろをいくつの要素で切るか: None = 全部）
    const AFTER: &[&str] = &[
        "/lib/rustlib/src/rust/library/",
        "/pkg/mod/",
        "/opt/go/src/",
        "/typeshed-fallback/",
        "/node_modules/",
        "/gems/",
        "/opt/zig/lib/",
        "/opt/moon/lib/",
        "/usr/include/",
    ];
    if let Some(i) = s.find("/registry/src/") {
        // registry/src/<index>-<hash>/<crate>-<版>/…
        let after = &s[i + "/registry/src/".len()..];
        if let Some(j) = after.find('/') {
            return after[j + 1..].to_string();
        }
    }
    if let Some(i) = s.find("/git/checkouts/") {
        // git/checkouts/<名前>-<hash>/<rev>/…
        let parts: Vec<&str> = s[i + "/git/checkouts/".len()..].splitn(3, '/').collect();
        if parts.len() == 3 {
            return format!("{}/{}", parts[0], parts[2]);
        }
    }
    for m in AFTER {
        // node_modules は入れ子があるので最後のもの
        let at = if *m == "/node_modules/" { s.rfind(m) } else { s.find(m) };
        if let Some(i) = at {
            let rest = &s[i + m.len()..];
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    let parts: Vec<&str> = s.rsplitn(4, '/').collect();
    let n = parts.len().min(3);
    parts[..n].iter().rev().copied().collect::<Vec<_>>().join("/")
}

/// サーバーを PATH で探す。
pub fn find(spec: &Spec) -> Option<PathBuf> {
    crate::runner::find_in_path(spec.program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_extension_has_one_server() {
        let mut seen = std::collections::HashMap::new();
        for s in SPECS {
            for e in s.exts {
                assert!(seen.insert(*e, s.name).is_none(), "拡張子 {e} が 2 つのサーバーにある");
                assert_ne!(language_id(&format!("a.{e}")), "plaintext", "{e} の languageId が無い");
            }
        }
        assert_eq!(spec_for("src/main.rs").unwrap().name, "rust-analyzer");
        assert_eq!(spec_for("a/b.mts").unwrap().name, "tsc");
        assert_eq!(spec_for("x.h").unwrap().name, "clangd");
        assert!(spec_for("README.md").is_none());
        assert!(spec_for(".rs").is_none());
        assert!(spec_for("Makefile").is_none());
    }

    #[test]
    fn configuration_sections() {
        let s = json!({ "python": { "analysis": { "diagnosticMode": "openFilesOnly" } } });
        assert_eq!(section_of(&s, Some("python.analysis")), json!({ "diagnosticMode": "openFilesOnly" }));
        assert_eq!(section_of(&s, Some("python.analysis.diagnosticMode")), json!("openFilesOnly"));
        assert_eq!(section_of(&s, Some("pyright")), Value::Null);
        assert_eq!(section_of(&s, None), s);
        assert_eq!(section_of(&s, Some("")), s);
    }

    #[test]
    fn external_labels() {
        assert_eq!(external_label(Path::new("/opt/rustup/toolchains/1.100.0-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/option.rs")), "core/src/option.rs");
        assert_eq!(external_label(Path::new("/var/lib/zc-buildd/cargo-home/registry/src/index.crates.io-1949cf8c6b5b557f/regex-1.11.1/src/lib.rs")), "regex-1.11.1/src/lib.rs");
        assert_eq!(external_label(Path::new("/var/lib/zc-buildd/go/pkg/mod/github.com/google/uuid@v1.6.0/uuid.go")), "github.com/google/uuid@v1.6.0/uuid.go");
        assert_eq!(external_label(Path::new("/opt/go/src/fmt/print.go")), "fmt/print.go");
        assert_eq!(external_label(Path::new("/opt/tools/node_modules/pyright/dist/typeshed-fallback/stdlib/os/__init__.pyi")), "stdlib/os/__init__.pyi");
        assert_eq!(external_label(Path::new("/var/lib/zc-buildd/proj/x/node_modules/a/node_modules/nanoid/index.d.ts")), "nanoid/index.d.ts");
        assert_eq!(external_label(Path::new("/a/b/c/d/e.rs")), "c/d/e.rs");
    }

    #[test]
    fn extension_of_a_path() {
        assert_eq!(ext_of("src/a.b/c.rs"), Some("rs"));
        assert_eq!(ext_of("src/a.b/c"), None);
        assert_eq!(ext_of(".bashrc"), None);
    }
}
