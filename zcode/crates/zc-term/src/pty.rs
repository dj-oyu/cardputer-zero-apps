//! pty を開き、子プロセスを起動・待機・停止する（Linux 専用）。
//!
//! fork した子では async-signal-safe な呼び出しだけを使う（zcode は複数のスレッドを
//! 持つので、fork 後に malloc などを呼ぶと固まりうる）。引数・環境変数・パスは
//! fork の前に全部 C の文字列にしておく。

use crate::{ExitStatus, TermCommand};
use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 子プロセスの `oom_score_adj`（メモリ不足のとき zcode より先に選ばれるように）。
const OOM_SCORE_ADJ: &[u8] = b"500";

/// `close_range` の「閉じずに CLOEXEC を付ける」指定（Linux 5.11 以降）。
const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;

fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// 起動した子プロセス。
pub(crate) struct Spawned {
    pub master: OwnedFd,
    pub pid: libc::pid_t,
}

/// pty の大きさを設定する（子の前景プロセスグループにはカーネルが SIGWINCH を送る）。
pub(crate) fn set_winsize(fd: RawFd, cols: u16, rows: u16) -> io::Result<()> {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: fd は開いている pty。ws は呼び出しの間有効。
    cvt(unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) })?;
    Ok(())
}

/// pty を開く。master は非ブロッキング・CLOEXEC、slave は CLOEXEC（子で 0〜2 に複製する）。
fn open_pty(cols: u16, rows: u16) -> io::Result<(OwnedFd, OwnedFd)> {
    // SAFETY: ただの open。戻り値は検査する。
    let m = cvt(unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) })?;
    // SAFETY: m は今開いた fd で、ほかに持ち主はいない。
    let master = unsafe { OwnedFd::from_raw_fd(m) };
    // SAFETY: m は pty の master。
    cvt(unsafe { libc::grantpt(m) })?;
    cvt(unsafe { libc::unlockpt(m) })?;
    let mut name = [0 as libc::c_char; 128];
    // SAFETY: name の長さを渡している。
    let r = unsafe { libc::ptsname_r(m, name.as_mut_ptr(), name.len()) };
    if r != 0 {
        return Err(io::Error::from_raw_os_error(r));
    }
    // SAFETY: ptsname_r が NUL 終端の名前を書いた。
    let s = cvt(unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) })?;
    // SAFETY: s は今開いた fd。
    let slave = unsafe { OwnedFd::from_raw_fd(s) };
    // 行編集で UTF-8 の字を 1 字として消せるように IUTF8 を付ける（カーネルの既定には無い）
    // SAFETY: termios は C の構造体で、ゼロ埋めから tcgetattr が埋める。
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(s, &mut t) == 0 {
            t.c_iflag |= libc::IUTF8;
            libc::tcsetattr(s, libc::TCSANOW, &t);
        }
    }
    set_winsize(m, cols, rows)?;
    // SAFETY: fcntl でフラグを足すだけ。
    unsafe {
        let fl = cvt(libc::fcntl(m, libc::F_GETFL))?;
        cvt(libc::fcntl(m, libc::F_SETFL, fl | libc::O_NONBLOCK))?;
    }
    Ok((master, slave))
}

fn cstring(s: impl AsRef<OsStr>) -> io::Result<CString> {
    CString::new(s.as_ref().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "文字列に NUL が入っている"))
}

/// 実行ファイルを `PATH` から探す（`/` を含むならそのまま。相対パスは `cwd` 基準で exec される）。
fn resolve(program: &str, path: Option<&OsStr>) -> io::Result<PathBuf> {
    if program.contains('/') {
        return Ok(PathBuf::from(program));
    }
    let path = path.unwrap_or(OsStr::new("/usr/local/bin:/usr/bin:/bin"));
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let cand = dir.join(program);
        if let Ok(md) = std::fs::metadata(&cand) {
            if md.is_file() && md.permissions().mode() & 0o111 != 0 {
                return Ok(cand);
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("{program}: コマンドが見つからない")))
}

/// 子に渡す環境変数（親の環境から `TERM`・`COLORTERM`・`LINES`・`COLUMNS` を除き、
/// `TERM=xterm-256color` と `cmd.env` を足す）。
fn child_env(cmd: &TermCommand) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| !matches!(k.as_bytes(), b"TERM" | b"COLORTERM" | b"LINES" | b"COLUMNS"))
        .collect();
    env.push(("TERM".into(), "xterm-256color".into()));
    for (k, v) in &cmd.env {
        env.retain(|(ek, _)| ek.as_os_str() != OsStr::new(k));
        env.push((k.into(), v.into()));
    }
    // ロケールが何も無い（C）と、readline などが UTF-8 の字幅を 1 バイト 1 桁と数え、
    // 日本語の行の折り返しが崩れる。何も指定が無ければ文字の扱いだけ C.UTF-8 にする
    let has = |name: &str| env.iter().any(|(k, v)| k.as_os_str() == OsStr::new(name) && !v.is_empty());
    if !has("LC_ALL") && !has("LC_CTYPE") && !has("LANG") {
        env.push(("LC_CTYPE".into(), "C.UTF-8".into()));
    }
    env
}

/// 子プロセスを起動する。exec に失敗したら（見つからない、`cwd` に入れないなど）、
/// そのエラーを返す（CLOEXEC のパイプで子から errno を受け取る）。
pub(crate) fn spawn(cmd: &TermCommand, cols: u16, rows: u16) -> io::Result<Spawned> {
    let env = child_env(cmd);
    let lookup = |name: &str| env.iter().rev().find(|(k, _)| k.as_os_str() == OsStr::new(name)).map(|(_, v)| v.as_os_str());
    let (exe, argv0) = match &cmd.program {
        None => {
            // ログインシェル: argv[0] の先頭に '-' を付ける（bash は -l と同じ扱い）
            let sh = lookup("SHELL").filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/bin/bash"));
            let base = sh.file_name().map(|b| b.to_os_string()).unwrap_or_else(|| "sh".into());
            let mut a = OsString::from("-");
            a.push(base);
            (sh, a)
        }
        Some(p) => (resolve(p, lookup("PATH"))?, OsString::from(p)),
    };
    let exe_c = cstring(&exe)?;
    let mut argv_c = vec![cstring(&argv0)?];
    for a in &cmd.args {
        argv_c.push(cstring(a)?);
    }
    let mut argv: Vec<*const libc::c_char> = argv_c.iter().map(|c| c.as_ptr()).collect();
    argv.push(std::ptr::null());
    let env_c = env
        .iter()
        .map(|(k, v)| {
            let mut kv = k.clone();
            kv.push("=");
            kv.push(v);
            cstring(kv)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let mut envp: Vec<*const libc::c_char> = env_c.iter().map(|c| c.as_ptr()).collect();
    envp.push(std::ptr::null());
    let cwd_c = cmd.cwd.as_deref().map(|p: &Path| cstring(p)).transpose()?;

    let (master, slave) = open_pty(cols, rows)?;
    let mut p = [0 as libc::c_int; 2];
    // SAFETY: p は 2 要素。
    cvt(unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC) })?;
    // SAFETY: どちらも今開いた fd。
    let (err_r, err_w) = unsafe { (OwnedFd::from_raw_fd(p[0]), OwnedFd::from_raw_fd(p[1])) };

    // SAFETY: 子では async-signal-safe な呼び出しだけを使い、必ず exec か _exit で終わる。
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        // SAFETY: fork 直後の子。ポインタはすべて fork 前に作ったもので有効。
        unsafe {
            child(
                slave.as_raw_fd(),
                err_w.as_raw_fd(),
                exe_c.as_ptr(),
                argv.as_ptr(),
                envp.as_ptr(),
                cwd_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            )
        }
    }
    drop(err_w);
    drop(slave);
    // exec まで待つ（成功すれば CLOEXEC でパイプが閉じて 0 バイト）
    let mut buf = [0u8; 4];
    let n = loop {
        // SAFETY: buf の長さを渡している。
        let n = unsafe { libc::read(err_r.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        break n;
    };
    if n == 4 {
        let errno = i32::from_ne_bytes(buf);
        let mut st = 0;
        // SAFETY: 子は _exit 済みか直前。待って回収する。
        unsafe { libc::waitpid(pid, &mut st, 0) };
        let e = io::Error::from_raw_os_error(errno);
        return Err(io::Error::new(e.kind(), format!("{}: {e}", exe.display())));
    }
    Ok(Spawned { master, pid })
}

/// fork した子の処理。戻らない。
///
/// # Safety
/// fork 直後の子からだけ呼ぶ。ポインタは NUL 終端の文字列（と NULL 終端の配列）。
unsafe fn child(
    mut slave: RawFd,
    mut err_fd: RawFd,
    exe: *const libc::c_char,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
    cwd: *const libc::c_char,
) -> ! {
    // シグナルを既定に戻す。zcode は SIGPIPE を無視しており、無視とブロックは exec を越えて
    // 引き継がれる（引き継ぐと `yes | head` が止まらない、Ctrl+C が効かないなど）
    let mut set: libc::sigset_t = std::mem::zeroed();
    libc::sigemptyset(&mut set);
    libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
    for sig in 1..65 {
        if sig != libc::SIGKILL && sig != libc::SIGSTOP {
            libc::signal(sig, libc::SIG_DFL);
        }
    }
    // 0〜2 に複製する前に、slave とエラー用のパイプを 3 以上へ逃がす
    // （zcode の標準入出力が閉じていると、どちらかが 0〜2 になっていることがある）
    if err_fd < 3 {
        err_fd = libc::fcntl(err_fd, libc::F_DUPFD_CLOEXEC, 3);
        if err_fd < 0 {
            libc::_exit(127);
        }
    }
    if slave < 3 {
        slave = libc::fcntl(slave, libc::F_DUPFD_CLOEXEC, 3);
        if slave < 0 {
            fail(err_fd);
        }
    }
    if libc::setsid() < 0 {
        fail(err_fd);
    }
    if libc::ioctl(slave, libc::TIOCSCTTY, 0) < 0 {
        fail(err_fd);
    }
    for fd in 0..3 {
        if libc::dup2(slave, fd) < 0 {
            fail(err_fd);
        }
    }
    if !cwd.is_null() && libc::chdir(cwd) < 0 {
        fail(err_fd);
    }
    let f = libc::open(c"/proc/self/oom_score_adj".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
    if f >= 0 {
        libc::write(f, OOM_SCORE_ADJ.as_ptr().cast(), OOM_SCORE_ADJ.len());
        libc::close(f);
    }
    // zcode の fd（evdev・fb0・eventfd など）を引き継がない。3 以上をすべて exec で閉じる
    // （エラー用のパイプは exec に失敗したときに使うので、閉じずに CLOEXEC にする）
    if libc::syscall(libc::SYS_close_range, 3 as libc::c_uint, libc::c_uint::MAX, CLOSE_RANGE_CLOEXEC) != 0 {
        let mut lim: libc::rlimit = std::mem::zeroed();
        let max = if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) == 0 { lim.rlim_cur.min(65536) as RawFd } else { 1024 };
        for fd in 3..max {
            if fd != err_fd {
                libc::close(fd);
            }
        }
    }
    libc::execve(exe, argv, envp);
    fail(err_fd)
}

/// errno を親へ知らせて終わる。
unsafe fn fail(err_fd: RawFd) -> ! {
    let e = *libc::__errno_location();
    let b = e.to_ne_bytes();
    libc::write(err_fd, b.as_ptr().cast(), b.len());
    libc::_exit(127)
}

fn decode(st: libc::c_int) -> Option<ExitStatus> {
    if libc::WIFEXITED(st) {
        Some(ExitStatus::Code(libc::WEXITSTATUS(st)))
    } else if libc::WIFSIGNALED(st) {
        Some(ExitStatus::Signal(libc::WTERMSIG(st)))
    } else {
        None
    }
}

/// 終わっていれば回収してその終わり方を返す（待たない）。
pub(crate) fn try_wait(pid: libc::pid_t) -> io::Result<Option<ExitStatus>> {
    let mut st = 0;
    loop {
        // SAFETY: 自分の子の pid。
        let r = unsafe { libc::waitpid(pid, &mut st, libc::WNOHANG) };
        if r == 0 {
            return Ok(None);
        }
        if r == pid {
            return Ok(decode(st));
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// 端末の前景プロセスグループ（無ければ `None`）。
pub(crate) fn foreground_pgrp(master: RawFd) -> Option<libc::pid_t> {
    // SAFETY: master は開いている pty。
    let g = unsafe { libc::tcgetpgrp(master) };
    (g > 0).then_some(g)
}

/// プロセスグループにシグナルを送る。
pub(crate) fn kill_group(pgrp: libc::pid_t, sig: libc::c_int) -> io::Result<()> {
    // SAFETY: kill はただのシステムコール。pgrp > 0 を前提に負にしてグループを指す。
    cvt(unsafe { libc::kill(-pgrp, sig) })?;
    Ok(())
}

/// [`crate::Term`] を捨てたあとに子を回収する。少し待っても終わらなければ SIGKILL して、
/// 終わるまで待つ（別スレッドで。メインは待たない）。
pub(crate) fn reap_later(pid: libc::pid_t) {
    let job = move || {
        for _ in 0..40 {
            match try_wait(pid) {
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                _ => return,
            }
        }
        let _ = kill_group(pid, libc::SIGKILL);
        let mut st = 0;
        // SAFETY: 自分の子の pid。終わるまで待つ。
        while unsafe { libc::waitpid(pid, &mut st, 0) } < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {}
    };
    if std::thread::Builder::new().name("zc-term-reap".into()).spawn(job).is_err() {
        // スレッドを作れないときは SIGKILL だけ送る（回収は zcode の終了時）
        let _ = kill_group(pid, libc::SIGKILL);
    }
}
