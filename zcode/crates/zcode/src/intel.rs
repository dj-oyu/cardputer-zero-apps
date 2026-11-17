//! エディタ画面と `zc-intel` のつなぎ（design.md §10.2 の最初の版。モック 12〜14）。
//!
//! - 言語の設定（`zc-highlight` の `LanguageInfo`）から `zc-intel` の形を作る
//!   （横の crate 同士は依存しないので、写すのはここ）
//! - 診断の置き場（`zc_intel::DiagStore`）の鍵、波線への直し方
//! - 実機の構文チェックを使い捨てで走らせる（`check` ワーカーで呼ぶ。docs/stage3-design.md §8.3）
//! - ビルド係の進み具合・終わり方を、ステータスバーの文にする（§8.6・§11.1）
//! - アウトラインの絞り込み（一致した項目の親を薄く残す）、試験の結果の一覧（モック 15）

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zc_core::{palette, CancelToken, Span, TokenKind};
use zc_highlight::{LanguageInfo, OutlineSpec};
use zc_intel::{Diagnostic, OutlineItem, OutlineKind, OutlineRule, OutlineRules, Severity};
use zc_remote_ipc::{DoneStatus, JobKind, Phase, TestReport};
use zc_ui::{LineMark, PickRow};

/// 自動で補完を出すのに要る文字数（モック 12）。
pub const AUTO_MIN_CHARS: usize = 2;
/// 補完の候補の最大数。
pub const MAX_ITEMS: usize = 5;

/// `LanguageInfo` の `[outline]` を `zc-intel` の規則に写して `f` に渡す。
pub fn with_outline_rules<R>(spec: &OutlineSpec, f: impl FnOnce(&OutlineRules<'_>) -> R) -> R {
    let items: Vec<OutlineRule<'_>> = spec
        .items
        .iter()
        .map(|(kw, k)| OutlineRule {
            keyword: kw,
            kind: match k {
                zc_highlight::OutlineKind::Function => OutlineKind::Function,
                zc_highlight::OutlineKind::Class => OutlineKind::Class,
                zc_highlight::OutlineKind::Heading => OutlineKind::Heading,
            },
        })
        .collect();
    let rules = OutlineRules { items: &items, modifiers: spec.modifiers, heading_char: spec.heading_char, indent_width: spec.indent_width.max(1) };
    f(&rules)
}

/// 構文チェックのコマンドと、エラー行を拾う正規表現（両方あるときだけ）。
pub fn check_command(info: &LanguageInfo) -> Option<(&'static str, &'static str)> {
    Some((info.check?, info.check_pattern?))
}

/// 行の中のバイト位置 `byte` の直前の文字が、文字列かコメントの中か（補完を出さない所）。
pub fn in_string_or_comment(spans: &[Span], byte: usize) -> bool {
    if byte == 0 {
        return false;
    }
    let b = (byte - 1) as u32;
    spans.iter().any(|s| s.start <= b && b < s.end && matches!(s.kind, TokenKind::String | TokenKind::Comment | TokenKind::Escape))
}

/// 診断の置き場の鍵: 作業フォルダ（ツリーの根）からの相対パス。外のファイルは絶対パスのまま。
/// 実機の構文チェック（絶対パスで分かる）と、ビルド係の診断（プロジェクトの根 + 相対パス）を、
/// どちらもこれに直してから取り込む（2 つの出どころが同じ鍵で出会う）。
pub fn diag_key(workspace: &Path, abs: &Path) -> Arc<Path> {
    Arc::from(abs.strip_prefix(workspace).unwrap_or(abs))
}

/// 実機の構文チェックができなかった理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckFail {
    /// 道具が入っていない（起動できない、または包みのシェルが 127 で終わった）。
    /// 同じ言語では 1 回だけ知らせる。
    Unavailable(String),
    /// 時間切れ・`check` の書き方の誤りなど。
    Other(String),
}

/// 実機の構文チェックの時間切れ（`tsc` は `--lib es2022,dom` で約 3.8 秒）。
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
/// 出力として読む量の上限。
const CHECK_MAX_OUTPUT: usize = 256 * 1024;

/// `command`（`language.toml` の `check`）を使い捨てで実行し、出力から `pattern` で診断を拾う。
/// **`check` ワーカーで呼ぶ**（`tsc` は数秒かかる。batch を塞がない）。
///
/// `zc_intel::run_check` と同じ手順だが、終了コードも見る: JS / TS の `check` は `bash -c` で
/// 包んであるので、`tsc` が無いと起動には成功して 127 で終わり、診断は 1 件も出ない。それを
/// 「エラーなし」と取り違えないように、127（シェルの「コマンドが無い」）で終わったら
/// [`CheckFail::Unavailable`] にする（bash の「line 1: tsc: command not found」が
/// `check_pattern` に合ってしまう言語でも、それを診断にしない）。
pub fn run_local_check(command: &str, pattern: &str, file: &Path, cancel: &CancelToken) -> Result<Vec<Diagnostic>, CheckFail> {
    let file_s = file.to_string_lossy().into_owned();
    let argv: Vec<String> =
        zc_intel::split_command(command).map_err(|e| CheckFail::Other(format!("check の書き方が不正: {e}")))?.into_iter().map(|a| a.replace("{file}", &file_s)).collect();
    let Some(program) = argv.first() else { return Err(CheckFail::Other("check が空".into())) };
    // 起動する前に、パターンの誤りを知らせる
    zc_intel::parse_diagnostics("", pattern, "").map_err(|e| CheckFail::Other(e.to_string()))?;
    let stale = || CheckFail::Other("依頼が古くなったので取り消した".into());
    if cancel.is_stale() {
        return Err(stale());
    }
    let mut cmd = Command::new(program);
    cmd.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    {
        // 子を自分の組長にして、止めるときは組ごと止める（bash の先の tsc も）
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(CheckFail::Unavailable(format!("{program} が無い"))),
        Err(e) => return Err(CheckFail::Other(format!("{program} を起動できない: {e}"))),
    };
    let bufs = [Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new()))];
    let pipes = [child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)];
    let readers: Vec<_> = pipes
        .into_iter()
        .zip(bufs.iter().cloned())
        .filter_map(|(pipe, buf)| {
            let mut pipe = pipe?;
            Some(std::thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    let mut b = buf.lock().unwrap();
                    if b.len() < CHECK_MAX_OUTPUT {
                        b.extend_from_slice(&chunk[..n]);
                    }
                }
            }))
        })
        .collect();
    let kill = |child: &mut std::process::Child| {
        // SAFETY: 自分が作った子の組に SIGKILL を送るだけ。
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    };
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {}
            Err(e) => {
                kill(&mut child);
                return Err(CheckFail::Other(e.to_string()));
            }
        }
        if cancel.is_stale() {
            kill(&mut child);
            return Err(stale());
        }
        if started.elapsed() >= CHECK_TIMEOUT {
            kill(&mut child);
            return Err(CheckFail::Other("チェックが時間切れ".into()));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // 読み取りの終わりを少し待つ（孫がパイプを持ち続けていても、ここで固まらない）
    let deadline = Instant::now() + Duration::from_millis(300);
    for r in &readers {
        while !r.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let mut text = String::from_utf8_lossy(&bufs[0].lock().unwrap()).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&bufs[1].lock().unwrap()));
    if status.code() == Some(127) {
        // シェルの「command not found」。どの道具かは出力の最後の行に出ている
        let why = text.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("道具が無い");
        return Err(CheckFail::Unavailable(why.to_string()));
    }
    zc_intel::parse_diagnostics(&text, pattern, &file_s).map_err(|e| CheckFail::Other(e.to_string()))
}

/// ビルド係の進み具合を、ステータスバーの右に出す文にする（§8.6）。`secs` は依頼してからの秒数。
pub fn progress_text(kind: JobKind, phase: Phase, done: u32, total: u32, secs: u64) -> String {
    let kb = |n: u32| n.div_ceil(1024);
    match phase {
        Phase::Connecting => "ビルド係: 接続中".into(),
        Phase::Scanned => "ビルド係: 準備中".into(),
        Phase::Uploading if total > 0 => format!("ビルド係: 送信中 {}/{}KB", kb(done), kb(total)),
        Phase::Uploading => "ビルド係: 送信中".into(),
        Phase::Queued => "ビルド係: 順番待ち".into(),
        Phase::Running => {
            let what = match kind {
                JobKind::Build => "ビルド中",
                JobKind::Check => "検査中",
                JobKind::Test => "試験中",
            };
            format!("ビルド係: {what} {secs} 秒")
        }
        Phase::Downloading if total > 0 => format!("ビルド係: 受信中 {}/{}KB", kb(done), kb(total)),
        Phase::Downloading => "ビルド係: 受信中".into(),
    }
}

/// ジョブがうまく終わらなかった理由を、人に見せる 1 行にする（§11.1）。`message` は
/// `zc-remote` が付けた文（あればそれを優先する）。成功・失敗（診断や試験の結果が別に届く）は `None`。
pub fn done_text(status: DoneStatus, message: &str) -> Option<String> {
    let default = match status {
        DoneStatus::Ok | DoneStatus::Failed => return None,
        DoneStatus::Cancelled => "取り消した",
        DoneStatus::NotApplicable => "このファイルはビルド係の対象ではない",
        DoneStatus::Offline => "オフライン（ビルド係に届かない）",
        DoneStatus::Auth => "認証できない（設定とトークンを確かめる）",
        DoneStatus::Clock => "時計が合っていない（NTP の同期待ち）",
        DoneStatus::Limit => "上限に達した",
        DoneStatus::Error => "失敗した",
    };
    let body = message.lines().next().map(str::trim).filter(|m| !m.is_empty()).unwrap_or(default);
    Some(format!("ビルド係: {body}"))
}

/// 試験の結果の一覧の 1 行と、`TestReport::cases` の中の番号。
pub struct TestRow {
    pub row: PickRow,
    pub case: usize,
}

/// 試験の結果を一覧の行にする（モック 15）。落ちた試験が先、通った試験が後。`query` で
/// 名前を絞り込む（ASCII の大文字小文字を無視した部分一致）。
pub fn test_rows(report: &TestReport, query: &str) -> Vec<TestRow> {
    let q = query.to_ascii_lowercase();
    let mut rows = Vec::new();
    for failed in [true, false] {
        for (case, c) in report.cases.iter().enumerate().filter(|(_, c)| c.passed != failed) {
            let hits = if q.is_empty() {
                Vec::new()
            } else {
                match c.name.to_ascii_lowercase().find(&q) {
                    Some(s) => vec![s..s + q.len()],
                    None => continue,
                }
            };
            let (icon, icon_color) = if c.passed { ("✓", palette::ACCENT) } else { ("✕", palette::ERROR) };
            let right = c.location.as_ref().map(|(p, l)| format!("{p}:{}", l + 1)).unwrap_or_default();
            rows.push(TestRow { row: PickRow { depth: 0, icon, icon_color, name: c.name.clone(), hits, right, context: false }, case });
        }
    }
    rows
}

/// 試験の結果の見出し（「2 失敗 / 3 成功 / 1 無視」）。
pub fn test_summary(report: &TestReport) -> String {
    let mut s = String::new();
    if report.failed > 0 {
        s.push_str(&format!("{} 失敗 / ", report.failed));
    }
    s.push_str(&format!("{} 成功", report.passed));
    if report.ignored > 0 {
        s.push_str(&format!(" / {} 無視", report.ignored));
    }
    s
}

/// 診断 → 波線（行の文字全体。エラーは赤、警告は黄）。置き場は編集でずらすだけで文書の
/// 長さを知らないので、行はここで文書の中（`len_lines` 行）に収める。
pub fn marks_of(diags: &[Diagnostic], len_lines: u32) -> Vec<LineMark> {
    let last = len_lines.saturating_sub(1);
    diags
        .iter()
        .map(|d| LineMark { line: d.line.min(last), cols: None, color: if d.severity == Severity::Error { palette::ERROR } else { palette::WARN } })
        .collect()
}

/// アウトラインの項目の印と色（モック 14）。
fn icon_of(kind: OutlineKind) -> (&'static str, zc_core::Color) {
    match kind {
        OutlineKind::Function => ("ƒ", palette::FUNCTION),
        OutlineKind::Class => ("C", palette::CLASS),
        OutlineKind::Heading => ("#", palette::KEYWORD),
    }
}

/// 絞り込んだアウトラインの 1 行と、選べる行ならその行番号（0 始まり）。
pub struct OutlineRow {
    pub row: PickRow,
    pub line: Option<u32>,
}

/// `query` で絞り込む（ASCII の大文字小文字を無視した部分一致）。一致した項目の親は
/// 薄い行（選べない）として残す。`query` が空なら全部。
pub fn filter_outline(items: &[OutlineItem], query: &str) -> Vec<OutlineRow> {
    let q = query.to_ascii_lowercase();
    let hit = |name: &str| -> Option<core::ops::Range<usize>> {
        if q.is_empty() {
            return Some(0..0);
        }
        name.to_ascii_lowercase().find(&q).map(|s| s..s + q.len())
    };
    let mut keep: Vec<Option<bool>> = vec![None; items.len()]; // Some(true) = 一致、Some(false) = 親として残す
    for (i, it) in items.iter().enumerate() {
        if hit(&it.name).is_some() {
            keep[i] = Some(true);
            // 親（前にあって、深さが浅いもの）をたどる
            let mut depth = it.depth;
            for j in (0..i).rev() {
                if depth == 0 {
                    break;
                }
                if items[j].depth < depth {
                    depth = items[j].depth;
                    if keep[j].is_none() {
                        keep[j] = Some(false);
                    }
                }
            }
        }
    }
    items
        .iter()
        .zip(keep)
        .filter_map(|(it, k)| {
            let matched = k?;
            let (icon, icon_color) = icon_of(it.kind);
            let hits = if matched { hit(&it.name).filter(|r| !r.is_empty()).into_iter().collect() } else { Vec::new() };
            let row = PickRow { depth: it.depth, icon, icon_color, name: it.name.clone(), hits, right: (it.line + 1).to_string(), context: !matched };
            Some(OutlineRow { row, line: matched.then_some(it.line) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_remote_ipc::TestCase;

    fn item(line: u32, depth: u8, kind: OutlineKind, name: &str) -> OutlineItem {
        OutlineItem { line, depth, kind, name: name.into() }
    }

    #[test]
    fn filter_keeps_parents_as_context() {
        let items = vec![
            item(2, 0, OutlineKind::Class, "Blinker"),
            item(3, 1, OutlineKind::Function, "__init__"),
            item(8, 1, OutlineKind::Function, "make_pattern"),
            item(17, 0, OutlineKind::Function, "main"),
            item(30, 0, OutlineKind::Function, "run_formatted"),
        ];
        let rows = filter_outline(&items, "MA");
        let names: Vec<(&str, bool)> = rows.iter().map(|r| (r.row.name.as_str(), r.row.context)).collect();
        assert_eq!(names, [("Blinker", true), ("make_pattern", false), ("main", false), ("run_formatted", false)]);
        assert_eq!(rows[0].line, None);
        assert_eq!(rows[1].line, Some(8));
        assert_eq!(rows[3].row.hits, vec![7..9]);
        assert_eq!(rows[1].row.right, "9");
        assert_eq!(filter_outline(&items, "").len(), 5);
        assert!(filter_outline(&items, "zzz").is_empty());
    }

    #[test]
    fn diag_key_is_relative_to_the_workspace() {
        let ws = Path::new("/home/u/code");
        assert_eq!(&*diag_key(ws, Path::new("/home/u/code/blink/src/main.rs")), Path::new("blink/src/main.rs"));
        // 作業フォルダの外は絶対パスのまま
        assert_eq!(&*diag_key(ws, Path::new("/tmp/x.py")), Path::new("/tmp/x.py"));
    }

    #[test]
    fn local_check_reports_missing_tool_instead_of_no_errors() {
        let never = CancelToken::never();
        let file = Path::new("/nonexistent/a.ts");
        // 道具そのものが無い
        let r = run_local_check("zcode-no-such-tool {file}", r"line (\d+)", file, &never);
        assert!(matches!(r, Err(CheckFail::Unavailable(_))), "{r:?}");
        if Command::new("bash").arg("-c").arg("true").status().map_or(true, |s| !s.success()) {
            eprintln!("bash が無いので飛ばす");
            return;
        }
        // bash で包んだ check: 中の道具が無いと 127 で終わる。bash の「line 1: … not found」は
        // パターンに合うが、診断にはしない
        let r = run_local_check(r#"bash -c 'exec zcode-no-such-tool "$0"' {file}"#, r"line (\d+)", file, &never);
        match r {
            Err(CheckFail::Unavailable(why)) => assert!(why.contains("zcode-no-such-tool"), "{why}"),
            other => panic!("{other:?}"),
        }
        // 0 以外で終わっても、診断が拾えればふつうの結果
        let r = run_local_check(r#"bash -c 'echo "x: line 3: bad"; exit 1'"#, r"line (\d+)(): (.*)", file, &never).unwrap();
        assert_eq!((r.len(), r[0].line), (1, 2));
        // エラーなし
        assert_eq!(run_local_check("bash -c true", r"line (\d+)", file, &never), Ok(Vec::new()));
        // 書き方の誤り
        assert!(matches!(run_local_check("bash -c 'x", r"line (\d+)", file, &never), Err(CheckFail::Other(_))));
    }

    #[test]
    fn marks_stay_inside_the_document() {
        let ds = [Diagnostic::at_line(2, Severity::Error, ""), Diagnostic::at_line(40, Severity::Warning, "")];
        let m = marks_of(&ds, 10);
        assert_eq!(m.iter().map(|m| m.line).collect::<Vec<_>>(), [2, 9]);
        assert_eq!(m[1].color, palette::WARN);
    }

    #[test]
    fn progress_and_done_texts() {
        assert_eq!(progress_text(JobKind::Check, Phase::Uploading, 12 * 1024, 40 * 1024, 0), "ビルド係: 送信中 12/40KB");
        assert_eq!(progress_text(JobKind::Test, Phase::Running, 0, 0, 8), "ビルド係: 試験中 8 秒");
        assert_eq!(done_text(DoneStatus::Ok, ""), None);
        assert_eq!(done_text(DoneStatus::Failed, "x"), None);
        assert_eq!(done_text(DoneStatus::Offline, "").unwrap(), "ビルド係: オフライン（ビルド係に届かない）");
        assert_eq!(done_text(DoneStatus::Auth, "トークンの権限が広い\n詳細").unwrap(), "ビルド係: トークンの権限が広い");
    }

    #[test]
    fn test_rows_put_failures_first_and_filter() {
        let case = |name: &str, passed: bool, loc: Option<(&str, u32)>| TestCase { name: name.into(), passed, location: loc.map(|(p, l)| (p.to_string(), l)), message: String::new() };
        let report = TestReport {
            passed: 2,
            failed: 1,
            ignored: 1,
            cases: vec![case("tests::parse_one", true, None), case("tests::parse_empty", false, Some(("src/lib.rs", 41))), case("wrap::tests::ascii", true, None)],
        };
        let rows = test_rows(&report, "");
        assert_eq!(rows.iter().map(|r| (r.row.name.as_str(), r.case)).collect::<Vec<_>>(), [("tests::parse_empty", 1), ("tests::parse_one", 0), ("wrap::tests::ascii", 2)]);
        assert_eq!(rows[0].row.right, "src/lib.rs:42");
        assert_eq!(rows[0].row.icon, "✕");
        let rows = test_rows(&report, "PARSE");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].row.hits, vec![7..12]);
        assert_eq!(test_summary(&report), "1 失敗 / 2 成功 / 1 無視");
        assert_eq!(test_summary(&TestReport { passed: 5, ..TestReport::default() }), "5 成功");
    }

    #[test]
    fn string_and_comment_spans_block_completion() {
        let spans = [Span { start: 0, end: 3, kind: TokenKind::Text }, Span { start: 4, end: 10, kind: TokenKind::String }];
        assert!(!in_string_or_comment(&spans, 2));
        assert!(in_string_or_comment(&spans, 6));
        assert!(!in_string_or_comment(&spans, 0));
    }
}
