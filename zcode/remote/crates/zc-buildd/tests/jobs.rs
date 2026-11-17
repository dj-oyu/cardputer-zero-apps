//! ジョブの台帳の受け入れ試験（課題 B1。docs/stage3-design.md §4.2）。
//!
//! 実行の口（[`Runner`]）は偽物: 試験ごとの関数を呼ぶだけ。台帳は `zc-wire` の関数
//! （目録・ID・WebSocket）にも置き場（`store`）にも触らないので、ほかの課題の中身が
//! 入る前でも全部走る。道具を止める試験だけは、本物の `sh` / `sleep` を起動する。

use sha2::Digest;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zc_buildd::jobs::{Feed, JobSpec, Registry, SubmitError, WatchEnd, Watcher, KILL_GRACE, LOG_BATCH_WAIT, LOG_OMITTED};
use zc_buildd::pack::Packed;
use zc_buildd::runner::{JobCtx, JobFailure, JobOutput, JobResult, Runner, TestResults};
use zc_buildd::Config;
use zc_wire::msg::{ArtifactKind, Diag, FileDiags, JobOpts, Kind, Lang, ServerMsg, Severity, Status, TestCase};

type RunFn = dyn Fn(&JobCtx<'_>, &dyn JobOutput) -> Result<JobResult, JobFailure> + Send + Sync;

/// 偽物の実行の口。
struct Fake(Box<RunFn>);

impl Runner for Fake {
    fn run(&self, ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
        (self.0)(ctx, out)
    }
}

fn fake(f: impl Fn(&JobCtx<'_>, &dyn JobOutput) -> Result<JobResult, JobFailure> + Send + Sync + 'static) -> Arc<dyn Runner> {
    Arc::new(Fake(Box::new(f)))
}

/// 試験ごとの置き場（終わったら消す）。
struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("zc-buildd-jobs-{}-{}-{name}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(tmp: &Tmp) -> Config {
    let mut c = Config::defaults();
    c.data_dir = tmp.0.clone();
    c
}

/// 形の合った id（中身は試験用。台帳は計算し直さない）。
fn jid(n: u32) -> String {
    format!("{n:032x}")
}

fn spec_of(job: u32, project: u32, kind: Kind) -> JobSpec {
    JobSpec { job: jid(job), project: format!("{project:016x}"), tree: format!("{job:064x}"), kind, lang: Lang::Rust, opts: JobOpts::default(), device: None }
}

fn spec(job: u32) -> JobSpec {
    spec_of(job, 1, Kind::Build)
}

fn ok(exit: i32) -> Result<JobResult, JobFailure> {
    Ok(JobResult { exit, complete: true, ..JobResult::default() })
}

const WAIT: Duration = Duration::from_secs(10);

/// 次の 1 つ（無ければ試験の失敗）。
fn next(w: &Watcher) -> Feed {
    w.recv(WAIT).expect("まだ終わっていないはず").expect("時間内に届くはず")
}

fn next_msg(w: &Watcher) -> ServerMsg {
    match next(w) {
        Feed::Msg(m) => m,
        other => panic!("メッセージのはず: {other:?}"),
    }
}

/// 終わりまで全部受け取る。
fn collect(w: &Watcher) -> Vec<Feed> {
    let mut v = Vec::new();
    loop {
        match w.recv(WAIT) {
            Ok(Some(f)) => v.push(f),
            Ok(None) => panic!("時間内に終わらない: {v:?}"),
            Err(WatchEnd::Finished) => return v,
            Err(WatchEnd::Lagged) => panic!("外された: {v:?}"),
        }
    }
}

fn msgs(feeds: &[Feed]) -> Vec<&ServerMsg> {
    feeds.iter().filter_map(|f| if let Feed::Msg(m) = f { Some(m) } else { None }).collect()
}

/// 種類の並び（`log` はまとめて 1 つに数える）。
fn shape(feeds: &[Feed]) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    for f in feeds {
        let name = match f {
            Feed::Msg(ServerMsg::Accepted { .. }) => "accepted",
            Feed::Msg(ServerMsg::Started { .. }) => "started",
            Feed::Msg(ServerMsg::Log { .. }) => "log",
            Feed::Msg(ServerMsg::Diagnostics { .. }) => "diagnostics",
            Feed::Msg(ServerMsg::Tests { .. }) => "tests",
            Feed::Msg(ServerMsg::File { .. }) => "file",
            Feed::Msg(ServerMsg::Artifact { .. }) => "artifact",
            Feed::Msg(ServerMsg::Done { .. }) => "done",
            Feed::Msg(_) => "other",
            Feed::ArtifactFile { .. } => "bytes",
        };
        if !(name == "log" && v.last() == Some(&"log")) {
            v.push(name);
        }
    }
    v
}

/// ログをつないだもの。offset が切れ目なく続いていることも確かめる。
fn log_text(feeds: &[Feed]) -> (u64, String) {
    let mut start = None;
    let mut text = String::new();
    for m in msgs(feeds) {
        if let ServerMsg::Log { offset, text: t, .. } = m {
            let s = *start.get_or_insert(*offset);
            assert_eq!(*offset, s + text.len() as u64, "ログの offset が続いていない");
            text.push_str(t);
        }
    }
    (start.unwrap_or(0), text)
}

fn done_of(feeds: &[Feed]) -> (Status, i32) {
    match msgs(feeds).last() {
        Some(ServerMsg::Done { status, exit, .. }) => (*status, *exit),
        other => panic!("最後は done のはず: {other:?}"),
    }
}

fn accepted_of(feeds: &[Feed]) -> (bool, u32) {
    match msgs(feeds).first() {
        Some(ServerMsg::Accepted { cached, queued, .. }) => (*cached, *queued),
        other => panic!("最初は accepted のはず: {other:?}"),
    }
}

/// 合図まで止まる偽物を作るための門。`open()` で 1 つ通す。
#[derive(Clone)]
struct Gate {
    tx: Sender<()>,
    rx: Arc<Mutex<Receiver<()>>>,
}

impl Gate {
    fn new() -> Gate {
        let (tx, rx) = channel();
        Gate { tx, rx: Arc::new(Mutex::new(rx)) }
    }
    fn open(&self) {
        self.tx.send(()).unwrap();
    }
    fn pass(&self) {
        let _ = self.rx.lock().unwrap().recv_timeout(WAIT);
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let until = Instant::now() + WAIT;
    while !f() {
        assert!(Instant::now() < until, "待っても成り立たない: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// ジョブ 1 のログの先頭が、もう捨てられているか（新しく付いた人の最初のログが 0 から始まらない）。
fn log_dropped(r: &Registry) -> bool {
    let w = r.attach(&jid(1), 0, 0).unwrap();
    loop {
        match w.recv(Duration::from_millis(200)) {
            Ok(Some(Feed::Msg(ServerMsg::Log { offset, .. }))) => return offset > 0,
            Ok(Some(_)) => {}
            _ => return false,
        }
    }
}

fn states(r: &Registry) -> Vec<(String, &'static str)> {
    r.status().into_iter().map(|j| (j.job, j.state)).collect()
}

// ───────────── 同じ id・結果の使い回し ─────────────

#[test]
fn two_watchers_of_the_same_id_get_the_same_result() {
    let tmp = Tmp::new("same");
    let gate = Gate::new();
    let runs = Arc::new(AtomicUsize::new(0));
    let (g, n) = (gate.clone(), runs.clone());
    let r = Registry::new(
        &config(&tmp),
        fake(move |_, out| {
            n.fetch_add(1, Ordering::SeqCst);
            out.log(b"compiling\n");
            g.pass();
            out.log(b"finished\n");
            ok(0)
        }),
    );
    let a = r.submit(spec(1), true).unwrap();
    let b = r.submit(spec(1), true).unwrap();
    gate.open();
    let (fa, fb) = (collect(&a), collect(&b));
    for f in [&fa, &fb] {
        assert_eq!(shape(f), ["accepted", "started", "log", "diagnostics", "done"]);
        assert_eq!(accepted_of(f), (false, 0));
        assert_eq!(log_text(f), (0, "compiling\nfinished\n".to_string()));
        assert_eq!(done_of(f), (Status::Ok, 0));
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1, "同じ id は 1 回しか走らない");
}

#[test]
fn finished_results_are_reused_but_cancelled_ones_rerun() {
    let tmp = Tmp::new("cached");
    let runs = Arc::new(AtomicUsize::new(0));
    let gate = Gate::new();
    let (n, g) = (runs.clone(), gate.clone());
    let r = Registry::new(
        &config(&tmp),
        fake(move |ctx, out| {
            n.fetch_add(1, Ordering::SeqCst);
            out.log(b"error[E0308]: mismatched types\n");
            match ctx.kind {
                // check は「コンパイルエラー」で終わる。
                Kind::Check => Ok(JobResult {
                    exit: 101,
                    complete: true,
                    diagnostics: vec![FileDiags { path: "src/main.rs".into(), diags: vec![Diag { line: 3, col: Some(4), end: None, severity: Severity::Error, code: Some("E0308".into()), message: "mismatched types".into() }] }],
                    ..JobResult::default()
                }),
                // test は取り消されるまで止まる。
                Kind::Test => {
                    g.pass();
                    ok(0)
                }
                Kind::Build => ok(0),
            }
        }),
    );
    // 成功: 2 回目は実行せずに結果を流す。
    let first = collect(&r.submit(spec(1), true).unwrap());
    let again = collect(&r.submit(spec(1), true).unwrap());
    assert_eq!(accepted_of(&first), (false, 0));
    assert_eq!(accepted_of(&again), (true, 0));
    assert_eq!(shape(&again), ["accepted", "log", "diagnostics", "done"], "残っていた結果には started を出さない");
    assert_eq!(log_text(&again), log_text(&first));
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    // コンパイルエラーの結果も残す。
    let failed = collect(&r.submit(spec_of(2, 1, Kind::Check), false).unwrap());
    assert_eq!(done_of(&failed), (Status::Failed, 101));
    assert_eq!(shape(&failed), ["accepted", "started", "diagnostics", "done"], "log:false にはログを渡さない");
    let failed2 = collect(&r.submit(spec_of(2, 1, Kind::Check), true).unwrap());
    assert_eq!(accepted_of(&failed2), (true, 0));
    assert_eq!(done_of(&failed2), (Status::Failed, 101));
    assert!(matches!(msgs(&failed2)[2], ServerMsg::Diagnostics { complete: true, files, .. } if files.len() == 1));
    assert_eq!(log_text(&failed2).1, "error[E0308]: mismatched types\n", "ログは台帳に残っている");
    assert_eq!(runs.load(Ordering::SeqCst), 2);

    // 取り消しの結果は残さない: 同じ id をもう一度頼むと、もう一度走る。
    let w = r.submit(spec_of(3, 1, Kind::Test), true).unwrap();
    wait_until("3 が走り始める", || runs.load(Ordering::SeqCst) == 3);
    r.cancel(&jid(3)).unwrap();
    gate.open();
    assert_eq!(done_of(&collect(&w)), (Status::Cancelled, 0));
    gate.open();
    let rerun = collect(&r.submit(spec_of(3, 1, Kind::Test), true).unwrap());
    assert_eq!(accepted_of(&rerun), (false, 0));
    assert_eq!(done_of(&rerun), (Status::Ok, 0));
    assert_eq!(runs.load(Ordering::SeqCst), 4);
}

#[test]
fn resubmitting_while_the_old_run_is_still_dying_waits_for_it() {
    // 保存 → 取り消し → 中身を変えずにもう一度保存。同じ id の古い方が死ぬまで待ってから走る。
    let tmp = Tmp::new("dying");
    let gate = Gate::new();
    let active = Arc::new(AtomicUsize::new(0));
    let overlap = Arc::new(AtomicUsize::new(0));
    let (g, a, o) = (gate.clone(), active.clone(), overlap.clone());
    let r = Registry::new(
        &config(&tmp),
        fake(move |_, _| {
            if a.fetch_add(1, Ordering::SeqCst) > 0 {
                o.fetch_add(1, Ordering::SeqCst);
            }
            g.pass();
            a.fetch_sub(1, Ordering::SeqCst);
            ok(0)
        }),
    );
    let old = r.submit(spec(1), true).unwrap();
    wait_until("走り始める", || active.load(Ordering::SeqCst) == 1);
    r.cancel(&jid(1)).unwrap();
    let new = r.submit(spec(1), true).unwrap();
    assert_eq!(next_msg(&new), ServerMsg::Accepted { job: jid(1), cached: false, queued: 0 });
    assert_eq!(new.recv(Duration::from_millis(100)).unwrap().map(|_| ()), None, "古い方が終わるまで始まらない");
    gate.open();
    assert_eq!(done_of(&collect(&old)), (Status::Cancelled, 0));
    gate.open();
    let f = collect(&new);
    assert_eq!(shape(&f), ["started", "diagnostics", "done"]);
    assert_eq!(done_of(&f), (Status::Ok, 0));
    assert_eq!(overlap.load(Ordering::SeqCst), 0);
}

// ───────────── 切っても続く・attach ─────────────

#[test]
fn job_outlives_its_watcher_and_attach_resumes_from_offsets() {
    let tmp = Tmp::new("attach");
    let gate = Gate::new();
    let g = gate.clone();
    let zdata: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    let z = zdata.clone();
    let r = Registry::new(
        &config(&tmp),
        fake(move |ctx, out| {
            out.log(b"one\n");
            g.pass();
            out.log(b"two\n");
            let zpath = ctx.out_dir.join("artifact.zst");
            std::fs::write(&zpath, &z).unwrap();
            Ok(JobResult {
                exit: 0,
                complete: true,
                artifact: Some(Packed { kind: ArtifactKind::Exe, name: "foo".into(), sha256: "ab".repeat(32), bytes: 9000, zpath, zbytes: z.len() as u64 }),
                write_back: vec![("Cargo.lock".into(), "version = 3\n".into())],
                ..JobResult::default()
            })
        }),
    );
    let w = r.submit(spec(1), true).unwrap();
    assert!(matches!(next_msg(&w), ServerMsg::Accepted { .. }));
    assert!(matches!(next_msg(&w), ServerMsg::Started { .. }));
    assert_eq!(next_msg(&w), ServerMsg::Log { job: jid(1), offset: 0, text: "one\n".into() });
    // 接続が切れた。ジョブは続く。
    drop(w);
    assert_eq!(states(&r), [(jid(1), "running")]);
    gate.open();
    wait_until("終わる", || states(&r) == [(jid(1), "done")]);

    // つなぎ直して、受け取り済みの位置（ログ 4 バイト・成果物 1234 バイト）から。
    let w = r.attach(&jid(1), 4, 1234).unwrap();
    let f = collect(&w);
    assert_eq!(shape(&f), ["started", "log", "diagnostics", "file", "artifact", "bytes", "done"]);
    assert_eq!(log_text(&f), (4, "two\n".to_string()), "続きだけ");
    let (meta, bytes) = (&f[4], &f[5]);
    assert!(matches!(meta, Feed::Msg(ServerMsg::Artifact { chan: 1, kind: ArtifactKind::Exe, name, bytes: 9000, zbytes: 5000, offset: 1234, .. }) if name == "foo"));
    match bytes {
        Feed::ArtifactFile { chan: 1, path, offset: 1234, zbytes: 5000 } => assert_eq!(std::fs::read(path).unwrap(), zdata),
        other => panic!("{other:?}"),
    }
    match &f[3] {
        Feed::Msg(ServerMsg::File { path, sha256, text, .. }) => {
            assert_eq!((path.as_str(), text.as_str()), ("Cargo.lock", "version = 3\n"));
            let want: String = sha2::Sha256::digest(b"version = 3\n").iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(*sha256, want);
        }
        other => panic!("{other:?}"),
    }

    // 全部受け取り済みなら、成果物は「もう無い」の合図だけ。位置が大きすぎても同じ。
    let f = collect(&r.attach(&jid(1), 8, 99_999).unwrap());
    assert_eq!(shape(&f), ["started", "diagnostics", "file", "artifact", "bytes", "done"]);
    assert!(matches!(&f[4], Feed::ArtifactFile { offset: 5000, zbytes: 5000, .. }));

    assert_eq!(r.attach(&jid(9), 0, 0).err(), Some(SubmitError::Unknown));
    assert_eq!(r.cancel(&jid(9)).err(), Some(SubmitError::Unknown));
}

#[test]
fn attach_to_a_queued_job_waits_for_it() {
    let tmp = Tmp::new("attachq");
    let gate = Gate::new();
    let g = gate.clone();
    let r = Registry::new(
        &config(&tmp),
        fake(move |_, _| {
            g.pass();
            ok(0)
        }),
    );
    let _a = r.submit(spec(1), true).unwrap();
    drop(r.submit(spec(2), true).unwrap());
    let w = r.attach(&jid(2), 0, 0).unwrap();
    assert!(w.recv(Duration::from_millis(50)).unwrap().is_none(), "待っている間は何も来ない（accepted も出さない）");
    gate.open();
    gate.open();
    assert_eq!(shape(&collect(&w)), ["started", "diagnostics", "done"]);
}

// ───────────── 取り消し・時間切れ ─────────────

/// 本物の道具を、新しいプロセスグループで起動して、終わるまで待つ（`spawn_tool` の代わり）。
fn run_tool(script: &str, out: &dyn JobOutput, pgid_out: &Mutex<Option<i32>>) -> i32 {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let mut child = Command::new("sh").arg("-c").arg(script).stdin(Stdio::null()).process_group(0).spawn().unwrap();
    let pgid = child.id() as i32;
    *pgid_out.lock().unwrap() = Some(pgid);
    out.set_process_group(Some(pgid));
    let st = child.wait().unwrap();
    out.set_process_group(None);
    st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0))
}

/// プロセスグループに誰か残っているか。
fn group_alive(pgid: i32) -> bool {
    // SAFETY: シグナル 0 は、相手がいるかを確かめるだけ。
    unsafe { libc::kill(-pgid, 0) == 0 }
}

#[test]
fn cancel_kills_the_whole_process_group() {
    let tmp = Tmp::new("cancel");
    let pgid = Arc::new(Mutex::new(None));
    let p = pgid.clone();
    let r = Registry::new(
        &config(&tmp),
        fake(move |_, out| {
            // 道具が孫を作る（cargo → rustc のように）。
            let exit = run_tool("sleep 300 & sleep 300 & wait", out, &p);
            Ok(JobResult { exit, complete: false, ..JobResult::default() })
        }),
    );
    let w = r.submit(spec(1), true).unwrap();
    wait_until("道具が起動する", || pgid.lock().unwrap().is_some());
    let pg = pgid.lock().unwrap().unwrap();
    assert!(group_alive(pg));
    let t = Instant::now();
    r.cancel(&jid(1)).unwrap();
    let f = collect(&w);
    assert_eq!(done_of(&f).0, Status::Cancelled);
    assert!(t.elapsed() < KILL_GRACE, "SIGTERM で終わるはず");
    assert!(matches!(msgs(&f)[msgs(&f).len() - 2], ServerMsg::Diagnostics { complete: false, .. }), "取り消しの診断は不完全");
    wait_until("孫まで消える", || !group_alive(pg));
    // 終わったジョブの取り消しは何もしない。
    assert_eq!(r.cancel(&jid(1)), Ok(()));
}

#[test]
fn a_tool_that_ignores_sigterm_gets_sigkill() {
    let tmp = Tmp::new("kill");
    let pgid = Arc::new(Mutex::new(None));
    let p = pgid.clone();
    let r = Registry::new(
        &config(&tmp),
        fake(move |_, out| {
            let exit = run_tool("trap '' TERM; sleep 300 & wait; sleep 300", out, &p);
            Ok(JobResult { exit, ..JobResult::default() })
        }),
    );
    let w = r.submit(spec(1), true).unwrap();
    wait_until("道具が起動する", || pgid.lock().unwrap().is_some());
    std::thread::sleep(Duration::from_millis(200)); // trap が入るのを待つ
    let t = Instant::now();
    r.cancel(&jid(1)).unwrap();
    assert_eq!(done_of(&collect(&w)).0, Status::Cancelled);
    assert!(t.elapsed() >= KILL_GRACE - Duration::from_millis(100), "SIGTERM を無視するので、2 秒後の SIGKILL まで終わらない");
    let pg = pgid.lock().unwrap().unwrap();
    wait_until("グループが消える", || !group_alive(pg));
}

#[test]
fn timeout_stops_the_job() {
    let tmp = Tmp::new("timeout");
    let mut cfg = config(&tmp);
    cfg.job_timeout = Duration::from_millis(300);
    let pgid = Arc::new(Mutex::new(None));
    let p = pgid.clone();
    let r = Registry::new(
        &cfg,
        fake(move |_, out| {
            out.log(b"started\n");
            let exit = run_tool("sleep 300", out, &p);
            Ok(JobResult { exit, complete: true, ..JobResult::default() })
        }),
    );
    let w = r.submit(spec(1), true).unwrap();
    let f = collect(&w);
    assert_eq!(done_of(&f).0, Status::Timeout);
    assert!(matches!(msgs(&f)[msgs(&f).len() - 2], ServerMsg::Diagnostics { complete: false, .. }));
    // 殺した直後は、刈り取られるまで zombie として残りうる（kill(-pg, 0) は zombie にも成功する）
    let pg = pgid.lock().unwrap().unwrap();
    wait_until("グループが消える", || !group_alive(pg));
    // 時間切れの結果は使い回さない。でも attach には本当の終わり方を返す。
    assert_eq!(done_of(&collect(&r.attach(&jid(1), 0, 0).unwrap())).0, Status::Timeout);
    assert_eq!(accepted_of(&[next(&r.submit(spec(1), true).unwrap())]), (false, 0));
}

#[test]
fn cancelling_a_queued_job_finishes_it_at_once_without_running() {
    let tmp = Tmp::new("cancelq");
    let gate = Gate::new();
    let runs = Arc::new(AtomicUsize::new(0));
    let (g, n) = (gate.clone(), runs.clone());
    let r = Registry::new(
        &config(&tmp),
        fake(move |_, _| {
            n.fetch_add(1, Ordering::SeqCst);
            g.pass();
            ok(0)
        }),
    );
    let a = r.submit(spec(1), true).unwrap();
    let b = r.submit(spec(2), true).unwrap();
    assert_eq!(states(&r), [(jid(1), "running"), (jid(2), "queued")]);
    r.cancel(&jid(2)).unwrap();
    let f = collect(&b);
    assert_eq!(shape(&f), ["accepted", "diagnostics", "done"], "走っていないので started は無い");
    assert_eq!(done_of(&f), (Status::Cancelled, 0));
    gate.open();
    collect(&a);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

// ───────────── 並べ方 ─────────────

#[test]
fn same_project_is_serial_and_other_projects_run_in_parallel() {
    let tmp = Tmp::new("serial");
    let gate = Gate::new();
    let started = Arc::new(Mutex::new(Vec::<String>::new()));
    let (g, s) = (gate.clone(), started.clone());
    let r = Registry::new(
        &config(&tmp),
        fake(move |ctx, _| {
            s.lock().unwrap().push(ctx.job.to_string());
            g.pass();
            ok(0)
        }),
    );
    let seen = || started.lock().unwrap().clone();
    let a = r.submit(spec_of(1, 1, Kind::Build), true).unwrap();
    let b = r.submit(spec_of(2, 1, Kind::Build), true).unwrap(); // 同じプロジェクト → 待つ
    let c = r.submit(spec_of(3, 2, Kind::Build), true).unwrap(); // 別のプロジェクト → 並んで走る
    let d = r.submit(spec_of(4, 3, Kind::Build), true).unwrap(); // 同時に 2 つまで → 待つ
    assert_eq!(accepted_of(&[next(&a)]), (false, 0));
    assert_eq!(accepted_of(&[next(&b)]), (false, 0));
    assert_eq!(accepted_of(&[next(&c)]), (false, 0));
    assert_eq!(accepted_of(&[next(&d)]), (false, 1), "前に 2 が並んでいる");
    wait_until("1 と 3 が走る", || seen().len() == 2);
    std::thread::sleep(Duration::from_millis(50));
    let mut first = seen();
    first.sort();
    assert_eq!(first, [jid(1), jid(3)], "2 と 4 は待っている");
    assert_eq!(states(&r), [(jid(1), "running"), (jid(3), "running"), (jid(2), "queued"), (jid(4), "queued")]);
    for _ in 0..4 {
        gate.open();
    }
    for w in [&a, &b, &c, &d] {
        assert_eq!(done_of(&collect(w)), (Status::Ok, 0));
    }
    assert_eq!(seen().len(), 4);
}

#[test]
fn checks_jump_the_queue_and_a_full_queue_refuses() {
    let tmp = Tmp::new("queue");
    let mut cfg = config(&tmp);
    cfg.max_running = 1;
    cfg.max_queued = 3;
    let gate = Gate::new();
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let (g, o) = (gate.clone(), order.clone());
    let r = Registry::new(
        &cfg,
        fake(move |ctx, _| {
            o.lock().unwrap().push(ctx.job.to_string());
            g.pass();
            ok(0)
        }),
    );
    let _a = r.submit(spec_of(1, 1, Kind::Build), true).unwrap();
    let _b = r.submit(spec_of(2, 2, Kind::Build), true).unwrap();
    let _c = r.submit(spec_of(3, 3, Kind::Test), true).unwrap();
    let d = r.submit(spec_of(4, 4, Kind::Check), true).unwrap();
    assert_eq!(accepted_of(&[next(&d)]), (false, 0), "check は列の先頭に入る");
    // 列は 3 つまで。
    assert_eq!(r.submit(spec_of(5, 5, Kind::Check), true).err(), Some(SubmitError::QueueFull));
    assert_eq!(r.attach(&jid(5), 0, 0).err(), Some(SubmitError::Unknown), "断ったジョブは台帳に残さない");
    // もうあるジョブに付くのは、列がいっぱいでもできる。
    assert_eq!(accepted_of(&[next(&r.submit(spec_of(3, 3, Kind::Test), true).unwrap())]), (false, 2));
    for _ in 0..4 {
        gate.open();
    }
    wait_until("全部終わる", || order.lock().unwrap().len() == 4 && r.status().iter().all(|j| j.state == "done"));
    assert_eq!(*order.lock().unwrap(), [jid(1), jid(4), jid(2), jid(3)]);
}

#[test]
fn ids_that_are_not_hex_are_refused() {
    let tmp = Tmp::new("ids");
    let r = Registry::new(&config(&tmp), fake(|_, _| ok(0)));
    let mut s = spec(1);
    s.job = "../../../../etc/passwd".into();
    assert_eq!(r.submit(s, true).err(), Some(SubmitError::BadJobId));
    let mut s = spec(1);
    s.project = "..".into();
    assert_eq!(r.submit(s, true).err(), Some(SubmitError::BadJobId));
    let mut s = spec(1);
    s.job = jid(1).to_uppercase().replace('0', "A");
    assert_eq!(r.submit(s, true).err(), Some(SubmitError::BadJobId));
    assert!(r.status().is_empty());
}

// ───────────── ログ ─────────────

#[test]
fn small_writes_are_batched_and_big_ones_go_at_once() {
    let tmp = Tmp::new("batch");
    let gate = Gate::new();
    let g = gate.clone();
    let r = Registry::new(
        &config(&tmp),
        fake(move |_, out| {
            for i in 0..20 {
                out.log(format!("line {i}\n").as_bytes());
                std::thread::sleep(Duration::from_millis(1));
            }
            g.pass();
            out.log(&vec![b'x'; 5000]);
            g.pass();
            ok(0)
        }),
    );
    let w = r.submit(spec(1), true).unwrap();
    next_msg(&w);
    next_msg(&w);
    let t = Instant::now();
    let mut small = Vec::new();
    while small.iter().map(|m: &String| m.len()).sum::<usize>() < 20 * 7 + 10 {
        match next_msg(&w) {
            ServerMsg::Log { text, .. } => small.push(text),
            other => panic!("{other:?}"),
        }
    }
    assert!(small.len() <= 3, "1ms おきの 20 行が、行ごとではなくまとまって届く: {} 個", small.len());
    assert!(t.elapsed() >= LOG_BATCH_WAIT - Duration::from_millis(25), "少しのログは 50ms 待ってから");
    // 4KB を超えたら待たない。
    gate.open();
    let t = Instant::now();
    match next_msg(&w) {
        ServerMsg::Log { text, .. } => assert_eq!(text.len(), 5000),
        other => panic!("{other:?}"),
    }
    assert!(t.elapsed() < LOG_BATCH_WAIT, "大きなログはすぐ");
    gate.open();
    collect(&w);
}

#[test]
fn log_is_capped_and_offsets_keep_counting() {
    let tmp = Tmp::new("cap");
    let gate = Gate::new();
    let g = gate.clone();
    let r = Registry::new(
        &config(&tmp),
        fake(move |_, out| {
            out.log(b"first\n");
            g.pass();
            // 1 行 100 バイトを 30000 行 = 3MB（上限は 1MiB）。
            for i in 0..30_000 {
                out.log(format!("{i:099}\n").as_bytes());
            }
            g.pass();
            out.log(b"the end\n");
            ok(0)
        }),
    );
    // 見ている途中で、続きが上限からこぼれた人は外される。
    let slow = r.submit(spec(1), true).unwrap();
    next_msg(&slow);
    next_msg(&slow);
    assert_eq!(next_msg(&slow), ServerMsg::Log { job: jid(1), offset: 0, text: "first\n".into() });
    gate.open();
    wait_until("ログが上限を超える", || log_dropped(&r));
    assert_eq!(slow.recv(WAIT).err(), Some(WatchEnd::Lagged), "遅い見る人は外される");
    assert_eq!(slow.recv(WAIT).err(), Some(WatchEnd::Lagged), "外された後も同じ答え");
    gate.open();
    wait_until("終わる", || states(&r) == [(jid(1), "done")]);

    // つなぎ直すと、「省略」の 1 行から始まる続きを受け取れる。位置は捨てた分も数えたまま。
    let total = 6 + 30_000u64 * 100 + 8;
    let f = collect(&r.attach(&jid(1), 0, 0).unwrap());
    let (start, text) = log_text(&f);
    assert!(text.starts_with(LOG_OMITTED), "{:?}", &text[..40]);
    assert_eq!(start + text.len() as u64, total);
    assert!(text.len() <= zc_wire::limits::MAX_LOG_BYTES + LOG_OMITTED.len());
    assert!(text.len() > zc_wire::limits::MAX_LOG_BYTES / 2);
    let body = &text[LOG_OMITTED.len()..];
    assert_eq!(body.find('\n'), Some(99), "行の境目で切る");
    assert!(body.ends_with("29999\nthe end\n"));
    assert_eq!(done_of(&f), (Status::Ok, 0));
    // 残っている範囲の中から attach すれば、印は入らない。
    let f = collect(&r.attach(&jid(1), total - 8, 0).unwrap());
    assert_eq!(log_text(&f), (total - 8, "the end\n".to_string()));
}

#[test]
fn log_bytes_split_inside_a_character_are_joined_and_bad_bytes_replaced() {
    let tmp = Tmp::new("utf8");
    let r = Registry::new(
        &config(&tmp),
        fake(|_, out| {
            let s = "警告: 日本語\n".as_bytes();
            out.log(&s[..4]); // 「警」の 3 バイト + 「告」の 1 バイト目
            out.log(&s[4..5]);
            out.log(&s[5..]);
            out.log(b"bad \xff byte\n");
            out.log(&"端".as_bytes()[..2]); // 最後が文字の途中で終わる
            ok(0)
        }),
    );
    let f = collect(&r.submit(spec(1), true).unwrap());
    assert_eq!(log_text(&f).1, "警告: 日本語\nbad \u{FFFD} byte\n\u{FFFD}");
}

// ───────────── 終わりの知らせ ─────────────

#[test]
fn results_are_forwarded_in_order() {
    let tmp = Tmp::new("finals");
    let r = Registry::new(
        &config(&tmp),
        fake(|ctx, out| {
            out.log(b"running 2 tests\n");
            assert!(ctx.out_dir.is_dir(), "成果物のフォルダは作ってある");
            assert!(ctx.src.ends_with("proj/0000000000000001/src"));
            assert_eq!(ctx.project_dir, ctx.src.parent().unwrap());
            // 診断は上限（500 件）で切る。
            let many = (0..600).map(|i| Diag { line: i, col: None, end: None, severity: Severity::Warning, code: None, message: "unused".into() }).collect();
            Ok(JobResult {
                exit: 101,
                complete: true,
                diagnostics: vec![FileDiags { path: "src/lib.rs".into(), diags: many }, FileDiags { path: "src/b.rs".into(), diags: vec![] }],
                tests: Some(TestResults { passed: 1, failed: 1, ignored: 0, cases: vec![TestCase { name: "t::fails".into(), passed: false, path: Some("src/lib.rs".into()), line: Some(7), message: "assertion failed".into() }] }),
                write_back: vec![("Cargo.lock".into(), "x".into()), ("huge".into(), "y".repeat(zc_wire::limits::MAX_WRITEBACK_BYTES + 1))],
                // 失敗したジョブの成果物は流さない。
                artifact: Some(Packed { kind: ArtifactKind::Exe, name: "foo".into(), sha256: "00".repeat(32), bytes: 1, zpath: ctx.out_dir.join("a.zst"), zbytes: 1 }),
            })
        }),
    );
    let f = collect(&r.submit(spec_of(1, 1, Kind::Test), true).unwrap());
    assert_eq!(shape(&f), ["accepted", "started", "log", "diagnostics", "tests", "file", "done"]);
    let m = msgs(&f);
    match m[3] {
        ServerMsg::Diagnostics { complete: true, truncated: true, files, .. } => {
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].diags.len(), 500);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(m[4], ServerMsg::Tests { passed: 1, failed: 1, ignored: 0, cases, .. } if cases.len() == 1));
    assert!(matches!(m[5], ServerMsg::File { path, .. } if path == "Cargo.lock"));
    match m[6] {
        ServerMsg::Done { status: Status::Failed, exit: 101, .. } => {}
        other => panic!("{other:?}"),
    }
    // フォルダは `done` を送った後に消すので、少し待つ（31 回に 1 回、消す前に見て落ちた）
    let gone = (0..200).any(|_| {
        let g = !tmp.0.join("out").join(jid(1)).exists();
        if !g {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        g
    });
    assert!(gone, "成果物の無いジョブのフォルダは残さない");
}

#[test]
fn runner_failures_and_panics_become_done_error() {
    let tmp = Tmp::new("error");
    let r = Registry::new(
        &config(&tmp),
        fake(|ctx, out| match ctx.kind {
            Kind::Build => {
                out.log(b"no newline");
                Err(JobFailure { status: Status::Error, message: "cargo が無い".into() })
            }
            _ => panic!("偽物の実行の口が落ちた（試験の一部）"),
        }),
    );
    let f = collect(&r.submit(spec(1), true).unwrap());
    assert_eq!(shape(&f), ["accepted", "started", "log", "diagnostics", "done"]);
    assert_eq!(done_of(&f).0, Status::Error);
    assert_eq!(log_text(&f).1, "no newline\nzc-buildd: cargo が無い\n");
    match msgs(&f)[3] {
        ServerMsg::Diagnostics { complete: false, files, .. } => {
            assert_eq!(files.len(), 1);
            assert_eq!((files[0].path.as_str(), files[0].diags[0].message.as_str()), ("", "cargo が無い"), "位置なしの診断で理由を伝える");
        }
        other => panic!("{other:?}"),
    }
    // 落ちても台帳は生きていて、枠も空く。
    let f = collect(&r.submit(spec_of(2, 1, Kind::Check), false).unwrap());
    assert_eq!(done_of(&f).0, Status::Error);
    // 失敗の結果は使い回さない。
    assert_eq!(accepted_of(&collect(&r.submit(spec(1), true).unwrap())), (false, 0));
}

#[test]
fn prepare_runs_once_before_the_runner_and_its_failure_fails_the_job() {
    let tmp = Tmp::new("prepare");
    let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let l = log.clone();
    let r = Registry::new(
        &config(&tmp),
        fake(move |_, _| {
            l.lock().unwrap().push("run");
            ok(0)
        }),
    );
    let l = log.clone();
    let w = r
        .submit_prepared(
            spec(1),
            true,
            Box::new(move || {
                l.lock().unwrap().push("prepare");
                Ok(())
            }),
        )
        .unwrap();
    assert_eq!(done_of(&collect(&w)), (Status::Ok, 0));
    // 残っていた結果に付くときは呼ばない。
    let l = log.clone();
    let w = r
        .submit_prepared(
            spec(1),
            true,
            Box::new(move || {
                l.lock().unwrap().push("prepare again");
                Ok(())
            }),
        )
        .unwrap();
    collect(&w);
    assert_eq!(*log.lock().unwrap(), ["prepare", "run"]);

    let w = r.submit_prepared(spec(2), true, Box::new(|| Err("作業の木を作れない".into()))).unwrap();
    let f = collect(&w);
    assert_eq!(done_of(&f).0, Status::Error);
    assert!(log_text(&f).1.contains("作業の木を作れない"));
    assert_eq!(*log.lock().unwrap(), ["prepare", "run"], "準備に失敗したら実行しない");
}

// ───────────── 残す数・止める ─────────────

#[test]
fn old_results_are_dropped_by_count_and_by_age() {
    let tmp = Tmp::new("keep");
    let mut cfg = config(&tmp);
    cfg.keep_done_max = 2;
    cfg.keep_done = Duration::from_millis(400);
    let r = Registry::new(
        &cfg,
        fake(|ctx, _| {
            let zpath = ctx.out_dir.join("artifact.zst");
            std::fs::write(&zpath, b"zz").unwrap();
            Ok(JobResult { exit: 0, complete: true, artifact: Some(Packed { kind: ArtifactKind::Js, name: "main.js".into(), sha256: "00".repeat(32), bytes: 2, zpath, zbytes: 2 }), ..JobResult::default() })
        }),
    );
    for i in 1..=3 {
        collect(&r.submit(spec_of(i, i, Kind::Build), true).unwrap());
    }
    let out = |i: u32| tmp.0.join("out").join(jid(i));
    assert_eq!(states(&r), [(jid(2), "done"), (jid(3), "done")], "2 つまで");
    assert!(!out(1).exists() && out(2).join("artifact.zst").exists() && out(3).exists());
    assert_eq!(r.attach(&jid(1), 0, 0).err(), Some(SubmitError::Unknown));
    wait_until("古い結果が消える", || r.status().is_empty());
    wait_until("成果物も消える", || !out(2).exists() && !out(3).exists());
    assert_eq!(accepted_of(&collect(&r.submit(spec_of(2, 2, Kind::Build), true).unwrap())), (false, 0), "消えた後はやり直す");
}

#[test]
fn shutdown_stops_running_jobs_and_refuses_new_ones() {
    let tmp = Tmp::new("shutdown");
    let pgid = Arc::new(Mutex::new(None));
    let p = pgid.clone();
    let r = Registry::new(
        &config(&tmp),
        fake(move |_, out| {
            let exit = run_tool("sleep 300", out, &p);
            Ok(JobResult { exit, ..JobResult::default() })
        }),
    );
    let a = r.submit(spec(1), true).unwrap();
    let b = r.submit(spec(2), true).unwrap();
    wait_until("道具が起動する", || pgid.lock().unwrap().is_some());
    r.shutdown();
    // 戻ったときには殺してある（刈り取られるまで zombie として残りうるので、消えるのを待つ）
    let pg = pgid.lock().unwrap().unwrap();
    wait_until("道具のグループが消える", || !group_alive(pg));
    assert_eq!(done_of(&collect(&a)).0, Status::Cancelled);
    assert_eq!(done_of(&collect(&b)).0, Status::Cancelled);
    assert_eq!(r.submit(spec(3), true).err(), Some(SubmitError::QueueFull));
}
