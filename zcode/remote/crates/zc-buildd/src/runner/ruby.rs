//! Ruby のジョブ（docs/stage3-design.md §4.4）。2026-11-17 に足した。
//!
//! | 種類 | 中でやること |
//! |---|---|
//! | check | ファイルごとに `ruby -wc ./<ファイル>`（構文と警告）。1 ファイルだけなら `entry`、`Gemfile` のある木なら木の中の `*.rb` 全部（`vendor/` と `.` で始まるフォルダは見ない。[`MAX_FILES`] まで） |
//! | build・test | 無い（実機の `ruby` でそのまま動かす。実機にはまだ ruby が無いので、`language.toml` の `run` が `sudo apt install ruby` を案内する） |
//!
//! ruby はイメージの Debian の `ruby`（3.3。版はベースのイメージの digest で決まる）。
//!
//! # 出力の形（ruby 3.3 の parse.y。tests/data/langs/ruby-*.txt）
//!
//! - 警告: `x.rb:4: warning: …`（桁なし）→ warning
//! - 構文の誤り: `ruby: x.rb:3: syntax error, … (SyntaxError)` → error。続く 2 行がその行の写しと
//!   `^` の行のことがある。**写しがファイルの行と同じときだけ**、`^` の前の空白の数を桁にする
//!   （parse.y は空白をバイトの数だけ並べる。日本語の後ろでも合うことを確かめた）
//! - `Syntax OK`（標準出力）は捨てる

use super::{
    check_cancelled, failure_line, files_with_ext, finish_diags, has_error, io_failure, line0, lines_from, note_diag, push_diag, rel_to_root, safe_entry, spawn_failure, spawn_tool, split_loc,
    tool_env, truncate_utf8, unsupported, which, JobCtx, JobFailure, JobOutput, JobResult, LineSource, Tap,
};
use std::path::Path;
use std::process::Command;
use zc_wire::msg::{Diag, FileDiags, Kind, Severity};

/// 木のときに見るファイルの上限（1 ファイルにつき ruby を 1 回起動する）。
pub const MAX_FILES: usize = 200;

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    if ctx.kind != Kind::Check {
        return Err(unsupported("Ruby", ctx.kind, "実機の ruby で実行する"));
    }
    let ruby = which("ruby")?;
    let src = ctx.src.canonicalize().map_err(|e| io_failure("作業の木が無い", e))?;
    let mut diags = Vec::new();
    let files = if src.join("Gemfile").is_file() {
        let (files, cut) = files_with_ext(&src, &[".rb"], &["vendor"], MAX_FILES);
        if cut {
            push_diag(&mut diags, "", note_diag(Severity::Warning, format!(".rb が多すぎるので、名前の順に {MAX_FILES} 個だけ見た")));
        }
        files
    } else {
        match ctx.opts.entry.as_deref() {
            Some(e) if safe_entry(e) && src.join(e).is_file() => vec![e.to_string()],
            Some(e) => return Ok(super::failed_result(1, format!("入口のファイルが無い: {e}"))),
            None => return Ok(super::failed_result(1, "検査するファイルが分からない")),
        }
    };
    let lines = lines_from(&src);
    let mut worst = 0;
    for f in &files {
        check_cancelled(ctx)?;
        let mut cmd = Command::new(&ruby);
        cmd.current_dir(&src).arg("-wc").arg(format!("./{f}"));
        tool_env(&mut cmd);
        let tap = Tap::capturing(out);
        let exit = spawn_tool(cmd, &tap, &mut |_| false).map_err(|e| spawn_failure("ruby", e))?;
        let text = tap.take();
        let found = parse_ruby(&text, &src, &lines, &mut diags);
        if exit != 0 && !found {
            let msg = failure_line(&text).unwrap_or_else(|| format!("ruby -wc が失敗した（終了コード {exit}）: {f}"));
            push_diag(&mut diags, "", note_diag(Severity::Error, msg));
        }
        if exit >= 128 {
            worst = exit;
        }
    }
    check_cancelled(ctx)?;
    super::python::log_diags(out, &diags);
    let exit = if worst != 0 { worst } else { has_error(&diags) as i32 };
    Ok(JobResult { exit, complete: worst < 128, diagnostics: finish_diags(diags), ..Default::default() })
}

/// `ruby -wc` の標準エラーを読む。error か warning を 1 つでも読めたら `true`。
pub fn parse_ruby(text: &str, src_root: &Path, lines: &LineSource<'_>, into: &mut Vec<FileDiags>) -> bool {
    let all: Vec<&str> = text.lines().collect();
    let mut found = false;
    let mut i = 0;
    while i < all.len() {
        let raw = all[i];
        i += 1;
        // 頭の「ruby: 」は起動したときの名前（絶対パスで起動すると `/usr/bin/ruby: `）
        let (l, is_error_line) = match raw.split_once(": ") {
            Some((prog, rest)) if !prog.contains(' ') && prog.rsplit('/').next().and_then(|n| n.strip_prefix("ruby")).is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.')) => (rest, true),
            _ => (raw, false),
        };
        let Some((path, ln, _, msg)) = split_loc(l) else { continue };
        let (severity, msg) = match msg.strip_prefix("warning: ") {
            Some(m) => (Severity::Warning, m),
            None if is_error_line || msg.contains("syntax error") => (Severity::Error, msg),
            None => continue,
        };
        let msg = msg.strip_suffix(" (SyntaxError)").unwrap_or(msg);
        let message = truncate_utf8(msg, 400);
        found = true;
        let Some(rel) = rel_to_root(src_root, src_root, path) else {
            push_diag(into, "", Diag { line: 0, col: None, end: None, severity, code: None, message });
            continue;
        };
        let line = line0(ln);
        // 写しと ^ の行
        let mut col = None;
        if severity == Severity::Error {
            if let (Some(echo), Some(caret)) = (all.get(i), all.get(i + 1)) {
                let caret_ok = caret.trim_start_matches(' ').starts_with('^') && caret.trim().chars().all(|c| c == '^' || c == '~');
                if caret_ok && lines(&rel, line).as_deref() == Some(echo) {
                    let c = caret.len() - caret.trim_start_matches(' ').len();
                    if c <= echo.len() && echo.is_char_boundary(c) {
                        col = Some(c as u32);
                    }
                    i += 2;
                }
            }
        }
        push_diag(into, &rel, Diag { line, col, end: None, severity, code: None, message });
    }
    found
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
    fn warnings_and_errors_are_read() {
        let mut v = Vec::new();
        assert!(parse_ruby(include_str!("../../tests/data/langs/ruby-warn.txt"), Path::new("/w"), &lines_of(""), &mut v));
        let v = finish_diags(v);
        let w = &v[0].diags;
        assert_eq!(v[0].path, "w.rb");
        assert_eq!((w[0].line, w[0].col, w[0].severity), (1, None, Severity::Warning));
        assert!(w[0].message.starts_with("assigned but unused variable"));
        // 日本語の後ろの誤り: ^ の位置はバイトで合う
        let src = "# 日本語\nx = \"日本\" 1\ny = 2\n";
        let mut v = Vec::new();
        assert!(parse_ruby(include_str!("../../tests/data/langs/ruby-err-cjk.txt"), Path::new("/w"), &lines_of(src), &mut v));
        let e = &v.iter().find(|f| f.path == "e1.rb").unwrap().diags;
        let err = e.iter().find(|d| d.severity == Severity::Error).unwrap();
        assert_eq!((err.line, err.col), (1, Some("x = \"日本\" ".len() as u32)));
        assert_eq!(err.message, "syntax error, unexpected integer literal, expecting end-of-input");
        // 写しが無い（ファイルの終わり）
        let mut v = Vec::new();
        assert!(parse_ruby(include_str!("../../tests/data/langs/ruby-err-eof.txt"), Path::new("/w"), &lines_of("def a\n  if true\nend\n"), &mut v));
        assert!(v[0].diags.iter().any(|d| d.severity == Severity::Error && d.line == 2 && d.col.is_none()));
        // 写しがファイルの行と違えば桁を付けない
        let mut v = Vec::new();
        assert!(parse_ruby(include_str!("../../tests/data/langs/ruby-err.txt"), Path::new("/w"), &lines_of("x\nchanged\n  puts\n"), &mut v));
        assert_eq!(v[0].diags[0].col, None);
        assert!(!parse_ruby("Syntax OK\n", Path::new("/w"), &lines_of(""), &mut Vec::new()));
        // 絶対パスで起動したときの頭（`/usr/bin/ruby: `）
        let mut v = Vec::new();
        assert!(parse_ruby("/usr/bin/ruby: ./a.rb:2: syntax error, unexpected end-of-input (SyntaxError)\n", Path::new("/w"), &lines_of(""), &mut v));
        assert_eq!((v[0].path.as_str(), v[0].diags[0].line, v[0].diags[0].severity), ("a.rb", 1, Severity::Error));
    }

    fn run_job(files: &[(&str, &str)], entry: &str) -> (JobResult, String) {
        let t = TempDir::new("rb");
        let src = t.path().join("proj/src");
        for (p, text) in files {
            let f = src.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        let cancelled = AtomicBool::new(false);
        let opts = JobOpts { entry: Some(entry.into()), ..Default::default() };
        let (project, outd) = (t.path().join("proj"), t.path().join("out"));
        let ctx = JobCtx { job: "j", kind: Kind::Check, lang: Lang::Ruby, opts: &opts, src: &src, project_dir: &project, out_dir: &outd, cancelled: &cancelled };
        let out = Collect::default();
        let r = run(&ctx, &out).unwrap();
        (r, out.text())
    }

    #[test]
    fn e2e_check() {
        if skip_without(&["ruby"]) {
            return;
        }
        let (r, log) = run_job(&[("a.rb", "# 日本語\nx = \"日本\" 1\n")], "a.rb");
        assert_eq!(r.exit, 1, "{log}");
        assert_eq!(r.diagnostics.len(), 1, "{:?}\n{log}", r.diagnostics);
        let d = r.diagnostics[0].diags.iter().find(|d| d.severity == Severity::Error).unwrap();
        assert_eq!((d.line, d.col), (1, Some("x = \"日本\" ".len() as u32)));
        // 警告だけなら 0
        let (r, log) = run_job(&[("w.rb", "def f\n  x = 1\nend\n")], "w.rb");
        assert_eq!(r.exit, 0, "{log}");
        assert_eq!(r.diagnostics[0].diags[0].severity, Severity::Warning);
        // Gemfile の木: 全部の .rb（vendor は見ない）
        let (r, _) = run_job(&[("Gemfile", ""), ("lib/ok.rb", "puts 1\n"), ("lib/bad.rb", "def\n"), ("vendor/x.rb", "def\n"), ("main.rb", "puts 2\n")], "main.rb");
        assert_eq!(r.exit, 1);
        assert_eq!(r.diagnostics.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["lib/bad.rb"]);
    }
}
