//! Rust のジョブ（docs/stage3-design.md §4.4）。課題 B3。
//!
//! | 種類 | 中でやること |
//! |---|---|
//! | build | `cargo build --target aarch64-unknown-linux-gnu --profile zc-f5 --message-format=json-diagnostic-rendered-ansi`（`release` のときは `--release`）。実行ファイルが 1 つならそれ、複数なら `opts.entry` か `default-run`、決まらなければ診断「実行ファイルが複数ある」 |
//! | check | `cargo check --target aarch64-unknown-linux-gnu --all-targets --message-format=json` |
//! | test | `cargo test --no-fail-fast --message-format=json`（**x86_64 のまま**）+ libtest の出力を読む |
//!
//! - **F5 用のプロファイル `zc-f5`**: `inherits = "dev"`・`opt-level = 2`・`debug = 0`・
//!   `strip = true`・`lto = false`・`incremental = true`・`codegen-units = 16`。プロジェクトの
//!   `Cargo.toml` は書き換えず、`--config 'profile.zc-f5.…'` で渡す
//! - `RUSTFLAGS` 相当も `--config target.aarch64-unknown-linux-gnu.rustflags=["-Ctarget-cpu=cortex-a53"]`
//! - `CARGO_TARGET_DIR` は `<project_dir>/target`。`--locked` は付けない: `Cargo.lock` が
//!   無い・古いときは cargo が作り直すので、**変わっていたら [`super::JobResult::write_back`]
//!   に入れて実機へ返す**（無いと job id が安定しない）
//! - `Cargo.toml` の無い「1 ファイルだけ」（`opts.entry` = `main.rs`）: `<project_dir>/single/`
//!   に、`[package] name = <ファイル名>`・`edition = "2021"`・`[[bin]] path = <src>/<entry>` の
//!   `Cargo.toml` を作って、同じ手順で回す
//! - ネットワーク: crates.io から依存を取る（コンテナから外へ出られること）。`build.rs` と
//!   手続きマクロはこのコンテナの中で動く
//!
//! # 診断（rustc の JSON → [`zc_wire::msg::Diag`]）
//!
//! - `reason = "compiler-message"` の `message` を読む。`level` が error / warning のものだけ
//!   （note・help は親のメッセージの後ろに 1 行で足す。合計 400 バイトまで）
//! - 位置は `is_primary` の span。**行は 1 始まり → 0 始まり。桁は 1 始まりの「文字数」→
//!   0 始まりの「行の中の UTF-8 のバイト位置」**（span の `text[].text` がその行の中身なので、
//!   そこから数え直す。日本語のコメントの後ろでずれないように）
//! - マクロの展開先・依存 crate の中を指す span は、`expansion` をたどって**手元の
//!   呼び出し元**の位置に寄せる。作業の木の外にしか位置が無ければ `path = ""`（位置なし）
//! - パスは木の根からの相対・`/` 区切りに直す（cargo は crate の根からの相対で出す）
//! - 同じ（パス・行・桁・メッセージ）は 1 つにする（lib と test で 2 回出る）
//! - 上限 [`zc_wire::limits::MAX_DIAGNOSTICS`]（error を先に残す）
//!
//! B3 で決めた細かいところ:
//!
//! - span の寄せ方（rustc 1.100 の JSON で確かめた）: 木の外の span は `expansion.span`
//!   （呼び出し元）へたどる。木の中の span に `expansion` があっても、**手元の `macro_rules!`
//!   の本体の中**（定義の場所 `def_site_span` が木の中で、span が呼び出しの範囲の外）のときだけ
//!   呼び出し元へ寄せる（`foo!` の形の展開だけ。`#[derive]`・`desugaring of …` は寄せない）。
//!   `#[derive]` の対象のフィールドや、`println!` に渡した引数のように、
//!   利用者が書いた字句を指す span はそのまま使う（その方が正確）
//! - メッセージは `message` に、主の span のラベル（「expected `i32`, found `&str`」）を
//!   `: ` でつなぎ、子（note / help）を ` / ` でつなぐ
//! - 本文の無いまとめ（`aborting due to …`・`N warnings emitted`）は捨てる。cargo が失敗した
//!   のに error の診断が 1 つも無いとき（依存が取れない、など）は、ログの末尾の error の行を
//!   位置なしの診断にする
//! - コンパイラの表示（`rendered`）はログに流す（F5 の端末で読める）。JSON の行そのものは
//!   ログに出さない
//! - 試験の絞り込みは `cargo test … -- <filter>`。`-` で始まる絞り込みは断る（libtest の
//!   オプションとして読まれるため）
//! - 試験の落ちた場所（[`zc_wire::msg::TestCase::line`]）も 0 始まり

use super::{
    check_cancelled, failure_line, finish_diags, has_error, io_failure, note_diag, push_diag, read_line, rel_to_root, safe_entry, spawn_failure, spawn_tool, tool_env, truncate_utf8, which,
    JobCtx, JobFailure, JobOutput, JobResult, Tap, TestResults,
};
use crate::pack::pack;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use zc_wire::msg::{ArtifactKind, Diag, FileDiags, Kind, Severity, Status, TestCase};

/// 実機の target。
pub const TARGET: &str = "aarch64-unknown-linux-gnu";
/// F5 用のプロファイルの名前。
pub const F5_PROFILE: &str = "zc-f5";
/// F5 用のプロファイル（`--config` で渡す）。
pub const F5_PROFILE_CONFIG: &[&str] = &[
    "profile.zc-f5.inherits=\"dev\"",
    "profile.zc-f5.opt-level=2",
    "profile.zc-f5.debug=0",
    "profile.zc-f5.strip=true",
    "profile.zc-f5.lto=false",
    "profile.zc-f5.incremental=true",
    "profile.zc-f5.codegen-units=16",
];
/// 実機の CPU（Cortex-A53。LSE などの新しい命令を使わない）。
pub const TARGET_RUSTFLAGS: &str = "target.aarch64-unknown-linux-gnu.rustflags=[\"-Ctarget-cpu=cortex-a53\"]";

/// メッセージ 1 件の上限（バイト）。
const MAX_MESSAGE: usize = 400;
/// 試験の出力を読むために溜める上限。
const MAX_TEST_OUTPUT: usize = 4 << 20;
/// 試験 1 件の落ちたときのメッセージの上限。
const MAX_TEST_MESSAGE: usize = 2 << 10;

/// cargo を回す場所。
struct Layout {
    /// `--manifest-path` に渡すもの。
    manifest: PathBuf,
    /// 道具の作業フォルダ（rustc の相対パスの基準 = ワークスペースの根）。
    workdir: PathBuf,
    /// 書き戻しを見る `Cargo.lock`（1 ファイルだけのときは無い）。
    lock: Option<PathBuf>,
}

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    let cargo = which("cargo")?;
    let src = ctx.src.canonicalize().map_err(|e| io_failure("作業の木が無い", e))?;
    std::fs::create_dir_all(ctx.project_dir).map_err(|e| io_failure("プロジェクトのフォルダを作れない", e))?;
    let project_dir = ctx.project_dir.canonicalize().map_err(|e| io_failure("プロジェクトのフォルダが無い", e))?;

    let layout = match layout(&src, &project_dir, ctx.opts.entry.as_deref()) {
        Ok(l) => l,
        Err(msg) => return Ok(failed(vec![FileDiags { path: String::new(), diags: vec![note_diag(Severity::Error, msg)] }])),
    };
    if let Some(f) = &ctx.opts.filter {
        if f.starts_with('-') {
            return Ok(failed(vec![FileDiags { path: String::new(), diags: vec![note_diag(Severity::Error, "試験の絞り込みが - で始まっている")] }]));
        }
    }
    let lock_before = layout.lock.as_ref().and_then(|p| std::fs::read(p).ok());

    let mut cmd = Command::new(&cargo);
    cmd.current_dir(&layout.workdir);
    cmd.env("CARGO_TARGET_DIR", project_dir.join("target"));
    tool_env(&mut cmd);
    match ctx.kind {
        Kind::Build => {
            cmd.arg("build").arg("--target").arg(TARGET);
            if ctx.opts.release {
                cmd.arg("--release");
            } else {
                cmd.arg("--profile").arg(F5_PROFILE);
                for c in F5_PROFILE_CONFIG {
                    cmd.arg("--config").arg(c);
                }
            }
            cmd.arg("--message-format=json-diagnostic-rendered-ansi");
        }
        Kind::Check => {
            cmd.arg("check").arg("--target").arg(TARGET).arg("--all-targets").arg("--message-format=json");
        }
        Kind::Test => {
            cmd.arg("test").arg("--no-fail-fast").arg("--message-format=json");
        }
    }
    cmd.arg("--manifest-path").arg(&layout.manifest);
    if ctx.kind != Kind::Test {
        cmd.arg("--config").arg(TARGET_RUSTFLAGS);
        // 環境にリンカの指定が無ければ、クロスの gcc を使う
        if std::env::var_os("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER").is_none() && super::find_in_path("aarch64-linux-gnu-gcc").is_some() {
            cmd.arg("--config").arg("target.aarch64-unknown-linux-gnu.linker=\"aarch64-linux-gnu-gcc\"");
        }
    }
    if ctx.kind == Kind::Test {
        if let Some(f) = &ctx.opts.filter {
            cmd.arg("--").arg(f);
        }
    }

    check_cancelled(ctx)?;
    let tap = Tap::new(out);
    let mut diags: Vec<FileDiags> = Vec::new();
    let mut exes: Vec<Exe> = Vec::new();
    let mut test_out: Vec<u8> = Vec::new();
    let kind = ctx.kind;
    let exit = spawn_tool(cmd, &tap, &mut |line| {
        if line.first() == Some(&b'{') {
            if let Ok(v) = serde_json::from_slice::<Value>(line) {
                if v.get("reason").is_some() {
                    handle_message(&v, &src, &layout.workdir, &mut diags, &mut exes, &tap);
                    return false;
                }
            }
        }
        if kind == Kind::Test && test_out.len() + line.len() <= MAX_TEST_OUTPUT {
            test_out.extend_from_slice(line);
        }
        true
    })
    .map_err(|e| spawn_failure("cargo", e))?;
    check_cancelled(ctx)?;

    let mut result = JobResult { exit, complete: exit < 128, ..Default::default() };
    if exit != 0 && !has_error(&diags) && kind != Kind::Test {
        // 依存が取れない・Cargo.toml が読めない、など（rustc の診断にならない失敗）
        let msg = failure_line(&tap.take()).unwrap_or_else(|| format!("cargo が失敗した（終了コード {exit}）"));
        push_diag(&mut diags, "", note_diag(Severity::Error, msg));
    }

    // Cargo.lock が作られた・変わった → 実機へ返す
    if let Some(lock) = &layout.lock {
        if let Ok(after) = std::fs::read(lock) {
            if lock_before.as_deref() != Some(&after[..]) && after.len() <= zc_wire::limits::MAX_WRITEBACK_BYTES {
                if let Ok(text) = String::from_utf8(after) {
                    let rel = rel_to_root(&src, &src, &lock.to_string_lossy()).unwrap_or_else(|| "Cargo.lock".into());
                    result.write_back.push((rel, text));
                }
            }
        }
    }

    match kind {
        Kind::Test => {
            let text = String::from_utf8_lossy(&test_out);
            let tests = parse_libtest(&text, &src);
            if exit != 0 && tests.failed == 0 && !has_error(&diags) {
                let msg = failure_line(&tap.take()).unwrap_or_else(|| format!("cargo test が失敗した（終了コード {exit}）"));
                push_diag(&mut diags, "", note_diag(Severity::Error, msg));
            }
            result.tests = Some(tests);
        }
        Kind::Build if exit == 0 => match pick_exe(&exes, &src, &layout, ctx.opts.entry.as_deref()) {
            Ok(Some(exe)) => {
                let packed = pack(&exe.path, ArtifactKind::Exe, &exe.name, ctx.out_dir).map_err(|e| JobFailure { status: Status::Error, message: e.0 })?;
                result.artifact = Some(packed);
            }
            Ok(None) => push_diag(&mut diags, "", note_diag(Severity::Warning, "実行ファイルが無い（ライブラリだけ）")),
            Err(msg) => {
                push_diag(&mut diags, "", note_diag(Severity::Error, msg));
                result.exit = 1;
            }
        },
        _ => {}
    }
    result.diagnostics = finish_diags(diags);
    Ok(result)
}

fn failed(diagnostics: Vec<FileDiags>) -> JobResult {
    JobResult { exit: 1, diagnostics, complete: true, ..Default::default() }
}

/// cargo を回す場所を決める。プロジェクトの `Cargo.toml` が無ければ、`entry` の 1 ファイル
/// だけの仮の crate を `<project_dir>/single/` に作る。
fn layout(src: &Path, project_dir: &Path, entry: Option<&str>) -> Result<Layout, String> {
    let manifest = src.join("Cargo.toml");
    if manifest.is_file() {
        return Ok(Layout { manifest, workdir: src.to_path_buf(), lock: Some(src.join("Cargo.lock")) });
    }
    let Some(entry) = entry else { return Err("Cargo.toml が無い（1 ファイルだけなら入口のファイルを指定する）".into()) };
    if !safe_entry(entry) || !entry.ends_with(".rs") || !src.join(entry).is_file() {
        return Err(format!("入口のファイルが無い: {entry}"));
    }
    let stem = Path::new(entry).file_stem().and_then(|s| s.to_str()).unwrap_or("main");
    let mut name: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c.to_ascii_lowercase() } else { '_' }).collect();
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        name = format!("bin_{name}");
    }
    let dir = project_dir.join("single");
    std::fs::create_dir_all(&dir).map_err(|e| format!("仮の crate を作れない: {e}"))?;
    let path = src.join(entry);
    let toml = format!(
        "# zc-buildd が作った仮の crate（Cargo.toml の無い 1 ファイルだけのプロジェクト）\n[package]\nname = \"zc-single\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[[bin]]\nname = \"{name}\"\npath = {path}\n\n[workspace]\n",
        path = serde_json::to_string(&path.to_string_lossy()).expect("JSON にできる"),
    );
    let manifest = dir.join("Cargo.toml");
    if std::fs::read_to_string(&manifest).ok().as_deref() != Some(toml.as_str()) {
        std::fs::write(&manifest, toml).map_err(|e| format!("仮の crate を作れない: {e}"))?;
    }
    Ok(Layout { manifest, workdir: dir, lock: None })
}

/// cargo の出した実行ファイル。
#[derive(Clone, Debug)]
struct Exe {
    name: String,
    path: PathBuf,
    src_path: PathBuf,
}

fn handle_message(v: &Value, src: &Path, workdir: &Path, diags: &mut Vec<FileDiags>, exes: &mut Vec<Exe>, out: &dyn JobOutput) {
    match v.get("reason").and_then(Value::as_str) {
        Some("compiler-message") => {
            if let Some(msg) = v.get("message") {
                if let Some(r) = msg.get("rendered").and_then(Value::as_str) {
                    let level = msg.get("level").and_then(Value::as_str).unwrap_or("");
                    if level.starts_with("error") || level == "warning" {
                        out.log(r.as_bytes());
                    }
                }
                diag_from_message(msg, src, workdir, diags);
            }
        }
        Some("compiler-artifact") => {
            let target = v.get("target");
            let is_bin = target.and_then(|t| t.get("kind")).and_then(Value::as_array).is_some_and(|k| k.iter().any(|x| x == "bin"));
            let exe = v.get("executable").and_then(Value::as_str);
            if let (true, Some(exe)) = (is_bin, exe) {
                let name = target.and_then(|t| t.get("name")).and_then(Value::as_str).unwrap_or("main").to_string();
                let src_path = target.and_then(|t| t.get("src_path")).and_then(Value::as_str).map(PathBuf::from).unwrap_or_default();
                let path = PathBuf::from(exe);
                if !exes.iter().any(|e| e.path == path) {
                    exes.push(Exe { name, path, src_path });
                }
            }
        }
        _ => {}
    }
}

/// 実行ファイルを 1 つに決める。
fn pick_exe(exes: &[Exe], src: &Path, layout: &Layout, entry: Option<&str>) -> Result<Option<Exe>, String> {
    match exes.len() {
        0 => return Ok(None),
        1 => return Ok(Some(exes[0].clone())),
        _ => {}
    }
    if let Some(entry) = entry.filter(|e| safe_entry(e)) {
        let want = src.join(entry);
        if let Some(e) = exes.iter().find(|e| e.src_path == want) {
            return Ok(Some(e.clone()));
        }
    }
    if let Some(name) = default_run(&layout.manifest) {
        if let Some(e) = exes.iter().find(|e| e.name == name) {
            return Ok(Some(e.clone()));
        }
    }
    let names: Vec<&str> = exes.iter().map(|e| e.name.as_str()).collect();
    Err(format!("実行ファイルが複数ある（{}）。どれを動かすか決められない", names.join("・")))
}

/// `Cargo.toml` の `default-run = "名前"`（toml の crate を使わない簡易な読み方）。
fn default_run(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    text.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix("default-run")?.trim_start().strip_prefix('=')?.trim();
        let rest = rest.strip_prefix('"')?;
        Some(rest[..rest.find('"')?].to_string())
    })
}

/// cargo の `--message-format=json` の 1 行を読んで、診断を `into` に足す。
/// `crate_root_hint` は、相対パスを木の根からに直すための crate の根（`manifest_path` から）。
pub fn parse_cargo_message(line: &str, src_root: &Path, into: &mut Vec<FileDiags>) {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    if v.get("reason").and_then(Value::as_str) != Some("compiler-message") {
        return;
    }
    if let Some(msg) = v.get("message") {
        diag_from_message(msg, src_root, src_root, into);
    }
}

/// rustc の診断（`message`）1 件を読む。`workdir` は rustc の相対パスの基準。
fn diag_from_message(msg: &Value, src: &Path, workdir: &Path, into: &mut Vec<FileDiags>) {
    let level = msg.get("level").and_then(Value::as_str).unwrap_or("");
    let severity = if level.starts_with("error") {
        Severity::Error
    } else if level == "warning" {
        Severity::Warning
    } else {
        return;
    };
    let text = msg.get("message").and_then(Value::as_str).unwrap_or("").trim();
    let spans = msg.get("spans").and_then(Value::as_array).map(|v| v.as_slice()).unwrap_or(&[]);
    if spans.is_empty() && (text.starts_with("aborting due to") || text.ends_with("emitted") || text.starts_with("For more information")) {
        return;
    }
    let code = msg.get("code").and_then(|c| c.get("code")).and_then(Value::as_str).map(str::to_string);

    let primary = spans.iter().find(|s| s.get("is_primary").and_then(Value::as_bool) == Some(true));
    let mut message = text.to_string();
    if let Some(label) = primary.and_then(|s| s.get("label")).and_then(Value::as_str) {
        if !label.is_empty() {
            message.push_str(": ");
            message.push_str(label);
        }
    }
    if let Some(children) = msg.get("children").and_then(Value::as_array) {
        for c in children {
            let (Some(l), Some(m)) = (c.get("level").and_then(Value::as_str), c.get("message").and_then(Value::as_str)) else { continue };
            if m.is_empty() || message.len() >= MAX_MESSAGE {
                continue;
            }
            message.push_str(&format!(" / {l}: {m}"));
        }
    }
    let message = truncate_utf8(&message, MAX_MESSAGE);

    // 主の span（複数あれば順に）を手元の位置に寄せる
    let resolved = spans
        .iter()
        .filter(|s| s.get("is_primary").and_then(Value::as_bool) == Some(true))
        .find_map(|s| resolve_span(s, src, workdir, 0));
    match resolved {
        Some((path, d)) => push_diag(into, &path, Diag { severity, code, message, ..d }),
        None => push_diag(into, "", Diag { line: 0, col: None, end: None, severity, code, message }),
    }
}

/// span が木の中ならそのパス、外なら `None`。
fn span_path(span: &Value, src: &Path, workdir: &Path) -> Option<String> {
    rel_to_root(src, workdir, span.get("file_name")?.as_str()?)
}

fn num(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(Value::as_u64)
}

/// span が `outer` の範囲の中か（同じファイル・バイトの範囲）。
fn span_within(span: &Value, outer: &Value) -> bool {
    span.get("file_name") == outer.get("file_name")
        && matches!((num(span, "byte_start"), num(span, "byte_end"), num(outer, "byte_start"), num(outer, "byte_end")), (Some(a), Some(b), Some(c), Some(d)) if a >= c && b <= d)
}

/// span を手元の位置（パスと、位置だけ入った [`Diag`]）にする。寄せ方はファイルの先頭。
fn resolve_span(span: &Value, src: &Path, workdir: &Path, depth: u32) -> Option<(String, Diag)> {
    if depth > 32 {
        return None;
    }
    let expansion = span.get("expansion").filter(|e| !e.is_null());
    match span_path(span, src, workdir) {
        Some(path) => {
            if let Some(exp) = expansion {
                let call = exp.get("span")?;
                let def_local = exp.get("def_site_span").filter(|d| !d.is_null()).and_then(|d| span_path(d, src, workdir)).is_some();
                // `foo!` の形だけが macro_rules!（属性・derive・「desugaring of …」は違う）
                let is_bang = exp.get("macro_decl_name").and_then(Value::as_str).is_some_and(|n| n.ends_with('!'));
                if def_local && is_bang && !span_within(span, call) {
                    // 手元の macro_rules! の本体の中 → 呼び出し元へ
                    return resolve_span(call, src, workdir, depth + 1);
                }
            }
            Some((path.clone(), span_position(span, &src.join(&path))))
        }
        None => resolve_span(expansion?.get("span")?, src, workdir, depth + 1),
    }
}

/// span の位置を 0 始まりの行と、行の中の UTF-8 のバイト位置にする。
fn span_position(span: &Value, file: &Path) -> Diag {
    let line_start = num(span, "line_start").unwrap_or(1).max(1) as u32;
    let line_end = num(span, "line_end").unwrap_or(line_start as u64).max(1) as u32;
    let col_start = num(span, "column_start").unwrap_or(1) as u32;
    let col_end = num(span, "column_end").unwrap_or(col_start as u64) as u32;
    let texts: Vec<&str> = span.get("text").and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.get("text").and_then(Value::as_str)).collect()).unwrap_or_default();
    let line_text = |n: u32, idx: Option<&&str>| -> Option<String> { idx.map(|s| s.to_string()).or_else(|| read_line(file, n - 1)) };
    let first = line_text(line_start, texts.first());
    let last = if line_end == line_start { first.clone() } else { line_text(line_end, texts.last()) };
    let col = first.as_deref().map(|t| char_col_to_byte(t, col_start)).unwrap_or(col_start.saturating_sub(1));
    let end_col = last.as_deref().map(|t| char_col_to_byte(t, col_end)).unwrap_or(col_end.saturating_sub(1));
    Diag { line: line_start - 1, col: Some(col), end: Some((line_end - 1, end_col)), severity: Severity::Error, code: None, message: String::new() }
}

/// 1 始まりの文字数の桁を、その行の中の UTF-8 のバイト位置（0 始まり）に直す。
/// 行より先を指していたら行末。
pub fn char_col_to_byte(line_text: &str, col_1based_chars: u32) -> u32 {
    let skip = col_1based_chars.saturating_sub(1) as usize;
    line_text.char_indices().nth(skip).map(|(i, _)| i).unwrap_or(line_text.len()) as u32
}

/// libtest の出力（`test foo::bar ... ok` / `FAILED` / `ignored`、`---- name stdout ----`、
/// `panicked at src/x.rs:12:5:`）から結果を作る。
pub fn parse_libtest(output: &str, src_root: &Path) -> TestResults {
    let mut r = TestResults::default();
    let lines: Vec<&str> = output.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(rest) = line.strip_prefix("test ") {
            if let Some(pos) = rest.rfind(" ... ") {
                let name = rest[..pos].to_string();
                let verdict = rest[pos + 5..].trim();
                if verdict == "ok" {
                    r.passed += 1;
                    r.cases.push(TestCase { name, passed: true, path: None, line: None, message: String::new() });
                } else if verdict.starts_with("FAILED") {
                    r.failed += 1;
                    r.cases.push(TestCase { name, passed: false, path: None, line: None, message: String::new() });
                } else if verdict.starts_with("ignored") {
                    r.ignored += 1;
                }
            }
        } else if let Some(name) = line.strip_prefix("---- ").and_then(|l| l.strip_suffix(" stdout ----").or_else(|| l.strip_suffix(" stderr ----"))) {
            // 落ちた試験の出力。次の「---- 」か「failures:」まで
            let mut j = i + 1;
            let mut body = Vec::new();
            while j < lines.len() && !lines[j].starts_with("---- ") && lines[j] != "failures:" {
                body.push(lines[j]);
                j += 1;
            }
            if let Some(case) = r.cases.iter_mut().rev().find(|c| c.name == name && !c.passed && c.message.is_empty()) {
                fill_failure(case, &body, src_root);
            }
            i = j;
            continue;
        }
        i += 1;
    }
    r
}

/// `thread 'x' panicked at src/lib.rs:9:9:` と、続くメッセージを読む。
fn fill_failure(case: &mut TestCase, body: &[&str], src_root: &Path) {
    let mut msg = Vec::new();
    for (k, l) in body.iter().enumerate() {
        let Some(pos) = l.find("panicked at ") else { continue };
        let loc = l[pos + "panicked at ".len()..].trim_end_matches(':');
        // 古い形: panicked at 'msg', src/x.rs:1:2
        let (loc, old_msg) = match loc.rfind("', ") {
            Some(q) if loc.starts_with('\'') => (&loc[q + 3..], Some(&loc[1..q])),
            _ => (loc, None),
        };
        let mut parts = loc.rsplitn(3, ':');
        let (_col, line, file) = (parts.next(), parts.next(), parts.next());
        if let (Some(file), Some(line)) = (file, line.and_then(|l| l.parse::<u32>().ok())) {
            if let Some(p) = rel_to_root(src_root, src_root, file) {
                case.path = Some(p);
                case.line = Some(line.saturating_sub(1));
            }
        }
        if let Some(m) = old_msg {
            msg.push(m.to_string());
        }
        for l2 in &body[k + 1..] {
            if l2.starts_with("note: run with `RUST_BACKTRACE") || l2.starts_with("stack backtrace:") {
                break;
            }
            msg.push(l2.to_string());
        }
        break;
    }
    let text = msg.join("\n");
    case.message = truncate_utf8(text.trim(), MAX_TEST_MESSAGE);
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{skip_without, Collect, TempDir};
    use super::*;
    use std::sync::atomic::AtomicBool;
    use zc_wire::msg::JobOpts;

    const SAMPLE_ROOT: &str = "/tmp/zc-sample/src";

    fn data(name: &str) -> String {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)).unwrap()
    }

    fn parse_all(name: &str) -> Vec<FileDiags> {
        let mut v = Vec::new();
        for line in data(name).lines() {
            parse_cargo_message(line, Path::new(SAMPLE_ROOT), &mut v);
        }
        finish_diags(v)
    }

    fn find<'a>(v: &'a [FileDiags], path: &str, line: u32) -> Vec<&'a Diag> {
        v.iter().filter(|f| f.path == path).flat_map(|f| &f.diags).filter(|d| d.line == line).collect()
    }

    // 受け入れ試験（課題 B3）。tests/data/ に rustc の JSON の実物を置いて突き合わせる
    // （作り方は tests/data/gen-rustc.sh）。
    #[test]
    fn columns_are_utf8_byte_offsets() {
        // 「日本語」は 3 文字 9 バイト。8 文字目（1 始まり）の x は 13 バイト目（0 始まり）
        assert_eq!(char_col_to_byte("// 日本語 x", 8), 13);
        assert_eq!(char_col_to_byte("let x = 1;", 5), 4);
        assert_eq!(char_col_to_byte("ab", 99), 2);
    }

    #[test]
    fn real_rustc_json_type_error_and_japanese() {
        let v = parse_all("rustc-errors.jsonl");
        // 型エラー（E0308）: 2 行目の "s"
        let d = find(&v, "src/lib.rs", 1);
        assert_eq!(d.len(), 1, "{v:?}");
        assert_eq!(d[0].col, Some(4));
        assert_eq!(d[0].end, Some((1, 7)));
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].code.as_deref(), Some("E0308"));
        assert!(d[0].message.starts_with("mismatched types: expected `i32`, found `&str`"), "{}", d[0].message);
        // 日本語のコメントの後ろ: rustc は 29 文字目。「    let _x: i32 = /* 」が 21 バイト + 日本語 9 バイト + 「 */ 」4 バイト = 34
        let d = find(&v, "src/lib.rs", 5);
        assert_eq!(d.len(), 1);
        let line = "    let _x: i32 = /* 日本語 */ \"y\";";
        assert_eq!(d[0].col, Some(line.find("\"y\"").unwrap() as u32));
        assert_eq!(d[0].col, Some(34));
        assert_eq!(d[0].end, Some((5, 37)));
    }

    #[test]
    fn real_rustc_json_macros_derive_and_warnings() {
        let v = parse_all("rustc-errors.jsonl");
        // 手元の macro_rules! の本体の中（11 行目）→ 呼び出し元 `bad!();`（16 行目 5 文字目）に寄る
        assert!(find(&v, "src/lib.rs", 10).is_empty(), "マクロの本体に残った: {v:?}");
        let d = find(&v, "src/lib.rs", 15);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].col, Some(4));
        assert!(d[0].message.contains("expected `u8`"));
        // derive の対象のフィールドは、利用者が書いた場所のまま（22 行目の `f: NoDebug`）
        let d = find(&v, "src/lib.rs", 21);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code.as_deref(), Some("E0277"));
        assert_eq!(d[0].col, Some(4));
        assert!(d[0].message.contains("help: consider annotating"));
        // 警告: lib と lib の試験で 2 回出るが 1 つにする
        let d = find(&v, "src/lib.rs", 25);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].code.as_deref(), Some("unused_variables"));
        assert_eq!(d[0].col, Some(8));
        // 全部で 5 件（failure-note は捨てる）。メッセージは 400 バイトまで
        let total: usize = v.iter().map(|f| f.diags.len()).sum();
        assert_eq!(total, 5, "{v:?}");
        assert!(v.iter().flat_map(|f| &f.diags).all(|d| d.message.len() <= MAX_MESSAGE + 3));
    }

    #[test]
    fn real_rustc_json_dependency_and_std_macro() {
        // 依存 crate（木の外）だけを指す → 位置なし
        let v = parse_all("rustc-dep.jsonl");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].path, "");
        assert_eq!(v[0].diags.len(), 1);
        assert_eq!(v[0].diags[0].col, None);
        assert!(v[0].diags[0].message.contains("mismatched types"));
        // println! に渡した引数（展開は標準ライブラリの中）→ 利用者が書いた `P` の位置
        let v = parse_all("rustc-std-macro.jsonl");
        let d = find(&v, "src/lib.rs", 2);
        assert_eq!(d.len(), 1, "{v:?}");
        assert_eq!(d[0].col, Some(19));
    }

    #[test]
    fn real_libtest_output() {
        let r = parse_libtest(&data("libtest.txt"), Path::new(SAMPLE_ROOT));
        assert_eq!((r.passed, r.failed, r.ignored), (1, 1, 1));
        let f = r.cases.iter().find(|c| !c.passed).unwrap();
        assert_eq!(f.name, "tests::fails");
        assert_eq!(f.path.as_deref(), Some("src/lib.rs"));
        assert_eq!(f.line, Some(8));
        assert!(f.message.contains("足し算が違う") && f.message.contains("left: 3"), "{}", f.message);
        assert!(!f.message.contains("RUST_BACKTRACE"));
    }

    // ---- 通しの試験（開発コンテナの cargo と aarch64 のリンカで。依存なし・ネットワーク不要）----

    struct Job {
        _t: TempDir,
        src: PathBuf,
        project: PathBuf,
        out: PathBuf,
    }

    fn job(files: &[(&str, &str)]) -> Job {
        let t = TempDir::new("rust");
        let src = t.path().join("proj/src");
        for (p, text) in files {
            let f = src.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        std::fs::create_dir_all(&src).unwrap();
        let project = t.path().join("proj");
        let out = t.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        Job { src, project, out, _t: t }
    }

    fn run_job(j: &Job, kind: Kind, opts: &JobOpts) -> (Result<JobResult, JobFailure>, Collect) {
        let cancelled = AtomicBool::new(false);
        let ctx = JobCtx { job: "j", kind, lang: zc_wire::msg::Lang::Rust, opts, src: &j.src, project_dir: &j.project, out_dir: &j.out, cancelled: &cancelled };
        let out = Collect::default();
        (run(&ctx, &out), out)
    }

    const CARGO_TOML: &str = "[package]\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

    #[test]
    fn e2e_check_reports_line_and_byte_column() {
        if skip_without(&["cargo"]) {
            return;
        }
        let j = job(&[("Cargo.toml", CARGO_TOML), ("src/main.rs", "fn main() {\n    let _s: i32 = /* 日本語 */ \"x\";\n}\n")]);
        let (r, _) = run_job(&j, Kind::Check, &JobOpts::default());
        let r = r.unwrap();
        assert_ne!(r.exit, 0);
        assert!(r.complete);
        let d = find(&r.diagnostics, "src/main.rs", 1);
        assert_eq!(d.len(), 1, "{:?}", r.diagnostics);
        assert_eq!(d[0].col, Some("    let _s: i32 = /* 日本語 */ ".len() as u32));
        // Cargo.lock が無かった → 作られたものを書き戻す
        assert_eq!(r.write_back.len(), 1);
        assert_eq!(r.write_back[0].0, "Cargo.lock");
        assert!(r.write_back[0].1.contains("name = \"hello\""));
        // 2 回目は lock が同じなので書き戻さない
        let (r2, _) = run_job(&j, Kind::Check, &JobOpts::default());
        assert!(r2.unwrap().write_back.is_empty());
    }

    #[test]
    fn e2e_build_gives_aarch64_executable() {
        if skip_without(&["cargo", "aarch64-linux-gnu-gcc", "aarch64-linux-gnu-strip", "zstd"]) {
            return;
        }
        let j = job(&[("Cargo.toml", CARGO_TOML), ("src/main.rs", "fn main() { println!(\"hi\"); }\n")]);
        let (r, out) = run_job(&j, Kind::Build, &JobOpts::default());
        let r = r.unwrap();
        assert_eq!(r.exit, 0, "{}", out.text());
        let a = r.artifact.expect("成果物");
        assert_eq!(a.name, "hello");
        assert_eq!(a.kind, ArtifactKind::Exe);
        assert!(a.zpath.starts_with(&j.out));
        // F5 のプロファイルで作った
        assert!(j.project.join("target").join(TARGET).join(F5_PROFILE).join("hello").is_file());
        let back = j.out.join("back");
        assert!(Command::new("zstd").args(["-q", "-d", "-o"]).arg(&back).arg(&a.zpath).status().unwrap().success());
        let file = Command::new("file").arg(&back).output().unwrap();
        let file = String::from_utf8_lossy(&file.stdout);
        assert!(file.contains("ARM aarch64"), "{file}");
    }

    #[test]
    fn e2e_build_failure_does_not_pack() {
        if skip_without(&["cargo"]) {
            return;
        }
        let j = job(&[("Cargo.toml", CARGO_TOML), ("src/main.rs", "fn main() { let x: u8 = 300; }\n")]);
        let (r, out) = run_job(&j, Kind::Build, &JobOpts::default());
        let r = r.unwrap();
        assert_ne!(r.exit, 0);
        assert!(r.artifact.is_none());
        assert!(has_error(&r.diagnostics));
        // コンパイラの表示はログに流れる
        assert!(out.text().contains("literal out of range"), "{}", out.text());
    }

    #[test]
    fn e2e_single_file_without_cargo_toml() {
        if skip_without(&["cargo"]) {
            return;
        }
        let j = job(&[("main.rs", "fn main() {\n    let v: i32 = \"no\";\n}\n")]);
        let opts = JobOpts { entry: Some("main.rs".into()), ..Default::default() };
        let (r, _) = run_job(&j, Kind::Check, &opts);
        let r = r.unwrap();
        let d = find(&r.diagnostics, "main.rs", 1);
        assert_eq!(d.len(), 1, "{:?}", r.diagnostics);
        assert_eq!(d[0].col, Some(17));
        assert!(r.write_back.is_empty(), "仮の crate の lock は書き戻さない");
        // 入口が無い・木の外を指す
        let bad = JobOpts { entry: Some("../x.rs".into()), ..Default::default() };
        let r = run_job(&j, Kind::Check, &bad).0.unwrap();
        assert_eq!(r.diagnostics[0].path, "");
        let r = run_job(&j, Kind::Check, &JobOpts::default()).0.unwrap();
        assert!(r.diagnostics[0].diags[0].message.contains("Cargo.toml が無い"));
    }

    #[test]
    fn e2e_test_counts_and_failure_location() {
        if skip_without(&["cargo"]) {
            return;
        }
        let lib = "pub fn add(a: i32, b: i32) -> i32 { a + b }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn passes() { assert_eq!(super::add(1, 2), 3); }\n    #[test]\n    fn fails() {\n        assert_eq!(super::add(1, 2), 4);\n    }\n    #[test]\n    #[ignore]\n    fn skipped() {}\n}\n";
        let j = job(&[("Cargo.toml", CARGO_TOML), ("src/lib.rs", lib)]);
        let (r, _) = run_job(&j, Kind::Test, &JobOpts::default());
        let r = r.unwrap();
        assert_ne!(r.exit, 0);
        let t = r.tests.unwrap();
        assert_eq!((t.passed, t.failed, t.ignored), (1, 1, 1));
        let f = t.cases.iter().find(|c| !c.passed).unwrap();
        assert_eq!((f.path.as_deref(), f.line), (Some("src/lib.rs"), Some(8)));
        // 絞り込み
        let opts = JobOpts { filter: Some("passes".into()), ..Default::default() };
        let t = run_job(&j, Kind::Test, &opts).0.unwrap();
        assert_eq!(t.exit, 0);
        assert_eq!(t.tests.unwrap().passed, 1);
        let opts = JobOpts { filter: Some("--logfile=/tmp/x".into()), ..Default::default() };
        assert_ne!(run_job(&j, Kind::Test, &opts).0.unwrap().exit, 0);
    }

    #[test]
    fn e2e_multiple_binaries() {
        if skip_without(&["cargo", "aarch64-linux-gnu-gcc", "aarch64-linux-gnu-strip", "zstd"]) {
            return;
        }
        let j = job(&[("Cargo.toml", CARGO_TOML), ("src/main.rs", "fn main() {}\n"), ("src/bin/other.rs", "fn main() {}\n")]);
        let r = run_job(&j, Kind::Build, &JobOpts::default()).0.unwrap();
        assert_eq!(r.exit, 1);
        assert!(r.diagnostics[0].diags[0].message.contains("実行ファイルが複数ある"), "{:?}", r.diagnostics);
        let opts = JobOpts { entry: Some("src/bin/other.rs".into()), ..Default::default() };
        let r = run_job(&j, Kind::Build, &opts).0.unwrap();
        assert_eq!(r.artifact.unwrap().name, "other");
    }

    #[test]
    fn missing_cargo_is_an_error() {
        // PATH を変えずに確かめられるのは which だけ
        let f = which("cargo-does-not-exist").unwrap_err();
        assert_eq!(f.status, Status::Error);
    }
}
