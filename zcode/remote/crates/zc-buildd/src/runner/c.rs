//! C と C++ のジョブ（docs/stage3-design.md §4.4）。課題 B4。C++ は 2026-11-17 に足した
//! （[`run_cpp`]。同じ流れで、道具が `aarch64-linux-gnu-g++`、ソースが `*.cpp` `*.cc` `*.cxx` `*.c++`）。
//!
//! 木は、ファイルのあるフォルダの `*.c` と `*.h`（下のフォルダは無い）。`opts.entry` は
//! F5 を押したファイル。
//!
//! | 種類 | 中でやること |
//! |---|---|
//! | build | `aarch64-linux-gnu-gcc -mcpu=cortex-a53 -O2 -Wall -Wextra -fdiagnostics-format=json -o <out>/<entry の名前> *.c -lm`。**`main` を持つ .c が複数あるときは `entry` だけ**をコンパイルする（`main` の有無は `\bmain\s*\(` の簡易判定） |
//! | check | 同じ引数で `-fsyntax-only` |
//! | test | 無い（`error`: 「C の試験は未対応」） |
//!
//! 診断は gcc の JSON（`-fdiagnostics-format=json`。`locations[0].caret` の `line`・
//! `byte-column`（1 始まりのバイト）を使う）。`kind` が error / warning のものだけ。
//!
//! B4 で決めた細かいところ:
//!
//! - `main` を持つ .c が複数あるときは、`entry` と、**`main` を持たない .c**（共通の関数）を
//!   コンパイルする（`entry` だけだと、分けて書いた関数がリンクできない）
//! - gcc 14 の JSON は複数行にわたる 1 つの配列で、標準エラーに出る。標準エラーは全部溜めて
//!   （上限つき）、JSON の配列を読んだ残り（リンカの `undefined reference` など）だけを
//!   そのままログに流す。診断はログにも `ファイル:行:桁: error: …` の形で出す
//! - リンクの失敗のように JSON にならない失敗は、ログの error の行を位置なしの診断にする
//! - 実行ファイルは `<project_dir>/c-out/` に作ってから仕上げる（成果物のフォルダには
//!   圧縮したものだけを置く）

use super::{
    check_cancelled, failure_line, finish_diags, has_error, io_failure, note_diag, push_diag, rel_to_root, safe_entry, spawn_failure, spawn_tool, tool_env, truncate_utf8, which, JobCtx,
    JobFailure, JobOutput, JobResult, Tap,
};
use crate::pack::pack;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use zc_wire::msg::{ArtifactKind, Diag, FileDiags, Kind, Severity, Status};

/// クロスの gcc。
pub const GCC: &str = "aarch64-linux-gnu-gcc";
/// クロスの g++。
pub const GXX: &str = "aarch64-linux-gnu-g++";
/// 共通の引数。
pub const CFLAGS: &[&str] = &["-mcpu=cortex-a53", "-O2", "-Wall", "-Wextra", "-fdiagnostics-format=json"];

/// 言語ごとの違い（C と C++）。
struct Flavor {
    /// 人に見せる名前。
    label: &'static str,
    compiler: &'static str,
    /// コンパイルするファイルの拡張子（ヘッダは目録には入るが、ここには入れない）。
    sources: &'static [&'static str],
}

const C: Flavor = Flavor { label: "C", compiler: GCC, sources: &[".c"] };
const CPP: Flavor = Flavor { label: "C++", compiler: GXX, sources: &[".cpp", ".cc", ".cxx", ".c++"] };

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    run_with(ctx, out, &C)
}

/// C++（`aarch64-linux-gnu-g++`）。引数・`main` の扱い・診断は C と同じ。
pub fn run_cpp(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    run_with(ctx, out, &CPP)
}

fn run_with(ctx: &JobCtx<'_>, out: &dyn JobOutput, fl: &Flavor) -> Result<JobResult, JobFailure> {
    if ctx.kind == Kind::Test {
        return Err(JobFailure { status: Status::Error, message: format!("{} の試験は未対応", fl.label) });
    }
    let gcc = which(fl.compiler)?;
    let src = ctx.src.canonicalize().map_err(|e| io_failure("作業の木が無い", e))?;

    // 直下の .c（名前の順）
    let mut sources: Vec<String> = std::fs::read_dir(&src)
        .map_err(|e| io_failure("作業の木を読めない", e))?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| fl.sources.iter().any(|e| n.ends_with(e)))
        .collect();
    sources.sort();
    if sources.is_empty() {
        return Ok(failed(format!("{} のファイルが無い", fl.label)));
    }
    let entry = match ctx.opts.entry.as_deref() {
        Some(e) if safe_entry(e) && sources.iter().any(|s| s == e) => Some(e.to_string()),
        Some(e) => return Ok(failed(format!("入口のファイルが無い: {e}"))),
        None if sources.len() == 1 => Some(sources[0].clone()),
        None => None,
    };
    let with_main: Vec<&String> = sources.iter().filter(|s| std::fs::read_to_string(src.join(s)).map(|t| has_main(&t)).unwrap_or(false)).collect();
    let files: Vec<String> = if with_main.len() > 1 {
        let Some(entry) = &entry else { return Ok(failed(format!("main を持つ {} のファイルが複数ある。入口のファイルを指定する", fl.label))) };
        sources.iter().filter(|s| *s == entry || !with_main.contains(s)).cloned().collect()
    } else {
        sources.clone()
    };
    let exe_name = entry
        .as_deref()
        .or(with_main.first().map(|s| s.as_str()))
        .unwrap_or(&sources[0])
        .to_string();
    let exe_name = match fl.sources.iter().find(|e| exe_name.ends_with(*e)) {
        Some(e) => exe_name[..exe_name.len() - e.len()].to_string(),
        None => exe_name,
    };

    let mut cmd = Command::new(&gcc);
    cmd.current_dir(&src);
    tool_env(&mut cmd);
    cmd.args(CFLAGS);
    let exe_dir = ctx.project_dir.join("c-out");
    let exe_path = exe_dir.join(&exe_name);
    match ctx.kind {
        Kind::Build => {
            std::fs::create_dir_all(&exe_dir).map_err(|e| io_failure("出力のフォルダを作れない", e))?;
            let _ = std::fs::remove_file(&exe_path);
            cmd.arg("-o").arg(&exe_path);
        }
        _ => {
            cmd.arg("-fsyntax-only");
        }
    }
    // `-` で始まる名前がオプションに読まれないように `./` を付ける
    cmd.args(files.iter().map(|f| format!("./{f}")));
    if ctx.kind == Kind::Build {
        cmd.arg("-lm");
    }

    check_cancelled(ctx)?;
    // 標準エラー（JSON）は溜める。標準出力は無いはず
    let tap = Tap::capturing(out);
    let exit = spawn_tool(cmd, &tap, &mut |_| true).map_err(|e| spawn_failure(fl.compiler, e))?;
    check_cancelled(ctx)?;

    let captured = tap.take();
    let mut diags = Vec::new();
    let rest = parse_gcc_output(&captured, &src, &mut diags);
    // 人が読む形でログへ
    for f in &diags {
        for d in &f.diags {
            let sev = if d.severity == Severity::Error { "error" } else { "warning" };
            out.log(format!("{}:{}:{}: {sev}: {}\n", f.path, d.line + 1, d.col.map(|c| c + 1).unwrap_or(0), d.message).as_bytes());
        }
    }
    out.log(rest.as_bytes());
    if exit != 0 && !has_error(&diags) {
        let msg = failure_line(&rest).unwrap_or_else(|| format!("{} が失敗した（終了コード {exit}）", fl.compiler));
        push_diag(&mut diags, "", note_diag(Severity::Error, msg));
    }

    let mut result = JobResult { exit, complete: exit < 128, ..Default::default() };
    if ctx.kind == Kind::Build && exit == 0 {
        let packed = pack(&exe_path, ArtifactKind::Exe, &exe_name, ctx.out_dir).map_err(|e| JobFailure { status: Status::Error, message: e.0 })?;
        result.artifact = Some(packed);
    }
    result.diagnostics = finish_diags(diags);
    Ok(result)
}

fn failed(msg: impl Into<String>) -> JobResult {
    JobResult { exit: 1, complete: true, diagnostics: vec![FileDiags { path: String::new(), diags: vec![note_diag(Severity::Error, msg)] }], ..Default::default() }
}

/// `\bmain\s*\(` の簡易判定（コメントや文字列の中も数える）。
fn has_main(text: &str) -> bool {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(i) = text[from..].find("main") {
        let at = from + i;
        let before_ok = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_');
        let mut j = at + 4;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if before_ok && j < b.len() && b[j] == b'(' {
            return true;
        }
        from = at + 4;
    }
    false
}

/// gcc の標準エラーから JSON の配列を探して読み、診断を `into` に足す。JSON 以外の部分
/// （リンカのメッセージなど）を返す。
fn parse_gcc_output(stderr: &str, src_root: &Path, into: &mut Vec<FileDiags>) -> String {
    let mut rest = String::new();
    let mut pos = 0;
    while pos < stderr.len() {
        let line_end = stderr[pos..].find('\n').map(|i| pos + i + 1).unwrap_or(stderr.len());
        let line = &stderr[pos..line_end];
        if line.starts_with('[') {
            let mut it = serde_json::Deserializer::from_str(&stderr[pos..]).into_iter::<Value>();
            if let Some(Ok(v)) = it.next() {
                diags_from_json(&v, src_root, into);
                pos += it.byte_offset();
                // 配列の後ろの改行
                if stderr[pos..].starts_with('\n') {
                    pos += 1;
                }
                continue;
            }
        }
        rest.push_str(line);
        pos = line_end;
    }
    rest
}

/// gcc の JSON の診断（標準エラーに出る 1 つの配列）を読む。
pub fn parse_gcc_json(stderr: &str, src_root: &std::path::Path, into: &mut Vec<FileDiags>) {
    let _ = parse_gcc_output(stderr, src_root, into);
}

fn diags_from_json(v: &Value, src_root: &Path, into: &mut Vec<FileDiags>) {
    let Some(arr) = v.as_array() else { return };
    for d in arr {
        let severity = match d.get("kind").and_then(Value::as_str) {
            Some("error") | Some("fatal error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => continue,
        };
        let mut message = d.get("message").and_then(Value::as_str).unwrap_or("").to_string();
        let code = d.get("option").and_then(Value::as_str).map(str::to_string);
        if let Some(children) = d.get("children").and_then(Value::as_array) {
            for c in children {
                if let (Some(k), Some(m)) = (c.get("kind").and_then(Value::as_str), c.get("message").and_then(Value::as_str)) {
                    message.push_str(&format!(" / {k}: {m}"));
                }
            }
        }
        let message = truncate_utf8(&message, 400);
        let loc = d.get("locations").and_then(Value::as_array).and_then(|a| a.first());
        let caret = loc.and_then(|l| l.get("caret"));
        let path = caret.and_then(|c| c.get("file")).and_then(Value::as_str).and_then(|f| rel_to_root(src_root, src_root, f));
        match (path, caret) {
            (Some(path), Some(caret)) => {
                let line = caret.get("line").and_then(Value::as_u64).unwrap_or(1).max(1) as u32 - 1;
                let col = caret.get("byte-column").and_then(Value::as_u64).map(|c| c.saturating_sub(1) as u32);
                let end = loc.and_then(|l| l.get("finish")).and_then(|f| {
                    let l = f.get("line")?.as_u64()?.max(1) as u32 - 1;
                    // finish は最後の字の位置（含む）。終わりは、その次のバイト
                    let c = f.get("byte-column")?.as_u64()? as u32;
                    Some((l, c))
                });
                push_diag(into, &path, Diag { line, col, end, severity, code, message });
            }
            _ => push_diag(into, "", Diag { line: 0, col: None, end: None, severity, code, message }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{skip_without, Collect, TempDir};
    use super::*;
    use std::sync::atomic::AtomicBool;
    use zc_wire::msg::JobOpts;

    /// gcc 14.2（開発コンテナ）の実物を縮めたもの。
    const GCC_JSON: &str = r#"[{"kind": "error",
  "message": "initialization of 'int' from 'char *' makes integer from pointer without a cast",
  "option": "-Wint-conversion",
  "children": [],
  "column-origin": 1,
  "locations": [{"caret": {"file": "c.c",
                           "line": 3,
                           "display-column": 26,
                           "byte-column": 26,
                           "column": 26},
                 "finish": {"file": "c.c",
                            "line": 3,
                            "display-column": 28,
                            "byte-column": 28,
                            "column": 28}}],
  "escape-source": false},
 {"kind": "warning",
  "message": "unused variable 'x'",
  "option": "-Wunused-variable",
  "children": [],
  "column-origin": 1,
  "locations": [{"caret": {"file": "/elsewhere/h.h",
                           "line": 1,
                           "byte-column": 22,
                           "column": 22}}],
  "escape-source": false}]
/usr/bin/ld: x.o: undefined reference to `g'
"#;

    #[test]
    fn gcc_json_is_read() {
        let mut v = Vec::new();
        let rest = parse_gcc_output(GCC_JSON, Path::new("/w/src"), &mut v);
        assert_eq!(rest, "/usr/bin/ld: x.o: undefined reference to `g'\n");
        let v = finish_diags(v);
        let c = v.iter().find(|f| f.path == "c.c").unwrap();
        assert_eq!(c.diags[0].line, 2);
        assert_eq!(c.diags[0].col, Some(25));
        assert_eq!(c.diags[0].end, Some((2, 28)));
        assert_eq!(c.diags[0].code.as_deref(), Some("-Wint-conversion"));
        // 木の外のヘッダは位置なし
        let none = v.iter().find(|f| f.path.is_empty()).unwrap();
        assert_eq!(none.diags[0].severity, Severity::Warning);
        assert!(has_main("int main (void)") && has_main("x;\nint\nmain(") && !has_main("int domain(void)") && !has_main("main_loop()"));
    }

    struct Job {
        _t: TempDir,
        src: std::path::PathBuf,
        project: std::path::PathBuf,
        out: std::path::PathBuf,
    }

    fn job(files: &[(&str, &str)]) -> Job {
        let t = TempDir::new("c");
        let src = t.path().join("proj/src");
        std::fs::create_dir_all(&src).unwrap();
        for (p, text) in files {
            std::fs::write(src.join(p), text).unwrap();
        }
        let out = t.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        Job { project: t.path().join("proj"), src, out, _t: t }
    }

    fn run_job(j: &Job, kind: Kind, entry: Option<&str>) -> (Result<JobResult, JobFailure>, Collect) {
        let cancelled = AtomicBool::new(false);
        let opts = JobOpts { entry: entry.map(str::to_string), ..Default::default() };
        let ctx = JobCtx { job: "j", kind, lang: zc_wire::msg::Lang::C, opts: &opts, src: &j.src, project_dir: &j.project, out_dir: &j.out, cancelled: &cancelled };
        let out = Collect::default();
        (run(&ctx, &out), out)
    }

    #[test]
    fn e2e_check_errors_warnings_and_header() {
        if skip_without(&[GCC]) {
            return;
        }
        let j = job(&[
            ("a.c", "#include \"h.h\"\n// 日本語\nint main(void) { int x = /* 日本 */ \"s\"; int unused; return f(); }\n"),
            ("h.h", "static int f(void) { return \"x\"; }\n"),
        ]);
        let (r, out) = run_job(&j, Kind::Check, Some("a.c"));
        let r = r.unwrap();
        assert_ne!(r.exit, 0);
        let a = r.diagnostics.iter().find(|f| f.path == "a.c").unwrap();
        let e = a.diags.iter().find(|d| d.severity == Severity::Error).unwrap();
        assert_eq!(e.line, 2);
        // gcc はバイトで数える: 「"s"」の位置
        let line = "int main(void) { int x = /* 日本 */ \"s\"; int unused; return f(); }";
        assert_eq!(e.col, Some(line.find("\"s\"").unwrap() as u32));
        assert!(a.diags.iter().any(|d| d.severity == Severity::Warning && d.message.contains("unused")));
        // ヘッダの中のエラーはヘッダに付く
        let h = r.diagnostics.iter().find(|f| f.path == "h.h").unwrap();
        assert_eq!(h.diags[0].line, 0);
        // ログは人が読む形（JSON はそのまま出さない）
        assert!(out.text().contains("a.c:3:"), "{}", out.text());
        assert!(!out.text().contains("\"kind\""));
    }

    #[test]
    fn e2e_build_hello_and_two_mains() {
        if skip_without(&[GCC, "aarch64-linux-gnu-strip", "zstd", "file"]) {
            return;
        }
        let j = job(&[
            ("hello.c", "#include <stdio.h>\n#include \"util.h\"\nint main(void) { printf(\"%d\\n\", twice(2)); return 0; }\n"),
            ("other.c", "int main(void) { return 1; }\n"),
            ("util.c", "int twice(int x) { return x * 2; }\n"),
            ("util.h", "int twice(int x);\n"),
        ]);
        // main が 2 つ・入口なし → 断る
        let r = run_job(&j, Kind::Build, None).0.unwrap();
        assert_eq!(r.exit, 1);
        assert!(r.diagnostics[0].diags[0].message.contains("main を持つ C のファイルが複数"));
        // 入口あり → hello.c と util.c だけ
        let (r, out) = run_job(&j, Kind::Build, Some("hello.c"));
        let r = r.unwrap();
        assert_eq!(r.exit, 0, "{}", out.text());
        let a = r.artifact.unwrap();
        assert_eq!(a.name, "hello");
        let back = j.out.join("back");
        assert!(Command::new("zstd").args(["-q", "-d", "-o"]).arg(&back).arg(&a.zpath).status().unwrap().success());
        let f = Command::new("file").arg(&back).output().unwrap();
        assert!(String::from_utf8_lossy(&f.stdout).contains("ARM aarch64"));
    }

    #[test]
    fn e2e_link_error_becomes_a_diagnostic() {
        if skip_without(&[GCC]) {
            return;
        }
        let j = job(&[("m.c", "int g(void);\nint main(void) { return g(); }\n")]);
        let (r, out) = run_job(&j, Kind::Build, None);
        let r = r.unwrap();
        assert_ne!(r.exit, 0);
        assert!(r.artifact.is_none());
        assert!(r.diagnostics[0].path.is_empty());
        assert!(r.diagnostics[0].diags[0].message.contains("undefined reference"), "{:?}", r.diagnostics);
        assert!(out.text().contains("undefined reference"));
    }

    /// g++ 14.2（ビルド係のイメージ）の実物（tests/data/langs/gxx.json）。形は gcc と同じ。
    #[test]
    fn gxx_json_is_read() {
        let text = include_str!("../../tests/data/langs/gxx.json");
        let mut v = Vec::new();
        let rest = parse_gcc_output(text, Path::new("/w/src"), &mut v);
        assert!(rest.trim().is_empty(), "{rest}");
        let v = finish_diags(v);
        let a = &v.iter().find(|f| f.path == "a.cpp").unwrap().diags;
        // `int main() { std::string s = /* 日本 */ 1.5; …`: 1.5 の位置（バイト）
        let line = "int main() { std::string s = /* 日本 */ 1.5; int unused; return s; }";
        assert_eq!((a[0].line, a[0].col, a[0].severity), (2, Some(line.find("1.5").unwrap() as u32), Severity::Error));
        assert!(a.iter().any(|d| d.severity == Severity::Warning && d.code.as_deref() == Some("-Wunused-variable")));
        assert!(a.iter().any(|d| d.col == Some(line.find("s; }").unwrap() as u32)));
    }

    fn run_cpp_job(j: &Job, kind: Kind, entry: Option<&str>) -> (Result<JobResult, JobFailure>, Collect) {
        let cancelled = AtomicBool::new(false);
        let opts = JobOpts { entry: entry.map(str::to_string), ..Default::default() };
        let ctx = JobCtx { job: "j", kind, lang: zc_wire::msg::Lang::Cpp, opts: &opts, src: &j.src, project_dir: &j.project, out_dir: &j.out, cancelled: &cancelled };
        let out = Collect::default();
        (run_cpp(&ctx, &out), out)
    }

    #[test]
    fn e2e_cpp_check_and_build() {
        if skip_without(&[GXX, "aarch64-linux-gnu-strip", "zstd", "file"]) {
            return;
        }
        let j = job(&[
            ("main.cpp", "#include <iostream>\n#include \"util.hpp\"\n// 日本語\nint main() { std::cout << twice(2) << \"\\n\"; return 0; }\n"),
            ("util.cc", "#include \"util.hpp\"\nint twice(int x) { return x * 2; }\n"),
            ("util.hpp", "int twice(int x);\n"),
            ("plain.c", "this is not compiled\n"),
        ]);
        let (r, out) = run_cpp_job(&j, Kind::Check, Some("main.cpp"));
        let r = r.unwrap();
        assert_eq!(r.exit, 0, "{}", out.text());
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        let (r, out) = run_cpp_job(&j, Kind::Build, Some("main.cpp"));
        let r = r.unwrap();
        assert_eq!(r.exit, 0, "{}", out.text());
        let a = r.artifact.unwrap();
        assert_eq!(a.name, "main");
        let back = j.out.join("back");
        assert!(Command::new("zstd").args(["-q", "-d", "-o"]).arg(&back).arg(&a.zpath).status().unwrap().success());
        let f = Command::new("file").arg(&back).output().unwrap();
        assert!(String::from_utf8_lossy(&f.stdout).contains("ARM aarch64"));
        // 誤り（CJK の後ろ）
        let j = job(&[("bad.cpp", "#include <string>\n// 日本語\nint main() { std::string s = /* 日本 */ 1.5; return 0; }\n")]);
        let r = run_cpp_job(&j, Kind::Check, Some("bad.cpp")).0.unwrap();
        assert_ne!(r.exit, 0);
        let d = &r.diagnostics.iter().find(|f| f.path == "bad.cpp").unwrap().diags[0];
        let line = "int main() { std::string s = /* 日本 */ 1.5; return 0; }";
        assert_eq!((d.line, d.col), (2, Some(line.find("1.5").unwrap() as u32)));
        assert!(run_cpp_job(&j, Kind::Test, None).0.unwrap_err().message.contains("C++ の試験は未対応"));
    }

    #[test]
    fn test_kind_is_unsupported() {
        let j = job(&[("m.c", "int main(void){return 0;}\n")]);
        let f = run_job(&j, Kind::Test, None).0.unwrap_err();
        assert_eq!(f.status, Status::Error);
        assert!(f.message.contains("未対応"));
    }
}
