//! Zig のジョブ（docs/stage3-design.md §4.4）。2026-11-17 に足した。
//!
//! 木: 根に `build.zig` があればプロジェクト、無ければ 1 ファイルだけ（`entry`）。
//!
//! | 種類 | 1 ファイル | `build.zig` のある木 |
//! |---|---|---|
//! | check | `zig ast-check` → 通れば（`pub fn main` があるとき）`zig build-exe -fno-emit-bin -target aarch64-linux-gnu -mcpu=cortex_a53` | 木の中の `*.zig` 全部に `zig ast-check` → 通れば `zig build -Dtarget=aarch64-linux-gnu -Dcpu=cortex_a53`（出力は `<project_dir>/zig-check`） |
//! | build | `zig build-exe -target aarch64-linux-gnu -mcpu=cortex_a53 -O ReleaseSmall` | `zig build -Dtarget=aarch64-linux-gnu -Dcpu=cortex_a53 --release=small --prefix <project_dir>/zig-out`。`bin/` の実行ファイルが 1 つならそれ、複数なら入口のファイルと同じ名前のもの |
//! | test | `zig test`（**x86_64 のまま**） | `zig build test --summary all`（x86_64） |
//!
//! - キャッシュ: 全体は `ZIG_GLOBAL_CACHE_DIR`（イメージの ENV。置き場の中）、プロジェクトごとは
//!   `<project_dir>/zig-cache`（`--cache-dir` と `ZIG_LOCAL_CACHE_DIR`）。作業の木の中の
//!   `.zig-cache` は、写すたびに消されるので使わない
//! - `build.zig` が `standardTargetOptions` を使っていないと `-Dtarget` が「知らない引数」で落ちる
//!   （その誤りがそのまま出る）。最適化は `--release=small`（`standardOptimizeOption` が拾う。
//!   使っていなければ無視される）
//!
//! # 診断（tests/data/langs/zig-*.txt）
//!
//! - `パス:行:桁: error: 本文` → error、`…: note: 本文` は前の 1 件に ` / note: …` で足す。行・桁は
//!   1 始まり、**桁はバイト**（日本語の後ろで確かめた）。続く写し・`^~~~`・`referenced by:` の行は捨てる
//! - パスは作業フォルダ（木の根）からの相対か絶対。木の外（標準ライブラリの中）の error は位置なし
//! - 位置の付いた error が 1 つも無いのに失敗したら、`error: …` の行を位置なしの 1 件にする
//!
//! # 試験
//!
//! - `zig test`: `1/3 x.test.add...OK|FAIL (…)|SKIP` の行と、`N passed; M skipped; K failed.`。
//!   落ちた試験の場所は、続くスタックの行（`パス:行:桁: 0x… in …`）のうち木の中の最初のもの
//! - `zig build test`: `error: 'x.test.add' failed:` の行（と続くスタック）と、
//!   `Build Summary: … ; 3/4 tests passed (1 failed)`

use super::{
    byte_col0, check_cancelled, failure_line, files_with_ext, finish_diags, has_error, io_failure, line0, lines_from, note_diag, push_diag, rel_to_root, safe_entry, spawn_failure, spawn_tool,
    split_loc, tool_env, truncate_utf8, which, JobCtx, JobFailure, JobOutput, JobResult, LineSource, Tap, TestResults,
};
use crate::pack::pack;
use std::path::Path;
use std::process::Command;
use zc_wire::msg::{ArtifactKind, Diag, FileDiags, Kind, Severity, Status, TestCase};

/// 実機向けの指定。
pub const TARGET: &[&str] = &["-target", "aarch64-linux-gnu", "-mcpu=cortex_a53"];
/// `zig build` に渡す実機向けの指定。
pub const BUILD_TARGET: &[&str] = &["-Dtarget=aarch64-linux-gnu", "-Dcpu=cortex_a53"];
/// 木のときに ast-check するファイルの上限。
const MAX_FILES: usize = 300;
const MAX_TEST_MESSAGE: usize = 2 << 10;

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    let zig = which("zig")?;
    let src = ctx.src.canonicalize().map_err(|e| io_failure("作業の木が無い", e))?;
    std::fs::create_dir_all(ctx.project_dir).map_err(|e| io_failure("プロジェクトのフォルダを作れない", e))?;
    let pdir = ctx.project_dir.canonicalize().map_err(|e| io_failure("プロジェクトのフォルダが無い", e))?;
    let cache = pdir.join("zig-cache");
    let cache_s = cache.display().to_string();
    let tree = src.join("build.zig").is_file();
    let entry = match ctx.opts.entry.as_deref() {
        Some(e) if safe_entry(e) && src.join(e).is_file() => Some(e.to_string()),
        Some(e) => return Ok(super::failed_result(1, format!("入口のファイルが無い: {e}"))),
        None if tree => None,
        None => return Ok(super::failed_result(1, "build.zig も入口のファイルも無い")),
    };
    let cmd = |args: &[&str]| {
        let mut c = Command::new(&zig);
        c.current_dir(&src).args(args).env("ZIG_LOCAL_CACHE_DIR", &cache);
        tool_env(&mut c);
        c
    };
    let lines = lines_from(&src);
    let mut diags = Vec::new();
    let mut result = JobResult { complete: true, ..Default::default() };
    // 道具を 1 回動かして、出力（標準出力 + 標準エラー）と終了コードを返す
    let go = |c: Command, what: &str| -> Result<(i32, String), JobFailure> {
        check_cancelled(ctx)?;
        let tap = Tap::capturing(out);
        let mut so = String::new();
        let exit = spawn_tool(c, &tap, &mut |l| {
            if so.len() < (8 << 20) {
                so.push_str(&String::from_utf8_lossy(l));
            }
            false
        })
        .map_err(|e| spawn_failure(what, e))?;
        let text = format!("{so}{}", tap.take());
        out.log(text.as_bytes());
        Ok((exit, text))
    };

    match ctx.kind {
        Kind::Check => {
            let files = match &entry {
                Some(e) if !tree => vec![e.clone()],
                _ => {
                    let (f, cut) = files_with_ext(&src, &[".zig"], &["zig-out", "zig-cache"], MAX_FILES);
                    if cut {
                        push_diag(&mut diags, "", note_diag(Severity::Warning, format!(".zig が多すぎるので、名前の順に {MAX_FILES} 個だけ ast-check した")));
                    }
                    f
                }
            };
            let mut exit = 0;
            for f in &files {
                let (e, text) = go(cmd(&["ast-check", &format!("./{f}")]), "zig ast-check")?;
                parse_zig(&text, &src, &lines, &mut diags);
                exit = exit.max(e);
            }
            let mut last = String::new();
            if exit == 0 && !has_error(&diags) {
                if tree {
                    let prefix = pdir.join("zig-check").display().to_string();
                    let mut args = vec!["build"];
                    args.extend(BUILD_TARGET);
                    args.extend(["--prefix", prefix.as_str(), "--cache-dir", cache_s.as_str()]);
                    (exit, last) = go(cmd(&args), "zig build")?;
                } else if let Some(e) = entry.as_deref().filter(|e| std::fs::read_to_string(src.join(e)).is_ok_and(|t| t.contains("pub fn main"))) {
                    let mut args = vec!["build-exe", "-fno-emit-bin"];
                    args.extend(TARGET);
                    let f = format!("./{e}");
                    args.push(&f);
                    (exit, last) = go(cmd(&args), "zig build-exe")?;
                }
                parse_zig(&last, &src, &lines, &mut diags);
            }
            finish_exit(&mut result, &mut diags, exit, &last, "zig");
        }
        Kind::Build => {
            let out_dir = pdir.join("zig-out");
            let _ = std::fs::remove_dir_all(out_dir.join("bin"));
            std::fs::create_dir_all(out_dir.join("bin")).map_err(|e| io_failure("出力のフォルダを作れない", e))?;
            let stem = entry.as_deref().and_then(|e| Path::new(e).file_stem()).and_then(|s| s.to_str()).map(str::to_string);
            let (exit, text) = if tree {
                let prefix = out_dir.display().to_string();
                let mut args = vec!["build"];
                args.extend(BUILD_TARGET);
                args.extend(["--release=small", "--prefix", prefix.as_str(), "--cache-dir", cache_s.as_str()]);
                go(cmd(&args), "zig build")?
            } else {
                let name = stem.clone().unwrap_or_else(|| "main".into());
                let emit = format!("-femit-bin={}", out_dir.join("bin").join(&name).display());
                let f = format!("./{}", entry.as_deref().unwrap_or_default());
                let mut args = vec!["build-exe"];
                args.extend(TARGET);
                args.extend(["-O", "ReleaseSmall", emit.as_str(), f.as_str()]);
                go(cmd(&args), "zig build-exe")?
            };
            parse_zig(&text, &src, &lines, &mut diags);
            finish_exit(&mut result, &mut diags, exit, &text, "zig");
            if exit == 0 {
                match pick_exe(&out_dir.join("bin"), stem.as_deref()) {
                    Ok((path, name)) => {
                        check_cancelled(ctx)?;
                        let packed = pack(&path, ArtifactKind::Exe, &name, ctx.out_dir).map_err(|e| JobFailure { status: Status::Error, message: e.0 })?;
                        result.artifact = Some(packed);
                    }
                    Err(msg) => {
                        push_diag(&mut diags, "", note_diag(Severity::Error, msg));
                        result.exit = 1;
                    }
                }
            }
        }
        Kind::Test => {
            let (exit, text) = if tree {
                go(cmd(&["build", "test", "--summary", "all", "--cache-dir", &cache_s]), "zig build test")?
            } else {
                go(cmd(&["test", &format!("./{}", entry.as_deref().unwrap_or_default())]), "zig test")?
            };
            let tests = if tree { parse_build_test(&text, &src) } else { parse_zig_test(&text, &src) };
            result.exit = exit;
            result.complete = exit < 128;
            if exit != 0 && tests.failed == 0 {
                // 試験までたどり着かなかった（コンパイルの誤り）
                parse_zig(&text, &src, &lines, &mut diags);
                if !has_error(&diags) {
                    let msg = failure_line(&text).unwrap_or_else(|| format!("zig の試験が失敗した（終了コード {exit}）"));
                    push_diag(&mut diags, "", note_diag(Severity::Error, msg));
                }
            }
            result.tests = Some(tests);
        }
    }
    result.diagnostics = finish_diags(diags);
    Ok(result)
}

/// 終了コードと診断から、結果の終了コードを決める（位置の付いた error が無い失敗には 1 件足す）。
fn finish_exit(result: &mut JobResult, diags: &mut Vec<FileDiags>, exit: i32, text: &str, tool: &str) {
    result.complete = exit < 128;
    if has_error(diags) {
        result.exit = if exit == 0 { 1 } else { exit };
    } else if exit != 0 {
        let msg = failure_line(text).unwrap_or_else(|| format!("{tool} が失敗した（終了コード {exit}）"));
        push_diag(diags, "", note_diag(Severity::Error, msg));
        result.exit = exit;
    } else {
        result.exit = 0;
    }
}

/// `bin/` の実行ファイルを選ぶ。
fn pick_exe(bin: &Path, stem: Option<&str>) -> Result<(std::path::PathBuf, String), String> {
    let mut names: Vec<String> = std::fs::read_dir(bin).map_err(|e| format!("bin/ が無い: {e}"))?.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).filter_map(|e| e.file_name().to_str().map(str::to_string)).collect();
    names.sort();
    let pick = match (names.len(), stem) {
        (0, _) => return Err("実行ファイルができなかった（build.zig が installArtifact していない？）".into()),
        (1, _) => names[0].clone(),
        (_, Some(s)) if names.iter().any(|n| n == s) => s.to_string(),
        _ => return Err(format!("実行ファイルが複数ある: {}", names.join(" "))),
    };
    Ok((bin.join(&pick), pick))
}

/// zig の出力から `パス:行:桁: error|note: 本文` を読む。
pub fn parse_zig(text: &str, src_root: &Path, lines: &LineSource<'_>, into: &mut Vec<FileDiags>) {
    let mut last: Option<(String, u32, Option<u32>)> = None;
    for raw in text.lines() {
        let l = raw.trim_start_matches(|c: char| c.is_whitespace() || "│├└─+|".contains(c));
        let Some((path, ln, col, msg)) = split_loc(l) else { continue };
        if !path.ends_with(".zig") && !path.ends_with(".zon") {
            continue;
        }
        let (severity, body) = if let Some(m) = msg.strip_prefix("error: ") {
            (Severity::Error, m)
        } else if let Some(m) = msg.strip_prefix("note: ") {
            if let Some((p, line, c)) = &last {
                if let Some(d) = into.iter_mut().find(|f| &f.path == p).and_then(|f| f.diags.iter_mut().rev().find(|d| d.line == *line && d.col == *c)) {
                    d.message = truncate_utf8(&format!("{} / note: {m}", d.message), 400);
                }
            }
            continue;
        } else if let Some(m) = msg.strip_prefix("warning: ") {
            (Severity::Warning, m)
        } else {
            continue;
        };
        let message = truncate_utf8(body, 400);
        match rel_to_root(src_root, src_root, path) {
            Some(rel) => {
                let line = line0(ln);
                let col = col.map(|c| byte_col0(lines, &rel, line, c));
                push_diag(into, &rel, Diag { line, col, end: None, severity, code: None, message });
                last = Some((rel, line, col));
            }
            None => {
                push_diag(into, "", Diag { line: 0, col: None, end: None, severity, code: None, message: truncate_utf8(&format!("{path}:{ln}: {body}"), 400) });
                last = None;
            }
        }
    }
}

/// スタックの行（`パス:行:桁: 0x… in …`）から、木の中の最初の場所。
fn trace_location(lines: &[&str], src_root: &Path) -> Option<(String, u32)> {
    lines.iter().find_map(|l| {
        let (p, ln, _, rest) = split_loc(l.trim_start())?;
        if !rest.starts_with("0x") {
            return None;
        }
        rel_to_root(src_root, src_root, p).map(|r| (r, line0(ln)))
    })
}

/// `zig test` の出力を読む。
pub fn parse_zig_test(text: &str, src_root: &Path) -> TestResults {
    let mut r = TestResults::default();
    let all: Vec<&str> = text.lines().collect();
    let mut summary: Option<(u32, u32, u32)> = None;
    for (i, l) in all.iter().enumerate() {
        if let Some(s) = parse_summary(l) {
            summary = Some(s);
            continue;
        }
        let Some((_, rest)) = l.split_once(' ').filter(|(n, _)| n.contains('/') && n.bytes().all(|b| b.is_ascii_digit() || b == b'/')) else { continue };
        let Some(dots) = rest.rfind("...") else { continue };
        let (name, verdict) = (rest[..dots].to_string(), &rest[dots + 3..]);
        if verdict.starts_with("OK") {
            r.passed += 1;
            r.cases.push(TestCase { name, passed: true, path: None, line: None, message: String::new() });
        } else if verdict.starts_with("SKIP") {
            r.ignored += 1;
        } else if verdict.starts_with("FAIL") {
            r.failed += 1;
            // 次の試験の行まで
            let body: Vec<&str> = all[i + 1..].iter().take_while(|l| !l.split(' ').next().is_some_and(|n| n.contains('/') && n.bytes().all(|b| b.is_ascii_digit() || b == b'/'))).copied().collect();
            let loc = trace_location(&body, src_root);
            let message = verdict.trim_start_matches("FAIL").trim().trim_matches(|c| c == '(' || c == ')').to_string();
            r.cases.push(TestCase { name, passed: false, path: loc.as_ref().map(|l| l.0.clone()), line: loc.map(|l| l.1), message: truncate_utf8(&message, MAX_TEST_MESSAGE) });
        }
    }
    if let Some((p, s, f)) = summary {
        (r.passed, r.ignored, r.failed) = (p, s, f);
    } else if let Some(n) = text.lines().find_map(|l| l.trim().strip_prefix("All ")?.split_once(" tests passed")?.0.parse::<u32>().ok()) {
        r.passed = n;
    }
    r
}

/// `N passed; M skipped; K failed.`
fn parse_summary(l: &str) -> Option<(u32, u32, u32)> {
    let l = l.trim().strip_suffix('.')?;
    let mut it = l.split("; ");
    let num = |s: Option<&str>, w: &str| s?.strip_suffix(w)?.trim().parse::<u32>().ok();
    let p = num(it.next(), " passed")?;
    let s = num(it.next(), " skipped")?;
    let f = num(it.next(), " failed")?;
    Some((p, s, f))
}

/// `zig build test` の出力を読む。
pub fn parse_build_test(text: &str, src_root: &Path) -> TestResults {
    let mut r = TestResults::default();
    let all: Vec<&str> = text.lines().collect();
    for (i, l) in all.iter().enumerate() {
        if let Some(rest) = l.strip_prefix("error: '") {
            if let Some(name) = rest.strip_suffix("' failed:") {
                let body: Vec<&str> = all[i + 1..].iter().take_while(|l| l.starts_with(' ')).copied().collect();
                let loc = trace_location(&body, src_root);
                let msg: Vec<&str> = body.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).take(6).collect();
                r.cases.push(TestCase { name: name.to_string(), passed: false, path: loc.as_ref().map(|l| l.0.clone()), line: loc.map(|l| l.1), message: truncate_utf8(&msg.join("\n"), MAX_TEST_MESSAGE) });
            }
        }
        if let Some(pos) = l.find(" tests passed") {
            // 「…; 3/4 tests passed (1 failed)」
            let head = &l[..pos];
            let frac = head.rsplit([' ', ';']).next().unwrap_or("");
            if let Some((a, b)) = frac.split_once('/') {
                if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                    let tail = &l[pos..];
                    let count = |w: &str| tail.find(w).and_then(|k| tail[..k].trim_end().rsplit([' ', '(', ';']).next()?.parse::<u32>().ok());
                    r.passed = a;
                    r.ignored = count(" skipped").unwrap_or(0);
                    r.failed = count(" failed").unwrap_or(b.saturating_sub(a + r.ignored));
                }
            }
        }
    }
    r.failed = r.failed.max(r.cases.iter().filter(|c| !c.passed).count() as u32);
    r
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{skip_without, Collect, TempDir};
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use zc_wire::msg::{JobOpts, Lang};

    fn lines_of(text: &'static str) -> impl Fn(&str, u32) -> Option<String> {
        move |_: &str, l: u32| text.split('\n').nth(l as usize).map(str::to_string)
    }

    #[test]
    fn errors_are_read() {
        // `    const x: u32 = "日本"; const y = 1` の行末（桁はバイト）
        let src = "const std = @import(\"std\");\n// 日本語\npub fn main() void {\n    const x: u32 = \"日本\"; const y = 1\n}\n";
        let mut v = Vec::new();
        parse_zig(include_str!("../../tests/data/langs/zig-ast.txt"), Path::new("/w"), &lines_of(src), &mut v);
        let a = &v[0].diags[0];
        assert_eq!(v[0].path, "a.zig");
        assert_eq!((a.line, a.col, a.severity), (3, Some("    const x: u32 = \"日本\"; const y = 1".len() as u32), Severity::Error));
        assert_eq!(a.message, "expected ';' after statement");
        // 意味の誤り。referenced by の標準ライブラリの行は捨てる
        let mut v = Vec::new();
        parse_zig(include_str!("../../tests/data/langs/zig-sema2.txt"), Path::new("/w"), &lines_of(""), &mut v);
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].diags[0].line, v[0].diags[0].col), (3, Some(19)));
        // note は前の 1 件に足す
        let mut v = Vec::new();
        parse_zig("src/a.zig:2:5: error: bad\n    x\n    ^\nsrc/a.zig:1:1: note: here\n/opt/zig/lib/std/x.zig:1:1: error: inside std\n", Path::new("/w"), &|_, _| Some("xxxxxx".into()), &mut v);
        let a = &v.iter().find(|f| f.path == "src/a.zig").unwrap().diags[0];
        assert_eq!(a.message, "bad / note: here");
        assert!(v.iter().any(|f| f.path.is_empty()));
    }

    #[test]
    fn test_outputs_are_read() {
        let r = parse_zig_test(include_str!("../../tests/data/langs/zig-test.txt"), Path::new("/tmp/fxw/zig"));
        assert_eq!((r.passed, r.failed, r.ignored), (1, 1, 0));
        let bad = r.cases.iter().find(|c| !c.passed).unwrap();
        assert_eq!((bad.name.as_str(), bad.path.as_deref(), bad.line), ("c.test.add", Some("c.zig"), Some(6)));
        assert_eq!(bad.message, "TestUnexpectedResult");
        let r = parse_zig_test(include_str!("../../tests/data/langs/zig-test-ok.txt"), Path::new("/w"));
        assert_eq!((r.passed, r.failed, r.ignored), (2, 0, 1));
        let r = parse_build_test(include_str!("../../tests/data/langs/zig-build-test.txt"), Path::new("/tmp/fxw2/zig/proj"));
        assert_eq!((r.passed, r.failed), (3, 1));
        assert_eq!((r.cases[0].name.as_str(), r.cases[0].path.as_deref(), r.cases[0].line), ("root.test.zc fails", Some("src/root.zig"), Some(20)));
        assert_eq!(parse_summary("1 passed; 0 skipped; 1 failed."), Some((1, 0, 1)));
    }

    struct Job {
        _t: TempDir,
        src: PathBuf,
        project: PathBuf,
        out: PathBuf,
    }

    fn job(files: &[(&str, &str)]) -> Job {
        let t = TempDir::new("zig");
        let src = t.path().join("proj/src");
        std::fs::create_dir_all(&src).unwrap();
        for (p, text) in files {
            let f = src.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Job { project: t.path().join("proj"), src, out: t.path().join("out"), _t: t }
    }

    fn run_job(j: &Job, kind: Kind, entry: Option<&str>) -> (JobResult, String) {
        let cancelled = AtomicBool::new(false);
        let opts = JobOpts { entry: entry.map(str::to_string), ..Default::default() };
        let ctx = JobCtx { job: "j", kind, lang: Lang::Zig, opts: &opts, src: &j.src, project_dir: &j.project, out_dir: &j.out, cancelled: &cancelled };
        let out = Collect::default();
        let r = run(&ctx, &out).unwrap_or_else(|f| panic!("{f:?}\n{}", out.text()));
        (r, out.text())
    }

    fn is_aarch64(j: &Job, a: &crate::pack::Packed) -> bool {
        let back = j.out.join("back");
        assert!(Command::new("zstd").args(["-q", "-f", "-d", "-o"]).arg(&back).arg(&a.zpath).status().unwrap().success());
        String::from_utf8_lossy(&Command::new("file").arg(&back).output().unwrap().stdout).contains("ARM aarch64")
    }

    const HELLO: &str = "const std = @import(\"std\");\npub fn main() void {\n    std.debug.print(\"日本\\n\", .{});\n}\nfn add(a: i32, b: i32) i32 { return a + b; }\ntest \"add\" {\n    try std.testing.expect(add(1, 2) == 4);\n}\ntest \"ok\" {\n    try std.testing.expect(add(1, 2) == 3);\n}\n";

    #[test]
    fn e2e_single_file() {
        if skip_without(&["zig", "zstd", "file", "aarch64-linux-gnu-strip"]) {
            return;
        }
        let j = job(&[("hello.zig", HELLO)]);
        let (r, log) = run_job(&j, Kind::Check, Some("hello.zig"));
        assert_eq!((r.exit, r.diagnostics.len()), (0, 0), "{log}");
        let (r, log) = run_job(&j, Kind::Build, Some("hello.zig"));
        assert_eq!(r.exit, 0, "{log}");
        let a = r.artifact.as_ref().unwrap();
        assert_eq!(a.name, "hello");
        assert!(is_aarch64(&j, a));
        let (r, log) = run_job(&j, Kind::Test, Some("hello.zig"));
        let t = r.tests.unwrap();
        assert_eq!((t.passed, t.failed), (1, 1), "{log}");
        assert_eq!(t.cases.iter().find(|c| !c.passed).unwrap().line, Some(6));
        // 構文の誤り（ast-check）と意味の誤り（build-exe）
        let j = job(&[("a.zig", "// 日本語\npub fn main() void {\n    const x: u32 = \"日本\"\n}\n"), ("b.zig", "pub fn main() void {\n    const s: []const u8 = \"日本\";\n    const n: u32 = s;\n    _ = n;\n}\n")]);
        let (r, log) = run_job(&j, Kind::Check, Some("a.zig"));
        assert_ne!(r.exit, 0, "{log}");
        assert_eq!(r.diagnostics[0].diags[0].line, 2);
        let (r, log) = run_job(&j, Kind::Check, Some("b.zig"));
        assert_ne!(r.exit, 0, "{log}");
        assert_eq!((r.diagnostics[0].path.as_str(), r.diagnostics[0].diags[0].line, r.diagnostics[0].diags[0].col), ("b.zig", 2, Some(19)));
    }

    #[test]
    fn e2e_build_zig_project() {
        if skip_without(&["zig", "zstd", "file", "aarch64-linux-gnu-strip"]) {
            return;
        }
        let build = "const std = @import(\"std\");\npub fn build(b: *std.Build) void {\n    const target = b.standardTargetOptions(.{});\n    const optimize = b.standardOptimizeOption(.{});\n    const exe = b.addExecutable(.{ .name = \"app\", .root_module = b.createModule(.{ .root_source_file = b.path(\"src/main.zig\"), .target = target, .optimize = optimize }) });\n    b.installArtifact(exe);\n    const t = b.addTest(.{ .root_module = exe.root_module });\n    const run_t = b.addRunArtifact(t);\n    b.step(\"test\", \"Run tests\").dependOn(&run_t.step);\n}\n";
        let j = job(&[("build.zig", build), ("src/main.zig", HELLO)]);
        let (r, log) = run_job(&j, Kind::Check, Some("src/main.zig"));
        assert_eq!(r.exit, 0, "{log}");
        let (r, log) = run_job(&j, Kind::Build, Some("src/main.zig"));
        assert_eq!(r.exit, 0, "{log}");
        let a = r.artifact.as_ref().unwrap();
        assert_eq!(a.name, "app");
        assert!(is_aarch64(&j, a));
        let (r, log) = run_job(&j, Kind::Test, Some("src/main.zig"));
        let t = r.tests.unwrap();
        assert_eq!((t.passed, t.failed), (1, 1), "{log}");
        assert_eq!(t.cases[0].path.as_deref(), Some("src/main.zig"));
        // 作業の木にキャッシュを作らない
        assert!(!j.src.join(".zig-cache").exists());
        // 誤り
        let j = job(&[("build.zig", build), ("src/main.zig", "pub fn main() void {\n    const n: u32 = \"日本\";\n    _ = n;\n}\n")]);
        let (r, log) = run_job(&j, Kind::Check, Some("src/main.zig"));
        assert_ne!(r.exit, 0, "{log}");
        assert_eq!((r.diagnostics[0].path.as_str(), r.diagnostics[0].diags[0].line), ("src/main.zig", 1), "{log}");
    }
}
