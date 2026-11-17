//! Go のジョブ（docs/stage3-design.md §4.4）。2026-11-17 に足した。
//!
//! 木: 根に `go.mod` があればモジュール（パッケージは `./...`）、無ければ 1 ファイルだけ（`./<entry>`）。
//!
//! | 種類 | 中でやること |
//! |---|---|
//! | check | `GOOS=linux GOARCH=arm64 CGO_ENABLED=0 go vet -json <pkgs>`。型・構文の誤りがあれば（vet は 1 件しか言わない）`go build -o <project_dir>/go-check/ <pkgs>` で全部を集め直す |
//! | build | `GOOS=linux GOARCH=arm64 CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o <project_dir>/go-out/<名前> <pkg>`。`<pkg>` は F5 を押したファイルのフォルダ（1 ファイルならそのファイル） |
//! | test | `go test -json <pkgs>`（**x86_64 のまま**） |
//!
//! - モジュールでは `-mod=mod`（`GOFLAGS`）で回し、`go.sum` / `go.mod` を道具が直したら実機へ
//!   書き戻す（Rust の `Cargo.lock` と同じ）。依存は proxy.golang.org から取る（コンテナから外へ出る）
//! - `GOTOOLCHAIN=local`（イメージの ENV）: `go.mod` がもっと新しい Go を求めても、勝手に取ってこない
//!   （その旨の誤りになる）
//! - 実行ファイルの名前: 1 ファイルならその名前、モジュールならパッケージのフォルダの名前
//!   （根なら `module` の最後の要素）
//!
//! # 診断
//!
//! - `go build`・`go vet`（型の誤りは `vet: ` が頭に付く）の `パス:行:桁: 本文` は error。行・桁は
//!   1 始まり、**桁はバイト**。パスは作業フォルダ（木の根）からの相対（`./` 付きもある）。
//!   タブで始まる続きの行は前の 1 件に足す。`# パッケージ` の行は捨てる
//! - `go vet -json` の指摘（`{"パッケージ": {"解析器": [{"posn": "絶対パス:行:桁", "message"}]}}`）は
//!   warning（コンパイルは通る）。`code` は解析器の名前（`printf` など）
//! - `go test -json` の `build-output`（試験のビルドの失敗。go test も vet の一部を回す）は error
//!
//! # 試験（`go test -json` = test2json）
//!
//! - `Test` の付いた `pass` / `fail` / `skip` を 1 件ずつ数える（`skip` は ignored）
//! - 落ちた試験の出力の `    x_test.go:7: …` から場所を取る（パスはパッケージのフォルダからの相対。
//!   パッケージのフォルダは import パスから `module` を外したもの）
//! - 試験が 1 つも落ちていないのにパッケージが落ちた（panic・ビルドの失敗）ら、パッケージ名の 1 件を
//!   落ちたものとして足す

use super::{
    byte_col0, check_cancelled, failure_line, finish_diags, has_error, io_failure, line0, lines_from, note_diag, push_diag, rel_to_root, safe_entry, spawn_failure, spawn_tool, split_loc,
    tool_env, truncate_utf8, which, JobCtx, JobFailure, JobOutput, JobResult, LineSource, Tap, TestResults,
};
use crate::pack::pack;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use zc_wire::msg::{ArtifactKind, Diag, FileDiags, Kind, Severity, Status, TestCase};

/// 試験のメッセージの上限（rust と同じ）。
const MAX_TEST_MESSAGE: usize = 2 << 10;

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    let go = which("go")?;
    let src = ctx.src.canonicalize().map_err(|e| io_failure("作業の木が無い", e))?;
    std::fs::create_dir_all(ctx.project_dir).map_err(|e| io_failure("プロジェクトのフォルダを作れない", e))?;
    let pdir = ctx.project_dir.canonicalize().map_err(|e| io_failure("プロジェクトのフォルダが無い", e))?;
    let module = std::fs::read_to_string(src.join("go.mod")).ok().map(|t| module_path(&t).unwrap_or_default());
    let entry = match ctx.opts.entry.as_deref() {
        Some(e) if safe_entry(e) && src.join(e).is_file() => Some(e.to_string()),
        Some(e) => return Ok(super::failed_result(1, format!("入口のファイルが無い: {e}"))),
        None => None,
    };
    let all_pkgs = match (&module, &entry) {
        (Some(_), _) => "./...".to_string(),
        (None, Some(e)) => format!("./{e}"),
        (None, None) => return Ok(super::failed_result(1, "go.mod も入口のファイルも無い")),
    };
    // 道具が直すかもしれない lock の、もとの中身
    let before: Vec<(&str, Option<Vec<u8>>)> = ["go.mod", "go.sum"].iter().map(|n| (*n, std::fs::read(src.join(n)).ok())).collect();
    let cmd = |args: &[&str], cross: bool| {
        let mut c = Command::new(&go);
        c.current_dir(&src).args(args);
        if cross {
            c.env("GOOS", "linux").env("GOARCH", "arm64").env("CGO_ENABLED", "0");
        }
        if module.is_some() {
            c.env("GOFLAGS", "-mod=mod");
        }
        tool_env(&mut c);
        c
    };

    let mut result = JobResult { complete: true, ..Default::default() };
    let mut diags = Vec::new();
    let lines = lines_from(&src);
    match ctx.kind {
        Kind::Check => {
            check_cancelled(ctx)?;
            let tap = Tap::capturing(out);
            let mut stdout = String::new();
            let exit = spawn_tool(cmd(&["vet", "-json", &all_pkgs], true), &tap, &mut collect(&mut stdout)).map_err(|e| spawn_failure("go vet", e))?;
            let text = format!("{stdout}{}", tap.take());
            let rest = parse_vet_json(&text, &src, &lines, &mut diags);
            let errors = parse_go_text(&rest, &src, &src, &lines, &mut diags);
            out.log(rest.as_bytes());
            result.exit = exit;
            result.complete = exit < 128;
            if exit != 0 && errors > 0 {
                // vet はパッケージごとに最初の 1 件しか言わない。ビルドで全部を集め直す
                check_cancelled(ctx)?;
                let tap = Tap::capturing(out);
                let dir = pdir.join("go-check");
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).map_err(|e| io_failure("出力のフォルダを作れない", e))?;
                let o = format!("{}/", dir.display());
                let bexit = spawn_tool(cmd(&["build", "-o", &o, &all_pkgs], true), &tap, &mut |_| true).map_err(|e| spawn_failure("go build", e))?;
                let btext = tap.take();
                let mut bdiags = Vec::new();
                if bexit != 0 && parse_go_text(&btext, &src, &src, &lines, &mut bdiags) > 0 {
                    diags.retain(|f: &FileDiags| f.diags.iter().all(|d| d.severity != Severity::Error));
                    diags.extend(bdiags);
                }
            }
            // -json の vet は、指摘があっても 0 で終わる。0 でないのは型・構文の誤りか道具の失敗
            if has_error(&diags) {
                result.exit = 1;
            } else if exit != 0 {
                let msg = failure_line(&rest).unwrap_or_else(|| format!("go vet が失敗した（終了コード {exit}）"));
                push_diag(&mut diags, "", note_diag(Severity::Error, msg));
            }
        }
        Kind::Build => {
            let (pkg, name) = match (&module, &entry) {
                (Some(m), Some(e)) => {
                    let dir = Path::new(e).parent().and_then(|p| p.to_str()).unwrap_or("");
                    let name = if dir.is_empty() { m.rsplit('/').next().unwrap_or("").to_string() } else { dir.rsplit('/').next().unwrap_or("").to_string() };
                    (if dir.is_empty() { ".".to_string() } else { format!("./{dir}") }, name)
                }
                (Some(m), None) => (".".to_string(), m.rsplit('/').next().unwrap_or("").to_string()),
                (None, Some(e)) => (format!("./{e}"), Path::new(e).file_stem().and_then(|s| s.to_str()).unwrap_or("main").to_string()),
                (None, None) => unreachable!(),
            };
            let name = clean_name(&name);
            let exe_dir = pdir.join("go-out");
            std::fs::create_dir_all(&exe_dir).map_err(|e| io_failure("出力のフォルダを作れない", e))?;
            let exe = exe_dir.join(&name);
            let _ = std::fs::remove_file(&exe);
            let exe_s = exe.display().to_string();
            check_cancelled(ctx)?;
            let tap = Tap::capturing(out);
            let exit = spawn_tool(cmd(&["build", "-trimpath", "-ldflags=-s -w", "-o", &exe_s, &pkg], true), &tap, &mut |_| true).map_err(|e| spawn_failure("go build", e))?;
            let text = tap.take();
            out.log(text.as_bytes());
            parse_go_text(&text, &src, &src, &lines, &mut diags);
            result.exit = exit;
            result.complete = exit < 128;
            if exit != 0 && !has_error(&diags) {
                let msg = failure_line(&text).unwrap_or_else(|| format!("go build が失敗した（終了コード {exit}）"));
                push_diag(&mut diags, "", note_diag(Severity::Error, msg));
            }
            if exit == 0 {
                check_cancelled(ctx)?;
                let packed = pack(&exe, ArtifactKind::Exe, &name, ctx.out_dir).map_err(|e| JobFailure { status: Status::Error, message: e.0 })?;
                result.artifact = Some(packed);
            }
        }
        Kind::Test => {
            check_cancelled(ctx)?;
            let tap = Tap::new(out);
            let mut stdout = String::new();
            let exit = spawn_tool(cmd(&["test", "-json", &all_pkgs], false), &tap, &mut collect(&mut stdout)).map_err(|e| spawn_failure("go test", e))?;
            let entry_dir = entry.as_deref().and_then(|e| Path::new(e).parent()).and_then(|p| p.to_str()).unwrap_or("").to_string();
            let (tests, build_out, human) = parse_test2json(&stdout, module.as_deref().unwrap_or(""), &entry_dir, &src);
            out.log(human.as_bytes());
            parse_go_text(&build_out, &src, &src, &lines, &mut diags);
            result.exit = exit;
            result.complete = exit < 128;
            if exit != 0 && tests.failed == 0 && !has_error(&diags) {
                let msg = failure_line(&format!("{build_out}{}", tap.take())).unwrap_or_else(|| format!("go test が失敗した（終了コード {exit}）"));
                push_diag(&mut diags, "", note_diag(Severity::Error, msg));
            }
            result.tests = Some(tests);
        }
    }
    check_cancelled(ctx)?;
    // 道具が直した go.mod / go.sum を書き戻す
    for (name, old) in before {
        if module.is_none() {
            break;
        }
        if let Ok(now) = std::fs::read(src.join(name)) {
            if old.as_deref() != Some(&now[..]) && now.len() <= zc_wire::limits::MAX_WRITEBACK_BYTES {
                if let Ok(text) = String::from_utf8(now) {
                    result.write_back.push((name.to_string(), text));
                }
            }
        }
    }
    result.diagnostics = finish_diags(diags);
    Ok(result)
}

/// 標準出力を溜める（ログには出さない）。
fn collect(buf: &mut String) -> impl FnMut(&[u8]) -> bool + '_ {
    move |line: &[u8]| {
        if buf.len() < (8 << 20) {
            buf.push_str(&String::from_utf8_lossy(line));
        }
        false
    }
}

/// 実行ファイルの名前に使えるか（使えない字は `_` に）。
fn clean_name(name: &str) -> String {
    let n: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    if n.is_empty() || n == "." || n == ".." {
        "main".into()
    } else {
        n
    }
}

/// `go.mod` の `module` の値。
pub fn module_path(go_mod: &str) -> Option<String> {
    go_mod.lines().find_map(|l| {
        let l = l.split("//").next()?.trim();
        let rest = l.strip_prefix("module")?;
        if !rest.starts_with([' ', '\t']) {
            return None;
        }
        Some(rest.trim().trim_matches('"').to_string())
    })
}

/// go の文字の出力から `パス:行:桁: 本文` を error として読む。足した件数を返す。`base` は
/// 道具の作業フォルダ。
pub fn parse_go_text(text: &str, src_root: &Path, base: &Path, lines: &LineSource<'_>, into: &mut Vec<FileDiags>) -> usize {
    let mut n = 0;
    let mut last: Option<(String, u32, Option<u32>)> = None;
    for raw in text.lines() {
        let l = raw.strip_prefix("vet: ").unwrap_or(raw);
        if l.starts_with('#') {
            last = None;
            continue;
        }
        if (raw.starts_with('\t') || raw.starts_with("    ")) && !raw.trim().is_empty() {
            // 続きの行（「have (…) want (…)」など）
            if let Some((p, line, col)) = &last {
                if let Some(f) = into.iter_mut().find(|f| &f.path == p) {
                    if let Some(d) = f.diags.iter_mut().rev().find(|d| d.line == *line && d.col == *col) {
                        d.message = truncate_utf8(&format!("{} / {}", d.message, raw.trim()), 400);
                    }
                }
            }
            continue;
        }
        let Some((path, ln, col, msg)) = split_loc(l) else {
            last = None;
            continue;
        };
        if !path.ends_with(".go") {
            last = None;
            continue;
        }
        let message = truncate_utf8(msg, 400);
        match rel_to_root(src_root, base, path) {
            Some(rel) => {
                let line = line0(ln);
                let col = col.map(|c| byte_col0(lines, &rel, line, c));
                push_diag(into, &rel, Diag { line, col, end: None, severity: Severity::Error, code: None, message });
                last = Some((rel, line, col));
            }
            None => {
                push_diag(into, "", Diag { line: 0, col: None, end: None, severity: Severity::Error, code: None, message: format!("{path}:{ln}: {msg}") });
                last = None;
            }
        }
        n += 1;
    }
    n
}

/// `go vet -json` の指摘を warning として読み、JSON でない残りを返す。
pub fn parse_vet_json(text: &str, src_root: &Path, lines: &LineSource<'_>, into: &mut Vec<FileDiags>) -> String {
    let mut rest = String::new();
    let mut pos = 0;
    while pos < text.len() {
        let line_end = text[pos..].find('\n').map(|i| pos + i + 1).unwrap_or(text.len());
        if text[pos..line_end].starts_with('{') {
            let mut it = serde_json::Deserializer::from_str(&text[pos..]).into_iter::<Value>();
            if let Some(Ok(v)) = it.next() {
                vet_findings(&v, src_root, lines, into);
                pos += it.byte_offset();
                if text[pos..].starts_with('\n') {
                    pos += 1;
                }
                continue;
            }
        }
        rest.push_str(&text[pos..line_end]);
        pos = line_end;
    }
    rest
}

fn vet_findings(v: &Value, src_root: &Path, lines: &LineSource<'_>, into: &mut Vec<FileDiags>) {
    let Some(pkgs) = v.as_object() else { return };
    for analyzers in pkgs.values() {
        let Some(analyzers) = analyzers.as_object() else { continue };
        for (name, found) in analyzers {
            let Some(found) = found.as_array() else {
                // `{"error": "…"}`（解析器そのものの失敗）
                if let Some(e) = found.get("error").and_then(Value::as_str) {
                    push_diag(into, "", note_diag(Severity::Warning, truncate_utf8(&format!("{name}: {e}"), 400)));
                }
                continue;
            };
            for f in found {
                let message = truncate_utf8(f.get("message").and_then(Value::as_str).unwrap_or(""), 400);
                let code = Some(name.clone());
                let loc = |k: &str| f.get(k).and_then(Value::as_str).and_then(split_posn);
                match loc("posn").and_then(|(p, l, c)| rel_to_root(src_root, src_root, &p).map(|r| (r, l, c))) {
                    Some((rel, l, c)) => {
                        let line = line0(l);
                        let col = c.map(|c| byte_col0(lines, &rel, line, c));
                        let end = loc("end").and_then(|(_, el, ec)| Some((line0(el), byte_col0(lines, &rel, line0(el), ec?))));
                        push_diag(into, &rel, Diag { line, col, end, severity: Severity::Warning, code, message });
                    }
                    None => push_diag(into, "", Diag { line: 0, col: None, end: None, severity: Severity::Warning, code, message }),
                }
            }
        }
    }
}

/// `/abs/x.go:8:14` → （パス, 行, 桁）。
fn split_posn(s: &str) -> Option<(String, u32, Option<u32>)> {
    let with_colon = format!("{s}:");
    let (p, l, c, _) = split_loc(&with_colon)?;
    Some((p.to_string(), l, c))
}

/// 試験 1 件の途中の状態。
#[derive(Default)]
struct Pending {
    output: Vec<String>,
}

/// `go test -json` の出力を読む。返すのは（結果, ビルドの出力 = 診断のもと, 人が読むログ）。
/// `module` は `go.mod` の `module`（1 ファイルなら空）、`entry_dir` は 1 ファイルのときの
/// そのファイルのフォルダ。
pub fn parse_test2json(stdout: &str, module: &str, entry_dir: &str, src_root: &Path) -> (TestResults, String, String) {
    let mut r = TestResults::default();
    let mut build_out = String::new();
    let mut human = String::new();
    let mut pending: std::collections::HashMap<(String, String), Pending> = std::collections::HashMap::new();
    let mut pkg_output: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut pkg_failed_tests: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            human.push_str(line);
            human.push('\n');
            continue;
        };
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let action = s("Action");
        let pkg = s("Package");
        let test = s("Test");
        match action.as_str() {
            "build-output" => {
                build_out.push_str(&s("Output"));
                human.push_str(&s("Output"));
            }
            "output" => {
                let o = s("Output");
                human.push_str(&o);
                if test.is_empty() {
                    pkg_output.entry(pkg).or_default().push(o);
                } else {
                    pending.entry((pkg, test)).or_default().output.push(o);
                }
            }
            "pass" | "fail" | "skip" if !test.is_empty() => {
                let p = pending.remove(&(pkg.clone(), test.clone())).unwrap_or_default();
                match action.as_str() {
                    "pass" => {
                        r.passed += 1;
                        r.cases.push(TestCase { name: test, passed: true, path: None, line: None, message: String::new() });
                    }
                    "skip" => r.ignored += 1,
                    _ => {
                        r.failed += 1;
                        pkg_failed_tests.insert(pkg.clone());
                        let dir = pkg_dir(&pkg, module, entry_dir);
                        let mut case = TestCase { name: test, passed: false, path: None, line: None, message: String::new() };
                        fill_failure(&mut case, &p.output, &dir, src_root);
                        r.cases.push(case);
                    }
                }
            }
            "fail" if !pkg_failed_tests.contains(&pkg) => {
                // 試験は落ちていないのにパッケージが落ちた（panic・ビルドの失敗・TestMain）
                r.failed += 1;
                let out: Vec<String> = pkg_output.remove(&pkg).unwrap_or_default();
                let msg: Vec<&str> = out.iter().map(|l| l.trim_end()).filter(|l| !l.is_empty() && !l.starts_with("FAIL") && !l.starts_with("ok ")).collect();
                let mut message = msg.join("\n");
                if message.is_empty() && v.get("FailedBuild").is_some() {
                    message = "ビルドに失敗した".into();
                }
                r.cases.push(TestCase { name: pkg, passed: false, path: None, line: None, message: truncate_utf8(&message, MAX_TEST_MESSAGE) });
            }
            _ => {}
        }
    }
    (r, build_out, human)
}

/// import パス → 木の根からのパッケージのフォルダ。
fn pkg_dir(pkg: &str, module: &str, entry_dir: &str) -> String {
    if module.is_empty() || pkg == "command-line-arguments" {
        return entry_dir.to_string();
    }
    match pkg.strip_prefix(module) {
        Some("") => String::new(),
        Some(rest) => rest.trim_start_matches('/').to_string(),
        None => String::new(),
    }
}

/// 落ちた試験の出力から、場所（`    x_test.go:7: …`）とメッセージを取る。
fn fill_failure(case: &mut TestCase, output: &[String], dir: &str, src_root: &Path) {
    let mut msg = Vec::new();
    for l in output {
        let t = l.trim_end();
        let tt = t.trim_start();
        if tt.starts_with("=== ") || tt.starts_with("--- ") || tt.is_empty() {
            continue;
        }
        if case.path.is_none() {
            if let Some((p, ln, _, _)) = split_loc(tt) {
                if p.ends_with(".go") {
                    let base: PathBuf = src_root.join(dir);
                    if let Some(rel) = rel_to_root(src_root, &base, p) {
                        case.path = Some(rel);
                        case.line = Some(line0(ln));
                    }
                }
            }
        }
        msg.push(tt.to_string());
    }
    case.message = truncate_utf8(msg.join("\n").trim(), MAX_TEST_MESSAGE);
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{skip_without, Collect, TempDir};
    use super::*;
    use std::sync::atomic::AtomicBool;
    use zc_wire::msg::{JobOpts, Lang};

    /// fixture の元（tests/data/langs/gen.sh）。
    const MAIN_ERR: &str = "package main\n\nimport \"fmt\"\n\n// 日本語\nfunc main() {\n\tvar s string = /* 日本 */ 1\n\tfmt.Println(s, undefinedX)\n}\n";
    const MAIN_VET: &str = "package main\n\nimport \"fmt\"\n\n// 日本語\nfunc main() {\n\tx := 3\n\tfmt.Printf(\"%s 日本 %d\\n\", x)\n}\n";

    fn lines_of(text: &'static str) -> impl Fn(&str, u32) -> Option<String> {
        move |_: &str, l: u32| text.split('\n').nth(l as usize).map(str::to_string)
    }

    #[test]
    fn build_errors_are_read() {
        let mut v = Vec::new();
        let n = parse_go_text(include_str!("../../tests/data/langs/go-build-err.txt"), Path::new("/w"), Path::new("/w"), &lines_of(MAIN_ERR), &mut v);
        assert_eq!(n, 2);
        let m = &v[0].diags;
        assert_eq!(v[0].path, "main.go");
        // `\tvar s string = /* 日本 */ 1` の 1（Go の桁はバイト）
        let l = "\tvar s string = /* 日本 */ 1";
        assert_eq!((m[0].line, m[0].col, m[0].severity), (6, Some(l.rfind('1').unwrap() as u32), Severity::Error));
        assert_eq!((m[1].line, m[1].col), (7, Some(16)));
        // vet の型の誤り（vet: の頭）
        let mut v = Vec::new();
        assert_eq!(parse_go_text(include_str!("../../tests/data/langs/go-vet-typeerr.txt"), Path::new("/w"), Path::new("/w"), &lines_of(MAIN_ERR), &mut v), 1);
        assert_eq!(v[0].diags[0].line, 6);
        // 下のパッケージの構文の誤り
        let mut v = Vec::new();
        parse_go_text(include_str!("../../tests/data/langs/go-build-syntax.txt"), Path::new("/w"), Path::new("/w"), &|_, _| Some("}".into()), &mut v);
        assert_eq!((v[0].path.as_str(), v[0].diags[0].line, v[0].diags[0].col), ("sub/sub.go", 5, Some(0)));
        // 続きの行は前の 1 件に足す。木の外は位置なし
        let mut v = Vec::new();
        parse_go_text("# x\n./a.go:3:2: not enough arguments\n\thave ()\n\twant (int)\n/usr/lib/go/x.go:1:1: y\n", Path::new("/w"), Path::new("/w"), &|_, _| Some("  x".into()), &mut v);
        let a = &v.iter().find(|f| f.path == "a.go").unwrap().diags[0];
        assert_eq!(a.message, "not enough arguments / have () / want (int)");
        assert!(v.iter().any(|f| f.path.is_empty()));
    }

    #[test]
    fn vet_json_findings_are_warnings() {
        let mut v = Vec::new();
        let rest = parse_vet_json(include_str!("../../tests/data/langs/go-vet-json.txt"), Path::new("/tmp/fxw/go"), &lines_of(MAIN_VET), &mut v);
        assert!(rest.trim().is_empty(), "{rest}");
        let d = &v[0].diags[0];
        let l = "\tfmt.Printf(\"%s 日本 %d\\n\", x)";
        assert_eq!(v[0].path, "main.go");
        assert_eq!((d.line, d.col, d.severity, d.code.as_deref()), (7, Some(13), Severity::Warning, Some("printf")));
        assert_eq!(d.end, Some((7, 15)));
        assert!(l.is_char_boundary(13));
        // 文字と JSON が混ざっていても分ける
        let mixed = "# example.com/x\nvet: ./a.go:1:1: bad\n{\n\t\"p\": {\n\t\t\"printf\": [{\"posn\": \"/w/a.go:2:3\", \"message\": \"m\"}]\n\t}\n}\n";
        let mut v = Vec::new();
        let rest = parse_vet_json(mixed, Path::new("/w"), &|_, _| Some("xxxxxx".into()), &mut v);
        assert_eq!(rest, "# example.com/x\nvet: ./a.go:1:1: bad\n");
        assert_eq!((v[0].diags[0].line, v[0].diags[0].col), (1, Some(2)));
    }

    #[test]
    fn test2json_is_read() {
        let (r, build, human) = parse_test2json(include_str!("../../tests/data/langs/go-test.json"), "example.com/hello", "", Path::new("/w"));
        assert_eq!((r.passed, r.failed, r.ignored), (1, 1, 1));
        let bad = r.cases.iter().find(|c| !c.passed).unwrap();
        assert_eq!((bad.name.as_str(), bad.path.as_deref(), bad.line), ("TestAdd", Some("main_test.go"), Some(6)));
        assert!(bad.message.contains("日本 add = 3"), "{}", bad.message);
        assert!(build.is_empty());
        assert!(human.contains("--- FAIL: TestAdd"));
        // 下のパッケージの試験のビルドが vet で落ちた
        let (r, build, _) = parse_test2json(include_str!("../../tests/data/langs/go-test-sub.json"), "example.com/hello", "", Path::new("/w"));
        assert_eq!((r.passed, r.failed), (0, 1));
        assert_eq!(r.cases[0].name, "example.com/hello/sub");
        let mut v = Vec::new();
        parse_go_text(&build, Path::new("/w"), Path::new("/w"), &|_, _| None, &mut v);
        assert_eq!((v[0].path.as_str(), v[0].diags[0].line), ("sub/sub.go", 4));
        assert_eq!(pkg_dir("example.com/hello/a/b", "example.com/hello", ""), "a/b");
        assert_eq!(pkg_dir("command-line-arguments", "", "x"), "x");
        assert_eq!(module_path("// c\nmodule \"example.com/m\" // x\n\ngo 1.24\n").as_deref(), Some("example.com/m"));
        assert_eq!(module_path("modulex y\n"), None);
    }

    struct Job {
        _t: TempDir,
        src: PathBuf,
        project: PathBuf,
        out: PathBuf,
    }

    fn job(files: &[(&str, &str)]) -> Job {
        let t = TempDir::new("go");
        let src = t.path().join("proj/src");
        for (p, text) in files {
            let f = src.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        std::fs::create_dir_all(&src).unwrap();
        let out = t.path().join("out");
        Job { project: t.path().join("proj"), src, out, _t: t }
    }

    fn run_job(j: &Job, kind: Kind, entry: Option<&str>) -> (JobResult, String) {
        let cancelled = AtomicBool::new(false);
        let opts = JobOpts { entry: entry.map(str::to_string), ..Default::default() };
        let ctx = JobCtx { job: "j", kind, lang: Lang::Go, opts: &opts, src: &j.src, project_dir: &j.project, out_dir: &j.out, cancelled: &cancelled };
        let out = Collect::default();
        let r = run(&ctx, &out).unwrap_or_else(|f| panic!("{f:?}\n{}", out.text()));
        (r, out.text())
    }

    fn is_aarch64(j: &Job, a: &crate::pack::Packed) -> bool {
        let back = j.out.join("back");
        assert!(Command::new("zstd").args(["-q", "-f", "-d", "-o"]).arg(&back).arg(&a.zpath).status().unwrap().success());
        let f = Command::new("file").arg(&back).output().unwrap();
        String::from_utf8_lossy(&f.stdout).contains("ARM aarch64")
    }

    #[test]
    fn e2e_module_check_build_test() {
        if skip_without(&["go", "zstd", "file", "aarch64-linux-gnu-strip"]) {
            return;
        }
        let gomod = "module example.com/hello\n\ngo 1.24\n";
        // 型の誤り（2 件とも出る。vet は 1 件しか言わない）
        let j = job(&[("go.mod", gomod), ("main.go", MAIN_ERR)]);
        let (r, log) = run_job(&j, Kind::Check, Some("main.go"));
        assert_ne!(r.exit, 0, "{log}");
        let m = &r.diagnostics.iter().find(|f| f.path == "main.go").unwrap().diags;
        assert_eq!(m.iter().filter(|d| d.severity == Severity::Error).count(), 2, "{m:?}");
        assert_eq!(m[0].col, Some("\tvar s string = /* 日本 */ ".len() as u32));
        // vet の指摘だけ → 警告、終了コード 0
        let j = job(&[("go.mod", gomod), ("main.go", MAIN_VET)]);
        let (r, log) = run_job(&j, Kind::Check, Some("main.go"));
        assert_eq!(r.exit, 0, "{log}");
        assert_eq!(r.diagnostics[0].diags[0].code.as_deref(), Some("printf"));
        // ビルド → aarch64 の ELF、名前は module の最後
        let j = job(&[
            ("go.mod", gomod),
            ("main.go", "package main\n\nimport \"fmt\"\n\nfunc main() { fmt.Println(add(1, 2)) }\n\nfunc add(a, b int) int { return a + b }\n"),
            ("main_test.go", "package main\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n\tif add(1, 2) != 4 {\n\t\tt.Errorf(\"日本 add = %d\", add(1, 2))\n\t}\n}\n\nfunc TestOk(t *testing.T) {}\n"),
            ("cmd/tool/main.go", "package main\n\nfunc main() {}\n"),
        ]);
        let (r, log) = run_job(&j, Kind::Build, Some("main.go"));
        assert_eq!(r.exit, 0, "{log}");
        let a = r.artifact.as_ref().unwrap();
        assert_eq!(a.name, "hello");
        assert!(is_aarch64(&j, a));
        // 下のフォルダの main
        let (r, _) = run_job(&j, Kind::Build, Some("cmd/tool/main.go"));
        assert_eq!(r.artifact.as_ref().unwrap().name, "tool");
        // 試験（x86_64）
        let (r, log) = run_job(&j, Kind::Test, Some("main.go"));
        let t = r.tests.unwrap();
        assert_eq!((t.passed, t.failed), (1, 1), "{log}");
        let bad = t.cases.iter().find(|c| !c.passed).unwrap();
        assert_eq!((bad.path.as_deref(), bad.line), (Some("main_test.go"), Some(6)));
        assert_ne!(r.exit, 0);
    }

    #[test]
    fn e2e_single_file() {
        if skip_without(&["go", "zstd", "file", "aarch64-linux-gnu-strip"]) {
            return;
        }
        let j = job(&[("hello.go", "package main\n\nimport \"fmt\"\n\nfunc main() { fmt.Println(\"日本\") }\n")]);
        let (r, log) = run_job(&j, Kind::Check, Some("hello.go"));
        assert_eq!((r.exit, r.diagnostics.len()), (0, 0), "{log}");
        let (r, log) = run_job(&j, Kind::Build, Some("hello.go"));
        assert_eq!(r.exit, 0, "{log}");
        let a = r.artifact.as_ref().unwrap();
        assert_eq!(a.name, "hello");
        assert!(is_aarch64(&j, a));
        assert!(r.write_back.is_empty());
        let j = job(&[("bad.go", "package main\n\nfunc main() {\n\tx := \"日本\" +\n}\n")]);
        let (r, _) = run_job(&j, Kind::Check, Some("bad.go"));
        assert_ne!(r.exit, 0);
        assert_eq!(r.diagnostics[0].path, "bad.go");
        assert_eq!(r.diagnostics[0].diags[0].line, 4);
    }
}
