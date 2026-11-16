//! 保存時の構文チェック（design.md §10.2 最初の版）。
//!
//! `language.toml` の `check`（コマンドの雛形。`{file}` がファイルのパス）を、シェルを
//! 通さずに子プロセスとして実行し、出力（標準出力 + 標準エラー）の各行に `check_pattern`
//! をかけて診断を作る。**batch スレッドで呼ぶ。** 実行中も版を見て、古くなったら子プロセス
//! （とその子孫）を止める。
//!
//! # `check_pattern` の書き方
//!
//! 出力の 1 行ごとに検索する。捕獲グループは順に **1 = 行番号（1 始まり）**、
//! 2 = 桁（1 始まり、省略可）、3 = メッセージ（省略可）。
//!
//! - メッセージが無ければ、マッチより後ろの同じ行の文字列、それも無ければ（Python の
//!   `File "x.py", line 3` のように）次の行からの最後の文（`SyntaxError: ...`）を使う
//! - マッチした行のうち、チェックしたファイルのパスを含む行があれば、含む行だけを使う
//!   （Python は `-c` の枠や `ast.py` の枠にも `line N` が出るため）
//! - マッチした行かメッセージに `warning` を含めば警告、そうでなければエラー

use crate::minire::Regex;
use crate::{Diagnostic, Severity};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zc_core::CancelToken;

/// 構文チェックのエラー。
#[derive(Debug)]
pub enum CheckError {
    /// 編集が入って古くなった（子プロセスは止めた）。
    Cancelled,
    /// コマンドを起動できなかった。
    Spawn(std::io::Error),
    /// 時間切れ（子プロセスは止めた）。
    Timeout,
    /// コマンドの雛形が読めない（空・引用符が閉じていない）。
    BadCommand(String),
    /// `check_pattern` が正規表現として読めない、または捕獲グループが無い。
    BadPattern(String),
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::Cancelled => f.write_str("依頼が古くなったので取り消した"),
            CheckError::Spawn(e) => write!(f, "チェックのコマンドを起動できない: {e}"),
            CheckError::Timeout => f.write_str("チェックが時間切れ"),
            CheckError::BadCommand(s) => write!(f, "check の書き方が不正: {s}"),
            CheckError::BadPattern(s) => write!(f, "check_pattern が不正: {s}"),
        }
    }
}

impl std::error::Error for CheckError {}

/// 既定の時間切れ。Python の `ast.parse` は実機で約 0.4 秒。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// 出力として読む量の上限（これを超えた分は捨てる）。
const MAX_OUTPUT: usize = 256 * 1024;

/// コマンドの雛形を引数の列に分ける（シェルは使わない）。単一引用符 `'...'`、二重引用符
/// `"..."`（中で `\"` `\\` を使える）、引数の外の `\` に対応する。展開（`$`、`*`、`~`、
/// パイプ）はしない。
pub fn split_command(cmd: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_arg = false;
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_arg {
                    args.push(std::mem::take(&mut cur));
                    in_arg = false;
                }
            }
            '\'' => {
                in_arg = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err("' が閉じていない".into()),
                    }
                }
            }
            '"' => {
                in_arg = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(e @ ('"' | '\\')) => cur.push(e),
                            Some(e) => {
                                cur.push('\\');
                                cur.push(e);
                            }
                            None => return Err("\" が閉じていない".into()),
                        },
                        Some(c) => cur.push(c),
                        None => return Err("\" が閉じていない".into()),
                    }
                }
            }
            '\\' => {
                in_arg = true;
                match chars.next() {
                    Some(e) => cur.push(e),
                    None => return Err("\\ で終わっている".into()),
                }
            }
            c => {
                in_arg = true;
                cur.push(c);
            }
        }
    }
    if in_arg {
        args.push(cur);
    }
    Ok(args)
}

/// 出力から診断を作る（[`run_check`] の後半。単体でも試せる）。`file` はチェックした
/// ファイルのパス（出力にそのまま出てくる形）。
pub fn parse_diagnostics(output: &str, pattern: &str, file: &str) -> Result<Vec<Diagnostic>, CheckError> {
    let re = Regex::new(pattern).map_err(CheckError::BadPattern)?;
    if re.group_count() == 0 {
        return Err(CheckError::BadPattern("捕獲グループ（行番号）が要る".into()));
    }
    let lines: Vec<&str> = output.lines().collect();
    let mut hits: Vec<(usize, crate::minire::Match)> =
        lines.iter().enumerate().filter_map(|(i, l)| re.find(l).map(|m| (i, m))).collect();
    if !file.is_empty() && hits.iter().any(|(i, _)| lines[*i].contains(file)) {
        hits.retain(|(i, _)| lines[*i].contains(file));
    }
    let mut out: Vec<Diagnostic> = Vec::new();
    for (n, (i, m)) in hits.iter().enumerate() {
        let Some(line_no) = m.groups.get(1).and_then(|g| g.as_deref()).and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let col = m.groups.get(2).and_then(|g| g.as_deref()).and_then(|s| s.parse::<u32>().ok()).map(|c| c.saturating_sub(1));
        let next_hit = hits.get(n + 1).map_or(lines.len(), |h| h.0);
        let message = m
            .groups
            .get(3)
            .and_then(|g| g.clone())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let rest = m.rest.trim_matches(|c: char| c.is_whitespace() || ":-,.".contains(c));
                (rest.chars().filter(|c| c.is_alphabetic()).count() >= 3).then(|| rest.to_string())
            })
            .or_else(|| {
                lines[i + 1..next_hit.max(i + 1)]
                    .iter()
                    .rev()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty() && !l.chars().all(|c| matches!(c, '^' | '~' | ' ')))
                    .map(|l| l.to_string())
            })
            .unwrap_or_else(|| lines[*i].trim().to_string());
        let is_warning = message.to_ascii_lowercase().contains("warning") || lines[*i].to_ascii_lowercase().contains("warning");
        let severity = if is_warning { Severity::Warning } else { Severity::Error };
        let d = Diagnostic { line: line_no.saturating_sub(1), col, severity, message };
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out.sort_by_key(|d| (d.line, d.col));
    Ok(out)
}

/// 保存時の構文チェック。**batch スレッドで呼ぶ。** [`DEFAULT_TIMEOUT`] で時間切れ。
/// 詳しくは [`run_check_with`]。
pub fn run_check(command: &str, pattern: &str, file: &Path, cancel: &CancelToken) -> Result<Vec<Diagnostic>, CheckError> {
    run_check_with(command, pattern, file, cancel, DEFAULT_TIMEOUT)
}

/// `command`（`language.toml` の `check`）の `{file}` を `file` に置き換えて、使い捨てで
/// 実行し、出力から `pattern`（`check_pattern`）で診断を拾う。
///
/// - コマンドが 0 以外で終わっても、パターンに合う行が無ければ診断は空
/// - `cancel` が古くなったら、子プロセスの組ごと止めて `Cancelled`
/// - `timeout` を過ぎたら、同じく止めて `Timeout`
pub fn run_check_with(
    command: &str,
    pattern: &str,
    file: &Path,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<Vec<Diagnostic>, CheckError> {
    let file_s = file.to_string_lossy().into_owned();
    let argv: Vec<String> = split_command(command).map_err(CheckError::BadCommand)?.into_iter().map(|a| a.replace("{file}", &file_s)).collect();
    if argv.is_empty() {
        return Err(CheckError::BadCommand("空".into()));
    }
    // 起動する前に、パターンの誤りを知らせる
    Regex::new(pattern).map_err(CheckError::BadPattern)?;
    cancel.check().map_err(|_| CheckError::Cancelled)?;

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(CheckError::Spawn)?;
    let out_buf = Arc::new(Mutex::new(Vec::new()));
    let err_buf = Arc::new(Mutex::new(Vec::new()));
    let readers: Vec<_> = [
        child.stdout.take().map(|p| (Box::new(p) as Box<dyn Read + Send>, out_buf.clone())),
        child.stderr.take().map(|p| (Box::new(p) as Box<dyn Read + Send>, err_buf.clone())),
    ]
    .into_iter()
    .flatten()
    .map(|(mut pipe, buf)| {
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut b = buf.lock().unwrap();
                if b.len() < MAX_OUTPUT {
                    b.extend_from_slice(&chunk[..n]);
                }
            }
        })
    })
    .collect();

    let started = Instant::now();
    let stop = |child: &mut std::process::Child| {
        kill_group(child);
        let _ = child.wait();
    };
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => {
                stop(&mut child);
                return Err(CheckError::Spawn(e));
            }
        }
        if cancel.is_stale() {
            stop(&mut child);
            return Err(CheckError::Cancelled);
        }
        if started.elapsed() >= timeout {
            stop(&mut child);
            return Err(CheckError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // 読み取りの終わりを少し待つ（孫がパイプを持ち続けていても、ここで固まらない）
    let deadline = Instant::now() + Duration::from_millis(300);
    for r in &readers {
        while !r.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    cancel.check().map_err(|_| CheckError::Cancelled)?;
    let mut text = String::from_utf8_lossy(&out_buf.lock().unwrap()).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&err_buf.lock().unwrap()));
    parse_diagnostics(&text, pattern, &file_s)
}

#[cfg(unix)]
fn kill_group(child: &mut std::process::Child) {
    // process_group(0) で子が自分の組長になっているので、組ごと止める
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::VersionCounter;

    #[test]
    fn split_command_quotes() {
        assert_eq!(split_command("a b  c").unwrap(), ["a", "b", "c"]);
        assert_eq!(
            split_command(r#"python3 -I -S -c 'import ast,sys; ast.parse(open(sys.argv[1]).read())' {file}"#).unwrap(),
            ["python3", "-I", "-S", "-c", "import ast,sys; ast.parse(open(sys.argv[1]).read())", "{file}"]
        );
        assert_eq!(split_command(r#"x "a b" "q\"r" 'it''s' a\ b"#).unwrap(), ["x", "a b", "q\"r", "its", "a b"]);
        assert_eq!(split_command("x '' y").unwrap(), ["x", "", "y"]);
        assert!(split_command("x 'abc").is_err());
        assert!(split_command("x \"abc").is_err());
        assert!(split_command("").unwrap().is_empty());
    }

    const PY_TRACE: &str = r#"Traceback (most recent call last):
  File "<string>", line 1, in <module>
    import ast,sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])
                    ~~~~~~~~^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
  File "/usr/lib/python3.13/ast.py", line 54, in parse
    return compile(source, filename, mode, flags,
  File "/tmp/bad.py", line 3
    def f(:
          ^
SyntaxError: invalid syntax
"#;

    #[test]
    fn parses_python_traceback() {
        let d = parse_diagnostics(PY_TRACE, r"line (\d+)", "/tmp/bad.py").unwrap();
        assert_eq!(d, vec![Diagnostic { line: 2, col: None, severity: Severity::Error, message: "SyntaxError: invalid syntax".into() }]);
        // ファイル名を含む行が無ければ、全部拾う
        let d = parse_diagnostics(PY_TRACE, r"line (\d+)", "/other.py").unwrap();
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].line, 0);
    }

    #[test]
    fn parses_gcc_style_with_columns_and_warnings() {
        let out = "src/a.c:10:5: error: expected ';' before 'x'\n   10 |     foo x\n      |     ^\nsrc/a.c:20:1: warning: unused variable\nnote: something\n";
        let d = parse_diagnostics(out, r"^(?:.*?):(\d+):(\d+): (?:error|warning): (.*)$", "src/a.c").unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].line, d[0].col, d[0].severity), (9, Some(4), Severity::Error));
        assert_eq!(d[0].message, "expected ';' before 'x'");
        assert_eq!((d[1].line, d[1].col, d[1].severity), (19, Some(0), Severity::Warning));
        // メッセージの捕獲が無い場合は、マッチの後ろ
        let d = parse_diagnostics("a.rs:3:7: boom here", r":(\d+):(\d+)", "a.rs").unwrap();
        assert_eq!(d[0].message, "boom here");
        assert_eq!(d[0].col, Some(6));
    }

    #[test]
    fn pattern_errors() {
        assert!(matches!(parse_diagnostics("x", "(", ""), Err(CheckError::BadPattern(_))));
        assert!(matches!(parse_diagnostics("x", r"line \d+", ""), Err(CheckError::BadPattern(_))));
        // 行番号が数字でなければ飛ばす。重複は 1 つに
        let d = parse_diagnostics("L: a\nL 3 x\nL 3 x\n", r"L (\w+)", "").unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].line, 2);
    }

    #[cfg(unix)]
    mod run {
        use super::*;
        use std::path::PathBuf;

        fn tmp(name: &str, body: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("zc-intel-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            p
        }

        const PY_CHECK: &str = "python3 -I -S -c 'import ast,sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])' {file}";

        fn have_python() -> bool {
            Command::new("python3").arg("-c").arg("pass").output().is_ok()
        }

        #[test]
        fn python_good_and_bad() {
            if !have_python() {
                eprintln!("python3 が無いので飛ばす");
                return;
            }
            let tok = VersionCounter::new().token();
            let good = tmp("good.py", "def f(x):\n    return x + 1\n\nprint(f(2))\n");
            assert_eq!(run_check(PY_CHECK, r"line (\d+)", &good, &tok).unwrap(), vec![]);
            let bad = tmp("bad.py", "x = 1\ny = 2\ndef f(:\n    pass\n");
            let d = run_check(PY_CHECK, r"line (\d+)", &bad, &tok).unwrap();
            assert_eq!(d.len(), 1, "{d:?}");
            assert_eq!(d[0].line, 2);
            assert!(d[0].message.contains("SyntaxError"), "{}", d[0].message);
            // 日本語を含むファイルも読める
            let ja = tmp("ja.py", "# コメント\nprint('こんにちは'\n");
            let d = run_check(PY_CHECK, r"line (\d+)", &ja, &tok).unwrap();
            assert_eq!(d.len(), 1, "{d:?}");
            // 時間の目安（実機の 0.4 秒との比較用に出す）
            let t = Instant::now();
            run_check(PY_CHECK, r"line (\d+)", &bad, &tok).unwrap();
            eprintln!("[bench] python ast.parse チェック: {:?}", t.elapsed());
        }

        #[test]
        fn spawn_error_and_bad_command() {
            let tok = VersionCounter::new().token();
            let f = tmp("x.txt", "");
            assert!(matches!(run_check("/nonexistent/zz {file}", r"(\d+)", &f, &tok), Err(CheckError::Spawn(_))));
            assert!(matches!(run_check("", r"(\d+)", &f, &tok), Err(CheckError::BadCommand(_))));
            assert!(matches!(run_check("echo 'x", r"(\d+)", &f, &tok), Err(CheckError::BadCommand(_))));
            assert!(matches!(run_check("echo {file}", "(", &f, &tok), Err(CheckError::BadPattern(_))));
        }

        #[test]
        fn nonzero_exit_and_stderr() {
            let tok = VersionCounter::new().token();
            let f = tmp("y.txt", "");
            let cmd = "sh -c 'echo \"$0:7:2: error: boom\" >&2; exit 3' {file}";
            let d = run_check(cmd, r":(\d+):(\d+): error: (.*)", &f, &tok).unwrap();
            assert_eq!(d.len(), 1);
            assert_eq!((d[0].line, d[0].col), (6, Some(1)));
            assert_eq!(d[0].message, "boom");
            // 標準出力でも拾う。出力が無ければ空
            let d = run_check("sh -c 'echo \"line 4 oops\"' {file}", r"line (\d+)", &f, &tok).unwrap();
            assert_eq!(d[0].line, 3);
            assert!(run_check("true", r"line (\d+)", &f, &tok).unwrap().is_empty());
        }

        #[test]
        fn timeout_kills_child_group() {
            let tok = VersionCounter::new().token();
            let f = tmp("z.txt", "");
            let t = Instant::now();
            // sh の孫（sleep）まで止まること、パイプを握ったまま固まらないことを確かめる
            let r = run_check_with("sh -c 'sleep 30; echo late' {file}", r"(\d+)", &f, &tok, Duration::from_millis(200));
            assert!(matches!(r, Err(CheckError::Timeout)), "{r:?}");
            assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        }

        #[test]
        fn stale_token_cancels() {
            let vc = VersionCounter::new();
            let tok = vc.token();
            let f = tmp("w.txt", "");
            let vc2 = vc.clone();
            let h = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                vc2.bump();
            });
            let t = Instant::now();
            let r = run_check("sleep 30", r"(\d+)", &f, &tok);
            h.join().unwrap();
            assert!(matches!(r, Err(CheckError::Cancelled)), "{r:?}");
            assert!(t.elapsed() < Duration::from_secs(3));
            // 最初から古い依頼は起動もしない
            let r = run_check("sleep 30", r"(\d+)", &f, &tok);
            assert!(matches!(r, Err(CheckError::Cancelled)));
        }
    }
}
