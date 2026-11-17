//! コマンドライン（docs/stage3-design.md §7.8）。課題 D5。
//!
//! ```text
//! zc-remote build <file> [--release]   ビルドして、置いた実行ファイルのパスを標準出力に出す
//! zc-remote check <file>               型検査。診断を「パス:行:桁: error[E0308]: …」で出す
//! zc-remote test  <file>               試験をビルド係で実行して、結果を出す
//! zc-remote run   <file> [-- 引数…]    ビルドして、その場で実行する（F5 がターミナルで動かす）
//! zc-remote applies <file>              ビルド係の対象か（0 = 対象、1 = 対象でない。通信しない）
//! zc-remote serve [--stats]            UNIX ソケットで待つ（ふつうは自動で起動される）
//! zc-remote status                     設定・つながるビルド係・走っているジョブ
//! zc-remote stop                       serve を終わらせる
//! 共通: --lang rust|c|node|python|shell|cpp|go|ruby|zig|moonbit（省略すると拡張子から）  --stats  --version  --help
//! ```
//!
//! - どれも serve に頼むだけ（無ければ切り離して起動する）。ログは標準エラーへそのまま流す
//! - `run`: 成功したら、置かれた実行ファイルに **`exec` で入れ替わる**（JS は `llrt <パス>`）。
//!   だから終了コードも Ctrl+C も、F5 の今の仕組み（OSC 777）のまま動く。ビルド中の
//!   SIGINT は「取り消して 130 で終わる」
//! - `run` でネットが無く、前に成功した版があるとき: 「オフライン。前の版（<日時>）を
//!   実行する？ [y/N]」と端末で聞く（端末でなければ実行しない。`--offline-ok` を付ければ
//!   聞かずに実行する）
//! - 終了コード: 0 成功 / 1 ビルド・型検査・試験の失敗 / 2 使い方の間違い /
//!   3 オフライン / 4 認証・設定 / 5 上限 / 6 そのほかの失敗 / 130 取り消し。
//!   `run` で実行ファイルを起動できなかったときは 126
//!
//! 決めたこと（実装）:
//!
//! - `check` の診断は標準出力に 1 件 1 行（行・桁は 1 始まり。桁は UTF-8 のバイト位置 + 1）。
//!   `build` / `run` / `test` は診断を出さない（道具の出力がログにそのまま入っている）
//! - `test` の絞り込み（`[filter]`）は未対応（UNIX ソケットの約束に項目が無い）。付けたら
//!   使い方の間違いで返す
//! - `applies` は根を探すだけ（[`crate::scan::find_project`]）。設定の有無は見ない
//! - `serve` の試験用の指定: `--socket <パス>`・`--idle-exit <秒>`・`--keep-warm <秒>`・
//!   `--detach`（fork して親はすぐ終わり、子が `setsid` する）

use crate::config::{self, Config};
use crate::scan;
use crate::serve::{self, ServeExit, ServeOptions};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use zc_remote_ipc as ipc;

/// 終了コード。
pub mod exit {
    pub const OK: u8 = 0;
    pub const FAILED: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const OFFLINE: u8 = 3;
    pub const AUTH: u8 = 4;
    pub const LIMIT: u8 = 5;
    pub const ERROR: u8 = 6;
    /// `run`: 置いた実行ファイルを起動できなかった。
    pub const CANNOT_EXEC: u8 = 126;
    pub const CANCELLED: u8 = 130;
}

const USAGE: &str = "\
使い方:
  zc-remote run   <file> [--offline-ok] [--release] [-- 引数…]   外でビルドして、その場で実行する
  zc-remote build <file> [--release]   ビルドして、置いた実行ファイルのパスを出す
  zc-remote check <file>               型検査（診断を「パス:行:桁: error[番号]: …」で出す）
  zc-remote test  <file>               試験をビルド係で実行する
  zc-remote applies <file>             ビルド係の対象か（終了コード 0 = 対象、1 = 対象でない）
  zc-remote status                     設定・つながるビルド係・走っているジョブ
  zc-remote stop                       serve を終わらせる
  zc-remote serve [--stats]            UNIX ソケットで待つ（ふつうは自動で起動される）
共通: --lang rust|c|node|python|shell|cpp|go|ruby|zig|moonbit（省略すると拡張子から）  --stats  --version  --help
設定: ~/.config/zcode/remote.json と remote-token.json（権限 0600）
終了コード: 0 成功 / 1 ビルド・型検査・試験の失敗 / 2 使い方 / 3 オフライン / 4 認証・設定 /
            5 上限 / 6 そのほか / 126 実行できない / 130 取り消し";

/// serve の起動を待つ時間（zcode の専用スレッドは 1 秒。CLI は SD から読む分のゆとりを持つ）。
const SPAWN_WAIT: Duration = Duration::from_secs(3);
/// 取り消しを頼んでから、終わりの知らせを待つ時間。
const CANCEL_WAIT: Duration = Duration::from_secs(3);

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// SIGINT で止まらずに印を付ける（`poll()` は EINTR で戻る）。
fn catch_sigint() {
    // SAFETY: ハンドラは AtomicBool に書くだけ。sa は初期化してから渡す。
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigint as extern "C" fn(libc::c_int) as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
    }
}

fn default_sigint() {
    // SAFETY: 既定の動きに戻すだけ。
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
    }
}

fn say(msg: impl core::fmt::Display) {
    eprintln!("zc-remote: {msg}");
}

fn usage_error(msg: impl core::fmt::Display) -> u8 {
    say(msg);
    eprintln!("{USAGE}");
    exit::USAGE
}

/// 拡張子から、ビルド係での言語の扱いを決める。
pub fn lang_of(file: &Path) -> Option<ipc::RemoteLang> {
    match file.extension()?.to_str()? {
        "rs" => Some(ipc::RemoteLang::Rust),
        "c" | "h" => Some(ipc::RemoteLang::C),
        "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" => Some(ipc::RemoteLang::Node),
        "py" | "pyi" => Some(ipc::RemoteLang::Python),
        "sh" | "bash" => Some(ipc::RemoteLang::Shell),
        "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" => Some(ipc::RemoteLang::Cpp),
        "go" => Some(ipc::RemoteLang::Go),
        "rb" => Some(ipc::RemoteLang::Ruby),
        "zig" => Some(ipc::RemoteLang::Zig),
        "mbt" => Some(ipc::RemoteLang::MoonBit),
        _ => None,
    }
}

/// UNIX ソケットの約束の言語 → ビルド係との約束の言語。
pub fn wire_lang(l: ipc::RemoteLang) -> zc_wire::msg::Lang {
    use zc_wire::msg::Lang;
    match l {
        ipc::RemoteLang::Rust => Lang::Rust,
        ipc::RemoteLang::C => Lang::C,
        ipc::RemoteLang::Node => Lang::Node,
        ipc::RemoteLang::Python => Lang::Python,
        ipc::RemoteLang::Shell => Lang::Shell,
        ipc::RemoteLang::Cpp => Lang::Cpp,
        ipc::RemoteLang::Go => Lang::Go,
        ipc::RemoteLang::Ruby => Lang::Ruby,
        ipc::RemoteLang::Zig => Lang::Zig,
        ipc::RemoteLang::MoonBit => Lang::Moonbit,
    }
}

/// 読み分けたコマンドライン。
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    command: String,
    file: Option<PathBuf>,
    lang: Option<ipc::RemoteLang>,
    release: bool,
    stats: bool,
    offline_ok: bool,
    /// `run` の `--` より後ろ。
    rest: Vec<OsString>,
    // serve 用
    socket: Option<PathBuf>,
    detach: bool,
    idle_exit: Option<Duration>,
    keep_warm: Option<Duration>,
}

fn parse(args: &[OsString]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.iter();
    let secs = |name: &str, v: Option<&OsString>| -> Result<Duration, String> {
        v.and_then(|v| v.to_str()).and_then(|v| v.parse::<f64>().ok()).filter(|s| s.is_finite() && *s >= 0.0 && *s <= 86_400.0).map(Duration::from_secs_f64).ok_or_else(|| format!("{name} は秒数"))
    };
    while let Some(arg) = it.next() {
        let s = arg.to_string_lossy();
        match s.as_ref() {
            "--" => {
                a.rest = it.cloned().collect();
                break;
            }
            "--lang" => {
                let v = it.next().and_then(|v| v.to_str()).and_then(ipc::RemoteLang::parse);
                a.lang = Some(v.ok_or("--lang は rust / c / node / python / shell / cpp / go / ruby / zig / moonbit のどれか")?);
            }
            "--release" => a.release = true,
            "--stats" => a.stats = true,
            "--offline-ok" => a.offline_ok = true,
            "--detach" => a.detach = true,
            "--socket" => a.socket = Some(PathBuf::from(it.next().ok_or("--socket の値が無い")?)),
            "--idle-exit" => a.idle_exit = Some(secs("--idle-exit", it.next())?),
            "--keep-warm" => a.keep_warm = Some(secs("--keep-warm", it.next())?),
            flag if flag.starts_with('-') && flag.len() > 1 => return Err(format!("知らない指定: {flag}")),
            _ if a.command.is_empty() => a.command = s.into_owned(),
            _ if a.file.is_none() => a.file = Some(PathBuf::from(arg)),
            _ if a.command == "test" => return Err("試験の絞り込みはまだ使えない".into()),
            _ => return Err(format!("余分な引数: {s}（実行するプログラムへの引数は -- の後ろに書く）")),
        }
    }
    Ok(a)
}

/// コマンドラインを実行して、終了コードを返す（`run` が成功したときは戻らない）。
pub fn main(args: Vec<OsString>) -> u8 {
    let args = &args[1.min(args.len())..];
    // `run` の `--` より後ろは、実行するプログラムのもの。
    let own = args.iter().position(|a| a == "--").map_or(args, |i| &args[..i]);
    if own.iter().any(|a| a == "--version") {
        println!("{}", crate::CLIENT_NAME);
        return exit::OK;
    }
    if own.iter().any(|a| a == "--help" || a == "-h") || args.is_empty() {
        println!("{USAGE}");
        return if args.is_empty() { exit::USAGE } else { exit::OK };
    }
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => return usage_error(e),
    };
    match a.command.as_str() {
        "build" | "check" | "test" | "run" => job(a),
        "applies" => applies(a),
        "serve" => serve_cmd(a),
        "status" => status(a),
        "stop" => stop(),
        "" => usage_error("コマンドが無い"),
        other => usage_error(format!("知らないコマンド: {other}")),
    }
}

/// 絶対パスにする（`..` やシンボリックリンクは解かない）。
fn absolute(file: &Path) -> PathBuf {
    std::path::absolute(file).unwrap_or_else(|_| file.to_owned())
}

fn applies(a: Args) -> u8 {
    let Some(file) = a.file else { return usage_error("ファイルが無い") };
    let Some(lang) = a.lang.or_else(|| lang_of(&file)) else { return 1 };
    match scan::find_project(&absolute(&file), wire_lang(lang)) {
        Ok(_) => exit::OK,
        Err(_) => 1,
    }
}

fn serve_cmd(a: Args) -> u8 {
    if a.detach {
        // 切り離す: 親はすぐ終わり、子が新しいセッションで続ける（まだスレッドは 1 本）。
        // SAFETY: fork の前後で、ほかのスレッドは無い。
        match unsafe { libc::fork() } {
            -1 => {
                say(format!("fork できない: {}", std::io::Error::last_os_error()));
                return exit::ERROR;
            }
            0 => {
                // SAFETY: 子でセッションを作り直すだけ。
                unsafe { libc::setsid() };
                let _ = std::env::set_current_dir("/");
            }
            _ => return exit::OK,
        }
    }
    // 打鍵を受けるのは zcode。こちらは譲る。
    // SAFETY: 自分の優先度を下げるだけ。
    unsafe { libc::nice(5) };
    // 設定が読めなくても待ち受けは始める（依頼には理由を付けて「設定」で返す）。
    let (cfg, err) = match Config::load() {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(e.to_string())),
    };
    let mut opts = ServeOptions::new(cfg);
    opts.stats = a.stats;
    if let Some(s) = a.socket {
        opts.socket = absolute(&s);
    }
    if let Some(d) = a.idle_exit {
        opts.idle_exit = d;
    }
    if let Some(d) = a.keep_warm {
        opts.keep_warm = d;
    }
    match serve::run_reloading(opts, err) {
        Ok(ServeExit::AlreadyRunning) => {
            if a.stats {
                say("もう動いている");
            }
            exit::OK
        }
        Ok(_) => exit::OK,
        Err(e) => {
            say(format!("serve を始められない: {e}"));
            exit::ERROR
        }
    }
}

// ---- serve との会話 ----

enum Recv {
    Msg(ipc::Response),
    /// 時間切れ・シグナルで戻った。
    Nothing,
    Closed,
}

struct Ipc {
    stream: UnixStream,
    reader: ipc::FrameReader,
    buf: Box<[u8]>,
}

impl Ipc {
    /// つなぐ。`spawn` なら、serve が動いていなければ切り離して起動する。
    fn connect(spawn: bool) -> Result<Ipc, String> {
        let socket = ipc::socket_path();
        let stream = if spawn {
            let program = std::env::current_exe().map_err(|e| format!("自分の実行ファイルが分からない: {e}"))?;
            serve::ensure_running(&program, &socket, true, SPAWN_WAIT).map_err(|e| e.to_string())?
        } else {
            UnixStream::connect(&socket).map_err(|e| e.to_string())?
        };
        let mut c = Ipc { stream, reader: ipc::FrameReader::new(), buf: vec![0u8; 16 << 10].into_boxed_slice() };
        c.send(&ipc::Request::Hello { version: ipc::IPC_VERSION, client: ipc::ClientKind::Cli }).map_err(|e| e.to_string())?;
        Ok(c)
    }

    fn send(&mut self, r: &ipc::Request) -> std::io::Result<()> {
        let mut out = Vec::new();
        ipc::encode_request(r, &mut out);
        self.stream.write_all(&out)
    }

    /// 次の知らせを待つ（最大 `wait`。SIGINT が来たら戻る）。
    fn recv(&mut self, wait: Duration) -> Recv {
        let deadline = Instant::now() + wait;
        loop {
            match self.reader.next_frame() {
                Ok(Some(frame)) => match ipc::decode_response(frame) {
                    Ok(r) => return Recv::Msg(r),
                    Err(ipc::DecodeError::UnknownKind(_)) => continue,
                    Err(_) => return Recv::Closed,
                },
                Ok(None) => {}
                Err(_) => return Recv::Closed,
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || INTERRUPTED.load(Ordering::SeqCst) {
                return Recv::Nothing;
            }
            let mut fd = libc::pollfd { fd: self.stream.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: fd は有効な 1 要素。
            let n = unsafe { libc::poll(&mut fd, 1, left.as_millis().min(60_000) as i32 + 1) };
            if n <= 0 {
                // 時間切れか EINTR。上で見直す。
                continue;
            }
            match self.stream.read(&mut self.buf) {
                // serve が終わった。ECONNRESET も同じ。
                Ok(0) => return Recv::Closed,
                Ok(n) => self.reader.push(&self.buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Recv::Closed,
            }
        }
    }
}

fn phase_text(kind: ipc::JobKind, phase: ipc::Phase, done: u32, total: u32) -> Option<String> {
    Some(match phase {
        ipc::Phase::Scanned => return None,
        ipc::Phase::Connecting => "ビルド係につないでいる".into(),
        ipc::Phase::Uploading => format!("変わったファイルを送っている（{}KB）", total.div_ceil(1024)),
        ipc::Phase::Queued if done > 0 => format!("順番待ち（前に {done}）"),
        ipc::Phase::Queued => "順番待ち".into(),
        ipc::Phase::Running => match kind {
            ipc::JobKind::Build => "ビルド係でビルドしている".into(),
            ipc::JobKind::Check => "ビルド係で型検査している".into(),
            ipc::JobKind::Test => "ビルド係で試験している".into(),
        },
        ipc::Phase::Downloading => format!("実行ファイルを受け取っている（{}KB）", total.div_ceil(1024)),
    })
}

fn backend_text(b: ipc::Backend) -> &'static str {
    match b {
        ipc::Backend::Unknown => "未定",
        ipc::Backend::Pc => "PC",
        ipc::Backend::Cloudflare => "Cloudflare",
    }
}

/// `check` のログの扱い。ビルド係は診断を、構造化した `Diagnostics` と、道具の出力（ログ）の
/// 両方で送ってくる。両方出すと 1 件が 2 回（失敗の知らせも数えると 3 回）並ぶので、診断が
/// 届いたらログは出さない。診断が 1 件も届かなかったとき（道具の出力を読めなかったなど）だけ、
/// 終わりにログをそのまま出す。
#[derive(Default)]
struct CheckLog {
    held: Vec<u8>,
    diags: usize,
    errors: usize,
}

impl CheckLog {
    fn log(&mut self, bytes: &[u8]) {
        self.held.extend_from_slice(bytes);
    }
    fn diags(&mut self, files: &[ipc::FileDiags]) {
        for f in files {
            self.diags += f.diags.len();
            self.errors += f.diags.iter().filter(|d| d.severity == zc_core::Severity::Error).count();
        }
    }
    /// 終わりに出すログ（診断が届いていれば無し）。
    fn leftover(&self) -> Option<&[u8]> {
        (self.diags == 0 && !self.held.is_empty()).then_some(&self.held[..])
    }
    /// 失敗の知らせ。診断が届いていれば件数だけ（中身はもう出してある）。
    fn failed_text(&self, message: &str, tool_exit: i32) -> String {
        if self.errors > 0 {
            format!("型検査でエラーが出た（エラー {} 件）", self.errors)
        } else if message.is_empty() {
            format!("型検査でエラーが出た（終了コード {tool_exit}）")
        } else {
            format!("型検査でエラーが出た: {message}")
        }
    }
}

/// `check` の診断を 1 件 1 行で出す。
fn print_diags(out: &mut impl Write, root: &Path, files: &[ipc::FileDiags]) {
    let cwd = std::env::current_dir().ok();
    for f in files {
        // 今いるフォルダからの相対で出す（端末で短く読めるように）。
        let full = root.join(&f.path);
        let shown = cwd.as_deref().and_then(|c| full.strip_prefix(c).ok()).unwrap_or(&full);
        for d in &f.diags {
            let sev = match d.severity {
                zc_core::Severity::Error => "error",
                zc_core::Severity::Warning => "warning",
            };
            let code = d.code.as_deref().map(|c| format!("[{c}]")).unwrap_or_default();
            let msg = d.message.replace('\n', "\n    ");
            let _ = if f.path.is_empty() {
                writeln!(out, "{sev}{code}: {msg}")
            } else {
                match d.col {
                    Some(col) => writeln!(out, "{}:{}:{}: {sev}{code}: {msg}", shown.display(), d.line + 1, col + 1),
                    None => writeln!(out, "{}:{}: {sev}{code}: {msg}", shown.display(), d.line + 1),
                }
            };
        }
    }
}

fn print_tests(out: &mut impl Write, r: &ipc::TestReport) {
    for c in r.cases.iter().filter(|c| !c.passed) {
        let at = c.location.as_ref().map(|(p, l)| format!(" ({p}:{})", l + 1)).unwrap_or_default();
        let _ = writeln!(out, "FAILED {}{at}", c.name);
        for line in c.message.lines() {
            let _ = writeln!(out, "    {line}");
        }
    }
    let _ = writeln!(out, "試験: 通過 {} / 失敗 {} / 無視 {}", r.passed, r.failed, r.ignored);
}

/// 時刻を「2026-11-17 10:23」の形で（その場所の時刻）。
fn local_time(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as libc::time_t;
    let mut buf = [0u8; 32];
    // SAFETY: tm と buf は有効な書き込み先。書式は NUL 終端。
    let n = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        libc::strftime(buf.as_mut_ptr().cast(), buf.len(), c"%Y-%m-%d %H:%M".as_ptr(), &tm)
    };
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// 置かれた成果物に入れ替わる（成功したら戻らない）。
///
/// 実行ファイルは `execv` で直接起動する（`execvp` は、実行できない形のファイルを `/bin/sh` に
/// 渡してしまう。壊れた成果物をシェルに読ませない）。JS は `PATH` の `llrt` で動かす。
fn exec_artifact(kind: ipc::ArtifactKind, path: &Path, rest: &[OsString]) -> u8 {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let js = kind == ipc::ArtifactKind::Js;
    let mut args: Vec<CString> = Vec::new();
    let mut ok = true;
    let mut push = |b: &[u8]| match CString::new(b) {
        Ok(c) => args.push(c),
        Err(_) => ok = false,
    };
    if js {
        push(b"llrt");
    }
    push(path.as_os_str().as_bytes());
    for a in rest {
        push(a.as_bytes());
    }
    if !ok {
        say("引数に NUL が入っている");
        return exit::USAGE;
    }
    let mut argv: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    default_sigint();
    // SAFETY: argv は NUL 終端の文字列の配列で、最後は null。成功したら戻らない。
    unsafe {
        if js {
            libc::execvp(argv[0], argv.as_ptr());
        } else {
            libc::execv(argv[0], argv.as_ptr());
        }
    }
    let e = std::io::Error::last_os_error();
    say(format!("{} を実行できない: {e}", if js { Path::new("llrt") } else { path }.display()));
    exit::CANNOT_EXEC
}

fn status_code(s: ipc::DoneStatus) -> u8 {
    match s {
        ipc::DoneStatus::Ok => exit::OK,
        ipc::DoneStatus::Failed => exit::FAILED,
        ipc::DoneStatus::Cancelled => exit::CANCELLED,
        ipc::DoneStatus::Offline => exit::OFFLINE,
        ipc::DoneStatus::Auth => exit::AUTH,
        ipc::DoneStatus::Limit => exit::LIMIT,
        ipc::DoneStatus::NotApplicable | ipc::DoneStatus::Clock | ipc::DoneStatus::Error => exit::ERROR,
    }
}

fn status_text(s: ipc::DoneStatus) -> &'static str {
    match s {
        ipc::DoneStatus::Ok => "成功",
        ipc::DoneStatus::Failed => "失敗",
        ipc::DoneStatus::Cancelled => "取り消した",
        ipc::DoneStatus::NotApplicable => "ビルド係の対象ではない",
        ipc::DoneStatus::Offline => "オフライン",
        ipc::DoneStatus::Auth => "認証・設定",
        ipc::DoneStatus::Clock => "時計が合っていない",
        ipc::DoneStatus::Limit => "上限",
        ipc::DoneStatus::Error => "失敗",
    }
}

/// build / check / test / run。
fn job(a: Args) -> u8 {
    let Some(file) = a.file.as_deref() else { return usage_error("ファイルが無い") };
    let file = absolute(file);
    if !file.is_file() {
        say(format!("ファイルが無い: {}", file.display()));
        return exit::USAGE;
    }
    let Some(lang) = a.lang.or_else(|| lang_of(&file)) else {
        return usage_error(format!("言語が分からない（--lang rust|c|node|go|… を付ける）: {}", file.display()));
    };
    let (kind, is_run) = match a.command.as_str() {
        "build" => (ipc::JobKind::Build, false),
        "check" => (ipc::JobKind::Check, false),
        "test" => (ipc::JobKind::Test, false),
        _ => (ipc::JobKind::Build, true),
    };
    if !a.rest.is_empty() && !is_run {
        return usage_error("-- の後ろの引数は run でだけ使える");
    }
    // 設定の読み間違いは、serve に頼む前にここで言う。
    if let Err(e) = Config::load() {
        say(e);
        return exit::AUTH;
    }
    let mut flags = ipc::SubmitFlags::WANT_LOG.0;
    if a.release {
        flags |= ipc::SubmitFlags::RELEASE.0;
    }
    if is_run {
        flags |= ipc::SubmitFlags::ALLOW_STALE.0;
    }

    catch_sigint();
    let started = Instant::now();
    let mut ipc_conn = match Ipc::connect(true) {
        Ok(c) => c,
        Err(e) => {
            say(format!("serve を使えない: {e}"));
            return exit::ERROR;
        }
    };
    let req = ipc::ReqId(1);
    if ipc_conn.send(&ipc::Request::Submit { req, kind, lang, flags: ipc::SubmitFlags(flags), file, overlay: Vec::new() }).is_err() {
        say("serve との接続が切れた");
        return exit::ERROR;
    }

    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut artifact: Option<(ipc::ArtifactKind, bool, PathBuf)> = None;
    let mut last_phase = None;
    let mut cancel_deadline: Option<Instant> = None;
    let mut check_log = CheckLog::default();
    let done = loop {
        if INTERRUPTED.load(Ordering::SeqCst) && cancel_deadline.is_none() {
            // 取り消して、終わりの知らせを少しだけ待つ。
            let _ = ipc_conn.send(&ipc::Request::Cancel { req });
            cancel_deadline = Some(Instant::now() + CANCEL_WAIT);
            INTERRUPTED.store(false, Ordering::SeqCst);
        }
        let wait = match cancel_deadline {
            Some(d) if Instant::now() >= d || INTERRUPTED.load(Ordering::SeqCst) => return exit::CANCELLED,
            Some(d) => d.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(3600),
        };
        match ipc_conn.recv(wait) {
            Recv::Nothing => {}
            Recv::Closed => {
                if cancel_deadline.is_some() {
                    return exit::CANCELLED;
                }
                say("serve との接続が切れた");
                return exit::ERROR;
            }
            Recv::Msg(m) => match m {
                ipc::Response::Hello { version } if version != ipc::IPC_VERSION => {
                    say(format!("動いている serve の版が違う（{version}）。zc-remote stop してからやり直す"));
                    return exit::ERROR;
                }
                ipc::Response::Hello { .. } | ipc::Response::Status { .. } | ipc::Response::LspResult { .. } | ipc::Response::LspState { .. } => {}
                ipc::Response::Phase { phase, done, total, .. } => {
                    if last_phase != Some(phase) {
                        last_phase = Some(phase);
                        if let Some(text) = phase_text(kind, phase, done, total) {
                            if a.stats {
                                say(format!("{text} [{}ms]", started.elapsed().as_millis()));
                            } else {
                                say(text);
                            }
                        }
                    }
                }
                ipc::Response::Log { bytes, .. } => {
                    if kind == ipc::JobKind::Check {
                        check_log.log(&bytes);
                    } else {
                        let _ = stderr.lock().write_all(&bytes);
                    }
                }
                ipc::Response::Diagnostics { root, files, .. } => {
                    if kind == ipc::JobKind::Check {
                        check_log.diags(&files);
                        print_diags(&mut stdout.lock(), &root, &files);
                    }
                }
                ipc::Response::Tests { report, .. } => print_tests(&mut stdout.lock(), &report),
                ipc::Response::Artifact { kind, stale, path, .. } => artifact = Some((kind, stale, path)),
                ipc::Response::FileWritten { path, .. } => say(format!("{} を書き戻した", path.display())),
                ipc::Response::Done { status, exit, backend, elapsed_ms, message, .. } => break (status, exit, backend, elapsed_ms, message),
            },
        }
    };
    let (status, tool_exit, backend, elapsed_ms, message) = done;
    if let Some(log) = check_log.leftover() {
        let _ = stderr.lock().write_all(log);
    }
    if a.stats {
        say(format!("{}（{}・{}ms）", status_text(status), backend_text(backend), elapsed_ms));
    }
    match status {
        ipc::DoneStatus::Ok => match (kind, artifact) {
            (ipc::JobKind::Build, Some((akind, _, path))) => {
                if is_run {
                    return exec_artifact(akind, &path, &a.rest);
                }
                println!("{}", path.display());
                exit::OK
            }
            (ipc::JobKind::Build, None) => {
                say("成功したのに実行ファイルが届いていない");
                exit::ERROR
            }
            _ => exit::OK,
        },
        ipc::DoneStatus::Failed => {
            let what = match kind {
                ipc::JobKind::Build => "ビルドに失敗した",
                ipc::JobKind::Check => "型検査でエラーが出た",
                ipc::JobKind::Test => "試験が失敗した",
            };
            if kind == ipc::JobKind::Check {
                say(check_log.failed_text(&message, tool_exit));
            } else if message.is_empty() {
                say(format!("{what}（終了コード {tool_exit}）"));
            } else {
                say(format!("{what}: {message}"));
            }
            exit::FAILED
        }
        ipc::DoneStatus::Offline => {
            say(format!("オフライン: {message}"));
            match artifact {
                Some((akind, true, path)) if is_run => {
                    let when = std::fs::metadata(&path).and_then(|m| m.modified()).map(local_time).unwrap_or_else(|_| "日時不明".into());
                    if a.offline_ok {
                        say(format!("前の版（{when}）を実行する"));
                        return exec_artifact(akind, &path, &a.rest);
                    }
                    // SAFETY: isatty は fd を見るだけ。
                    if unsafe { libc::isatty(0) } == 1 {
                        eprint!("zc-remote: 前の版（{when}）を実行する？ [y/N] ");
                        let mut answer = String::new();
                        let _ = std::io::stdin().read_line(&mut answer);
                        if matches!(answer.trim(), "y" | "Y" | "yes") {
                            return exec_artifact(akind, &path, &a.rest);
                        }
                    } else {
                        say(format!("前の版（{when}）がある。実行するなら --offline-ok を付ける"));
                    }
                    exit::OFFLINE
                }
                _ => exit::OFFLINE,
            }
        }
        other => {
            if message.is_empty() {
                say(status_text(other));
            } else {
                say(format!("{}: {message}", status_text(other)));
            }
            status_code(other)
        }
    }
}

fn status(_a: Args) -> u8 {
    // 設定（通信しないで分かること）。
    let cfg_path = config::config_path();
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            say(e);
            return exit::AUTH;
        }
    };
    println!("設定: {}{}", cfg_path.display(), if cfg_path.exists() { "" } else { "（無い）" });
    println!("  backend: {:?}", cfg.backend);
    println!("  pc: {}", cfg.pc_addr.as_deref().unwrap_or("（無い）"));
    println!("  cloudflare: {}", cfg.cloudflare_url.as_deref().unwrap_or("（無い）"));
    let token = config::token_path();
    let token_state = match crate::auth::AccessServiceToken::load(&token) {
        Ok(_) => "あり".to_owned(),
        Err(crate::auth::AuthError::Missing) => "無い".to_owned(),
        Err(crate::auth::AuthError::Permissions(mode)) => format!("権限が広い（{mode:o}）。chmod 600 にする"),
        Err(e) => format!("読めない（{e:?}）"),
    };
    println!("  トークン: {}（{token_state}）", token.display());
    if let Some(why) = cfg.unusable() {
        say(why);
        return exit::AUTH;
    }
    // つないでみる（serve が無ければ起動する）。
    let mut c = match Ipc::connect(true) {
        Ok(c) => c,
        Err(e) => {
            say(format!("serve を使えない: {e}"));
            return exit::ERROR;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let _ = c.send(&ipc::Request::Warm { file: cwd });
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut last = (ipc::Backend::Unknown, false, 0u16);
    let mut asked = 0;
    loop {
        match c.recv(Duration::from_millis(150)) {
            Recv::Msg(ipc::Response::Status { backend, connected, running_jobs }) => {
                last = (backend, connected, running_jobs);
                // つながったか、つなぐのをやめた（2 回目以降で未定）なら終わり。
                if connected || (asked > 1 && backend == ipc::Backend::Unknown) {
                    break;
                }
            }
            Recv::Msg(_) => {}
            Recv::Closed => break,
            Recv::Nothing => {
                if Instant::now() >= deadline {
                    break;
                }
                asked += 1;
                let _ = c.send(&ipc::Request::Status);
            }
        }
    }
    let (backend, connected, jobs) = last;
    if connected {
        println!("ビルド係: {}（つながっている）", backend_text(backend));
    } else {
        println!("ビルド係: つながらない");
    }
    println!("走っているジョブ: {jobs}");
    if connected {
        exit::OK
    } else {
        exit::OFFLINE
    }
}

fn stop() -> u8 {
    let Ok(mut c) = Ipc::connect(false) else {
        say("serve は動いていない");
        return exit::OK;
    };
    let _ = c.send(&ipc::Request::Shutdown);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Recv::Closed = c.recv(Duration::from_millis(200)) {
            return exit::OK;
        }
    }
    say("serve が終わらない");
    exit::ERROR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_log_is_dropped_when_diagnostics_arrive() {
        use zc_core::{Diagnostic, Severity};
        // 診断が届いた: ログ（同じ中身の行）は出さず、失敗の知らせは件数だけ
        let mut c = CheckLog::default();
        c.log(b"bad.py:4:10: error: int is not str\n");
        c.diags(&[ipc::FileDiags { path: "bad.py".into(), diags: vec![Diagnostic::at_line(3, Severity::Error, "int is not str"), Diagnostic::at_line(5, Severity::Warning, "w")] }]);
        assert_eq!(c.leftover(), None);
        assert_eq!(c.failed_text("bad.py:4:10: error: int is not str", 1), "型検査でエラーが出た（エラー 1 件）");
        // 警告だけ（成功）でもログは出さない
        let mut c = CheckLog::default();
        c.log(b"t.sh:2:6: warning[SC2086]: quote\n");
        c.diags(&[ipc::FileDiags { path: "t.sh".into(), diags: vec![Diagnostic::at_line(1, Severity::Warning, "quote")] }]);
        assert_eq!(c.leftover(), None);
        // 診断が 1 件も読めなかった: 道具の出力をそのまま出し、知らせは今まで通り
        let mut c = CheckLog::default();
        c.log(b"something odd\n");
        assert_eq!(c.leftover(), Some(&b"something odd\n"[..]));
        assert_eq!(c.failed_text("", 2), "型検査でエラーが出た（終了コード 2）");
        assert_eq!(c.failed_text("odd", 2), "型検査でエラーが出た: odd");
    }

    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_commands_and_flags() {
        let a = parse(&args(&["run", "src/main.rs", "--release", "--", "--version", "x"])).unwrap();
        assert_eq!((a.command.as_str(), a.file.as_deref(), a.release), ("run", Some(Path::new("src/main.rs")), true));
        assert_eq!(a.rest, args(&["--version", "x"]));
        let a = parse(&args(&["--lang", "c", "check", "a.txt", "--stats"])).unwrap();
        assert_eq!((a.command.as_str(), a.lang, a.stats), ("check", Some(ipc::RemoteLang::C), true));
        let a = parse(&args(&["serve", "--socket", "/tmp/s", "--idle-exit", "0.5", "--keep-warm", "2", "--detach"])).unwrap();
        assert_eq!((a.socket.as_deref(), a.idle_exit, a.keep_warm, a.detach), (Some(Path::new("/tmp/s")), Some(Duration::from_millis(500)), Some(Duration::from_secs(2)), true));
        for bad in [&["run", "a.rs", "b.rs"][..], &["test", "a.rs", "filter"], &["run", "--frob"], &["--lang", "cobol", "run", "a"], &["serve", "--idle-exit", "soon"], &["serve", "--socket"]] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn language_from_extension() {
        assert_eq!(lang_of(Path::new("/a/main.rs")), Some(ipc::RemoteLang::Rust));
        assert_eq!(lang_of(Path::new("x.c")), Some(ipc::RemoteLang::C));
        assert_eq!(lang_of(Path::new("x.h")), Some(ipc::RemoteLang::C));
        for f in ["a.js", "a.mjs", "a.cjs", "a.ts", "a.mts", "a.cts"] {
            assert_eq!(lang_of(Path::new(f)), Some(ipc::RemoteLang::Node), "{f}");
        }
        for (f, l) in [
            ("a.py", ipc::RemoteLang::Python),
            ("a.sh", ipc::RemoteLang::Shell),
            ("a.cpp", ipc::RemoteLang::Cpp),
            ("a.hpp", ipc::RemoteLang::Cpp),
            ("a.go", ipc::RemoteLang::Go),
            ("a.rb", ipc::RemoteLang::Ruby),
            ("a.zig", ipc::RemoteLang::Zig),
            ("a.mbt", ipc::RemoteLang::MoonBit),
        ] {
            assert_eq!(lang_of(Path::new(f)), Some(l), "{f}");
        }
        assert_eq!(lang_of(Path::new("a.txt")), None);
        assert_eq!(lang_of(Path::new("Makefile")), None);
    }

    #[test]
    fn diagnostics_print_one_based_positions() {
        let d = |line, col, code: Option<&str>, msg: &str| zc_core::Diagnostic { line, col, end: None, severity: zc_core::Severity::Error, code: code.map(Into::into), message: msg.into() };
        let files = vec![
            ipc::FileDiags { path: "src/main.rs".into(), diags: vec![d(2, Some(17), Some("E0308"), "mismatched types"), d(4, None, None, "a\nb")] },
            ipc::FileDiags { path: String::new(), diags: vec![d(0, None, None, "リンクに失敗した")] },
        ];
        let mut out = Vec::new();
        print_diags(&mut out, Path::new("/nonexistent-zc-root/p"), &files);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "/nonexistent-zc-root/p/src/main.rs:3:18: error[E0308]: mismatched types\n/nonexistent-zc-root/p/src/main.rs:5: error: a\n    b\nerror: リンクに失敗した\n"
        );
    }

    #[test]
    fn exit_codes_follow_the_design() {
        use ipc::DoneStatus as S;
        let codes: Vec<u8> = [S::Ok, S::Failed, S::Cancelled, S::Offline, S::Auth, S::Limit, S::Error, S::NotApplicable, S::Clock].into_iter().map(status_code).collect();
        assert_eq!(codes, [0, 1, 130, 3, 4, 5, 6, 6, 6]);
    }
}
