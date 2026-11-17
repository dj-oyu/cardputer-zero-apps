//! MoonBit のジョブ（docs/stage3-design.md §4.4）。2026-11-17 に足した。
//!
//! 木: 根に `moon.mod`（古い形の `moon.mod.json` も）があればモジュール、無ければ 1 ファイルだけ
//! （`moon check ./x.mbt` の単独ファイルの形。moon は「非推奨、.mbtx を使う」と言うが動く）。
//!
//! | 種類 | 中でやること |
//! |---|---|
//! | check | `moon check --target native --output-json` |
//! | build | `MOON_CC=aarch64-linux-gnu-gcc MOON_AR=aarch64-linux-gnu-ar moon build --target native --release --output-json`。**native の後ろ向きのクロスビルド**: moon は MOON_CC がホスト向けでないと分かると、x86_64 用に配っている `libmoonbitrun.o`（mimalloc）と `libbacktrace.a` を使わず、ランタイムを C のソース（`$MOON_HOME/lib/runtime/*.c`）からクロスの gcc でコンパイルし、システムの malloc を使う（0.10.20 で確かめた）。出てくるのは動的リンクの PIE（glibc。実機と同じ Debian 13） |
//! | test | `moon test --target native --test-failure-json`（**x86_64 のまま**。C は `/usr/bin/cc`） |
//!
//! - 出力は `<project_dir>/moon-target`（`--target-dir`）。作業の木の `_build` は、写すたびに消される
//!   ので使わない
//! - 版: moon 0.1.20261104・moonc v0.10.20+65efd5b93（イメージの ARG）。core は native だけ束ねてある
//!   （wasm・js の検査はしない。`preferred_target` は見ない）
//! - **`moon.mod.json` の木は、この版の moon が断る**（「moon fmt で moon.mod に移す」）。その誤りが
//!   そのまま位置なしの診断になる
//!
//! # 診断（`--output-json`。tests/data/langs/moon-check.json）
//!
//! - 1 行 1 件の JSON（`$message_type = diagnostic`）。`level` は error / warning、`path` は絶対パス、
//!   `loc` は `行:桁-行:桁`（1 始まり、**桁は文字（コードポイント）**。絵文字で確かめた）、
//!   `error_code` は数（`code` は 4 桁の `0002` の形）。メッセージの改行は ` / ` に
//!
//! # 試験（`--test-failure-json`）
//!
//! - 落ちた試験ごとに `{"package","filename","test_name","message"}`。場所は `message` の頭の
//!   `x_test.mbt:5:3-5:22@パッケージ`、`inspect` なら `@EXPECT_FAILED {"loc": {"filename", "start_line"}}`
//!   から取る（パスは木の根からか、パッケージのフォルダから。あるほうを使う）
//! - 件数は `Total tests: N, passed: P, failed: F.`

use super::{
    char_col0, check_cancelled, failure_line, finish_diags, has_error, io_failure, line0, lines_from, note_diag, push_diag, rel_to_root, safe_entry, spawn_failure, spawn_tool, split_loc,
    tool_env, truncate_utf8, which, JobCtx, JobFailure, JobOutput, JobResult, LineSource, Tap, TestResults,
};
use crate::pack::pack;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use zc_wire::msg::{ArtifactKind, Diag, FileDiags, Kind, Severity, Status, TestCase};

/// 実機向けのクロスの C コンパイラ（`MOON_CC`）とアーカイバ（`MOON_AR`）。
pub const CROSS_CC: &str = "aarch64-linux-gnu-gcc";
pub const CROSS_AR: &str = "aarch64-linux-gnu-ar";
const MAX_TEST_MESSAGE: usize = 2 << 10;

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    let moon = which("moon")?;
    let src = ctx.src.canonicalize().map_err(|e| io_failure("作業の木が無い", e))?;
    std::fs::create_dir_all(ctx.project_dir).map_err(|e| io_failure("プロジェクトのフォルダを作れない", e))?;
    let pdir = ctx.project_dir.canonicalize().map_err(|e| io_failure("プロジェクトのフォルダが無い", e))?;
    let tdir = pdir.join("moon-target").display().to_string();
    let module = module_name(&src);
    let entry = match ctx.opts.entry.as_deref() {
        Some(e) if safe_entry(e) && src.join(e).is_file() => Some(e.to_string()),
        Some(e) => return Ok(super::failed_result(1, format!("入口のファイルが無い: {e}"))),
        None => None,
    };
    let mut single = match (&module, &entry) {
        (Some(_), _) => None,
        (None, Some(e)) => Some(format!("./{e}")),
        (None, None) => return Ok(super::failed_result(1, "moon.mod も入口のファイルも無い")),
    };
    // 1 ファイルの build: `moon build` は単独の `.mbt` を受けない（`.mbtx` だけ）ので、
    // `<project_dir>/moon-single/<名前>.mbtx` に写して作る。診断のパスは元のファイルに戻す
    let mut copied: Option<(String, String)> = None;
    if let (Some(e), Kind::Build) = (entry.as_deref().filter(|_| single.is_some()), ctx.kind) {
        let dir = pdir.join("moon-single");
        std::fs::create_dir_all(&dir).map_err(|e| io_failure("写しのフォルダを作れない", e))?;
        let stem = Path::new(e).file_stem().and_then(|s| s.to_str()).unwrap_or("main");
        let copy = dir.join(format!("{stem}.mbtx"));
        std::fs::copy(src.join(e), &copy).map_err(|e| io_failure("写せない", e))?;
        single = Some(copy.display().to_string());
        copied = Some((copy.display().to_string(), src.join(e).display().to_string()));
    }
    let lines = lines_from(&src);
    let mut diags = Vec::new();
    let mut result = JobResult { complete: true, ..Default::default() };

    let (sub, extra): (&str, &[&str]) = match ctx.kind {
        Kind::Check => ("check", &["--output-json"]),
        Kind::Build => ("build", &["--release", "--output-json"]),
        Kind::Test => ("test", &["--output-json", "--test-failure-json"]),
    };
    let mut cmd = Command::new(&moon);
    cmd.current_dir(&src).arg(sub).args(["--target", "native", "--target-dir", &tdir]).args(extra);
    if let Some(f) = &single {
        cmd.arg(f);
    }
    if ctx.kind == Kind::Build {
        cmd.env("MOON_CC", CROSS_CC).env("MOON_AR", CROSS_AR);
    }
    tool_env(&mut cmd);
    check_cancelled(ctx)?;
    let tap = Tap::new(out);
    let mut stdout = String::new();
    let exit = spawn_tool(cmd, &tap, &mut |l| {
        // JSON の行はログに出さない（下で人が読む形にする）
        if stdout.len() < (8 << 20) {
            stdout.push_str(&String::from_utf8_lossy(l));
        }
        !l.starts_with(b"{")
    })
    .map_err(|e| spawn_failure("moon", e))?;
    check_cancelled(ctx)?;
    if let Some((copy, orig)) = &copied {
        let esc = |s: &str| serde_json::to_string(s).unwrap_or_default();
        stdout = stdout.replace(&format!("\"path\":{}", esc(copy)), &format!("\"path\":{}", esc(orig)));
    }
    parse_moon_diags(&stdout, &src, &lines, &mut diags);
    super::python::log_diags(out, &diags);
    result.exit = exit;
    result.complete = exit < 128;

    match ctx.kind {
        Kind::Test => {
            let tests = parse_moon_tests(&stdout, module.as_deref().unwrap_or(""), &src);
            if exit != 0 && tests.failed == 0 && !has_error(&diags) {
                let msg = failure_line(&tap.take()).unwrap_or_else(|| format!("moon test が失敗した（終了コード {exit}）"));
                push_diag(&mut diags, "", note_diag(Severity::Error, msg));
            }
            result.tests = Some(tests);
        }
        _ => {
            if exit != 0 && !has_error(&diags) {
                let msg = failure_line(&tap.take()).unwrap_or_else(|| format!("moon {sub} が失敗した（終了コード {exit}）"));
                push_diag(&mut diags, "", note_diag(Severity::Error, msg));
            } else if exit == 0 && has_error(&diags) {
                result.exit = 1;
            }
            if ctx.kind == Kind::Build && result.exit == 0 {
                let exes = artifacts(&stdout);
                match pick_exe(&exes, entry.as_deref(), module.as_deref()) {
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
    }
    result.diagnostics = finish_diags(diags);
    Ok(result)
}

/// モジュールの名前（`moon.mod` の `name = "…"`、古い `moon.mod.json` の `"name"`）。モジュールで
/// なければ `None`。
pub fn module_name(src: &Path) -> Option<String> {
    if let Ok(t) = std::fs::read_to_string(src.join("moon.mod")) {
        let name = t.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == "name").then(|| v.trim().trim_matches('"').to_string())
        });
        return Some(name.unwrap_or_default());
    }
    let t = std::fs::read_to_string(src.join("moon.mod.json")).ok()?;
    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
    Some(v.get("name").and_then(Value::as_str).unwrap_or("").to_string())
}

/// `--output-json` の診断を読む。
pub fn parse_moon_diags(stdout: &str, src_root: &Path, lines: &LineSource<'_>, into: &mut Vec<FileDiags>) {
    for l in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
        if v.get("$message_type").and_then(Value::as_str) != Some("diagnostic") {
            continue;
        }
        let severity = match v.get("level").and_then(Value::as_str) {
            Some("error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => continue,
        };
        let message = truncate_utf8(&v.get("message").and_then(Value::as_str).unwrap_or("").split('\n').map(str::trim).collect::<Vec<_>>().join(" / "), 400);
        let code = v.get("error_code").and_then(Value::as_u64).map(|n| format!("{n:04}"));
        let path = v.get("path").and_then(Value::as_str).unwrap_or("");
        let loc = v.get("loc").and_then(Value::as_str).and_then(parse_loc);
        match (rel_to_root(src_root, src_root, path), loc) {
            (Some(rel), Some(((l1, c1), end))) => {
                let line = line0(l1);
                let col = char_col0(lines, &rel, line, c1);
                let end = end.map(|(l2, c2)| (line0(l2), char_col0(lines, &rel, line0(l2), c2)));
                push_diag(into, &rel, Diag { line, col: Some(col), end, severity, code, message });
            }
            _ => push_diag(into, "", Diag { line: 0, col: None, end: None, severity, code, message }),
        }
    }
}

/// `3:17-3:21` → （始まり, 終わり）。
type LineCol = (u32, u32);

fn parse_loc(s: &str) -> Option<(LineCol, Option<LineCol>)> {
    let pair = |p: &str| -> Option<(u32, u32)> {
        let (a, b) = p.split_once(':')?;
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    };
    match s.split_once('-') {
        Some((a, b)) => Some((pair(a)?, pair(b))),
        None => Some((pair(s)?, None)),
    }
}

/// `moon build --output-json` の成果物（パッケージ, パス）。JSON の形
/// `{"$message_type":"artifact","package":…,"target_backend":"native","path":…}` と、
/// `--output-json` なしの `Artifacts:` の行の形（`パッケージ  native  パス`）の両方を読む。
fn artifacts(stdout: &str) -> Vec<(String, PathBuf)> {
    stdout
        .lines()
        .filter_map(|l| {
            if let Ok(v) = serde_json::from_str::<Value>(l) {
                let s = |k: &str| v.get(k).and_then(Value::as_str);
                return (s("$message_type")? == "artifact" && s("target_backend")? == "native" && s("path")?.ends_with(".exe")).then(|| (s("package").unwrap_or("").to_string(), PathBuf::from(s("path").unwrap_or(""))));
            }
            let mut it = l.split_whitespace();
            let (pkg, backend, path) = (it.next()?, it.next()?, it.next()?);
            (backend == "native" && path.ends_with(".exe") && it.next().is_none()).then(|| (pkg.to_string(), PathBuf::from(path)))
        })
        .collect()
}

/// 実行ファイルを選ぶ。名前は 1 ファイルならその名前、モジュールならパッケージの最後の要素
/// （`main` ならモジュールの名前の最後の要素）。
fn pick_exe(exes: &[(String, PathBuf)], entry: Option<&str>, module: Option<&str>) -> Result<(PathBuf, String), String> {
    let chosen = match exes {
        [] => return Err("実行ファイルができなかった（main のパッケージが無い？）".into()),
        [one] => one,
        many => {
            let dir = entry.and_then(|e| Path::new(e).parent()).and_then(|p| p.to_str()).unwrap_or("");
            let want = match module {
                Some(m) if !dir.is_empty() => format!("{m}/{dir}"),
                Some(m) => m.to_string(),
                None => String::new(),
            };
            match many.iter().find(|(p, _)| *p == want) {
                Some(x) => x,
                None => return Err(format!("実行ファイルが複数ある: {}", many.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>().join(" "))),
            }
        }
    };
    let name = match (module, entry) {
        (None, Some(e)) => Path::new(e).file_stem().and_then(|s| s.to_str()).unwrap_or("main").to_string(),
        _ => {
            let last = chosen.0.rsplit('/').next().unwrap_or("main");
            if last == "main" {
                module.and_then(|m| m.rsplit('/').next()).filter(|s| !s.is_empty()).unwrap_or("main").to_string()
            } else {
                last.to_string()
            }
        }
    };
    let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    Ok((chosen.1.clone(), name))
}

/// `moon test --test-failure-json` の出力を読む。
pub fn parse_moon_tests(stdout: &str, module: &str, src_root: &Path) -> TestResults {
    let mut r = TestResults::default();
    for l in stdout.lines() {
        if let Some(rest) = l.strip_prefix("Total tests: ") {
            // 「Total tests: 3, passed: 1, failed: 2.」
            let num = |k: &str| rest.split(", ").find_map(|p| p.trim_end_matches('.').strip_prefix(k)?.parse::<u32>().ok());
            r.passed = num("passed: ").unwrap_or(0);
            r.failed = num("failed: ").unwrap_or(0);
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
        let Some(name) = v.get("test_name").and_then(Value::as_str) else { continue };
        let pkg = v.get("package").and_then(Value::as_str).unwrap_or("");
        let message = v.get("message").and_then(Value::as_str).unwrap_or("");
        let pkg_dir = pkg.strip_prefix(module).map(|d| d.trim_start_matches('/')).unwrap_or("");
        let mut case = TestCase { name: name.to_string(), passed: false, path: None, line: None, message: String::new() };
        let resolve = |p: &str| -> Option<String> {
            let direct = rel_to_root(src_root, src_root, p).filter(|r| src_root.join(r).is_file());
            direct.or_else(|| rel_to_root(src_root, &src_root.join(pkg_dir), p))
        };
        if let Some(json) = message.strip_prefix("@EXPECT_FAILED ") {
            let e: Value = serde_json::from_str(json).unwrap_or(Value::Null);
            if let (Some(f), Some(ln)) = (e.pointer("/loc/filename").and_then(Value::as_str), e.pointer("/loc/start_line").and_then(Value::as_u64)) {
                case.path = resolve(f);
                case.line = case.path.as_ref().map(|_| line0(ln as u32));
            }
            let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            case.message = truncate_utf8(&format!("inspect: 期待 {:?}、実際 {:?}", s("expect"), s("actual")), MAX_TEST_MESSAGE);
        } else {
            let head = message.split(['@', ' ']).next().unwrap_or("");
            if let Some((p, ln, _, _)) = split_loc(&format!("{head}:")) {
                case.path = resolve(p);
                case.line = case.path.as_ref().map(|_| line0(ln));
            }
            let body = message.split_once(" FAILED: ").map(|(_, b)| b).unwrap_or(message);
            case.message = truncate_utf8(body, MAX_TEST_MESSAGE);
        }
        r.cases.push(case);
    }
    r.failed = r.failed.max(r.cases.len() as u32);
    r
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{skip_without, Collect, TempDir};
    use super::*;
    use std::sync::atomic::AtomicBool;
    use zc_wire::msg::{JobOpts, Lang};

    fn lines_of(text: &'static str) -> impl Fn(&str, u32) -> Option<String> {
        move |_: &str, l: u32| text.split('\n').nth(l as usize).map(str::to_string)
    }

    #[test]
    fn diagnostics_are_read() {
        // fixture の元（tests/data/langs/gen.sh）
        let src = "// 日本語\nfn helper() -> Int {\n  let x : Int = \"日本\"\n  let unused = 1\n  x\n}\n";
        let mut v = Vec::new();
        parse_moon_diags(include_str!("../../tests/data/langs/moon-check.json"), Path::new("/tmp/fxw/mb/hello"), &lines_of(src), &mut v);
        let v = finish_diags(v);
        assert_eq!(v[0].path, "cmd/main/err.mbt");
        let d = &v[0].diags;
        assert_eq!(d.len(), 3);
        let e = d.iter().find(|d| d.severity == Severity::Error).unwrap();
        // `  let x : Int = "日本"`: 字 17 → バイト 16、終わりは字 21 → バイト 24
        assert_eq!((e.line, e.col, e.end), (2, Some(16), Some((2, 24))));
        assert_eq!(e.code.as_deref(), Some("4014"));
        assert_eq!(e.message, "Expr Type Mismatch / has type : String / wanted   : Int");
        // 絵文字（コードポイントで数える）
        let mut v = Vec::new();
        let line = r#"{"$message_type":"diagnostic","level":"error","error_code":4014,"path":"/w/err.mbt","loc":"3:31-3:34","message":"m"}"#;
        parse_moon_diags(line, Path::new("/w"), &lines_of("///|\nfn helper() -> Int {\n  let _ = \"😀😀\"; let x : Int = \"s\"\n"), &mut v);
        assert_eq!(v[0].diags[0].col, Some("  let _ = \"😀😀\"; let x : Int = ".len() as u32));
        assert_eq!(parse_loc("1:2"), Some(((1, 2), None)));
    }

    #[test]
    fn test_failures_are_read() {
        let t = TempDir::new("mbt");
        std::fs::create_dir_all(t.path().join("sub")).unwrap();
        std::fs::write(t.path().join("hello_test.mbt"), "").unwrap();
        std::fs::write(t.path().join("sub/sub_test.mbt"), "").unwrap();
        let r = parse_moon_tests(include_str!("../../tests/data/langs/moon-test-json.txt"), "username/hello", t.path());
        assert_eq!((r.passed, r.failed), (1, 2));
        assert_eq!((r.cases[0].name.as_str(), r.cases[0].path.as_deref(), r.cases[0].line), ("fails", Some("hello_test.mbt"), Some(4)));
        assert!(r.cases[0].message.starts_with("`2 != 3`"), "{}", r.cases[0].message);
        assert_eq!((r.cases[1].path.as_deref(), r.cases[1].line), (Some("sub/sub_test.mbt"), Some(1)));
        assert!(r.cases[1].message.contains("\"2\""));
    }

    #[test]
    fn artifacts_and_names() {
        let out = "Finished. moon: ran 31 tasks, now up to date\nArtifacts:\n  username/hello/cmd/main  native  /p/_build/native/release/build/cmd/main/main.exe\n  username/hello/tool  native  /p/t/tool.exe\n";
        let a = artifacts(out);
        assert_eq!(a.len(), 2);
        // --output-json の形（moon 0.1.20261104 の実物）
        let j = artifacts(r#"{"$message_type":"artifact","package":"zc/hello/cmd/main","target_backend":"native","path":"/tmp/m2t/native/release/build/cmd/main/main.exe"}"#);
        assert_eq!(j, [("zc/hello/cmd/main".to_string(), PathBuf::from("/tmp/m2t/native/release/build/cmd/main/main.exe"))]);
        assert_eq!(pick_exe(&a, Some("cmd/main/main.mbt"), Some("username/hello")).unwrap().1, "hello");
        assert_eq!(pick_exe(&a, Some("tool/x.mbt"), Some("username/hello")).unwrap().1, "tool");
        assert!(pick_exe(&a, Some("lib.mbt"), Some("username/hello")).is_err());
        assert_eq!(pick_exe(&a[..1], None, None).unwrap().0, PathBuf::from("/p/_build/native/release/build/cmd/main/main.exe"));
        assert_eq!(pick_exe(&a[1..], Some("hi.mbt"), None).unwrap().1, "hi");
    }

    struct Job {
        _t: TempDir,
        src: PathBuf,
        project: PathBuf,
        out: PathBuf,
    }

    fn job(files: &[(&str, &str)]) -> Job {
        let t = TempDir::new("moon");
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
        let ctx = JobCtx { job: "j", kind, lang: Lang::Moonbit, opts: &opts, src: &j.src, project_dir: &j.project, out_dir: &j.out, cancelled: &cancelled };
        let out = Collect::default();
        let r = run(&ctx, &out).unwrap_or_else(|f| panic!("{f:?}\n{}", out.text()));
        (r, out.text())
    }

    fn is_aarch64(j: &Job, a: &crate::pack::Packed) -> bool {
        let back = j.out.join("back");
        assert!(Command::new("zstd").args(["-q", "-f", "-d", "-o"]).arg(&back).arg(&a.zpath).status().unwrap().success());
        String::from_utf8_lossy(&Command::new("file").arg(&back).output().unwrap().stdout).contains("ARM aarch64")
    }

    #[test]
    fn e2e_module() {
        if skip_without(&["moon", CROSS_CC, "zstd", "file", "aarch64-linux-gnu-strip"]) {
            return;
        }
        let files = [
            ("moon.mod", "name = \"zc/hello\"\n\nversion = \"0.1.0\"\n"),
            ("moon.pkg", ""),
            ("lib.mbt", "///|\npub fn add(a : Int, b : Int) -> Int {\n  a + b\n}\n"),
            ("lib_test.mbt", "///|\ntest \"add\" {\n  assert_eq(@hello.add(1, 2), 4)\n}\n\n///|\ntest \"ok\" {\n  assert_eq(@hello.add(1, 2), 3)\n}\n"),
            ("cmd/main/moon.pkg", "import {\n  \"zc/hello\" @lib,\n}\n\npkgtype(kind: \"executable\")\n"),
            ("cmd/main/main.mbt", "///|\nfn main {\n  println(\"日本 \\{@lib.add(1, 2)}\")\n}\n"),
        ];
        let j = job(&files);
        let (r, log) = run_job(&j, Kind::Check, Some("cmd/main/main.mbt"));
        assert_eq!(r.exit, 0, "{log}");
        assert!(!has_error(&r.diagnostics), "{:?}", r.diagnostics);
        let (r, log) = run_job(&j, Kind::Build, Some("cmd/main/main.mbt"));
        assert_eq!(r.exit, 0, "{log}");
        let a = r.artifact.as_ref().unwrap();
        assert_eq!(a.name, "hello");
        assert!(is_aarch64(&j, a));
        assert!(!j.src.join("_build").exists(), "作業の木に _build を作らない");
        let (r, log) = run_job(&j, Kind::Test, Some("lib.mbt"));
        let t = r.tests.unwrap();
        assert_eq!((t.passed, t.failed), (1, 1), "{log}");
        assert_eq!((t.cases[0].path.as_deref(), t.cases[0].line), (Some("lib_test.mbt"), Some(2)), "{t:?}");
        // 型の誤り（日本語の後ろ）
        let mut bad = files.to_vec();
        bad[5] = ("cmd/main/main.mbt", "///|\nfn main {\n  let _ = \"日本\"; let s : Int = \"x\"\n  println(s)\n}\n");
        let j = job(&bad);
        let (r, log) = run_job(&j, Kind::Check, Some("cmd/main/main.mbt"));
        assert_ne!(r.exit, 0, "{log}");
        let d = r.diagnostics.iter().find(|f| f.path == "cmd/main/main.mbt").unwrap().diags.iter().find(|d| d.severity == Severity::Error).unwrap().clone();
        assert_eq!((d.line, d.col), (2, Some("  let _ = \"日本\"; let s : Int = ".len() as u32)), "{log}");
    }

    #[test]
    fn e2e_single_file() {
        if skip_without(&["moon", CROSS_CC, "zstd", "file", "aarch64-linux-gnu-strip"]) {
            return;
        }
        let j = job(&[("hi.mbt", "///|\nfn main {\n  println(\"日本\")\n}\n")]);
        let (r, log) = run_job(&j, Kind::Check, Some("hi.mbt"));
        assert_eq!(r.exit, 0, "{log}");
        let (r, log) = run_job(&j, Kind::Build, Some("hi.mbt"));
        assert_eq!(r.exit, 0, "{log}");
        let a = r.artifact.as_ref().unwrap();
        assert_eq!(a.name, "hi");
        assert!(is_aarch64(&j, a));
        let j = job(&[("bad.mbt", "fn main {\n  let x : Int = \"s\"\n  println(x)\n}\n")]);
        let (r, log) = run_job(&j, Kind::Check, Some("bad.mbt"));
        assert_ne!(r.exit, 0, "{log}");
        assert_eq!((r.diagnostics[0].path.as_str(), r.diagnostics[0].diags[0].line, r.diagnostics[0].diags[0].col), ("bad.mbt", 1, Some(16)));
    }
}
