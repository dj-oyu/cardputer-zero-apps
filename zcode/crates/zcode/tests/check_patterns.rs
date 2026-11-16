//! 各言語の `language.toml` の `check_pattern` が、実機（Debian 13）のチェックの道具の
//! 本物の出力からエラー行を拾えるか。出力は 2026-11-16 に実機で取ったもの
//! （Python 3.13.5・bash 5.2.37・gcc 14.2.0。一時フォルダのパスはそのまま）。

use zc_highlight::Registry;
use zc_intel::{parse_diagnostics, Severity};

fn pattern(key: &str) -> &'static str {
    let r = Registry::builtin();
    let id = r.by_key(key).unwrap_or_else(|| panic!("{key} が無い"));
    r.info(id).check_pattern.unwrap_or_else(|| panic!("{key} に check_pattern が無い"))
}

#[test]
fn python_ast_parse_output() {
    let out = r#"Traceback (most recent call last):
  File "<string>", line 1, in <module>
    import ast,sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])
                    ~~~~~~~~~^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
  File "/usr/lib/python3.13/ast.py", line 50, in parse
    return compile(source, filename, mode, flags,
                   _feature_version=feature_version, optimize=optimize)
  File "/tmp/zccheck.rX28/bad.py", line 2
    for i in range(3)
                     ^
SyntaxError: expected ':'
"#;
    let d = parse_diagnostics(out, pattern("python"), "/tmp/zccheck.rX28/bad.py").unwrap();
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].line, 1);
    assert_eq!(d[0].message, "SyntaxError: expected ':'");
}

#[test]
fn bash_n_output() {
    let out = "/tmp/zccheck.rX28/bad.sh: line 7: syntax error: unexpected end of file\n";
    let d = parse_diagnostics(out, pattern("shell"), "/tmp/zccheck.rX28/bad.sh").unwrap();
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].line, d[0].col), (6, None));
    assert_eq!(d[0].message, "syntax error: unexpected end of file");
}

#[test]
fn gcc_fsyntax_only_output() {
    // gcc は UTF-8 のロケールでは引用符に ‘ ’ を使う
    let out = "/tmp/zccheck.rX28/bad.c: In function \u{2018}main\u{2019}:
/tmp/zccheck.rX28/bad.c:4:5: error: expected \u{2018},\u{2019} or \u{2018};\u{2019} before \u{2018}printf\u{2019}
    4 |     printf(\"%d
      |     ^~~~~~
/tmp/zccheck.rX28/bad.c:4:12: warning: missing terminating \" character
    4 |     printf(\"%d
      |            ^
/tmp/zccheck.rX28/bad.c:4:12: error: missing terminating \" character
    4 |     printf(\"%d
      |            ^~~
/tmp/zccheck.rX28/bad.c:5:1: warning: missing terminating \" character
    5 | \", x);
      | ^
/tmp/zccheck.rX28/bad.c:5:1: error: missing terminating \" character
    5 | \", x);
      | ^~~~~~
";
    let d = parse_diagnostics(out, pattern("c"), "/tmp/zccheck.rX28/bad.c").unwrap();
    let got: Vec<(u32, Option<u32>, Severity)> = d.iter().map(|d| (d.line, d.col, d.severity)).collect();
    assert_eq!(
        got,
        [
            (3, Some(4), Severity::Error),
            (3, Some(11), Severity::Warning),
            (3, Some(11), Severity::Error),
            (4, Some(0), Severity::Warning),
            (4, Some(0), Severity::Error),
        ],
        "{d:?}"
    );
    assert!(d[0].message.starts_with("expected"), "{:?}", d[0].message);
}

#[test]
fn tsc_pretty_false_output() {
    // TypeScript 7.0.2（Go 版）の `--pretty false` の出力（実機で取ったもの）。
    // メッセージに TS の番号を残す
    let out = "bad.ts(1,7): error TS2322: Type 'string' is not assignable to type 'number'.\n";
    let d = parse_diagnostics(out, pattern("typescript"), "bad.ts").unwrap();
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].line, d[0].col), (0, Some(6)));
    assert_eq!(d[0].severity, Severity::Error);
    assert_eq!(d[0].message, "TS2322: Type 'string' is not assignable to type 'number'.");
}

#[test]
fn tsc_syntax_error_in_js() {
    // JS（--allowJs）では構文エラーだけが出る（形は TS と同じ。ファイル名に括弧があってもよい）
    let out = "src/app (1)/bad.js(3,14): error TS1005: ';' expected.\nbad.js(7,1): error TS1128: Declaration or statement expected.\n";
    let d = parse_diagnostics(out, pattern("javascript"), "").unwrap();
    assert_eq!(d.len(), 2, "{d:?}");
    assert_eq!((d[0].line, d[0].col), (2, Some(13)));
    assert_eq!(d[0].message, "TS1005: ';' expected.");
    assert_eq!((d[1].line, d[1].col), (6, Some(0)));
    assert!(d[1].message.starts_with("TS1128:"), "{:?}", d[1].message);
}
