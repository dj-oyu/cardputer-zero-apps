//! ジョブの実行の口（docs/stage3-design.md §4.3）。形は雛形で確定。
//!
//! 台帳（[`crate::jobs`]）は [`Runner::run`] を、ジョブごとのスレッドで呼ぶ。実行する側は
//! 道具を [`spawn_tool`] で起動し、出力を [`JobOutput`] に流し、終わり方を返す。
//!
//! 言語ごとの中身: [`rust`]（課題 B3）、[`c`]・[`node`]（課題 B4）。
//!
//! # 道具の起動（[`spawn_tool`]）の決まり
//!
//! 道具の中では任意のコード（`build.rs`・手続きマクロ・`npm test`）が動くので、次を守る:
//!
//! - 新しいプロセスグループで起動し、その番号を台帳に知らせる（取り消し・時間切れでは、
//!   台帳がグループごと SIGTERM → SIGKILL する）
//! - 環境変数は [`tool_env`] で絞る（受付の環境に何かあっても渡さない）
//! - 標準入力は `/dev/null`。シグナルのマスクと処理は既定に戻す。受付が死んだら道具も死ぬ
//!   （`PR_SET_PDEATHSIG`）
//! - 出力を溜め込まない: 1 行は [`MAX_LINE_BYTES`] まで（超えた分は捨てる）、合計が
//!   [`MAX_OUTPUT_BYTES`] を超えたらグループごと止める
//! - 道具の親が終わったら、グループに残ったもの（`build.rs` が裏で起動したもの）を SIGKILL
//!   する。パイプを握ったまま逃げたもの（`setsid`）は、少し待って読むのをやめる
//! - 台帳の時間切れ（15 分）とは別に、[`MAX_TOOL_TIME`] の最後の守りを持つ

pub mod c;
pub mod node;
pub mod rust;

use crate::pack::Packed;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use zc_wire::msg::{Diag, FileDiags, JobOpts, Kind, Lang, Severity, Status, TestCase};

/// ジョブ 1 つの文脈。
pub struct JobCtx<'a> {
    pub job: &'a str,
    pub kind: Kind,
    pub lang: Lang,
    pub opts: &'a JobOpts,
    /// 作業の木（目録のとおりに写してある。`proj/<project id>/src`）。
    pub src: &'a Path,
    /// このプロジェクトの、ビルドの出力を置くフォルダ（`proj/<project id>`。`target/` など）。
    pub project_dir: &'a Path,
    /// このジョブの成果物を置くフォルダ（`out/<job id>`）。
    pub out_dir: &'a Path,
    /// 取り消されたら真になる。[`spawn_tool`] で起動した道具は台帳が止めるので、実行する側は
    /// 道具と道具の間でこれを見て、次を始めないだけでよい。
    pub cancelled: &'a AtomicBool,
}

/// 試験の結果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestResults {
    pub passed: u32,
    pub failed: u32,
    pub ignored: u32,
    pub cases: Vec<TestCase>,
}

/// 実行中に出すもの。台帳が実装する。
pub trait JobOutput: Send + Sync {
    /// ログ（道具の標準出力・標準エラーをそのまま）。
    fn log(&self, bytes: &[u8]);
    /// 起動した道具のプロセスグループを知らせる（取り消し・時間切れで止める相手）。
    /// 道具が終わったら `None` で外す。[`spawn_tool`] が呼ぶ。
    fn set_process_group(&self, pgid: Option<i32>);
}

/// 終わり方。
#[derive(Clone, Debug, Default)]
pub struct JobResult {
    /// 道具の終了コード。
    pub exit: i32,
    /// 診断（ファイルごと・行の順）。`complete` は道具が最後まで走ったか。
    pub diagnostics: Vec<FileDiags>,
    pub complete: bool,
    pub tests: Option<TestResults>,
    /// 実機のプロジェクトに書き戻すファイル（木の根からの相対パスと中身）。
    pub write_back: Vec<(String, String)>,
    /// 成果物（build が成功したとき）。
    pub artifact: Option<Packed>,
}

/// 実行できなかった理由（`done` の `status` になる）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobFailure {
    /// `Error`（道具が無い・ディスクがいっぱい）か `Cancelled`。
    pub status: Status,
    pub message: String,
}

/// ジョブの実行の口。
pub trait Runner: Send + Sync {
    /// ジョブを最後まで実行する。コンパイルエラーや試験の失敗は `Ok`（`exit != 0`）で返す。
    fn run(&self, ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure>;
}

/// 本物の実行の口（言語で振り分ける）。
pub struct Tools;

impl Runner for Tools {
    fn run(&self, ctx: &JobCtx<'_>, out: &dyn JobOutput) -> Result<JobResult, JobFailure> {
        match ctx.lang {
            Lang::Rust => rust::run(ctx, out),
            Lang::C => c::run(ctx, out),
            Lang::Node => node::run(ctx, out),
        }
    }
}

/// 1 行の上限。超えた分は捨てる（cargo の JSON の 1 行でも数十 KB まで）。
pub const MAX_LINE_BYTES: usize = 1 << 20;
/// 道具 1 回の出力の合計の上限。超えたらグループごと止める（ログは台帳が 1MiB に切るが、
/// 無限に出し続ける道具で CPU を使い続けない）。
pub const MAX_OUTPUT_BYTES: u64 = 256 << 20;
/// 道具 1 回の時間の最後の守り（ふだんは台帳の時間切れ 15 分が先に効く）。
pub const MAX_TOOL_TIME: Duration = Duration::from_secs(60 * 60);
/// 親が終わったあと、パイプの残りを読む時間（逃げた子がパイプを握っていても待ち続けない）。
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// 残す環境変数（[`tool_env`]）。
const KEEP_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER",
    "RUSTC_WRAPPER",
    "npm_config_cache",
];

fn keep_env(name: &std::ffi::OsStr) -> bool {
    let Some(n) = name.to_str() else { return false };
    KEEP_ENV.contains(&n) || n.starts_with("SCCACHE_")
}

/// 道具に渡す環境変数を絞る: `PATH`・`HOME`・`LANG`・`CARGO_HOME`・`RUSTUP_HOME`・
/// `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER`・`RUSTC_WRAPPER`・`SCCACHE_*`・
/// `npm_config_cache` だけを残し、あとは消す（受付の環境に何かあっても、`build.rs` や
/// `postinstall` に渡さない）。`CARGO_TERM_COLOR=never`・`NO_COLOR=1`・`CI=1` を足す。
///
/// 呼ぶ前に `cmd` に明示して入れた変数（`CARGO_TARGET_DIR` など）はそのまま残る。
pub fn tool_env(cmd: &mut Command) {
    let explicit: Vec<(OsString, Option<OsString>)> = cmd.get_envs().map(|(k, v)| (k.to_os_string(), v.map(|v| v.to_os_string()))).collect();
    cmd.env_clear();
    for (k, v) in std::env::vars_os() {
        if keep_env(&k) {
            cmd.env(k, v);
        }
    }
    cmd.env("CARGO_TERM_COLOR", "never").env("NO_COLOR", "1").env("CI", "1");
    for (k, v) in explicit {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
}

/// [`spawn_tool`] の上限（試験で小さくするために分けてある）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct ToolLimits {
    pub max_line: usize,
    pub max_output: u64,
    pub max_time: Duration,
}

impl ToolLimits {
    pub(crate) const DEFAULT: ToolLimits = ToolLimits { max_line: MAX_LINE_BYTES, max_output: MAX_OUTPUT_BYTES, max_time: MAX_TOOL_TIME };
}

/// 道具を起動して、終わるまで待つ。標準出力と標準エラーを 1 行ずつ `on_stdout` /
/// [`JobOutput::log`] に流す（`on_stdout` が `false` を返した行はログに出さない =
/// cargo の JSON の行）。新しいプロセスグループで起動し、[`JobOutput::set_process_group`]
/// で台帳に知らせる。標準入力は `/dev/null`。戻り値は終了コード（シグナルで終わったら
/// 128 + 番号）。
pub fn spawn_tool(cmd: Command, out: &dyn JobOutput, on_stdout: &mut dyn FnMut(&[u8]) -> bool) -> std::io::Result<i32> {
    spawn_tool_with(cmd, out, on_stdout, ToolLimits::DEFAULT)
}

/// 行に切り分ける受け口（1 行の上限つき）。
struct LineBuf {
    buf: Vec<u8>,
    /// 今の行が上限を超えて、改行まで捨てている。
    skipping: bool,
    max: usize,
}

impl LineBuf {
    fn new(max: usize) -> LineBuf {
        LineBuf { buf: Vec::new(), skipping: false, max }
    }

    /// 読んだ分を足し、完成した行（改行つき）を `emit` に渡す。
    fn push(&mut self, mut data: &[u8], emit: &mut dyn FnMut(&[u8])) {
        while !data.is_empty() {
            let nl = data.iter().position(|&b| b == b'\n');
            let (part, rest, done) = match nl {
                Some(i) => (&data[..=i], &data[i + 1..], true),
                None => (data, &data[data.len()..], false),
            };
            if !self.skipping {
                let room = self.max.saturating_sub(self.buf.len());
                if part.len() <= room {
                    self.buf.extend_from_slice(part);
                } else {
                    // 上限まで入れて、残りは改行まで捨てる
                    self.buf.extend_from_slice(&part[..room]);
                    self.buf.push(b'\n');
                    emit(&self.buf);
                    self.buf.clear();
                    self.skipping = !done;
                    data = rest;
                    continue;
                }
                if done {
                    emit(&self.buf);
                    self.buf.clear();
                }
            } else if done {
                self.skipping = false;
            }
            data = rest;
        }
    }

    fn flush(&mut self, emit: &mut dyn FnMut(&[u8])) {
        if !self.buf.is_empty() {
            emit(&self.buf);
            self.buf.clear();
        }
    }
}

fn set_nonblocking(fd: i32) {
    // SAFETY: 自分の持っている fd のフラグを変えるだけ
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        if fl >= 0 {
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
    }
}

fn killpg(pgid: i32, sig: i32) {
    if pgid > 0 {
        // SAFETY: 自分で作ったプロセスグループにシグナルを送るだけ
        unsafe {
            libc::kill(-pgid, sig);
        }
    }
}

pub(crate) fn spawn_tool_with(mut cmd: Command, out: &dyn JobOutput, on_stdout: &mut dyn FnMut(&[u8]) -> bool, limits: ToolLimits) -> std::io::Result<i32> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.process_group(0);
    // SAFETY: fork と exec の間で、async-signal-safe な呼び出しだけをする
    unsafe {
        cmd.pre_exec(|| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::pthread_sigmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            for sig in [libc::SIGPIPE, libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
                libc::signal(sig, libc::SIG_DFL);
            }
            // 受付（を起動したスレッド）が死んだら道具も死ぬ
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, 0, 0, 0);
            // コアダンプを書かない（ディスクを埋めない）
            let rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            libc::setrlimit(libc::RLIMIT_CORE, &rl);
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let pgid = child.id() as i32;
    out.set_process_group(Some(pgid));

    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    set_nonblocking(stdout.as_raw_fd());
    set_nonblocking(stderr.as_raw_fd());

    let mut lines_out = LineBuf::new(limits.max_line);
    let mut lines_err = LineBuf::new(limits.max_line);
    let mut emit_out = |line: &[u8]| {
        if on_stdout(line) {
            out.log(line);
        }
    };
    let mut emit_err = |line: &[u8]| out.log(line);

    let start = Instant::now();
    let mut open = [true, true];
    let mut total: u64 = 0;
    let mut status = None;
    let mut drain_until: Option<Instant> = None;
    let mut killed_note: Option<&str> = None;
    let mut buf = vec![0u8; 64 << 10];

    loop {
        // 親の終わりを見る
        if status.is_none() {
            if let Some(st) = child.try_wait()? {
                status = Some(st);
                // グループに残ったものを片付けて、パイプの残りを少しだけ読む
                killpg(pgid, libc::SIGKILL);
                out.set_process_group(None);
                drain_until = Some(Instant::now() + DRAIN_GRACE);
            }
        }
        if !open[0] && !open[1] && status.is_some() {
            break;
        }
        if let Some(t) = drain_until {
            if Instant::now() >= t {
                break;
            }
        }
        if status.is_none() && killed_note.is_none() && start.elapsed() >= limits.max_time {
            killed_note = Some("（道具の時間の上限を超えたので止めた）\n");
            killpg(pgid, libc::SIGKILL);
        }

        let mut fds = [
            libc::pollfd { fd: if open[0] { stdout.as_raw_fd() } else { -1 }, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: if open[1] { stderr.as_raw_fd() } else { -1 }, events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: 長さ 2 の pollfd の配列を渡す
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, 50) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            killpg(pgid, libc::SIGKILL);
            let _ = child.wait();
            out.set_process_group(None);
            return Err(e);
        }
        for (i, pfd) in fds.iter().enumerate() {
            if !open[i] || pfd.revents == 0 {
                continue;
            }
            // 読めるだけ読む
            loop {
                let r = if i == 0 { stdout.read(&mut buf) } else { stderr.read(&mut buf) };
                match r {
                    Ok(0) => {
                        open[i] = false;
                        break;
                    }
                    Ok(k) => {
                        total += k as u64;
                        if i == 0 {
                            lines_out.push(&buf[..k], &mut emit_out);
                        } else {
                            lines_err.push(&buf[..k], &mut emit_err);
                        }
                        if total > limits.max_output && killed_note.is_none() {
                            killed_note = Some("（出力が多すぎるので止めた）\n");
                            killpg(pgid, libc::SIGKILL);
                        }
                        if k < buf.len() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        open[i] = false;
                        break;
                    }
                }
            }
        }
    }
    lines_out.flush(&mut emit_out);
    lines_err.flush(&mut emit_err);
    if let Some(note) = killed_note {
        out.log(note.as_bytes());
    }
    let st = match status {
        Some(st) => st,
        None => child.wait()?,
    };
    out.set_process_group(None);
    Ok(match (st.code(), st.signal()) {
        (Some(c), _) => c,
        (None, Some(s)) => 128 + s,
        _ => 255,
    })
}

/// 道具の場所を `PATH` から探す（無ければ [`JobFailure`]: 「cargo が無い」）。
pub fn which(name: &str) -> Result<PathBuf, JobFailure> {
    find_in_path(name).ok_or_else(|| JobFailure { status: Status::Error, message: format!("{name} が無い（ビルド係のイメージに入っていない）") })
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

pub(crate) fn find_in_path(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_executable(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_executable(p))
}

/// 取り消されていたら [`JobFailure`]（`Cancelled`）。
pub(crate) fn check_cancelled(ctx: &JobCtx<'_>) -> Result<(), JobFailure> {
    if ctx.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        Err(JobFailure { status: Status::Cancelled, message: "取り消した".into() })
    } else {
        Ok(())
    }
}

/// 道具を起動できなかった（`spawn` の失敗）を [`JobFailure`] にする。
pub(crate) fn spawn_failure(tool: &str, e: std::io::Error) -> JobFailure {
    JobFailure { status: Status::Error, message: format!("{tool} を起動できない: {e}") }
}

/// 入出力の失敗を [`JobFailure`] にする（ディスクがいっぱい、など）。
pub(crate) fn io_failure(what: &str, e: std::io::Error) -> JobFailure {
    JobFailure { status: Status::Error, message: format!("{what}: {e}") }
}

/// ログを見ながら、末尾の数 KB を覚えておく包み（失敗の理由を診断に出すため）。
/// `capture` が真なら、ログを渡さずに全部溜める（gcc の JSON。上限つき）。
pub(crate) struct Tap<'a> {
    inner: &'a dyn JobOutput,
    capture: bool,
    buf: Mutex<Vec<u8>>,
}

const TAP_TAIL: usize = 4 << 10;
const TAP_CAPTURE_MAX: usize = 8 << 20;

impl<'a> Tap<'a> {
    pub(crate) fn new(inner: &'a dyn JobOutput) -> Tap<'a> {
        Tap { inner, capture: false, buf: Mutex::new(Vec::new()) }
    }

    pub(crate) fn capturing(inner: &'a dyn JobOutput) -> Tap<'a> {
        Tap { inner, capture: true, buf: Mutex::new(Vec::new()) }
    }

    /// 溜めた分（末尾、または全部）を取り出す。
    pub(crate) fn take(&self) -> String {
        let mut b = self.buf.lock().unwrap();
        let s = String::from_utf8_lossy(&b).into_owned();
        b.clear();
        s
    }
}

impl JobOutput for Tap<'_> {
    fn log(&self, bytes: &[u8]) {
        let mut b = self.buf.lock().unwrap();
        if self.capture {
            let room = TAP_CAPTURE_MAX.saturating_sub(b.len());
            b.extend_from_slice(&bytes[..bytes.len().min(room)]);
            return;
        }
        b.extend_from_slice(bytes);
        if b.len() > TAP_TAIL * 2 {
            let cut = b.len() - TAP_TAIL;
            b.drain(..cut);
        }
        drop(b);
        self.inner.log(bytes);
    }

    fn set_process_group(&self, pgid: Option<i32>) {
        self.inner.set_process_group(pgid);
    }
}

/// ログの末尾から、失敗の理由らしい 1 行を拾う: `error`・`undefined reference` を含む行の
/// うち、まとめの行（`ld returned 1 exit status`・`could not compile`）でない最初のもの。
/// 無ければまとめの行、それも無ければ最後の行。
pub(crate) fn failure_line(log_tail: &str) -> Option<String> {
    let lines: Vec<String> = log_tail.lines().map(|l| strip_ansi(l.trim())).filter(|l| !l.is_empty()).collect();
    let is_candidate = |l: &str| {
        let l = l.to_ascii_lowercase();
        l.contains("error") || l.contains("undefined reference")
    };
    let is_summary = |l: &str| l.contains("ld returned") || l.contains("could not compile") || l.contains("aborting due to");
    let pick = lines
        .iter()
        .find(|l| is_candidate(l) && !is_summary(l))
        .or_else(|| lines.iter().rev().find(|l| is_candidate(l)))
        .or(lines.last())?;
    Some(truncate_utf8(pick, 400))
}

/// ANSI のエスケープ（`ESC [ … 文字`）を外す。
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                for d in it.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// 文字の境目で `max` バイトまでに切る。
pub(crate) fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// 道具が出したパスを、木の根からの相対・`/` 区切りに直す。`base` は相対パスの基準
/// （道具の作業フォルダ）。木の外なら `None`。`..` は字面のまま畳む（シンボリックリンクは
/// たどらない。木の中にシンボリックリンクは無い）。
pub(crate) fn rel_to_root(src_root: &Path, base: &Path, file: &str) -> Option<String> {
    if file.is_empty() || file.starts_with('<') {
        return None;
    }
    let p = Path::new(file);
    let joined = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    let mut norm = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                if !norm.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => norm.push(other.as_os_str()),
        }
    }
    let rel = norm.strip_prefix(src_root).ok()?;
    let parts: Vec<&str> = rel.components().map(|c| c.as_os_str().to_str()).collect::<Option<Vec<_>>>()?;
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// 1 件の診断を足す（同じパス・行・桁・メッセージは 1 つにする）。
pub(crate) fn push_diag(into: &mut Vec<FileDiags>, path: &str, d: Diag) {
    let fd = match into.iter_mut().position(|f| f.path == path) {
        Some(i) => &mut into[i],
        None => {
            into.push(FileDiags { path: path.to_string(), diags: Vec::new() });
            into.last_mut().unwrap()
        }
    };
    if fd.diags.iter().any(|x| x.line == d.line && x.col == d.col && x.message == d.message) {
        return;
    }
    fd.diags.push(d);
}

/// 位置なしの診断（ステータスバーに出す 1 行）。
pub(crate) fn note_diag(severity: Severity, message: impl Into<String>) -> Diag {
    Diag { line: 0, col: None, end: None, severity, code: None, message: message.into() }
}

/// 診断を仕上げる: パスの順・行と桁の順に並べ、上限（[`zc_wire::limits::MAX_DIAGNOSTICS`]）を
/// 超えたら error を先に残して切り、最後に「n 件を省いた」の位置なしの 1 件を足す。
pub(crate) fn finish_diags(mut v: Vec<FileDiags>) -> Vec<FileDiags> {
    let max = zc_wire::limits::MAX_DIAGNOSTICS;
    let total: usize = v.iter().map(|f| f.diags.len()).sum();
    if total > max {
        let keep = max - 1;
        let errors: usize = v.iter().flat_map(|f| &f.diags).filter(|d| d.severity == Severity::Error).count();
        let mut err_left = keep.min(errors);
        let mut warn_left = keep - err_left;
        for f in v.iter_mut() {
            f.diags.retain(|d| {
                let left = if d.severity == Severity::Error { &mut err_left } else { &mut warn_left };
                if *left > 0 {
                    *left -= 1;
                    true
                } else {
                    false
                }
            });
        }
        v.retain(|f| !f.diags.is_empty());
        push_diag(&mut v, "", note_diag(Severity::Warning, format!("診断が多すぎるので {} 件を省いた", total - keep)));
    }
    v.sort_by(|a, b| a.path.cmp(&b.path));
    for f in v.iter_mut() {
        f.diags.sort_by_key(|d| (d.line, d.col.unwrap_or(0)));
    }
    v
}

/// 診断に error があるか。
pub(crate) fn has_error(v: &[FileDiags]) -> bool {
    v.iter().flat_map(|f| &f.diags).any(|d| d.severity == Severity::Error)
}

/// 木の中の相対パスとして安全か（`opts.entry` など、実機から来たパスを使う前に通す）。
pub(crate) fn safe_entry(entry: &str) -> bool {
    crate::store::safe_rel_path(entry)
}

/// ファイルの `line`（0 始まり）行目の中身（改行なし）。
pub(crate) fn read_line(path: &Path, line: u32) -> Option<String> {
    let text = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&text);
    text.split('\n').nth(line as usize).map(|l| l.trim_end_matches('\r').to_string())
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::JobOutput;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// 試験用の受け口: ログを溜め、プロセスグループを覚える。`kill_after` があれば、
    /// グループを知らされてからその時間で SIGTERM を送る（台帳の取り消しの代わり）。
    #[derive(Default)]
    pub struct Collect {
        pub log: Mutex<Vec<u8>>,
        pub groups: Mutex<Vec<Option<i32>>>,
        pub kill_after: Option<std::time::Duration>,
    }

    impl Collect {
        pub fn text(&self) -> String {
            String::from_utf8_lossy(&self.log.lock().unwrap()).into_owned()
        }
    }

    impl JobOutput for Collect {
        fn log(&self, bytes: &[u8]) {
            self.log.lock().unwrap().extend_from_slice(bytes);
        }
        fn set_process_group(&self, pgid: Option<i32>) {
            self.groups.lock().unwrap().push(pgid);
            if let (Some(pg), Some(after)) = (pgid, self.kill_after) {
                std::thread::spawn(move || {
                    std::thread::sleep(after);
                    // SAFETY: 試験で作ったグループに送るだけ
                    unsafe {
                        libc::kill(-pg, libc::SIGTERM);
                    }
                });
            }
        }
    }

    /// 試験ごとに別の一時フォルダ（消すのは Drop で）。
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> TempDir {
            static N: AtomicU64 = AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!("zc-buildd-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 道具が無ければ理由を出して `true`（その試験を飛ばす）。
    pub fn skip_without(tools: &[&str]) -> bool {
        for t in tools {
            if super::find_in_path(t).is_none() {
                eprintln!("{t} が無いので飛ばす");
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::Collect;
    use super::*;
    use std::time::Instant;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.arg("-c").arg(script);
        c
    }

    #[test]
    fn env_is_scrubbed() {
        // 親の環境にある、残す一覧に無い変数を 1 つ探す（cargo test が CARGO_MANIFEST_DIR などを入れる）
        let secret = std::env::vars().map(|(k, _)| k).find(|k| !keep_env(std::ffi::OsStr::new(k)) && !["CARGO_TERM_COLOR", "NO_COLOR", "CI"].contains(&k.as_str()));
        let mut cmd = Command::new("env");
        cmd.env("ZC_EXPLICIT", "yes");
        tool_env(&mut cmd);
        let out = Collect::default();
        let code = spawn_tool(cmd, &out, &mut |_| true).unwrap();
        assert_eq!(code, 0);
        let text = out.text();
        assert!(text.contains("CI=1\n") && text.contains("NO_COLOR=1\n") && text.contains("CARGO_TERM_COLOR=never\n"), "{text}");
        assert!(text.contains("ZC_EXPLICIT=yes\n"), "明示した変数は残る");
        assert!(text.lines().any(|l| l.starts_with("PATH=")));
        if let Some(s) = secret {
            assert!(!text.lines().any(|l| l.starts_with(&format!("{s}="))), "{s} が漏れた");
        }
    }

    #[test]
    fn lines_go_to_stdout_filter_and_log() {
        let out = Collect::default();
        let mut seen = Vec::new();
        let code = spawn_tool(sh("echo '{\"json\":1}'; echo plain; echo err >&2; printf tail"), &out, &mut |l| {
            seen.push(String::from_utf8_lossy(l).into_owned());
            !l.starts_with(b"{")
        })
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(seen, vec!["{\"json\":1}\n", "plain\n", "tail"]);
        let log = out.text();
        assert!(!log.contains("json"));
        assert!(log.contains("plain\n") && log.contains("err\n") && log.contains("tail"));
        // 起動で知らせ、終わりで外す
        let g = out.groups.lock().unwrap().clone();
        assert!(matches!(g.first(), Some(Some(_))));
        assert_eq!(g.last(), Some(&None));
    }

    #[test]
    fn missing_tool_and_nonzero_exit() {
        let f = which("zc-no-such-tool-xyz").unwrap_err();
        assert_eq!(f.status, Status::Error);
        assert!(f.message.contains("zc-no-such-tool-xyz"));
        assert!(which("sh").is_ok());
        let out = Collect::default();
        assert!(spawn_tool(Command::new("/nonexistent/tool"), &out, &mut |_| true).is_err());
        assert_eq!(spawn_tool(sh("exit 3"), &out, &mut |_| true).unwrap(), 3);
        assert_eq!(spawn_tool(sh("kill -9 $$"), &out, &mut |_| true).unwrap(), 128 + 9);
    }

    #[test]
    fn cancel_kills_the_whole_group() {
        let out = Collect { kill_after: Some(Duration::from_millis(300)), ..Default::default() };
        let t = Instant::now();
        // 裏で起動した sleep もグループごと止まる
        let code = spawn_tool(sh("sleep 30 & echo $! ; sleep 30; wait"), &out, &mut |_| true).unwrap();
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        assert_eq!(code, 128 + libc::SIGTERM);
        let bg: i32 = out.text().trim().parse().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        // SAFETY: 存在の確認だけ（シグナル 0）
        let alive = unsafe { libc::kill(bg, 0) } == 0;
        assert!(!alive, "裏の sleep が残った");
    }

    #[test]
    fn leftover_children_do_not_hang() {
        // 同じグループに残ってパイプを握る子は、親が終わったらすぐ止める
        let out = Collect::default();
        let t = Instant::now();
        let code = spawn_tool(sh("(sleep 30 &) ; echo done"), &out, &mut |_| true).unwrap();
        assert_eq!(code, 0);
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        assert!(out.text().contains("done"));
        // setsid でグループから逃げてパイプを握る子は、少し待って見捨てる
        if super::testutil::skip_without(&["perl"]) {
            return;
        }
        let out = Collect::default();
        let t = Instant::now();
        let code = spawn_tool(sh("perl -MPOSIX -e '$|=1; POSIX::setsid(); print \"$$\\n\"; sleep 30' & sleep 0.2; echo done"), &out, &mut |_| true).unwrap();
        assert_eq!(code, 0);
        let took = t.elapsed();
        assert!(took >= DRAIN_GRACE && took < DRAIN_GRACE + Duration::from_secs(3), "{took:?}");
        let text = out.text();
        assert!(text.contains("done"), "{text}");
        if let Some(pid) = text.lines().find_map(|l| l.trim().parse::<i32>().ok()) {
            // SAFETY: 試験で起動した、逃げた子を片付けるだけ
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }

    #[test]
    fn oversized_output_is_bounded() {
        let out = Collect::default();
        let limits = ToolLimits { max_line: 1000, max_output: 1 << 20, max_time: MAX_TOOL_TIME };
        let mut longest = 0;
        let code = spawn_tool_with(sh("yes"), &out, &mut |l| {
            longest = longest.max(l.len());
            false
        }, limits)
        .unwrap();
        assert_eq!(code, 128 + 9);
        assert!(out.text().contains("出力が多すぎる"));
        // 1 行が長すぎるときは切って、次の行から続ける
        let out = Collect::default();
        let mut lines = Vec::new();
        let code = spawn_tool_with(sh("head -c 5000 /dev/zero | tr '\\0' a; echo; echo next"), &out, &mut |l| {
            lines.push(l.len());
            false
        }, limits)
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(lines, vec![1001, 5]);
    }

    #[test]
    fn time_limit_backstop() {
        let out = Collect::default();
        let limits = ToolLimits { max_time: Duration::from_millis(300), ..ToolLimits::DEFAULT };
        let t = Instant::now();
        let code = spawn_tool_with(sh("sleep 30"), &out, &mut |_| true, limits).unwrap();
        assert_eq!(code, 128 + 9);
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(out.text().contains("時間の上限"));
    }

    #[test]
    fn paths_are_made_relative_to_root() {
        let root = Path::new("/w/proj/src");
        assert_eq!(rel_to_root(root, root, "src/main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(rel_to_root(root, root, "/w/proj/src/a/b.rs").as_deref(), Some("a/b.rs"));
        assert_eq!(rel_to_root(root, &root.join("sub"), "../x.rs").as_deref(), Some("x.rs"));
        assert_eq!(rel_to_root(root, root, "../dep/src/lib.rs"), None);
        assert_eq!(rel_to_root(root, root, "/rustc/abc/library/core/src/fmt/mod.rs"), None);
        assert_eq!(rel_to_root(root, root, "<anon>"), None);
    }

    #[test]
    fn diagnostics_are_capped_errors_first() {
        let mut v = Vec::new();
        for i in 0..400 {
            push_diag(&mut v, "a.rs", Diag { line: i, col: Some(0), end: None, severity: Severity::Warning, code: None, message: "w".into() });
            push_diag(&mut v, "b.rs", Diag { line: i, col: Some(0), end: None, severity: Severity::Error, code: None, message: "e".into() });
        }
        // 重複は 1 つ
        push_diag(&mut v, "b.rs", Diag { line: 0, col: Some(0), end: None, severity: Severity::Error, code: None, message: "e".into() });
        let v = finish_diags(v);
        let total: usize = v.iter().map(|f| f.diags.len()).sum();
        assert_eq!(total, zc_wire::limits::MAX_DIAGNOSTICS);
        let errs = v.iter().flat_map(|f| &f.diags).filter(|d| d.severity == Severity::Error).count();
        assert_eq!(errs, 400);
        assert_eq!(v[0].path, "");
        // 800 件のうち 499 件を残し、301 件を省いた
        assert!(v[0].diags[0].message.contains("301"), "{}", v[0].diags[0].message);
    }

    #[test]
    fn failure_line_prefers_the_cause() {
        let ld = "/usr/bin/ld: /tmp/m.o: in function `main':\nm.c:(.text+0x8): undefined reference to `g'\ncollect2: error: ld returned 1 exit status\n";
        assert!(failure_line(ld).unwrap().contains("undefined reference"));
        let cargo = "    Updating crates.io index\n\u{1b}[31merror\u{1b}[0m: failed to get `foo` as a dependency\n\nCaused by:\n  x\n";
        assert_eq!(failure_line(cargo).unwrap(), "error: failed to get `foo` as a dependency");
        assert_eq!(failure_line("only\nlast\n").unwrap(), "last");
        assert_eq!(failure_line(""), None);
    }
}
