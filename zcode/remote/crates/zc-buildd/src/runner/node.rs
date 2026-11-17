//! JavaScript / TypeScript（npm の依存あり）のジョブ（docs/stage3-design.md §4.4）。課題 B4。
//!
//! 木の根は `package.json` のあるフォルダ。`opts.entry` は F5 を押したファイル。
//!
//! | 種類 | 中でやること |
//! |---|---|
//! | （共通） | `package-lock.json` があれば `npm ci --ignore-scripts --no-audit --no-fund`、無ければ `npm install --ignore-scripts --package-lock-only` で作ってから `npm ci`（作った lock は書き戻す）。`node_modules` は `<project_dir>/node_modules` に置き、lock の SHA-256 が前回と同じなら飛ばす |
//! | check | `tsc --noEmit --pretty false`（`tsconfig.json` があればそれ、無ければ実機と同じ既定: `--strict --target es2022 --module esnext --moduleResolution bundler --skipLibCheck`、JS は `--allowJs` だけ。型は検査せず構文の誤りだけ） |
//! | build | check と同じ型検査 → 通ったら `esbuild <entry> --bundle --platform=node --format=esm --target=es2022 --outfile=<out>/<名前>.js`（LLRT が持っている `node:*` は外に残す）。成果物の種類は `Js` |
//! | test | `npm test`（`package.json` に `scripts.test` があるとき。**`--ignore-scripts` は install だけ**で、試験は明示して頼まれたものなので動かす）。結果は終了コードと出力だけ（件数は 0 / 1） |
//!
//! `postinstall` などのインストールスクリプトは動かさない。動かさないと使えない依存
//! （ネイティブのアドオン）は、どのみち LLRT で動かない。
//!
//! 診断は tsc の出力 `path(行,桁): error TS2322: …`（行・桁は 1 始まり。**桁は UTF-16 の
//! 単位**なので、その行を読んで UTF-8 のバイト位置に直す）。
//!
//! B4 で決めた細かいところ:
//!
//! - `node_modules` を `<project_dir>` に置くため、`package.json` と `package-lock.json` を
//!   `<project_dir>` に写して、そこで npm を回す（作業の木 `<project_dir>/src` から見て親の
//!   `node_modules` なので、node・tsc・esbuild の探し方でそのまま見つかる。作業の木は写すたびに
//!   目録とぴったり同じにされるので、木の中には置けない）。そのため **`file:../x` のような
//!   相対パスの依存は使えない**
//! - lock が古くて `npm ci` が断ったら、`--package-lock-only` で作り直して 1 回だけやり直す
//!   （作り直した lock は書き戻す）
//! - `tsc` はイメージに入っているもの（TypeScript 7）を先に使い、無ければプロジェクトの
//!   `node_modules/.bin/tsc`。`esbuild` も同じ順
//! - `tsconfig.json` が無いときに型検査するファイル: `entry` があればそれ（tsc が import を
//!   たどる）、無ければ木の中の .ts / .mts / .cts（JS だけのプロジェクトなら .js / .mjs / .cjs）
//! - build は `entry` が要る（無ければ `package.json` の `main`）

use super::{
    check_cancelled, failure_line, find_in_path, finish_diags, has_error, io_failure, note_diag, push_diag, read_line, rel_to_root, safe_entry, spawn_failure, spawn_tool, tool_env,
    truncate_utf8, which, JobCtx, JobFailure, JobOutput, JobResult, Tap, TestResults,
};
use crate::pack::pack;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use zc_wire::msg::{ArtifactKind, Diag, FileDiags, Kind, Severity, Status};

/// 型検査の既定（`tsconfig.json` が無いとき。実機の `check` と同じ）。
pub const TSC_DEFAULTS: &[&str] = &["--strict", "--target", "es2022", "--module", "esnext", "--moduleResolution", "bundler", "--skipLibCheck"];
/// npm の install に付けるもの。
const NPM_FLAGS: &[&str] = &["--ignore-scripts", "--no-audit", "--no-fund"];
/// 前回の install の印（package.json と lock の SHA-256）。
const STAMP: &str = ".zc-npm-stamp";

/// 途中で終わったとき（失敗の結果をそのまま返す）。
enum Stop {
    Result(Box<JobResult>),
    Failure(JobFailure),
}

impl From<JobFailure> for Stop {
    fn from(f: JobFailure) -> Stop {
        Stop::Failure(f)
    }
}

pub fn run(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
    match run_inner(ctx, out) {
        Ok(r) => Ok(r),
        Err(Stop::Result(r)) => Ok(*r),
        Err(Stop::Failure(f)) => Err(f),
    }
}

fn failed(exit: i32, msg: impl Into<String>) -> Stop {
    Stop::Result(Box::new(JobResult { exit, complete: true, diagnostics: vec![FileDiags { path: String::new(), diags: vec![note_diag(Severity::Error, msg)] }], ..Default::default() }))
}

fn run_inner(ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, Stop> {
    let npm = which("npm")?;
    which("node")?;
    let src = ctx.src.canonicalize().map_err(|e| io_failure("作業の木が無い", e))?;
    std::fs::create_dir_all(ctx.project_dir).map_err(|e| io_failure("プロジェクトのフォルダを作れない", e))?;
    let pdir = ctx.project_dir.canonicalize().map_err(|e| io_failure("プロジェクトのフォルダが無い", e))?;
    let pkg_path = src.join("package.json");
    let pkg_text = std::fs::read_to_string(&pkg_path).map_err(|_| failed(1, "package.json が無い"))?;
    let pkg: Value = serde_json::from_str(&pkg_text).map_err(|e| failed(1, format!("package.json が読めない: {e}")))?;

    let mut result = JobResult { complete: true, ..Default::default() };
    install(ctx, &npm, &src, &pdir, &pkg_text, out, &mut result)?;
    check_cancelled(ctx)?;

    if ctx.kind == Kind::Test {
        let mut cmd = Command::new(&npm);
        cmd.current_dir(&src).arg("test");
        tool_env(&mut cmd);
        let exit = spawn_tool(cmd, out, &mut |_| true).map_err(|e| spawn_failure("npm", e))?;
        check_cancelled(ctx)?;
        result.exit = exit;
        result.complete = exit < 128;
        let ok = exit == 0;
        result.tests = Some(TestResults { passed: ok as u32, failed: !ok as u32, ignored: 0, cases: Vec::new() });
        return Ok(result);
    }

    // 型検査（check と build で同じ）
    let tsc = find_in_path("tsc").or_else(|| local_bin(&pdir, "tsc")).ok_or_else(|| JobFailure { status: Status::Error, message: "tsc が無い".into() })?;
    let mut cmd = Command::new(&tsc);
    cmd.current_dir(&src).arg("--noEmit").arg("--pretty").arg("false");
    if src.join("tsconfig.json").is_file() {
        cmd.arg("-p").arg("tsconfig.json");
    } else {
        let files = match ctx.opts.entry.as_deref() {
            Some(e) if safe_entry(e) && src.join(e).is_file() => vec![e.to_string()],
            Some(e) => return Err(failed(1, format!("入口のファイルが無い: {e}"))),
            None => script_files(&src),
        };
        if files.is_empty() {
            return Err(failed(1, "型検査するファイルが無い"));
        }
        cmd.args(TSC_DEFAULTS);
        if files.iter().any(|f| is_js(f)) {
            // JS は構文の誤りだけ（実機の `language.toml` の check と同じ。`--checkJs` にすると
            // LLRT の `process` などが型の誤りになって、ビルドが通らない。2026-11-17 実機で確認）
            cmd.arg("--allowJs");
        }
        cmd.args(files.iter().map(|f| format!("./{f}")));
    }
    tool_env(&mut cmd);
    let tap = Tap::new(out);
    let mut tsc_out = String::new();
    let exit = spawn_tool(cmd, &tap, &mut |line| {
        if tsc_out.len() < (4 << 20) {
            tsc_out.push_str(&String::from_utf8_lossy(line));
        }
        true
    })
    .map_err(|e| spawn_failure("tsc", e))?;
    check_cancelled(ctx)?;
    let mut diags = Vec::new();
    parse_tsc(&tsc_out, &src, &mut diags);
    if exit != 0 && !has_error(&diags) {
        let msg = failure_line(&tap.take()).unwrap_or_else(|| format!("tsc が失敗した（終了コード {exit}）"));
        push_diag(&mut diags, "", note_diag(Severity::Error, msg));
    }
    result.exit = exit;
    result.complete = exit < 128;

    if ctx.kind == Kind::Build && exit == 0 {
        let entry = ctx.opts.entry.clone().or_else(|| pkg.get("main").and_then(Value::as_str).map(|m| m.trim_start_matches("./").to_string()));
        let Some(entry) = entry.filter(|e| safe_entry(e) && src.join(e).is_file()) else {
            return Err(failed(1, "入口のファイルが分からない（package.json の main か、F5 を押したファイル）"));
        };
        let esbuild = find_in_path("esbuild").or_else(|| local_bin(&pdir, "esbuild")).ok_or_else(|| JobFailure { status: Status::Error, message: "esbuild が無い".into() })?;
        let stem = Path::new(&entry).file_stem().and_then(|s| s.to_str()).unwrap_or("main").to_string();
        let name = format!("{stem}.js");
        let js_dir = pdir.join("js-out");
        std::fs::create_dir_all(&js_dir).map_err(|e| io_failure("出力のフォルダを作れない", e))?;
        let js = js_dir.join(&name);
        let _ = std::fs::remove_file(&js);
        let mut cmd = Command::new(&esbuild);
        cmd.current_dir(&src)
            .arg(format!("./{entry}"))
            .args(["--bundle", "--platform=node", "--format=esm", "--target=es2022", "--log-level=warning"])
            .arg(format!("--outfile={}", js.display()));
        tool_env(&mut cmd);
        let tap = Tap::new(out);
        let exit = spawn_tool(cmd, &tap, &mut |_| true).map_err(|e| spawn_failure("esbuild", e))?;
        check_cancelled(ctx)?;
        result.exit = exit;
        result.complete = exit < 128;
        if exit != 0 {
            let msg = failure_line(&tap.take()).unwrap_or_else(|| format!("esbuild が失敗した（終了コード {exit}）"));
            push_diag(&mut diags, "", note_diag(Severity::Error, msg));
        } else {
            let packed = pack(&js, ArtifactKind::Js, &name, ctx.out_dir).map_err(|e| JobFailure { status: Status::Error, message: e.0 })?;
            result.artifact = Some(packed);
        }
    }
    result.diagnostics = finish_diags(diags);
    Ok(result)
}

fn local_bin(pdir: &Path, name: &str) -> Option<PathBuf> {
    let p = pdir.join("node_modules/.bin").join(name);
    find_in_path(&p.to_string_lossy())
}

fn is_js(f: &str) -> bool {
    f.ends_with(".js") || f.ends_with(".mjs") || f.ends_with(".cjs")
}

fn is_ts(f: &str) -> bool {
    (f.ends_with(".ts") || f.ends_with(".mts") || f.ends_with(".cts")) && !f.ends_with(".d.ts")
}

/// 木の中の .ts（無ければ .js）を名前の順に。
fn script_files(src: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            match e.file_type() {
                Ok(t) if t.is_dir() => walk(&e.path(), &rel, out),
                Ok(t) if t.is_file() => out.push(rel),
                _ => {}
            }
        }
    }
    let mut all = Vec::new();
    walk(src, "", &mut all);
    all.sort();
    let ts: Vec<String> = all.iter().filter(|f| is_ts(f)).cloned().collect();
    if ts.is_empty() {
        all.into_iter().filter(|f| is_js(f)).collect()
    } else {
        ts
    }
}

fn sha256(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p);
    }
    crate::store::hex(&h.finalize())
}

/// npm を回す（`<project_dir>` で）。終了コードが 0 でなければ、その結果で止める。
fn npm(npm: &Path, pdir: &Path, args: &[&str], out: &dyn JobOutput, what: &str) -> Result<(), Stop> {
    let mut cmd = Command::new(npm);
    cmd.current_dir(pdir).args(args).args(NPM_FLAGS);
    tool_env(&mut cmd);
    let tap = Tap::new(out);
    let exit = spawn_tool(cmd, &tap, &mut |_| true).map_err(|e| spawn_failure("npm", e))?;
    if exit != 0 {
        let msg = failure_line(&tap.take()).unwrap_or_else(|| format!("{what} が失敗した（終了コード {exit}）"));
        return Err(Stop::Result(Box::new(JobResult { exit, complete: exit < 128, diagnostics: vec![FileDiags { path: String::new(), diags: vec![note_diag(Severity::Error, msg)] }], ..Default::default() })));
    }
    Ok(())
}

/// 依存を入れる。lock を作った・作り直したら `result.write_back` に入れる。
fn install(ctx: &JobCtx<'_>, npm_bin: &Path, src: &Path, pdir: &Path, pkg_text: &str, out: &dyn JobOutput, result: &mut JobResult) -> Result<(), Stop> {
    let io = |what: &str, e: std::io::Error| Stop::Failure(io_failure(what, e));
    let lock_name = "package-lock.json";
    if std::fs::read_to_string(pdir.join("package.json")).ok().as_deref() != Some(pkg_text) {
        std::fs::write(pdir.join("package.json"), pkg_text).map_err(|e| io("package.json を写せない", e))?;
    }
    let user_lock = std::fs::read(src.join(lock_name)).ok();
    let plock = pdir.join(lock_name);
    let write_back_lock = |result: &mut JobResult| -> Result<(), Stop> {
        let text = std::fs::read_to_string(&plock).map_err(|e| io("package-lock.json を読めない", e))?;
        if user_lock.as_deref() != Some(text.as_bytes()) && text.len() <= zc_wire::limits::MAX_WRITEBACK_BYTES {
            result.write_back.retain(|(p, _)| p != lock_name);
            result.write_back.push((lock_name.to_string(), text));
        }
        Ok(())
    };
    match &user_lock {
        Some(b) => {
            if std::fs::read(&plock).ok().as_deref() != Some(&b[..]) {
                std::fs::write(&plock, b).map_err(|e| io("package-lock.json を写せない", e))?;
            }
        }
        None => {
            let _ = std::fs::remove_file(&plock);
            check_cancelled(ctx)?;
            npm(npm_bin, pdir, &["install", "--package-lock-only"], out, "npm install --package-lock-only")?;
            write_back_lock(result)?;
        }
    }

    let stamp = |lock: &[u8]| sha256(&[pkg_text.as_bytes(), lock]);
    let lock_now = std::fs::read(&plock).map_err(|e| io("package-lock.json を読めない", e))?;
    if std::fs::read_to_string(pdir.join(STAMP)).ok().as_deref() == Some(stamp(&lock_now).as_str()) && pdir.join("node_modules").is_dir() {
        return Ok(());
    }
    check_cancelled(ctx)?;
    if let Err(stop) = npm(npm_bin, pdir, &["ci"], out, "npm ci") {
        if user_lock.is_none() {
            return Err(stop);
        }
        // lock が古い（package.json と合わない）かもしれない: 作り直して 1 回だけやり直す
        check_cancelled(ctx)?;
        npm(npm_bin, pdir, &["install", "--package-lock-only"], out, "npm install --package-lock-only")?;
        if std::fs::read(&plock).ok().as_deref() == user_lock.as_deref() {
            return Err(stop);
        }
        write_back_lock(result)?;
        check_cancelled(ctx)?;
        npm(npm_bin, pdir, &["ci"], out, "npm ci")?;
    }
    let lock_now = std::fs::read(&plock).map_err(|e| io("package-lock.json を読めない", e))?;
    std::fs::write(pdir.join(STAMP), stamp(&lock_now)).map_err(|e| io("印を書けない", e))?;
    Ok(())
}

/// tsc の 1 件: 位置（パス・行・桁、1 始まり）・重さ・コード・メッセージ。
type TscItem = (Option<(String, u32, u32)>, Severity, String, String);

/// tsc の `--pretty false` の出力を読む。
pub fn parse_tsc(output: &str, src_root: &std::path::Path, into: &mut Vec<FileDiags>) {
    // 1 件ずつ（パス・行・桁・重さ・コード・メッセージ）にまとめてから足す
    let mut cur: Option<TscItem> = None;
    let flush = |cur: &mut Option<TscItem>, into: &mut Vec<FileDiags>| {
        let Some((loc, severity, code, message)) = cur.take() else { return };
        let message = truncate_utf8(&message, 400);
        let code = Some(code);
        match loc.and_then(|(p, l, c)| rel_to_root(src_root, src_root, &p).map(|r| (r, l, c))) {
            Some((path, line, col)) => {
                let line0 = line.saturating_sub(1);
                let col = match read_line(&src_root.join(&path), line0) {
                    Some(text) => utf16_col_to_byte(&text, col),
                    None => col.saturating_sub(1),
                };
                push_diag(into, &path, Diag { line: line0, col: Some(col), end: None, severity, code, message });
            }
            None => push_diag(into, "", Diag { line: 0, col: None, end: None, severity, code, message }),
        }
    };
    for line in output.lines() {
        if let Some(d) = parse_tsc_head(line) {
            flush(&mut cur, into);
            cur = Some(d);
        } else if line.starts_with(' ') && !line.trim().is_empty() {
            if let Some((_, _, _, m)) = cur.as_mut() {
                m.push(' ');
                m.push_str(line.trim());
            }
        } else {
            flush(&mut cur, into);
        }
    }
    flush(&mut cur, into);
}

/// `path(行,桁): error TS1234: …` か `error TS1234: …` を読む。
fn parse_tsc_head(line: &str) -> Option<TscItem> {
    let (loc, rest) = if line.starts_with("error ") || line.starts_with("warning ") {
        (None, line)
    } else {
        let close = line.find("): ")?;
        let open = line[..close].rfind('(')?;
        let mut nums = line[open + 1..close].split(',');
        let l: u32 = nums.next()?.trim().parse().ok()?;
        let c: u32 = nums.next()?.trim().parse().ok()?;
        (Some((line[..open].to_string(), l, c)), &line[close + 3..])
    };
    let (sev, rest) = match rest.strip_prefix("error ") {
        Some(r) => (Severity::Error, r),
        None => (Severity::Warning, rest.strip_prefix("warning ")?),
    };
    let colon = rest.find(": ")?;
    let code = &rest[..colon];
    if !code.starts_with("TS") || !code[2..].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((loc, sev, code.to_string(), rest[colon + 2..].to_string()))
}

/// 1 始まりの UTF-16 の桁を、その行の中の UTF-8 のバイト位置（0 始まり）に直す。
pub fn utf16_col_to_byte(line_text: &str, col_1based_utf16: u32) -> u32 {
    let target = col_1based_utf16.saturating_sub(1) as usize;
    let mut units = 0usize;
    for (i, ch) in line_text.char_indices() {
        if units >= target {
            return i as u32;
        }
        units += ch.len_utf16();
        if units > target {
            // サロゲートの対の途中を指していた: その字の頭
            return i as u32;
        }
    }
    line_text.len() as u32
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{skip_without, Collect, TempDir};
    use super::*;
    use std::sync::atomic::AtomicBool;
    use zc_wire::msg::JobOpts;

    #[test]
    fn utf16_columns_become_bytes() {
        assert_eq!(utf16_col_to_byte("let x = 1;", 5), 4);
        // 「日本」は 2 単位・6 バイト
        assert_eq!(utf16_col_to_byte("// 日本 x", 7), 10);
        // 😀 は UTF-16 で 2 単位、UTF-8 で 4 バイト
        assert_eq!(utf16_col_to_byte("😀x", 3), 4);
        assert_eq!(utf16_col_to_byte("😀x", 2), 0);
        assert_eq!(utf16_col_to_byte("ab", 99), 2);
        assert_eq!(utf16_col_to_byte("", 1), 0);
    }

    #[test]
    fn tsc_output_is_parsed() {
        let t = TempDir::new("tsc");
        std::fs::create_dir_all(t.path().join("src")).unwrap();
        std::fs::write(t.path().join("src/a.ts"), "const s = '😀';\nconst n: number = /* 日本 */ \"x\";\n").unwrap();
        let out = "src/a.ts(2,28): error TS2322: Type 'string' is not assignable to type 'number'.\n\
                   ../outside.ts(1,1): error TS1005: ';' expected.\n\
                   error TS5023: Unknown compiler option 'foo'.\n\
                   src/a.ts(1,7): error TS2345: Argument of type 'X' is not assignable.\n  Type 'X' is missing properties.\n";
        let mut v = Vec::new();
        parse_tsc(out, t.path(), &mut v);
        let v = finish_diags(v);
        let a = v.iter().find(|f| f.path == "src/a.ts").unwrap();
        assert_eq!(a.diags.len(), 2);
        // 2 行目の "x": UTF-16 で 28 単位目 → バイトでは 「const n: number = /* 」21 + 日本 6 + 「 */ 」4 = 31
        let d = &a.diags[1];
        assert_eq!((d.line, d.col), (1, Some(31)));
        assert_eq!(d.code.as_deref(), Some("TS2322"));
        assert!(a.diags[0].message.ends_with("Type 'X' is missing properties."));
        let none = v.iter().find(|f| f.path.is_empty()).unwrap();
        assert_eq!(none.diags.len(), 2);
    }

    // 通しの試験は道具（node・npm・tsc・esbuild）があるときだけ（ビルド係のイメージの中）。
    // 依存の無い package.json なので、ネットワークは要らない。
    fn job(files: &[(&str, &str)]) -> (TempDir, PathBuf, PathBuf, PathBuf) {
        let t = TempDir::new("node");
        let src = t.path().join("proj/src");
        std::fs::create_dir_all(&src).unwrap();
        for (p, text) in files {
            std::fs::write(src.join(p), text).unwrap();
        }
        let out = t.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let proj = t.path().join("proj");
        (t, src, proj, out)
    }

    fn run_job(src: &Path, proj: &Path, out_dir: &Path, kind: Kind, entry: Option<&str>) -> (Result<JobResult, JobFailure>, Collect) {
        let cancelled = AtomicBool::new(false);
        let opts = JobOpts { entry: entry.map(str::to_string), ..Default::default() };
        let ctx = JobCtx { job: "j", kind, lang: zc_wire::msg::Lang::Node, opts: &opts, src, project_dir: proj, out_dir, cancelled: &cancelled };
        let out = Collect::default();
        (run(&ctx, &out), out)
    }

    const PKG: &str = r#"{"name":"t","version":"1.0.0","type":"module","scripts":{"test":"node test.mjs"}}"#;

    #[test]
    fn js_is_not_type_checked() {
        // LLRT の `process` などを使う JS も通る（JS は構文の誤りだけを見る）。構文の誤りは落ちる
        if skip_without(&["node", "npm", "tsc", "esbuild", "zstd"]) {
            return;
        }
        let (_t, src, proj, out_dir) = job(&[
            ("package.json", PKG),
            ("app.js", "console.log(typeof process !== 'undefined' ? process.argv.length : 0);\n"),
        ]);
        let (r, log) = run_job(&src, &proj, &out_dir, Kind::Build, Some("app.js"));
        let r = r.unwrap();
        assert_eq!(r.exit, 0, "{}", log.text());
        assert_eq!(r.artifact.unwrap().name, "app.js");
        std::fs::write(src.join("app.js"), "console.log((;\n").unwrap();
        let (r, log) = run_job(&src, &proj, &out_dir, Kind::Check, Some("app.js"));
        assert_ne!(r.unwrap().exit, 0, "{}", log.text());
    }

    #[test]
    fn e2e_check_build_test_and_lock() {
        if skip_without(&["node", "npm", "tsc", "esbuild", "zstd"]) {
            return;
        }
        let (_t, src, proj, out_dir) = job(&[
            ("package.json", PKG),
            ("main.ts", "import { two } from './lib.js';\nconsole.log(two());\n"),
            // TS 7 は戻り値の型の誤りを `return` の位置で報告するので、誤りは日本語の後ろの引数に置く
            // （桁が UTF-8 のバイト数で数えられているかを確かめる）
            ("lib.ts", "export function two(): number {\n  return /* 日本 */ half(\"4\");\n}\nfunction half(n: number): number { return n / 2; }\n"),
            ("test.mjs", "process.exit(0);\n"),
        ]);
        let (r, log) = run_job(&src, &proj, &out_dir, Kind::Check, Some("main.ts"));
        let r = r.unwrap();
        assert_ne!(r.exit, 0, "{}", log.text());
        let d = r.diagnostics.iter().find(|f| f.path == "lib.ts").expect("lib.ts の診断");
        assert_eq!(d.diags[0].line, 1);
        assert_eq!(d.diags[0].col, Some("  return /* 日本 */ half(".len() as u32));
        // lock が無かった → 作って書き戻す
        assert!(r.write_back.iter().any(|(p, t)| p == "package-lock.json" && t.contains("\"name\": \"t\"")));
        assert!(proj.join("node_modules").is_dir() || proj.join(STAMP).is_file());

        std::fs::write(src.join("lib.ts"), "export function two(): number {\n  return 2;\n}\n").unwrap();
        let (r, log) = run_job(&src, &proj, &out_dir, Kind::Build, Some("main.ts"));
        let r = r.unwrap();
        assert_eq!(r.exit, 0, "{}", log.text());
        let a = r.artifact.unwrap();
        assert_eq!((a.kind, a.name.as_str()), (ArtifactKind::Js, "main.js"));

        let (r, _) = run_job(&src, &proj, &out_dir, Kind::Test, None);
        let t = r.unwrap().tests.unwrap();
        assert_eq!((t.passed, t.failed), (1, 0));
    }

    #[test]
    fn missing_package_json() {
        if skip_without(&["node", "npm"]) {
            return;
        }
        let (_t, src, proj, out_dir) = job(&[("main.ts", "")]);
        let r = run_job(&src, &proj, &out_dir, Kind::Check, None).0.unwrap();
        assert!(r.diagnostics[0].diags[0].message.contains("package.json が無い"));
    }

    #[test]
    fn missing_node_is_an_error() {
        if find_in_path("npm").is_some() {
            return;
        }
        let (_t, src, proj, out_dir) = job(&[("package.json", PKG)]);
        let f = run_job(&src, &proj, &out_dir, Kind::Check, None).0.unwrap_err();
        assert_eq!(f.status, Status::Error);
        assert!(f.message.contains("npm が無い"));
    }
}
