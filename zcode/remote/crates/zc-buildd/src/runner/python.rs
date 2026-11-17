//! Python のジョブ（docs/stage3-design.md §4.4）。2026-11-17 に足した。
//!
//! | 種類 | 中でやること |
//! |---|---|
//! | check | `pyright --outputjson --pythonversion 3.13 --pythonplatform Linux`（根が木なら引数なしで木全体。設定は `pyproject.toml` の `[tool.pyright]` が効く。1 ファイルだけなら `./<entry>`） |
//! | build | 無い（実機の `python3` でそのまま動かす） |
//! | test | 無い |
//!
//! - pyright は npm の `pyright`（イメージの `/opt/tools`。typeshed の標準ライブラリの型は同梱）。
//!   版は Python 3.13（実機の Python）に合わせる
//! - **入っていない外部のパッケージの import は warning**（`reportMissingImports`・
//!   `reportMissingModuleSource`・`reportMissingTypeStubs`）。ビルド係には実機の
//!   site-packages が無いので、error にすると何でも赤くなる
//! - 重さ: error / warning だけ（information は捨てる）。error が無ければ終了コードは 0
//!   （import を warning にしたので、pyright の終了コード 1 のままにしない）
//! - 位置: `range.start` の `line` は 0 始まり、`character` は **0 始まりの UTF-16 の単位**
//!   （LSP と同じ）。その行を読んで UTF-8 のバイト位置に直す。パスは絶対パスで来るので木の根からの
//!   相対に直す（木の外は位置なし）
//! - 標準出力の JSON はログに出さず、`パス:行:桁: error: …` の形でログへ書く

use super::{
    check_cancelled, failure_line, finish_diags, has_error, io_failure, lines_from, note_diag, push_diag, rel_to_root, safe_entry, spawn_failure, spawn_tool, tool_env, truncate_utf8,
    unsupported, utf16_col0, which, JobCtx, JobFailure, JobOutput, JobResult, LineSource, Tap,
};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use zc_wire::msg::{Diag, FileDiags, Kind, Severity};

/// 実機の Python の版。
pub const PYTHON_VERSION: &str = "3.13";
/// warning に下げる規則（入っていない外部のパッケージ）。
pub const MISSING_IMPORT_RULES: &[&str] = &["reportMissingImports", "reportMissingModuleSource", "reportMissingTypeStubs"];

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    if ctx.kind != Kind::Check {
        return Err(unsupported("Python", ctx.kind, "実機の python3 で実行する"));
    }
    let pyright = which("pyright")?;
    let src = ctx.src.canonicalize().map_err(|e| io_failure("作業の木が無い", e))?;
    let mut cmd = Command::new(&pyright);
    cmd.current_dir(&src).args(["--outputjson", "--pythonversion", PYTHON_VERSION, "--pythonplatform", "Linux"]);
    let tree = src.join("pyproject.toml").is_file() || src.join("setup.cfg").is_file();
    if !tree {
        match ctx.opts.entry.as_deref() {
            Some(e) if safe_entry(e) && src.join(e).is_file() => {
                cmd.arg(format!("./{e}"));
            }
            Some(e) => return Ok(super::failed_result(1, format!("入口のファイルが無い: {e}"))),
            None => return Ok(super::failed_result(1, "検査するファイルが分からない")),
        }
    }
    tool_env(&mut cmd);
    check_cancelled(ctx)?;
    let tap = Tap::new(out);
    let mut json = String::new();
    let exit = spawn_tool(cmd, &tap, &mut |line| {
        if json.len() < (8 << 20) {
            json.push_str(&String::from_utf8_lossy(line));
        }
        false
    })
    .map_err(|e| spawn_failure("pyright", e))?;
    check_cancelled(ctx)?;

    let mut diags = Vec::new();
    let parsed = parse_pyright(&json, &src, &lines_from(&src), &mut diags);
    log_diags(out, &diags);
    let mut result = JobResult { complete: exit < 128, ..Default::default() };
    // 0 = 誤りなし、1 = 誤りあり。それ以外（2 = 致命的・3 = 設定・4 = 引数）と読めない JSON は失敗
    if !parsed || exit > 1 {
        let msg = failure_line(&tap.take()).unwrap_or_else(|| format!("pyright が失敗した（終了コード {exit}）"));
        push_diag(&mut diags, "", note_diag(Severity::Error, msg));
        result.exit = if exit == 0 { 1 } else { exit };
    } else {
        result.exit = has_error(&diags) as i32;
    }
    result.diagnostics = finish_diags(diags);
    Ok(result)
}

/// 人が読む形でログへ。
pub(crate) fn log_diags(out: &dyn JobOutput, diags: &[FileDiags]) {
    for f in diags {
        for d in &f.diags {
            let sev = if d.severity == Severity::Error { "error" } else { "warning" };
            let code = d.code.as_deref().map(|c| format!("[{c}]")).unwrap_or_default();
            if f.path.is_empty() {
                out.log(format!("{sev}{code}: {}\n", d.message).as_bytes());
            } else {
                out.log(format!("{}:{}:{}: {sev}{code}: {}\n", f.path, d.line + 1, d.col.map(|c| c + 1).unwrap_or(0), d.message).as_bytes());
            }
        }
    }
}

/// `pyright --outputjson` の出力を読む。JSON として読めなければ `false`。
pub fn parse_pyright(json: &str, src_root: &Path, lines: &LineSource<'_>, into: &mut Vec<FileDiags>) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(json.trim()) else { return false };
    let Some(arr) = v.get("generalDiagnostics").and_then(Value::as_array) else { return false };
    for d in arr {
        let rule = d.get("rule").and_then(Value::as_str);
        let severity = match d.get("severity").and_then(Value::as_str) {
            _ if rule.is_some_and(|r| MISSING_IMPORT_RULES.contains(&r)) => Severity::Warning,
            Some("error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => continue,
        };
        let message = d.get("message").and_then(Value::as_str).unwrap_or("").replace('\n', " / ");
        let message = truncate_utf8(&message, 400);
        let code = rule.map(str::to_string);
        let file = d.get("file").and_then(Value::as_str).unwrap_or("");
        let pos = |k: &str| -> Option<(u32, u32)> {
            let p = d.get("range")?.get(k)?;
            Some((p.get("line")?.as_u64()? as u32, p.get("character")?.as_u64()? as u32))
        };
        match (rel_to_root(src_root, src_root, file), pos("start")) {
            (Some(path), Some((line, ch))) => {
                let col = utf16_col0(lines, &path, line, ch);
                let end = pos("end").map(|(l, c)| (l, utf16_col0(lines, &path, l, c)));
                push_diag(into, &path, Diag { line, col: Some(col), end, severity, code, message });
            }
            _ => push_diag(into, "", Diag { line: 0, col: None, end: None, severity, code, message }),
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{skip_without, Collect, TempDir};
    use super::*;
    use std::sync::atomic::AtomicBool;
    use zc_wire::msg::{JobOpts, Lang};

    /// fixture の元（tests/data/langs/gen.sh）。
    const A_PY: &str = "import requests\nimport os\n# 日本語\nx: int = \"日本\" + 1\ndef f() -> str:\n    return 1\nprint(undefined_name)\n";

    fn lines_of(text: &'static str) -> impl Fn(&str, u32) -> Option<String> {
        move |_: &str, l: u32| text.split('\n').nth(l as usize).map(str::to_string)
    }

    #[test]
    fn pyright_json_is_read() {
        let json = include_str!("../../tests/data/langs/pyright.json");
        let mut v = Vec::new();
        assert!(parse_pyright(json, Path::new("/tmp/fxw/py"), &lines_of(A_PY), &mut v));
        let v = finish_diags(v);
        let a = &v.iter().find(|f| f.path == "a.py").unwrap().diags;
        // import requests は warning のまま、残りは error
        assert_eq!((a[0].line, a[0].col, a[0].severity, a[0].code.as_deref()), (0, Some(7), Severity::Warning, Some("reportMissingModuleSource")));
        // `x: int = "日本" + 1`: 字 9（UTF-16）→ バイト 9、終わりは字 17 → バイト 21
        assert_eq!((a[1].line, a[1].col, a[1].end), (3, Some(9), Some((3, 21))));
        assert!(a[2].message.contains(" / "), "改行は / にする: {}", a[2].message);
        assert_eq!((a[3].line, a[3].col, a[3].severity), (6, Some(6), Severity::Error));
        // 読めない出力
        assert!(!parse_pyright("No configuration file found.\n", Path::new("/"), &lines_of(""), &mut Vec::new()));
    }

    #[test]
    fn missing_imports_are_warnings_and_utf16_columns() {
        // pyright 1.1.414 の実物（絵文字 = UTF-16 で 2 単位）
        let json = r#"{"generalDiagnostics":[
          {"file":"/w/src/e.py","severity":"error","message":"Import \"numpy\" could not be resolved","range":{"start":{"line":0,"character":7},"end":{"line":0,"character":12}},"rule":"reportMissingImports"},
          {"file":"/w/src/e.py","severity":"error","message":"Type mismatch","range":{"start":{"line":1,"character":21},"end":{"line":1,"character":24}},"rule":"reportAssignmentType"},
          {"file":"/w/src/e.py","severity":"information","message":"x","range":{"start":{"line":1,"character":0},"end":{"line":1,"character":1}}},
          {"file":"/usr/lib/x.py","severity":"error","message":"outside","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}]}"#;
        let mut v = Vec::new();
        assert!(parse_pyright(json, Path::new("/w/src"), &lines_of("import numpy\ns = \"😀😀\"; y: int = \"x\"\n"), &mut v));
        let v = finish_diags(v);
        let e = &v.iter().find(|f| f.path == "e.py").unwrap().diags;
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].severity, Severity::Warning);
        let line = "s = \"😀😀\"; y: int = \"x\"";
        assert_eq!(e[1].col, Some(line.find("\"x\"").unwrap() as u32));
        assert!(v.iter().any(|f| f.path.is_empty() && f.diags[0].message == "outside"));
    }

    fn run_job(files: &[(&str, &str)], kind: Kind, entry: Option<&str>) -> (Result<JobResult, JobFailure>, String) {
        let t = TempDir::new("py");
        let src = t.path().join("proj/src");
        std::fs::create_dir_all(&src).unwrap();
        for (p, text) in files {
            let f = src.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        let cancelled = AtomicBool::new(false);
        let opts = JobOpts { entry: entry.map(str::to_string), ..Default::default() };
        let (project, outd) = (t.path().join("proj"), t.path().join("out"));
        let ctx = JobCtx { job: "j", kind, lang: Lang::Python, opts: &opts, src: &src, project_dir: &project, out_dir: &outd, cancelled: &cancelled };
        let out = Collect::default();
        let r = run(&ctx, &out);
        (r, out.text())
    }

    #[test]
    fn e2e_check_single_file_and_project() {
        if skip_without(&["pyright"]) {
            return;
        }
        let (r, log) = run_job(&[("a.py", A_PY)], Kind::Check, Some("a.py"));
        let r = r.unwrap();
        assert_eq!(r.exit, 1, "{log}");
        let a = &r.diagnostics.iter().find(|f| f.path == "a.py").unwrap().diags;
        assert!(a.iter().any(|d| d.line == 3 && d.severity == Severity::Error));
        assert!(log.contains("a.py:4:10: error"), "{log}");
        // 外部のパッケージが無いだけなら、通る（終了コード 0）
        let (r, log) = run_job(&[("b.py", "import numpy\nimport requests\nprint(numpy, requests)\n")], Kind::Check, Some("b.py"));
        let r = r.unwrap();
        assert_eq!(r.exit, 0, "{log}");
        assert!(r.diagnostics.iter().flat_map(|f| &f.diags).all(|d| d.severity == Severity::Warning));
        // pyproject.toml のある木: 全部を見る（入口でないファイルの誤りも出る）
        let (r, log) = run_job(
            &[("pyproject.toml", "[project]\nname = \"x\"\n"), ("pkg/__init__.py", ""), ("pkg/m.py", "def g() -> int:\n    return \"s\"\n"), ("main.py", "from pkg.m import g\nprint(g())\n")],
            Kind::Check,
            Some("main.py"),
        );
        let r = r.unwrap();
        assert_eq!(r.exit, 1, "{log}");
        assert!(r.diagnostics.iter().any(|f| f.path == "pkg/m.py" && f.diags[0].line == 1), "{:?}", r.diagnostics);
        assert!(!r.diagnostics.iter().any(|f| f.path == "main.py"), "パッケージの import は解決する: {:?}", r.diagnostics);
    }

    #[test]
    fn build_and_test_are_unsupported() {
        for k in [Kind::Build, Kind::Test] {
            let f = run_job(&[("a.py", "")], k, Some("a.py")).0.unwrap_err();
            assert!(f.message.contains("未対応"), "{}", f.message);
        }
    }
}
