//! 会話の状態機械の受け入れ試験（課題 D2）。台本の形: 入力を 1 つ与えるたびに、出てきた
//! 「やってほしいこと」の列を確かめる。ソケットも時計も使わない。
//!
//! `zc-wire` の中身が入るまでは `src/session.rs` の中の試験として動かしている
//! （`tests/inproc/fake_wire.rs` を参照）。公開の形しか使っていない。

use super::wire::msg::{encode, ArtifactKind, BinHeader, BinKind, ClientMsg, Diag, ErrorCode, FileDiags, FileEntry, JobOpts, Kind, Lang, ServerMsg, Severity, Status};
use super::wire::tree::{job_id, Manifest};
use super::{Action, Event, JobRequest, Outcome, Session, Ticket};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const P: &str = "0123456789abcdef";

fn sha(data: &str) -> String {
    Sha256::digest(data.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// (パス, 中身) から目録を作る。
fn mf(files: &[(&str, &str)]) -> Arc<Manifest> {
    Arc::new(Manifest::new(files.iter().map(|(p, c)| FileEntry { path: (*p).into(), size: c.len() as u64, sha256: sha(c), exec: false }).collect()).unwrap())
}

fn req(m: &Arc<Manifest>, kind: Kind) -> JobRequest {
    JobRequest { project: P.into(), manifest: m.clone(), kind, lang: Lang::Rust, opts: JobOpts::default(), want_log: kind != Kind::Check }
}

fn jid(m: &Manifest, kind: Kind) -> String {
    job_id(&m.tree_id(), kind, Lang::Rust, &JobOpts::default())
}

fn drain(s: &mut Session) -> Vec<Action> {
    std::iter::from_fn(|| s.poll_action()).collect()
}

fn text(m: &ClientMsg) -> Action {
    Action::SendText(encode(m))
}

fn hello() -> Action {
    text(&ClientMsg::Hello { v: 1, client: crate::CLIENT_NAME.into() })
}

fn manifest(m: &Manifest) -> Action {
    text(&ClientMsg::Manifest { project: P.into(), tree: m.tree_id(), files: m.files().to_vec() })
}

fn job(m: &Manifest, kind: Kind) -> Action {
    text(&ClientMsg::Job { job: jid(m, kind), project: P.into(), tree: m.tree_id(), kind, lang: Lang::Rust, opts: JobOpts::default(), log: kind != Kind::Check })
}

fn done(ticket: Ticket, status: Status) -> Action {
    Action::Emit(Event::Done { ticket, outcome: Outcome::Finished { status, exit: 0, queue_ms: 0, run_ms: 0 } })
}

/// 受付からのメッセージを 1 つ与えて、出てきたものを返す。
fn rx(s: &mut Session, m: ServerMsg) -> Vec<Action> {
    s.on_text(0, &encode(&m));
    drain(s)
}

fn frame(chan: u32, offset: u64, last: bool, body: &[u8]) -> Vec<u8> {
    let mut f = BinHeader { kind: BinKind::Artifact, last, id: chan, offset }.encode().to_vec();
    f.extend_from_slice(body);
    f
}

/// 依頼して、つないで、木をそろえて、`job` を送り、`accepted` を受けたところまで進める。
fn up_to_accepted(s: &mut Session, m: &Arc<Manifest>, kind: Kind) -> Ticket {
    let t = s.submit(0, req(m, kind));
    assert_eq!(drain(s), [Action::Connect]);
    s.on_open(0);
    assert_eq!(drain(s), [hello(), manifest(m)]);
    assert_eq!(rx(s, ServerMsg::Hello { v: 1, server: "zc-buildd/0".into() }), []);
    assert_eq!(rx(s, ServerMsg::Need { tree: m.tree_id(), blobs: vec![] }), []);
    assert_eq!(rx(s, ServerMsg::Ready { tree: m.tree_id() }), [job(m, kind)]);
    assert_eq!(rx(s, ServerMsg::Accepted { job: jid(m, kind), cached: false, queued: 0 }), [Action::Emit(Event::Queued { ticket: t, ahead: 0 })]);
    t
}

#[test]
fn build_from_manifest_to_done() {
    let m = mf(&[("Cargo.toml", "[package]"), ("src/main.rs", "fn main() {}")]);
    let mut s = Session::new();
    let t = s.submit(0, req(&m, Kind::Build));
    assert_eq!(drain(&mut s), [Action::Connect]);
    assert_eq!(s.active(), 1);
    assert_eq!(s.job_of(t), Some(jid(&m, Kind::Build).as_str()));

    // hello の返事を待たずに目録を送る
    s.on_open(0);
    assert_eq!(drain(&mut s), [hello(), manifest(&m)]);
    assert_eq!(rx(&mut s, ServerMsg::Hello { v: 1, server: "x".into() }), []);

    // 足りない中身を 1 つずつ
    let (h_main, h_toml) = (sha("fn main() {}"), sha("[package]"));
    let acts = rx(&mut s, ServerMsg::Need { tree: m.tree_id(), blobs: vec![h_main.clone(), h_toml.clone()] });
    assert_eq!(acts, [Action::Emit(Event::Uploading { ticket: t, done: 0, total: 21 }), Action::SendBlob { id: 0, manifest: m.clone(), path: "src/main.rs".into(), sha256: h_main, size: 12 }]);
    s.blob_sent(0, 0);
    assert_eq!(drain(&mut s), [Action::Emit(Event::Uploading { ticket: t, done: 12, total: 21 }), Action::SendBlob { id: 1, manifest: m.clone(), path: "Cargo.toml".into(), sha256: h_toml, size: 9 }]);
    s.blob_sent(0, 1);
    assert_eq!(drain(&mut s), [Action::Emit(Event::Uploading { ticket: t, done: 21, total: 21 })]);

    assert_eq!(rx(&mut s, ServerMsg::Ready { tree: m.tree_id() }), [job(&m, Kind::Build)]);
    let j = jid(&m, Kind::Build);
    assert_eq!(rx(&mut s, ServerMsg::Accepted { job: j.clone(), cached: false, queued: 2 }), [Action::Emit(Event::Queued { ticket: t, ahead: 2 })]);
    assert_eq!(rx(&mut s, ServerMsg::Started { job: j.clone() }), [Action::Emit(Event::Running { ticket: t })]);
    assert_eq!(rx(&mut s, ServerMsg::Log { job: j.clone(), offset: 0, text: "Compiling\n".into() }), [Action::Emit(Event::Log { ticket: t, text: "Compiling\n".into() })]);
    let files = vec![FileDiags { path: "src/main.rs".into(), diags: vec![Diag { line: 1, col: Some(2), end: None, severity: Severity::Warning, code: None, message: "w".into() }] }];
    assert_eq!(rx(&mut s, ServerMsg::Diagnostics { job: j.clone(), complete: true, truncated: false, files: files.clone() }), [Action::Emit(Event::Diagnostics { ticket: t, complete: true, files })]);
    assert_eq!(
        rx(&mut s, ServerMsg::File { job: j.clone(), path: "Cargo.lock".into(), sha256: "ab".into(), text: "lock".into() }),
        [Action::Emit(Event::WriteBack { ticket: t, path: "Cargo.lock".into(), sha256: "ab".into(), text: "lock".into() })]
    );
    assert_eq!(
        rx(&mut s, ServerMsg::Artifact { job: j.clone(), chan: 7, kind: ArtifactKind::Exe, name: "foo".into(), sha256: "cd".into(), bytes: 100, zbytes: 5, offset: 0 }),
        [Action::Emit(Event::ArtifactBegin { ticket: t, kind: ArtifactKind::Exe, name: "foo".into(), sha256: "cd".into(), bytes: 100, zbytes: 5, offset: 0 })]
    );
    s.on_binary(0, &frame(7, 0, false, b"abc"));
    assert_eq!(drain(&mut s), [Action::Emit(Event::ArtifactData { ticket: t, offset: 0, data: 16..19, last: false })]);
    s.on_binary(0, &frame(7, 3, true, b"de"));
    assert_eq!(drain(&mut s), [Action::Emit(Event::ArtifactData { ticket: t, offset: 3, data: 16..18, last: true })]);
    // 知らない番号の成果物・中身の種類違いは捨てる
    s.on_binary(0, &frame(9, 0, true, b"zz"));
    s.on_binary(0, b"short");
    assert_eq!(drain(&mut s), []);

    s.on_text(0, &encode(&ServerMsg::Done { job: j, status: Status::Ok, exit: 0, queue_ms: 3, run_ms: 40 }));
    assert_eq!(drain(&mut s), [Action::Emit(Event::Done { ticket: t, outcome: Outcome::Finished { status: Status::Ok, exit: 0, queue_ms: 3, run_ms: 40 } }), Action::Idle]);
    assert_eq!(s.active(), 0);
    assert_eq!(s.job_of(t), None);
    assert_eq!(s.deadline_us(), None);
}

#[test]
fn ready_tree_is_remembered_per_connection() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t1 = up_to_accepted(&mut s, &m, Kind::Check);
    assert_eq!(rx(&mut s, ServerMsg::Done { job: jid(&m, Kind::Check), status: Status::Ok, exit: 0, queue_ms: 0, run_ms: 0 }), [done(t1, Status::Ok), Action::Idle]);

    // 同じ接続・同じ木: 目録を送らずに job
    let t2 = s.submit(0, req(&m, Kind::Build));
    assert_eq!(drain(&mut s), [job(&m, Kind::Build)]);
    assert_eq!(rx(&mut s, ServerMsg::Done { job: jid(&m, Kind::Build), status: Status::Failed, exit: 101, queue_ms: 0, run_ms: 0 }), [Action::Emit(Event::Done { ticket: t2, outcome: Outcome::Finished { status: Status::Failed, exit: 101, queue_ms: 0, run_ms: 0 } }), Action::Idle]);

    // serve が温める時間のあと閉じた。次の接続では目録から（need は空）
    s.on_closed(0, false);
    assert_eq!(drain(&mut s), []);
    s.submit(0, req(&m, Kind::Build));
    assert_eq!(drain(&mut s), [Action::Connect]);
    s.on_open(0);
    assert_eq!(drain(&mut s), [hello(), manifest(&m)]);
    assert_eq!(rx(&mut s, ServerMsg::Need { tree: m.tree_id(), blobs: vec![] }), []);
    assert_eq!(rx(&mut s, ServerMsg::Ready { tree: m.tree_id() }), [job(&m, Kind::Build)]);
}

#[test]
fn cut_while_uploading_starts_over_from_manifest() {
    let m = mf(&[("a.rs", "aaaa"), ("b.rs", "bb")]);
    let mut s = Session::new();
    let t = s.submit(0, req(&m, Kind::Build));
    s.on_open(0);
    drain(&mut s);
    let acts = rx(&mut s, ServerMsg::Need { tree: m.tree_id(), blobs: vec![sha("aaaa"), sha("bb")] });
    assert!(matches!(acts[1], Action::SendBlob { id: 0, .. }));
    s.on_closed(0, false);
    assert_eq!(drain(&mut s), [Action::Reconnect { after_ms: 500 }]);
    // 古い接続の blob_sent が遅れて来ても何も起きない
    s.blob_sent(0, 0);
    assert_eq!(drain(&mut s), []);
    s.on_open(0);
    assert_eq!(drain(&mut s), [hello(), manifest(&m)]);
    // 今度は 1 つだけ足りない
    let acts = rx(&mut s, ServerMsg::Need { tree: m.tree_id(), blobs: vec![sha("bb")] });
    assert_eq!(acts, [Action::Emit(Event::Uploading { ticket: t, done: 0, total: 2 }), Action::SendBlob { id: 0, manifest: m.clone(), path: "b.rs".into(), sha256: sha("bb"), size: 2 }]);
}

#[test]
fn log_cut_then_attach_without_duplicates() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t = up_to_accepted(&mut s, &m, Kind::Build);
    let j = jid(&m, Kind::Build);
    rx(&mut s, ServerMsg::Started { job: j.clone() });
    assert_eq!(rx(&mut s, ServerMsg::Log { job: j.clone(), offset: 0, text: "abcあ".into() }), [Action::Emit(Event::Log { ticket: t, text: "abcあ".into() })]);

    s.on_closed(0, false);
    assert_eq!(drain(&mut s), [Action::Reconnect { after_ms: 500 }]);
    assert_eq!(s.active(), 1);
    s.on_open(0);
    assert_eq!(drain(&mut s), [hello(), text(&ClientMsg::Attach { job: j.clone(), log: 6, artifact: 0 })]);

    // 重なって届いた分は捨てる（全部重なり・一部重なり・文字の途中から）
    assert_eq!(rx(&mut s, ServerMsg::Log { job: j.clone(), offset: 0, text: "abc".into() }), []);
    assert_eq!(rx(&mut s, ServerMsg::Log { job: j.clone(), offset: 3, text: "あde".into() }), [Action::Emit(Event::Log { ticket: t, text: "de".into() })]);
    assert_eq!(rx(&mut s, ServerMsg::Log { job: j.clone(), offset: 4, text: "\u{3042}".into() }), []);
    // 位置が先へ飛んだら（受付が先頭を捨てた）そのまま受け取る
    assert_eq!(rx(&mut s, ServerMsg::Log { job: j.clone(), offset: 100, text: "x".into() }), [Action::Emit(Event::Log { ticket: t, text: "x".into() })]);
    s.on_closed(0, false);
    s.on_open(0);
    assert_eq!(drain(&mut s), [Action::Reconnect { after_ms: 500 }, hello(), text(&ClientMsg::Attach { job: j, log: 101, artifact: 0 })]);
}

#[test]
fn artifact_cut_then_resumes_from_received_offset() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t = up_to_accepted(&mut s, &m, Kind::Build);
    let j = jid(&m, Kind::Build);
    let art = |offset| ServerMsg::Artifact { job: j.clone(), chan: 1, kind: ArtifactKind::Exe, name: "a".into(), sha256: "s".into(), bytes: 9, zbytes: 6, offset };
    rx(&mut s, art(0));
    s.on_binary(0, &frame(1, 0, false, b"abc"));
    drain(&mut s);

    s.on_closed(0, false);
    s.on_open(0);
    assert_eq!(drain(&mut s), [Action::Reconnect { after_ms: 500 }, hello(), text(&ClientMsg::Attach { job: j.clone(), log: 0, artifact: 3 })]);
    assert_eq!(rx(&mut s, art(3)), [Action::Emit(Event::ArtifactBegin { ticket: t, kind: ArtifactKind::Exe, name: "a".into(), sha256: "s".into(), bytes: 9, zbytes: 6, offset: 3 })]);
    // 受け取り済みの範囲は捨てる
    s.on_binary(0, &frame(1, 0, false, b"abc"));
    assert_eq!(drain(&mut s), []);
    s.on_binary(0, &frame(1, 3, true, b"def"));
    assert_eq!(drain(&mut s), [Action::Emit(Event::ArtifactData { ticket: t, offset: 3, data: 16..19, last: true })]);

    // 位置が飛んだら、つなぎ直して続きをもらう
    rx(&mut s, art(0));
    s.on_binary(0, &frame(1, 5, false, b"zz"));
    assert_eq!(drain(&mut s), [Action::Reconnect { after_ms: 500 }]);
    s.on_open(0);
    assert_eq!(drain(&mut s), [hello(), text(&ClientMsg::Attach { job: j.clone(), log: 0, artifact: 0 })]);
}

#[test]
fn unknown_job_after_reconnect_starts_over_from_manifest() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t = up_to_accepted(&mut s, &m, Kind::Build);
    let j = jid(&m, Kind::Build);
    rx(&mut s, ServerMsg::Log { job: j.clone(), offset: 0, text: "abc".into() });
    s.on_closed(0, false);
    s.on_open(0);
    drain(&mut s);
    // 受付が再起動していた
    let unknown = ServerMsg::Error { job: Some(j.clone()), code: ErrorCode::UnknownJob, message: "?".into(), retry_after_s: None };
    assert_eq!(rx(&mut s, unknown.clone()), [manifest(&m)]);
    rx(&mut s, ServerMsg::Need { tree: m.tree_id(), blobs: vec![] });
    assert_eq!(rx(&mut s, ServerMsg::Ready { tree: m.tree_id() }), [job(&m, Kind::Build)]);
    // ログは最初から受け取る
    assert_eq!(rx(&mut s, ServerMsg::Log { job: j.clone(), offset: 0, text: "abc".into() }), [Action::Emit(Event::Log { ticket: t, text: "abc".into() })]);
    assert_eq!(s.active(), 1);
}

#[test]
fn done_lost_is_retried_three_times() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t = up_to_accepted(&mut s, &m, Kind::Test);
    let j = jid(&m, Kind::Test);
    let lost = ServerMsg::Done { job: j.clone(), status: Status::Lost, exit: 0, queue_ms: 0, run_ms: 0 };
    for _ in 0..3 {
        // 木はこの接続でそろっているので、job を出し直すだけ
        assert_eq!(rx(&mut s, lost.clone()), [job(&m, Kind::Test)]);
        rx(&mut s, ServerMsg::Started { job: j.clone() });
    }
    assert_eq!(rx(&mut s, lost), [done(t, Status::Lost), Action::Idle]);
}

#[test]
fn second_save_cancels_previous_check() {
    let (a, b) = (mf(&[("a.rs", "1")]), mf(&[("a.rs", "2")]));
    let mut s = Session::new();
    let ta = up_to_accepted(&mut s, &a, Kind::Check);
    let ja = jid(&a, Kind::Check);
    // build は種類が違うので取り消さない
    let tb = s.submit(0, req(&a, Kind::Build));
    assert_eq!(drain(&mut s), [job(&a, Kind::Build)]);

    let t2 = s.submit(1_000_000, req(&b, Kind::Check));
    // 前の check に cancel を送ってから、新しい木の確認へ（done は待たない）
    assert_eq!(drain(&mut s), [text(&ClientMsg::Cancel { job: ja.clone() }), manifest(&b)]);
    assert_eq!(s.deadline_us(), Some(6_000_000));
    assert_eq!(s.active(), 3);
    // 取り消したジョブの診断は捨てる
    assert_eq!(rx(&mut s, ServerMsg::Diagnostics { job: ja.clone(), complete: false, truncated: false, files: vec![] }), []);
    assert_eq!(rx(&mut s, ServerMsg::Done { job: ja, status: Status::Cancelled, exit: 0, queue_ms: 0, run_ms: 0 }), [done(ta, Status::Cancelled)]);
    assert_eq!(s.deadline_us(), None);
    rx(&mut s, ServerMsg::Need { tree: b.tree_id(), blobs: vec![] });
    assert_eq!(rx(&mut s, ServerMsg::Ready { tree: b.tree_id() }), [job(&b, Kind::Check)]);
    let _ = (tb, t2);
}

#[test]
fn same_job_id_is_merged() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t1 = up_to_accepted(&mut s, &m, Kind::Build);
    let j = jid(&m, Kind::Build);
    rx(&mut s, ServerMsg::Started { job: j.clone() });
    // 同じ木・同じ種類: 送るものは無い。今の段階だけ知らせる
    let mut r = req(&m, Kind::Build);
    r.want_log = false;
    let t2 = s.submit(0, r);
    assert_eq!(drain(&mut s), [Action::Emit(Event::Running { ticket: t2 })]);
    assert_eq!(s.active(), 1);
    assert_eq!(s.job_of(t1), s.job_of(t2));

    // ログは欲しい依頼にだけ、成果物の中身は先頭の依頼にだけ、ほかは両方に
    assert_eq!(rx(&mut s, ServerMsg::Log { job: j.clone(), offset: 0, text: "x".into() }), [Action::Emit(Event::Log { ticket: t1, text: "x".into() })]);
    let acts = rx(&mut s, ServerMsg::Artifact { job: j.clone(), chan: 1, kind: ArtifactKind::Exe, name: "a".into(), sha256: "s".into(), bytes: 1, zbytes: 1, offset: 0 });
    assert_eq!(acts.len(), 2);
    s.on_binary(0, &frame(1, 0, true, b"z"));
    assert_eq!(drain(&mut s), [Action::Emit(Event::ArtifactData { ticket: t1, offset: 0, data: 16..17, last: true })]);

    // 片方だけ取り消す: ジョブは続く（cancel は送らない）
    s.cancel(0, t1);
    assert_eq!(drain(&mut s), [done(t1, Status::Cancelled)]);
    assert_eq!(rx(&mut s, ServerMsg::Done { job: j, status: Status::Ok, exit: 0, queue_ms: 0, run_ms: 0 }), [done(t2, Status::Ok), Action::Idle]);
}

#[test]
fn three_failed_dials_then_gave_up() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t = s.submit(0, req(&m, Kind::Build));
    assert_eq!(drain(&mut s), [Action::Connect]);
    for ms in [500, 1000, 2000] {
        s.on_closed(0, false);
        assert_eq!(drain(&mut s), [Action::Reconnect { after_ms: ms }]);
    }
    s.on_closed(0, false);
    assert_eq!(drain(&mut s), [Action::Emit(Event::Done { ticket: t, outcome: Outcome::GaveUp }), Action::Idle]);
    // 次の依頼は、またつなぐところから
    s.submit(0, req(&m, Kind::Build));
    assert_eq!(drain(&mut s), [Action::Connect]);
}

#[test]
fn progress_resets_the_failure_count() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    up_to_accepted(&mut s, &m, Kind::Build);
    let j = jid(&m, Kind::Build);
    for _ in 0..5 {
        s.on_closed(0, false);
        assert_eq!(drain(&mut s), [Action::Reconnect { after_ms: 500 }]);
        s.on_open(0);
        drain(&mut s);
        rx(&mut s, ServerMsg::Started { job: j.clone() });
    }
    assert_eq!(s.active(), 1);
}

#[test]
fn check_is_not_retried() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t = up_to_accepted(&mut s, &m, Kind::Check);
    s.on_closed(0, false);
    assert_eq!(drain(&mut s), [Action::Emit(Event::Done { ticket: t, outcome: Outcome::GaveUp }), Action::Idle]);

    // つなげなかったときも同じ
    let t = s.submit(0, req(&m, Kind::Check));
    assert_eq!(drain(&mut s), [Action::Connect]);
    s.on_closed(0, false);
    assert_eq!(drain(&mut s), [Action::Emit(Event::Done { ticket: t, outcome: Outcome::GaveUp }), Action::Idle]);
}

#[test]
fn stale_warm_connection_gets_one_free_retry() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t1 = up_to_accepted(&mut s, &m, Kind::Check);
    rx(&mut s, ServerMsg::Done { job: jid(&m, Kind::Check), status: Status::Ok, exit: 0, queue_ms: 0, run_ms: 0 });
    let _ = t1;

    // 温まっているはずの接続に出したら、実は切れていた: 数えずにすぐつなぎ直す（check でも）
    let m2 = mf(&[("a.rs", "2")]);
    let t2 = s.submit(0, req(&m2, Kind::Check));
    assert_eq!(drain(&mut s), [manifest(&m2)]);
    s.on_closed(0, false);
    assert_eq!(drain(&mut s), [Action::Reconnect { after_ms: 0 }]);
    // 2 回目は無い
    s.on_closed(0, false);
    assert_eq!(drain(&mut s), [Action::Emit(Event::Done { ticket: t2, outcome: Outcome::GaveUp }), Action::Idle]);
}

#[test]
fn limit_error_is_not_retried() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t = up_to_accepted(&mut s, &m, Kind::Build);
    // ジョブあて
    let acts = rx(&mut s, ServerMsg::Error { job: Some(jid(&m, Kind::Build)), code: ErrorCode::Limit, message: "daily".into(), retry_after_s: None });
    assert_eq!(acts, [Action::Emit(Event::Done { ticket: t, outcome: Outcome::Rejected { code: ErrorCode::Limit, message: "daily".into() } }), Action::Idle]);

    // 接続全体（Worker / DO）: 全部の依頼を断る。切れてもつなぎ直さない
    let t1 = s.submit(0, req(&m, Kind::Build));
    let t2 = s.submit(0, req(&m, Kind::Test));
    drain(&mut s);
    let acts = rx(&mut s, ServerMsg::Error { job: None, code: ErrorCode::Limit, message: "rate".into(), retry_after_s: Some(30) });
    let out = Outcome::Rejected { code: ErrorCode::Limit, message: "rate（あと 30 秒）".into() };
    assert_eq!(acts, [Action::Emit(Event::Done { ticket: t1, outcome: out.clone() }), Action::Emit(Event::Done { ticket: t2, outcome: out }), Action::Idle]);
    s.on_closed(0, false);
    assert_eq!(drain(&mut s), []);
}

#[test]
fn unavailable_is_retried_and_reported() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t = s.submit(0, req(&m, Kind::Build));
    drain(&mut s);
    for ms in [500, 1000, 2000] {
        s.on_open(0);
        drain(&mut s);
        assert_eq!(rx(&mut s, ServerMsg::Error { job: None, code: ErrorCode::Unavailable, message: "cold".into(), retry_after_s: None }), []);
        s.on_closed(0, false);
        assert_eq!(drain(&mut s), [Action::Reconnect { after_ms: ms }]);
    }
    s.on_open(0);
    drain(&mut s);
    rx(&mut s, ServerMsg::Error { job: None, code: ErrorCode::Unavailable, message: "cold".into(), retry_after_s: None });
    s.on_closed(0, false);
    assert_eq!(drain(&mut s), [Action::Emit(Event::Done { ticket: t, outcome: Outcome::Rejected { code: ErrorCode::Unavailable, message: "cold".into() } }), Action::Idle]);
}

#[test]
fn unknown_types_and_garbage_are_ignored() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    up_to_accepted(&mut s, &m, Kind::Build);
    for t in [r#"{"type":"future_thing","x":1}"#, "not json", "{}", r#"{"type":"log"}"#, r#"{"type":"started","job":"someone-else"}"#, r#"{"type":"done","job":"nobody","status":"ok"}"#] {
        s.on_text(0, t);
        assert_eq!(drain(&mut s), [], "{t}");
    }
    // 知らない項目が付いていても読む
    s.on_text(0, &format!(r#"{{"type":"started","job":"{}","extra":[1,2]}}"#, jid(&m, Kind::Build)));
    assert_eq!(drain(&mut s).len(), 1);
    assert_eq!(s.active(), 1);
}

#[test]
fn version_mismatch_rejects_everything() {
    let m = mf(&[("a.rs", "1")]);
    let version = |t| Action::Emit(Event::Done { ticket: t, outcome: Outcome::Rejected { code: ErrorCode::Version, message: "約束の版が合わない".into() } });

    // 受付の hello の版が違う
    let mut s = Session::new();
    let t = s.submit(0, req(&m, Kind::Build));
    s.on_open(0);
    drain(&mut s);
    assert_eq!(rx(&mut s, ServerMsg::Hello { v: 2, server: "x".into() }), [version(t), Action::Idle]);
    // 同じ接続への次の依頼もすぐ断る（送らない）
    let t = s.submit(0, req(&m, Kind::Check));
    assert_eq!(drain(&mut s), [version(t), Action::Idle]);

    // error {version}
    let mut s = Session::new();
    let t = s.submit(0, req(&m, Kind::Build));
    s.on_open(0);
    drain(&mut s);
    assert_eq!(rx(&mut s, ServerMsg::Error { job: None, code: ErrorCode::Version, message: "v".into(), retry_after_s: None }), [version(t), Action::Idle]);
    // つなぎ直せば、また試す
    s.on_closed(0, false);
    s.submit(0, req(&m, Kind::Build));
    assert_eq!(drain(&mut s), [Action::Connect]);
}

#[test]
fn cancel_before_anything_was_sent() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t = s.submit(0, req(&m, Kind::Build));
    drain(&mut s);
    s.cancel(0, t);
    assert_eq!(drain(&mut s), [done(t, Status::Cancelled), Action::Idle]);
    // 2 回目・知らない番号は何もしない
    s.cancel(0, t);
    s.cancel(0, Ticket(999));
    assert_eq!(drain(&mut s), []);
    // つながっても、送るのは hello だけ
    s.on_open(0);
    assert_eq!(drain(&mut s), [hello()]);

    // 木を送っている途中で取り消す: 送り終えても job は出さない
    let t = s.submit(0, req(&m, Kind::Build));
    assert_eq!(drain(&mut s), [manifest(&m)]);
    rx(&mut s, ServerMsg::Need { tree: m.tree_id(), blobs: vec![sha("1")] });
    s.cancel(0, t);
    assert_eq!(drain(&mut s), [done(t, Status::Cancelled), Action::Idle]);
    s.blob_sent(0, 0);
    assert_eq!(rx(&mut s, ServerMsg::Ready { tree: m.tree_id() }), []);
}

#[test]
fn cancel_races_with_done() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t = up_to_accepted(&mut s, &m, Kind::Build);
    let j = jid(&m, Kind::Build);
    s.cancel(10, t);
    assert_eq!(drain(&mut s), [text(&ClientMsg::Cancel { job: j.clone() })]);
    s.cancel(20, t);
    assert_eq!(drain(&mut s), []);
    // 行き違いで成功の done と成果物が届いた: 結果は渡さず「取り消し」
    assert_eq!(rx(&mut s, ServerMsg::Artifact { job: j.clone(), chan: 1, kind: ArtifactKind::Exe, name: "a".into(), sha256: "s".into(), bytes: 1, zbytes: 1, offset: 0 }), []);
    s.on_binary(0, &frame(1, 0, true, b"z"));
    assert_eq!(drain(&mut s), []);
    assert_eq!(rx(&mut s, ServerMsg::Done { job: j, status: Status::Ok, exit: 0, queue_ms: 1, run_ms: 2 }), [Action::Emit(Event::Done { ticket: t, outcome: Outcome::Finished { status: Status::Cancelled, exit: 0, queue_ms: 1, run_ms: 2 } }), Action::Idle]);
}

#[test]
fn cancel_without_done_times_out() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t = up_to_accepted(&mut s, &m, Kind::Build);
    s.cancel(1_000, t);
    drain(&mut s);
    assert_eq!(s.deadline_us(), Some(5_001_000));
    s.on_tick(5_000_999);
    assert_eq!(drain(&mut s), []);
    s.on_tick(5_001_000);
    assert_eq!(drain(&mut s), [done(t, Status::Cancelled), Action::Idle]);

    // 受付が「知らないジョブ」と答えたら、すぐ取り消せたことにする
    let t = s.submit(0, req(&m, Kind::Build));
    drain(&mut s);
    s.cancel(0, t);
    drain(&mut s);
    assert_eq!(rx(&mut s, ServerMsg::Error { job: Some(jid(&m, Kind::Build)), code: ErrorCode::UnknownJob, message: "?".into(), retry_after_s: None }), [done(t, Status::Cancelled), Action::Idle]);

    // 取り消しを待っている間に切れたら、そこで終わり（つなぎ直さない）
    let t = s.submit(0, req(&m, Kind::Build));
    drain(&mut s);
    s.cancel(0, t);
    drain(&mut s);
    s.on_closed(0, false);
    assert_eq!(drain(&mut s), [done(t, Status::Cancelled), Action::Idle]);
}

#[test]
fn resubmitting_a_cancelled_job_waits_for_its_done() {
    let (a, b) = (mf(&[("a.rs", "1")]), mf(&[("a.rs", "2")]));
    let mut s = Session::new();
    let ta = up_to_accepted(&mut s, &a, Kind::Check);
    let ja = jid(&a, Kind::Check);
    // 保存 2 回目（中身 B）→ A を取り消す
    let tb = s.submit(0, req(&b, Kind::Check));
    assert_eq!(drain(&mut s), [text(&ClientMsg::Cancel { job: ja.clone() }), manifest(&b)]);
    // 保存 3 回目で中身が A に戻った → B を取り消す。A は、前の A の done を待ってから出す
    let ta2 = s.submit(0, req(&a, Kind::Check));
    assert_eq!(drain(&mut s), [done(tb, Status::Cancelled)]);
    assert_eq!(s.active(), 2);
    // 前の A の accepted / done が届く。新しい依頼のものと取り違えない
    assert_eq!(rx(&mut s, ServerMsg::Started { job: ja.clone() }), []);
    assert_eq!(rx(&mut s, ServerMsg::Done { job: ja.clone(), status: Status::Cancelled, exit: 0, queue_ms: 0, run_ms: 0 }), [done(ta, Status::Cancelled), job(&a, Kind::Check)]);
    assert_eq!(rx(&mut s, ServerMsg::Done { job: ja, status: Status::Ok, exit: 0, queue_ms: 0, run_ms: 0 }), [done(ta2, Status::Ok), Action::Idle]);
    // 送りかけだった B の need / ready が遅れて来ても何もしない
    assert_eq!(rx(&mut s, ServerMsg::Need { tree: b.tree_id(), blobs: vec![] }), []);
    assert_eq!(rx(&mut s, ServerMsg::Ready { tree: b.tree_id() }), []);
}

#[test]
fn cancel_while_disconnected_is_delivered_on_reconnect() {
    let (a, b) = (mf(&[("a.rs", "1")]), mf(&[("a.rs", "2")]));
    let mut s = Session::new();
    let ta = up_to_accepted(&mut s, &a, Kind::Build);
    s.on_closed(0, false);
    drain(&mut s);
    // 切れている間に次の F5: 前のビルドはその場で「取り消し」、受付へはつながってから伝える
    let tb = s.submit(0, req(&b, Kind::Build));
    assert_eq!(drain(&mut s), [done(ta, Status::Cancelled)]);
    s.on_open(0);
    assert_eq!(drain(&mut s), [hello(), text(&ClientMsg::Cancel { job: jid(&a, Kind::Build) }), manifest(&b)]);
    let _ = tb;

    // 取り消したものと同じ中身に戻したら、取り消さずに続きを受ける
    let mut s = Session::new();
    up_to_accepted(&mut s, &a, Kind::Build);
    s.on_closed(0, false);
    let t = s.submit(0, req(&a, Kind::Build));
    s.cancel(0, t);
    drain(&mut s);
    s.submit(0, req(&a, Kind::Build));
    s.on_open(0);
    // つなぎ直しはもう頼んである（Reconnect）ので、Connect は出ない
    assert_eq!(drain(&mut s), [hello(), text(&ClientMsg::Attach { job: jid(&a, Kind::Build), log: 0, artifact: 0 })]);
}

#[test]
fn blob_failed_drops_the_tree_and_keeps_other_jobs() {
    let (a, b) = (mf(&[("a.rs", "1")]), mf(&[("b.rs", "2")]));
    let mut s = Session::new();
    up_to_accepted(&mut s, &a, Kind::Build);
    // 別のプロジェクトとして B を頼む（A は取り消されない）
    let mut r = req(&b, Kind::Build);
    r.project = "ffffffffffffffff".into();
    let tb = s.submit(0, r);
    drain(&mut s);
    rx(&mut s, ServerMsg::Need { tree: b.tree_id(), blobs: vec![sha("2")] });
    // 関係ない番号は無視
    assert_eq!(s.blob_failed(0, 5), []);
    assert_eq!(s.blob_failed(0, 0), [tb]);
    // 受付は中身を待ったままなので、接続を作り直す。A は attach で続く。Done は出ない
    assert_eq!(drain(&mut s), [Action::Reconnect { after_ms: 0 }]);
    assert_eq!(s.active(), 1);
    s.on_open(0);
    assert_eq!(drain(&mut s), [hello(), text(&ClientMsg::Attach { job: jid(&a, Kind::Build), log: 0, artifact: 0 })]);

    // その木の依頼しか無かったときも、接続は作り直させる
    let mut s = Session::new();
    let t = s.submit(0, req(&b, Kind::Build));
    s.on_open(0);
    drain(&mut s);
    rx(&mut s, ServerMsg::Need { tree: b.tree_id(), blobs: vec![sha("2")] });
    assert_eq!(s.blob_failed(0, 0), [t]);
    assert_eq!(drain(&mut s), [Action::Reconnect { after_ms: 0 }, Action::Idle]);
}

#[test]
fn fatal_close_gives_up_everything() {
    let m = mf(&[("a.rs", "1")]);
    let mut s = Session::new();
    let t1 = up_to_accepted(&mut s, &m, Kind::Build);
    let t2 = s.submit(0, req(&m, Kind::Test));
    drain(&mut s);
    s.on_closed(0, true);
    let acts = drain(&mut s);
    assert_eq!(acts.len(), 3);
    for t in [t1, t2] {
        assert!(acts.contains(&Action::Emit(Event::Done { ticket: t, outcome: Outcome::GaveUp })));
    }
    assert_eq!(acts[2], Action::Idle);
    assert_eq!(s.active(), 0);
}

#[test]
fn need_with_many_blobs_and_unknown_hash() {
    // 表を作る経路（9 個以上）。同じ中身のファイルは 1 回だけ求められる
    let contents: Vec<String> = (0..12).map(|i| format!("file {i}")).collect();
    let mut files: Vec<(String, &str)> = contents.iter().enumerate().map(|(i, c)| (format!("f{i:02}.rs"), c.as_str())).collect();
    files.push(("dup.rs".into(), "file 3"));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), *c)).collect();
    let m = mf(&refs);
    let mut s = Session::new();
    s.submit(0, req(&m, Kind::Build));
    s.on_open(0);
    drain(&mut s);
    let acts = rx(&mut s, ServerMsg::Need { tree: m.tree_id(), blobs: contents.iter().rev().map(|c| sha(c)).collect() });
    assert!(matches!(&acts[1], Action::SendBlob { id: 0, path, .. } if path == "f11.rs"));
    for id in 0..12u32 {
        s.blob_sent(0, id);
        let acts = drain(&mut s);
        if id == 7 {
            // "file 3" は、パスの順で先のもの（dup.rs）を読む
            assert!(matches!(&acts[1], Action::SendBlob { id: 8, path, .. } if path == "dup.rs"), "{acts:?}");
        }
    }
    assert_eq!(rx(&mut s, ServerMsg::Ready { tree: m.tree_id() }), [job(&m, Kind::Build)]);

    // 目録に無い中身を求められたら、つなぎ直す
    let mut s = Session::new();
    s.submit(0, req(&m, Kind::Build));
    s.on_open(0);
    drain(&mut s);
    assert_eq!(rx(&mut s, ServerMsg::Need { tree: m.tree_id(), blobs: vec!["00".repeat(32)] }), [Action::Reconnect { after_ms: 500 }]);
}

#[test]
fn manifest_json_matches_the_wire_type() {
    // 目録を写さずに作った JSON が、zc-wire の型から作ったものと同じ
    let m = mf(&[("src/日本語.rs", "x"), ("run.sh", "#!/bin/sh")]);
    let mut s = Session::new();
    s.submit(0, req(&m, Kind::Build));
    s.on_open(0);
    let acts = drain(&mut s);
    assert_eq!(acts[2], manifest(&m));
    assert_eq!(acts.len(), 3);
}
