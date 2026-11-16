//! serve と CLI の受け入れ試験（課題 D5）。ビルド係は、同じプロセスの中の偽物
//! （zc-wire で書いた台本どおりに返す TCP サーバ。TLS なし = PC の経路）。本物の受付との
//! 通しは `tests/e2e.rs`。
//!
//! serve と CLI は本物の実行ファイルを、試験ごとの一時フォルダに向けて動かす（`tests/d5/`）。

mod api {
    pub use zc_remote::auth::*;
    pub use zc_remote::net::*;
}
mod d5;
mod support;

use d5::*;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use support::{serve_plain, Msg, Srv};
use zc_remote_ipc as ipc;
use zc_wire::msg::{decode, encode, ArtifactKind, BinHeader, BinKind, ClientMsg, Diag, FileDiags, Kind, ServerMsg, Severity, Status};

const WAIT: Duration = Duration::from_secs(20);

/// 偽物の受付。`hello`・目録（中身は全部持っていることにする）・`cancel` は決まった返事を
/// し、`job` が来たら `on_job(接続, job id, 種類)` を呼ぶ。つながれた回数を返す。
fn fake_builder(on_job: impl Fn(&mut Srv, &str, Kind) + Send + Sync + 'static) -> (u16, Arc<AtomicUsize>) {
    let conns = Arc::new(AtomicUsize::new(0));
    let n = conns.clone();
    let port = serve_plain(move |_, s| {
        let Some(mut srv) = Srv::accept(s, &[]) else { return };
        n.fetch_add(1, Ordering::SeqCst);
        while let Some(m) = srv.recv() {
            let Msg::Text(t) = m else { continue };
            match decode::<ClientMsg>(&t).unwrap() {
                Some(ClientMsg::Hello { v, .. }) => srv.text(&encode(&ServerMsg::Hello { v, server: "fake/0".into() })),
                Some(ClientMsg::Manifest { tree, .. }) => {
                    srv.text(&encode(&ServerMsg::Need { tree: tree.clone(), blobs: Vec::new() }));
                    srv.text(&encode(&ServerMsg::Ready { tree }));
                }
                Some(ClientMsg::Job { job, kind, .. }) => on_job(&mut srv, &job, kind),
                Some(ClientMsg::Cancel { job }) => srv.text(&encode(&ServerMsg::Done { job, status: Status::Cancelled, exit: 0, queue_ms: 0, run_ms: 0 })),
                _ => {}
            }
        }
    });
    (port, conns)
}

/// 型エラーで失敗するジョブ（ログ 1 行と診断 1 件）。
fn failing_job(srv: &mut Srv, job: &str, _: Kind) {
    let job = job.to_owned();
    srv.text(&encode(&ServerMsg::Accepted { job: job.clone(), cached: false, queued: 0 }));
    srv.text(&encode(&ServerMsg::Started { job: job.clone() }));
    srv.text(&encode(&ServerMsg::Log { job: job.clone(), offset: 0, text: "error[E0308]: mismatched types\n".into() }));
    let diags = vec![Diag { line: 2, col: Some(17), end: Some((2, 22)), severity: Severity::Error, code: Some("E0308".into()), message: "mismatched types".into() }];
    srv.text(&encode(&ServerMsg::Diagnostics { job: job.clone(), complete: true, truncated: false, files: vec![FileDiags { path: "main.rs".into(), diags }] }));
    srv.text(&encode(&ServerMsg::Done { job, status: Status::Failed, exit: 101, queue_ms: 1, run_ms: 2 }));
}

/// `tests/data/lines.zst` の中身（tests/artifact.rs と同じ式）。
fn lines() -> Vec<u8> {
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let mut s = String::new();
    for i in 0..16000 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        s.push_str(&format!("line {i} value {}\n", x >> 40));
    }
    s.into_bytes()
}

/// 成果物（`lines.zst`）を返して成功するジョブ。
fn artifact_job(srv: &mut Srv, job: &str, _: Kind) {
    let job = job.to_owned();
    let z = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/lines.zst")).unwrap();
    let content = lines();
    let sha256: String = Sha256::digest(&content).iter().map(|b| format!("{b:02x}")).collect();
    srv.text(&encode(&ServerMsg::Accepted { job: job.clone(), cached: false, queued: 0 }));
    srv.text(&encode(&ServerMsg::Started { job: job.clone() }));
    srv.text(&encode(&ServerMsg::Diagnostics { job: job.clone(), complete: true, truncated: false, files: Vec::new() }));
    srv.text(&encode(&ServerMsg::Artifact { job: job.clone(), chan: 7, kind: ArtifactKind::Exe, name: "one".into(), sha256, bytes: content.len() as u64, zbytes: z.len() as u64, offset: 0 }));
    let mut off = 0;
    for part in z.chunks(30_000) {
        let mut frame = BinHeader { kind: BinKind::Artifact, last: off + part.len() == z.len(), id: 7, offset: off as u64 }.encode().to_vec();
        frame.extend_from_slice(part);
        srv.binary(&frame);
        off += part.len();
    }
    srv.text(&encode(&ServerMsg::Done { job, status: Status::Ok, exit: 0, queue_ms: 1, run_ms: 2 }));
}

fn pc(port: u16) -> Vec<(&'static str, String)> {
    vec![("ZC_REMOTE_PC", format!("127.0.0.1:{port}")), ("ZC_REMOTE_BACKEND", "pc".into())]
}

const RUST: ipc::RemoteLang = ipc::RemoteLang::Rust;

#[test]
fn version_help_and_usage() {
    let sb = Sandbox::new("usage");
    let out = run({ let mut c = sb.cmd(&[]); c.arg("--version"); c }, WAIT);
    assert_eq!((out.status.code(), text(&out.stdout).trim()), (Some(0), zc_remote::CLIENT_NAME));
    let out = run({ let mut c = sb.cmd(&[]); c.arg("--help"); c }, WAIT);
    assert_eq!(out.status.code(), Some(0));
    for word in ["zc-remote run", "zc-remote applies", "zc-remote check", "zc-remote status", "remote.json"] {
        assert!(text(&out.stdout).contains(word), "{word}");
    }
    for bad in [&["frobnicate"][..], &["run"], &["run", "--frob", "a.rs"], &["build", "/nonexistent/a.rs"], &["test", "a.rs", "filter"]] {
        let out = run({ let mut c = sb.cmd(&[]); c.args(bad); c }, WAIT);
        assert_eq!(out.status.code(), Some(2), "{bad:?}: {}", text(&out.stderr));
    }
    // 使い方の間違いでは serve を起動しない
    assert!(!sb.socket().exists());
}

/// `applies` は通信せずに根だけを見る（language.toml の JS / TS の `run` が使う）。
#[test]
fn applies_decides_without_talking() {
    let sb = Sandbox::new("applies");
    let rs = sb.write("code/a/main.rs", "fn main() {}\n");
    let c = sb.write("code/a/x.c", "int main(void) { return 0; }\n");
    let ts_alone = sb.write("code/b/x.ts", "export {};\n");
    sb.write("code/n/package.json", r#"{ "name": "n", "dependencies": { "left-pad": "1.3.0" } }"#);
    let ts_deps = sb.write("code/n/src/x.ts", "export {};\n");
    let js_deps = sb.write("code/n/x.mjs", "export {};\n");
    sb.write("code/e/package.json", r#"{ "name": "e" }"#);
    let ts_nodeps = sb.write("code/e/x.ts", "export {};\n");
    let py = sb.write("code/a/x.py", "pass\n");
    for (file, want) in [(&rs, 0), (&c, 0), (&ts_deps, 0), (&js_deps, 0), (&ts_alone, 1), (&ts_nodeps, 1), (&py, 1), (&sb.home().join("code/none.ts"), 1)] {
        // language.toml と同じ呼び方（作業フォルダはファイルのフォルダ、引数はファイル名）
        let mut cmd = sb.cmd(&[]);
        cmd.current_dir(file.parent().unwrap()).arg("applies").arg(file.file_name().unwrap());
        let out = run(cmd, WAIT);
        assert_eq!(out.status.code(), Some(want), "{}: {}", file.display(), text(&out.stderr));
        assert!(out.stdout.is_empty());
    }
    assert!(!sb.socket().exists(), "applies は serve を起動しない");
}

/// 対象でないファイルは「対象ではない」、設定が無ければ「認証・設定」。どちらも通信しない。
#[test]
fn not_applicable_and_missing_config() {
    let sb = Sandbox::new("nocfg");
    let _serve = sb.serve(&[], &[]);
    let ts = sb.write("code/b/x.ts", "export {};\n");
    let rs = sb.write("code/a/main.rs", "fn main() {}\n");
    let mut c = sb.client(ipc::ClientKind::Editor);
    let tag_of = |got: &[ipc::Response]| match got.last() {
        Some(ipc::Response::Done { tag, .. }) => *tag,
        other => panic!("{other:?}"),
    };
    c.submit(1, ipc::JobKind::Check, ipc::RemoteLang::Node, ipc::SubmitFlags::NONE, &ts);
    let got = c.until_done(WAIT);
    assert_eq!((got.len(), done_of(&got).0), (1, ipc::DoneStatus::NotApplicable));
    let first = tag_of(&got);
    c.submit(2, ipc::JobKind::Check, RUST, ipc::SubmitFlags::NONE, &rs);
    let got = c.until_done(WAIT);
    let (status, _, _, message) = done_of(&got);
    assert_eq!(status, ipc::DoneStatus::Auth);
    assert!(message.contains("remote.json"), "{message}");
    // Done の tag は 0 でない・依頼ごとに別
    assert!(first != ipc::JobTag(0) && tag_of(&got) != ipc::JobTag(0) && tag_of(&got) != first);
    // 状態を聞ける
    c.send(&ipc::Request::Status);
    assert_eq!(c.recv(WAIT), Some(ipc::Response::Status { backend: ipc::Backend::Unknown, connected: false, running_jobs: 0 }));

    // CLI: 設定が無ければ終了コード 4。壊れた設定は、理由つきで 4
    let out = run({ let mut c = sb.cmd(&[]); c.arg("check").arg(&rs); c }, WAIT);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    std::fs::write(sb.root.join("config/zcode/remote.json"), "{ \"backend\": \"aws\" }").unwrap();
    let out = run({ let mut c = sb.cmd(&[]); c.arg("check").arg(&rs); c }, WAIT);
    assert_eq!(out.status.code(), Some(4));
    assert!(text(&out.stderr).contains("backend"), "{}", text(&out.stderr));
    // 動いている serve も、書き換わった設定を読み直して理由を返す
    c.submit(3, ipc::JobKind::Check, RUST, ipc::SubmitFlags::NONE, &rs);
    let (status, _, _, message) = done_of(&c.until_done(WAIT));
    assert_eq!(status, ipc::DoneStatus::Auth);
    assert!(message.contains("backend"), "{message}");
}

/// 依頼 → 知らせの列 → Done。zcode の相手には、ほかの相手（F5 の CLI）が頼んだジョブの
/// 進み具合・診断・終了が `req` = 0 で届く。ログは届かない。
#[test]
fn editor_gets_other_clients_diagnostics_but_not_their_logs() {
    let sb = Sandbox::new("editor");
    let (port, _) = fake_builder(failing_job);
    let _serve = sb.serve(&pc(port), &[]);
    let file = sb.write("code/one/main.rs", "fn main() {\n    let name = 1;\n    let x: i32 = \"str\";\n}\n");
    let root = file.parent().unwrap().to_owned();

    let mut editor = sb.client(ipc::ClientKind::Editor);
    let mut cli = sb.client(ipc::ClientKind::Cli);
    // 編集の相手が先につながっていることを確かめてから頼む
    editor.send(&ipc::Request::Status);
    assert!(matches!(editor.recv(WAIT), Some(ipc::Response::Status { .. })));
    cli.submit(9, ipc::JobKind::Build, RUST, ipc::SubmitFlags::WANT_LOG, &file);

    let got = cli.until_done(WAIT);
    let (status, exit, backend, _) = done_of(&got);
    assert_eq!((status, exit, backend), (ipc::DoneStatus::Failed, 101, ipc::Backend::Pc));
    // 頼んだ相手: 全部 req = 9・同じ tag。走査 → … → ログ → 診断 → Done
    let tag = match &got[0] {
        ipc::Response::Phase { req, tag, root: r, phase: ipc::Phase::Scanned, .. } => {
            assert_eq!((*req, r), (ipc::ReqId(9), &root));
            *tag
        }
        other => panic!("最初は Scanned: {other:?}"),
    };
    assert_ne!(tag, ipc::JobTag(0));
    let log: Vec<u8> = got.iter().filter_map(|r| if let ipc::Response::Log { req, tag: t, bytes } = r { assert_eq!((*req, *t), (ipc::ReqId(9), tag)); Some(bytes.clone()) } else { None }).flatten().collect();
    assert_eq!(text(&log), "error[E0308]: mismatched types\n");
    let diag = zc_core::Diagnostic { line: 2, col: Some(17), end: Some((2, 22)), severity: zc_core::Severity::Error, code: Some("E0308".into()), message: "mismatched types".into() };
    let files = vec![ipc::FileDiags { path: "main.rs".into(), diags: vec![diag] }];
    let pos = |got: &[ipc::Response], want: &ipc::Response| got.iter().position(|r| r == want).unwrap_or_else(|| panic!("{want:?} が無い: {got:#?}"));
    let d = pos(&got, &ipc::Response::Diagnostics { req: ipc::ReqId(9), tag, root: root.clone(), complete: true, last: true, files: files.clone() });
    let l = got.iter().position(|r| matches!(r, ipc::Response::Log { .. })).unwrap();
    assert!(l < d && d + 1 == got.len() - 1, "ログ → 診断 → Done の順: {got:#?}");
    assert!(got.iter().any(|r| matches!(r, ipc::Response::Phase { phase: ipc::Phase::Running, .. })));

    // 編集の相手: 同じ tag・req = 0。ログは無い
    let seen = editor.until_done(WAIT);
    assert!(seen.iter().all(|r| !matches!(r, ipc::Response::Log { .. } | ipc::Response::Artifact { .. })), "{seen:#?}");
    assert!(matches!(&seen[0], ipc::Response::Phase { req: ipc::ReqId::NONE, tag: t, phase: ipc::Phase::Scanned, root: r, .. } if *t == tag && *r == root), "{seen:#?}");
    pos(&seen, &ipc::Response::Diagnostics { req: ipc::ReqId::NONE, tag, root: root.clone(), complete: true, last: true, files });
    assert!(matches!(seen.last(), Some(ipc::Response::Done { req: ipc::ReqId::NONE, tag: t, status: ipc::DoneStatus::Failed, exit: 101, .. }) if *t == tag));

    // CLI の `run`: ログは標準エラーにそのまま、終了コード 1。編集の相手に診断が届く
    let out = run({ let mut c = sb.cmd(&pc(port)); c.current_dir(&root).args(["run", "main.rs"]); c }, WAIT);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("error[E0308]: mismatched types"), "{}", text(&out.stderr));
    let seen = editor.until_done(WAIT);
    assert!(seen.iter().any(|r| matches!(r, ipc::Response::Diagnostics { req: ipc::ReqId::NONE, complete: true, .. })), "{seen:#?}");
    // CLI の `check`: 診断を「パス:行:桁: error[番号]: …」で標準出力に（1 始まり）
    let out = run({ let mut c = sb.cmd(&pc(port)); c.current_dir(&root).args(["check", "main.rs"]); c }, WAIT);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "main.rs:3:18: error[E0308]: mismatched types\n");

    // 状態: つながっている（温めている間）
    editor.send(&ipc::Request::Status);
    loop {
        match editor.recv(WAIT) {
            Some(ipc::Response::Status { backend, connected, running_jobs }) => {
                assert_eq!((backend, connected, running_jobs), (ipc::Backend::Pc, true, 0));
                break;
            }
            Some(_) => {}
            None => panic!("Status が届かない"),
        }
    }
    let out = run({ let mut c = sb.cmd(&pc(port)); c.arg("status"); c }, WAIT);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("ビルド係: PC"), "{}", text(&out.stdout));
}

/// 同じプロジェクトの 2 回目の check は、前の check を取り消す。相手が去ったら、その依頼も取り消す。
#[test]
fn a_newer_check_cancels_the_older_one() {
    let sb = Sandbox::new("cancel");
    // 1 つ目のジョブには返事をしない（走りっぱなし）。2 つ目からは失敗で返す
    let jobs = Arc::new(AtomicUsize::new(0));
    let n = jobs.clone();
    let (port, _) = fake_builder(move |srv, job, kind| {
        if n.fetch_add(1, Ordering::SeqCst) == 0 {
            srv.text(&encode(&ServerMsg::Accepted { job: job.to_owned(), cached: false, queued: 0 }));
            srv.text(&encode(&ServerMsg::Started { job: job.to_owned() }));
        } else {
            failing_job(srv, job, kind);
        }
    });
    let _serve = sb.serve(&pc(port), &[]);
    let file = sb.write("code/one/main.rs", "fn main() {}\n");
    let mut c = sb.client(ipc::ClientKind::Editor);
    c.submit(1, ipc::JobKind::Check, RUST, ipc::SubmitFlags::NONE, &file);
    // 走り始めるまで待つ
    loop {
        if let Some(ipc::Response::Phase { phase: ipc::Phase::Running, .. }) = c.recv(WAIT) {
            break;
        }
    }
    // 保存し直した（中身が変わった）
    std::fs::write(&file, "fn main() { }\n").unwrap();
    c.submit(2, ipc::JobKind::Check, RUST, ipc::SubmitFlags::NONE, &file);
    let mut done = Vec::new();
    while done.len() < 2 {
        match c.recv(WAIT) {
            Some(ipc::Response::Done { req, status, .. }) => done.push((req.0, status)),
            Some(_) => {}
            None => panic!("Done が 2 つ届かない: {done:?}"),
        }
    }
    done.sort_by_key(|d| d.0);
    assert_eq!(done, [(1, ipc::DoneStatus::Cancelled), (2, ipc::DoneStatus::Failed)]);
}

/// `auto`: PC に届かなければ Cloudflare 側（ここでは `ws://` の偽物）へ。
#[test]
fn auto_falls_back_to_cloudflare_when_the_pc_is_unreachable() {
    let sb = Sandbox::new("auto");
    let (port, conns) = fake_builder(failing_job);
    let env = vec![("ZC_REMOTE_PC", format!("127.0.0.1:{}", closed_port())), ("ZC_REMOTE_URL", format!("ws://127.0.0.1:{port}/v1/ws")), ("ZC_REMOTE_BACKEND", "auto".into())];
    let _serve = sb.serve(&env, &[]);
    let file = sb.write("code/one/main.rs", "fn main() {}\n");
    let mut c = sb.client(ipc::ClientKind::Cli);
    c.submit(1, ipc::JobKind::Check, RUST, ipc::SubmitFlags::NONE, &file);
    let (status, _, backend, _) = done_of(&c.until_done(WAIT));
    assert_eq!((status, backend, conns.load(Ordering::SeqCst)), (ipc::DoneStatus::Failed, ipc::Backend::Cloudflare, 1));

    // PC に届けば PC（別の serve で）
    let sb2 = Sandbox::new("auto2");
    let (pc_port, pc_conns) = fake_builder(failing_job);
    let env = vec![("ZC_REMOTE_PC", format!("127.0.0.1:{pc_port}")), ("ZC_REMOTE_URL", format!("ws://127.0.0.1:{port}/v1/ws"))];
    let _serve2 = sb2.serve(&env, &[]);
    let file = sb2.write("code/one/main.rs", "fn main() {}\n");
    let mut c = sb2.client(ipc::ClientKind::Cli);
    c.submit(1, ipc::JobKind::Check, RUST, ipc::SubmitFlags::NONE, &file);
    let (status, _, backend, _) = done_of(&c.until_done(WAIT));
    assert_eq!((status, backend, pc_conns.load(Ordering::SeqCst), conns.load(Ordering::SeqCst)), (ipc::DoneStatus::Failed, ipc::Backend::Pc, 1, 1));
}

/// 成功した build は tmpfs に置かれ、serve が終わるときに SD の控えになる。再起動のあと
/// （tmpfs が空）ネットが無ければ、控えを戻して「前の版」として知らせる。
#[test]
fn offline_build_offers_the_last_good_copy() {
    let sb = Sandbox::new("stale");
    let (port, _) = fake_builder(artifact_job);
    let file = sb.write("code/one/main.rs", "fn main() {}\n");
    let build = ipc::SubmitFlags(ipc::SubmitFlags::WANT_LOG.0 | ipc::SubmitFlags::ALLOW_STALE.0);

    let serve = sb.serve(&pc(port), &[]);
    let mut c = sb.client(ipc::ClientKind::Cli);
    c.submit(1, ipc::JobKind::Build, RUST, build, &file);
    let got = c.until_done(WAIT);
    assert_eq!(done_of(&got).0, ipc::DoneStatus::Ok, "{got:#?}");
    let placed = got.iter().find_map(|r| if let ipc::Response::Artifact { kind: ipc::ArtifactKind::Exe, stale: false, path, .. } = r { Some(path.clone()) } else { None }).expect("Artifact が届く");
    assert!(placed.starts_with(sb.run_bin()) && placed.file_name().unwrap().to_string_lossy().starts_with("one-"), "{}", placed.display());
    assert_eq!(std::fs::read(&placed).unwrap(), lines());
    assert!(got.iter().any(|r| matches!(r, ipc::Response::Phase { phase: ipc::Phase::Downloading, .. })));
    // まだ SD には書いていない（成功の 60 秒後か、終わるとき）
    assert_eq!(std::fs::read_dir(sb.cache_bin()).map(|d| d.count()).unwrap_or(0), 0);

    // 止める（Shutdown）→ 終わる前に控えを書く
    drop(c);
    let out = run({ let mut c = sb.cmd(&[]); c.arg("stop"); c }, WAIT);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let (code, err) = serve.wait(WAIT);
    assert_eq!(code, Some(0), "{err}");
    assert!(!sb.socket().exists());
    let saved: Vec<_> = std::fs::read_dir(sb.cache_bin()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(saved.len() == 1 && saved[0].ends_with(".zcb"), "{saved:?}");

    // 再起動: tmpfs が空になる。受付には届かない
    std::fs::remove_dir_all(sb.run_bin()).unwrap();
    let offline = pc(closed_port());
    let _serve = sb.serve(&offline, &[]);
    let mut c = sb.client(ipc::ClientKind::Cli);
    let t0 = std::time::Instant::now();
    c.submit(1, ipc::JobKind::Build, RUST, build, &file);
    let got = c.until_done(WAIT);
    assert_eq!(done_of(&got).0, ipc::DoneStatus::Offline, "{got:#?}");
    assert!(t0.elapsed() < Duration::from_secs(3), "オフラインはすぐ分かる: {:?}", t0.elapsed());
    let stale = got.iter().find_map(|r| if let ipc::Response::Artifact { stale: true, path, .. } = r { Some(path.clone()) } else { None }).expect("前の版が届く");
    assert_eq!(std::fs::read(&stale).unwrap(), lines());
    // 代用を頼んでいなければ、届かない
    c.submit(2, ipc::JobKind::Build, RUST, ipc::SubmitFlags::WANT_LOG, &file);
    let got = c.until_done(WAIT);
    assert!(done_of(&got).0 == ipc::DoneStatus::Offline && !got.iter().any(|r| matches!(r, ipc::Response::Artifact { .. })), "{got:#?}");
    // check は代用しない
    c.submit(3, ipc::JobKind::Check, RUST, ipc::SubmitFlags::NONE, &file);
    assert_eq!(done_of(&c.until_done(WAIT)).0, ipc::DoneStatus::Offline);

    // CLI の run: 端末でなければ実行せず、終了コード 3 で「前の版がある」と言う
    let root = file.parent().unwrap();
    let out = run({ let mut c = sb.cmd(&offline); c.current_dir(root).args(["run", "main.rs"]); c }, WAIT);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("前の版") && text(&out.stderr).contains("--offline-ok"), "{}", text(&out.stderr));
    // --offline-ok なら前の版に入れ替わる（ここの「実行ファイル」はただの文字なので、起動は失敗 = 126）
    let out = run({ let mut c = sb.cmd(&offline); c.current_dir(root).args(["run", "main.rs", "--offline-ok"]); c }, WAIT);
    assert_eq!(out.status.code(), Some(126), "{}", text(&out.stderr));
    // build は前の版のパスを出さない（終了コード 3）
    let out = run({ let mut c = sb.cmd(&offline); c.current_dir(root).args(["build", "main.rs"]); c }, WAIT);
    assert_eq!((out.status.code(), out.stdout.len()), (Some(3), 0), "{}", text(&out.stderr));
}

/// 相手もジョブも無いまま時間が経てば終わる。相手がいる間は終わらない。
#[test]
fn exits_when_idle() {
    let sb = Sandbox::new("idle");
    let serve = sb.serve(&[], &["--idle-exit", "0.4"]);
    let mut c = sb.client(ipc::ClientKind::Editor);
    std::thread::sleep(Duration::from_millis(900));
    c.send(&ipc::Request::Status);
    assert!(matches!(c.recv(WAIT), Some(ipc::Response::Status { .. })), "相手がいる間は動いている");
    drop(c);
    let (code, err) = serve.wait(Duration::from_secs(5));
    assert_eq!(code, Some(0), "{err}");
    assert!(err.contains("Idle"), "{err}");
    assert!(!sb.socket().exists(), "ソケットを消して終わる");
}

/// 二重起動: もう動いていれば、何もせずに終わる。動いている方はそのまま。
#[test]
fn a_second_serve_exits_at_once() {
    let sb = Sandbox::new("twice");
    let _first = sb.serve(&[], &[]);
    let out = run({ let mut c = sb.cmd(&[]); c.arg("serve"); c }, Duration::from_secs(5));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let mut c = sb.client(ipc::ClientKind::Cli);
    c.send(&ipc::Request::Status);
    assert!(matches!(c.recv(WAIT), Some(ipc::Response::Status { .. })));
    // 版の違う相手には、版を返して切る
    let mut raw = std::os::unix::net::UnixStream::connect(sb.socket()).unwrap();
    let mut hello = Vec::new();
    ipc::encode_request(&ipc::Request::Hello { version: ipc::IPC_VERSION + 1, client: ipc::ClientKind::Cli }, &mut hello);
    std::io::Write::write_all(&mut raw, &hello).unwrap();
    let mut back = Vec::new();
    std::io::Read::read_to_end(&mut raw, &mut back).unwrap();
    let mut want = Vec::new();
    ipc::encode_response(&ipc::Response::Hello { version: ipc::IPC_VERSION }, &mut want);
    assert_eq!(back, want);
}

/// SIGTERM（APPLaunch の Esc 長押し）: 手早く片づけて終わる。古いソケットが残っていても、
/// 次の serve は作り直す。
#[test]
fn sigterm_stops_and_a_stale_socket_is_replaced() {
    let sb = Sandbox::new("term");
    let serve = sb.serve(&[], &[]);
    let _c = sb.client(ipc::ClientKind::Editor);
    // SAFETY: 自分が起動した子に送るだけ。
    unsafe { libc::kill(serve.pid(), libc::SIGTERM) };
    let (code, err) = serve.wait(Duration::from_secs(5));
    assert_eq!(code, Some(0), "{err}");
    assert!(err.contains("Stopped"), "{err}");
    assert!(!sb.socket().exists());

    // 殺された serve のソケットが残っている
    let serve = sb.serve(&[], &[]);
    unsafe { libc::kill(serve.pid(), libc::SIGKILL) };
    drop(serve);
    assert!(sb.socket().exists());
    let _again = sb.serve(&[], &[]);
    let mut c = sb.client(ipc::ClientKind::Cli);
    c.send(&ipc::Request::Status);
    assert!(matches!(c.recv(WAIT), Some(ipc::Response::Status { .. })));
}

/// ビルド中の Ctrl+C（SIGINT）: 取り消しを頼み、終わりの知らせを受けて 130 で終わる。
#[test]
fn ctrl_c_cancels_the_build_and_exits_130() {
    let sb = Sandbox::new("int");
    // 走りっぱなしのジョブ（cancel には、偽物の受付が done {cancelled} を返す）
    let jobs = Arc::new(AtomicUsize::new(0));
    let n = jobs.clone();
    let (port, _) = fake_builder(move |srv, job, _| {
        srv.text(&encode(&ServerMsg::Accepted { job: job.to_owned(), cached: false, queued: 0 }));
        srv.text(&encode(&ServerMsg::Started { job: job.to_owned() }));
        n.fetch_add(1, Ordering::SeqCst);
    });
    let file = sb.write("code/one/main.rs", "fn main() {}\n");
    let mut cmd = sb.cmd(&pc(port));
    cmd.current_dir(file.parent().unwrap()).args(["run", "main.rs"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    let child = cmd.spawn().unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    while jobs.load(Ordering::SeqCst) == 0 {
        assert!(std::time::Instant::now() < deadline, "ジョブが受付に届かない");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(100));
    let t0 = std::time::Instant::now();
    // SAFETY: 自分が起動した子に送るだけ。
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(130), "{}", text(&out.stderr));
    assert!(t0.elapsed() < Duration::from_secs(2), "終わりの知らせを受けて、すぐ終わる: {:?}", t0.elapsed());
    assert!(text(&out.stderr).contains("取り消した"), "{}", text(&out.stderr));
    // serve は残っていて、ジョブは無くなっている
    let mut c = sb.client(ipc::ClientKind::Cli);
    c.send(&ipc::Request::Status);
    assert!(matches!(c.recv(WAIT), Some(ipc::Response::Status { running_jobs: 0, .. })));
}
