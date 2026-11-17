//! Shell のジョブ（docs/stage3-design.md §4.4）。2026-11-17 に足した。
//!
//! | 種類 | 中でやること |
//! |---|---|
//! | check | `shellcheck -f json1 -S info ./<entry>`（1 行目が `#!` でなければ `--shell=bash`。実機は bash で動かす） |
//! | build・test | 無い（実機の `bash` でそのまま動かす） |
//!
//! - 木はいつも 1 ファイルだけ（`source` 先は送らない。shellcheck の SC1091 は info で出る）
//! - 重さ: error → error、warning と info → warning、style は捨てる。error が無ければ終了コード 0
//! - 位置: `line` / `column` は 1 始まり、**桁は文字（コードポイント）で数える**（タブも 1 字。
//!   0.11.0 の json1 で確かめた）。その行を読んで UTF-8 のバイト位置に直す。`code` は `SC2086` の形

use super::{
    char_col0, check_cancelled, failure_line, finish_diags, has_error, io_failure, line0, lines_from, note_diag, push_diag, rel_to_root, safe_entry, spawn_failure, spawn_tool, tool_env,
    truncate_utf8, unsupported, which, JobCtx, JobFailure, JobOutput, JobResult, LineSource, Tap,
};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use zc_wire::msg::{Diag, FileDiags, Kind, Severity};

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    if ctx.kind != Kind::Check {
        return Err(unsupported("Shell", ctx.kind, "実機の bash で実行する"));
    }
    let shellcheck = which("shellcheck")?;
    let src = ctx.src.canonicalize().map_err(|e| io_failure("作業の木が無い", e))?;
    let entry = match ctx.opts.entry.as_deref() {
        Some(e) if safe_entry(e) && src.join(e).is_file() => e.to_string(),
        Some(e) => return Ok(super::failed_result(1, format!("入口のファイルが無い: {e}"))),
        None => return Ok(super::failed_result(1, "検査するファイルが分からない")),
    };
    let first = std::fs::read(src.join(&entry)).map_err(|e| io_failure("ファイルを読めない", e))?;
    let mut cmd = Command::new(&shellcheck);
    cmd.current_dir(&src).args(["-f", "json1", "-S", "info"]);
    if !first.starts_with(b"#!") {
        cmd.arg("--shell=bash");
    }
    cmd.arg(format!("./{entry}"));
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
    .map_err(|e| spawn_failure("shellcheck", e))?;
    check_cancelled(ctx)?;

    let mut diags = Vec::new();
    let parsed = parse_shellcheck(&json, &src, &lines_from(&src), &mut diags);
    super::python::log_diags(out, &diags);
    let mut result = JobResult { complete: exit < 128, ..Default::default() };
    // 0 = 指摘なし、1 = 指摘あり。2 以上は shellcheck の失敗
    if !parsed || exit > 1 {
        let msg = failure_line(&tap.take()).unwrap_or_else(|| format!("shellcheck が失敗した（終了コード {exit}）"));
        push_diag(&mut diags, "", note_diag(Severity::Error, msg));
        result.exit = if exit == 0 { 1 } else { exit };
    } else {
        result.exit = has_error(&diags) as i32;
    }
    result.diagnostics = finish_diags(diags);
    Ok(result)
}

/// `shellcheck -f json1` の出力を読む。JSON として読めなければ `false`。
pub fn parse_shellcheck(json: &str, src_root: &Path, lines: &LineSource<'_>, into: &mut Vec<FileDiags>) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(json.trim()) else { return false };
    let Some(arr) = v.get("comments").and_then(Value::as_array) else { return false };
    for c in arr {
        let severity = match c.get("level").and_then(Value::as_str) {
            Some("error") => Severity::Error,
            Some("warning") | Some("info") => Severity::Warning,
            _ => continue,
        };
        let message = truncate_utf8(c.get("message").and_then(Value::as_str).unwrap_or(""), 400);
        let code = c.get("code").and_then(Value::as_u64).map(|n| format!("SC{n}"));
        let num = |k: &str| c.get(k).and_then(Value::as_u64).map(|n| n as u32);
        let file = c.get("file").and_then(Value::as_str).unwrap_or("");
        match (rel_to_root(src_root, src_root, file), num("line")) {
            (Some(path), Some(l)) => {
                let line = line0(l);
                let col = num("column").map(|c| char_col0(lines, &path, line, c));
                let end = match (num("endLine"), num("endColumn")) {
                    (Some(el), Some(ec)) => Some((line0(el), char_col0(lines, &path, line0(el), ec))),
                    _ => None,
                };
                push_diag(into, &path, Diag { line, col, end, severity, code, message });
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
    const B_SH: &str = "#!/bin/bash\necho \"日本語\" $foo\n\techo $bar\n";
    const A_SH: &str = "#!/bin/bash\n# 日本語\necho \"日本\" $1\n\tif [ $x = 1 ] then\nfi\ncd foo\n";

    fn lines_of(text: &'static str) -> impl Fn(&str, u32) -> Option<String> {
        move |_: &str, l: u32| text.split('\n').nth(l as usize).map(str::to_string)
    }

    #[test]
    fn shellcheck_json_is_read() {
        let mut v = Vec::new();
        assert!(parse_shellcheck(include_str!("../../tests/data/langs/shellcheck.json"), Path::new("/w"), &lines_of(B_SH), &mut v));
        let v = finish_diags(v);
        let b = &v.iter().find(|f| f.path == "b.sh").unwrap().diags;
        // `echo "日本語" $foo`: $foo は 12 字目 → バイト 17、終わりは 16 字目 → バイト 21
        let l1 = "echo \"日本語\" $foo";
        assert_eq!((b[0].line, b[0].col, b[0].end), (1, Some(l1.find('$').unwrap() as u32), Some((1, l1.len() as u32))));
        assert_eq!(b[0].code.as_deref(), Some("SC2154"));
        assert!(b.iter().all(|d| d.severity == Severity::Warning));
        // SC2086（info）も warning で残る
        assert!(b.iter().any(|d| d.code.as_deref() == Some("SC2086")));
        // タブは 1 字: `\techo $bar` の $bar はバイト 6
        assert!(b.iter().any(|d| d.line == 2 && d.col == Some(6)));
    }

    #[test]
    fn shellcheck_syntax_errors() {
        let mut v = Vec::new();
        assert!(parse_shellcheck(include_str!("../../tests/data/langs/shellcheck-syntax.json"), Path::new("/w"), &lines_of(A_SH), &mut v));
        let v = finish_diags(v);
        let a = &v[0].diags;
        assert!(has_error(&v));
        assert!(a.iter().any(|d| d.severity == Severity::Error && d.code.as_deref() == Some("SC1073") && d.line == 3 && d.col == Some(15)));
        assert!(!parse_shellcheck("", Path::new("/w"), &lines_of(""), &mut Vec::new()));
    }

    fn run_job(name: &str, text: &str, kind: Kind) -> (Result<JobResult, JobFailure>, String) {
        let t = TempDir::new("sh");
        let src = t.path().join("proj/src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join(name), text).unwrap();
        let cancelled = AtomicBool::new(false);
        let opts = JobOpts { entry: Some(name.to_string()), ..Default::default() };
        let (project, outd) = (t.path().join("proj"), t.path().join("out"));
        let ctx = JobCtx { job: "j", kind, lang: Lang::Shell, opts: &opts, src: &src, project_dir: &project, out_dir: &outd, cancelled: &cancelled };
        let out = Collect::default();
        let r = run(&ctx, &out);
        (r, out.text())
    }

    #[test]
    fn e2e_check() {
        if skip_without(&["shellcheck"]) {
            return;
        }
        let (r, log) = run_job("a.sh", A_SH, Kind::Check);
        let r = r.unwrap();
        assert_eq!(r.exit, 1, "{log}");
        assert!(has_error(&r.diagnostics));
        // 警告だけなら 0。#! の無いファイルは bash として読む（SC2148 が出ない）
        let (r, log) = run_job("b.sh", "echo \"日本\" $x\n", Kind::Check);
        let r = r.unwrap();
        assert_eq!(r.exit, 0, "{log}");
        let d = &r.diagnostics[0].diags;
        assert!(d.iter().all(|d| d.code.as_deref() != Some("SC2148")), "{d:?}");
        assert!(d.iter().any(|d| d.col == Some("echo \"日本\" ".len() as u32)), "{d:?}");
        // 何も無い
        let (r, _) = run_job("c.sh", "#!/bin/sh\necho ok\n", Kind::Check);
        let r = r.unwrap();
        assert_eq!((r.exit, r.diagnostics.len()), (0, 0));
        assert!(run_job("c.sh", "", Kind::Build).0.unwrap_err().message.contains("未対応"));
    }
}
