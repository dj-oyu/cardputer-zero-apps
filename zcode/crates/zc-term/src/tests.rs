//! 本物の pty と子プロセスを使う試験（開発コンテナで回す。実機は要らない）。

use super::*;
use std::collections::BTreeSet;
use zc_core::{palette, Attrs, Color, Key, Mods};

fn cmd(program: &str, args: &[&str]) -> TermCommand {
    TermCommand { program: Some(program.into()), args: args.iter().map(|s| s.to_string()).collect(), cwd: None, env: vec![] }
}

fn sh(script: &str, cols: u16, rows: u16) -> Term {
    Term::spawn(&cmd("/bin/sh", &["-c", script]), cols, rows).expect("spawn")
}

fn bash(script: &str, cols: u16, rows: u16) -> Term {
    Term::spawn(&cmd("bash", &["-c", script]), cols, rows).expect("spawn")
}

fn wait_fd(fd: RawFd, ms: i32) {
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    unsafe { libc::poll(&mut p, 1, ms) };
}

/// `until` が真になるまで読み続ける（最大 `secs` 秒）。
fn pump(t: &mut Term, secs: u64, mut until: impl FnMut(&mut Term) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if until(t) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        let r = t.read(ReadBudget::default()).expect("read");
        if r.closed {
            return until(t);
        }
        if r.bytes == 0 {
            wait_fd(t.fd(), 20);
        }
    }
}

/// 子が終わって pty が閉じるまで読む。
fn finish(t: &mut Term) {
    assert!(pump(t, 10, |t| t.closed), "子が終わらない");
}

/// 終わり方が取れるまで待つ。
fn wait_exit(t: &mut Term) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(s) = t.try_wait() {
            return s;
        }
        assert!(Instant::now() < deadline, "終わり方が取れない");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn text(t: &Term, r: u16) -> String {
    let row = t.row(r);
    let mut s = String::new();
    let mut i = 0;
    while i < row.len() {
        s.push(row[i].ch);
        i += if row[i].wide { 2 } else { 1 };
    }
    s.trim_end().to_string()
}

fn lines(t: &Term) -> Vec<String> {
    (0..t.size().1).map(|r| text(t, r)).collect()
}

fn alive(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn plain_output_and_exit_code() {
    let mut t = sh("printf 'hello\\nworld\\n'; exit 3", 20, 5);
    finish(&mut t);
    assert_eq!(&lines(&t)[..3], ["hello", "world", ""]);
    assert_eq!(t.cursor(), Some((0, 2)));
    assert_eq!(wait_exit(&mut t), ExitStatus::Code(3));
    let r = t.read(ReadBudget::default()).unwrap();
    assert!(r.closed && r.bytes == 0);
}

#[test]
fn wide_cjk() {
    let mut t = sh("printf '漢字ab\\n'", 10, 3);
    finish(&mut t);
    let row = t.row(0);
    assert_eq!((row[0].ch, row[0].wide), ('漢', true));
    assert_eq!((row[1].ch, row[1].wide), (' ', false));
    assert_eq!((row[2].ch, row[2].wide), ('字', true));
    assert_eq!(row[4].ch, 'a');
    assert_eq!(row[5].ch, 'b');
    // 右端に入らない全角は次の行へ
    let mut t = sh("printf 'abcdefghi漢'", 10, 3);
    finish(&mut t);
    assert_eq!(text(&t, 0), "abcdefghi");
    assert!(t.row(1)[0].wide);
}

#[test]
fn sgr_colours() {
    let mut t = sh(
        "printf '\\033[31mR\\033[1;32mG\\033[38;5;208mO\\033[48;2;1;2;3mT\\033[0mN\\033[7;4;3mI\\033[0m\\033[2mD\\033[0;95mB'",
        20,
        3,
    );
    finish(&mut t);
    let r = t.row(0);
    assert_eq!(r[0].style, Style::new(Color::hex(0xcd3131)));
    // 太字の 0〜7 は明るい色
    assert_eq!(r[1].style.fg, Color::hex(0x23d18b));
    assert!(r[1].style.attrs.contains(Attrs::BOLD));
    assert_eq!(r[2].style.fg, screen::indexed_color(208));
    assert_eq!(r[3].style.bg, Some(Color::rgb(1, 2, 3)));
    assert_eq!(r[4].style, Style::new(palette::FG));
    assert_eq!(r[4].style.bg, None);
    assert!(r[5].style.attrs.contains(Attrs::REVERSE | Attrs::UNDERLINE | Attrs::ITALIC));
    assert_eq!(r[6].style.fg, palette::FG.mix(palette::BG, 128));
    assert_eq!(r[7].style.fg, Color::hex(0xd670d6));
}

#[test]
fn cursor_moves_and_erase() {
    let mut t = sh(
        "printf 'abcdef\\033[1;3H\\033[K\\033[2;1Hxyz\\033[2;2H\\033[1K\\033[3;5H*\\033[2A+\\033[5G-\\033[3d\\033[3D<'",
        10,
        4,
    );
    finish(&mut t);
    // 1 行目: ab の後ろを消し、2 行上の + と 5 桁目の - は 1 行目
    assert_eq!(text(&t, 0), "ab  -+");
    assert_eq!(text(&t, 1), "  z");
    assert_eq!(text(&t, 2), "  < *");
    // 画面の下半分を消す
    let mut t = sh("printf '1\\n2\\n3\\n4\\033[2;2H\\033[J'", 10, 4);
    finish(&mut t);
    assert_eq!(lines(&t), ["1", "2", "", ""]);
}

#[test]
fn insert_delete_lines() {
    let mut t = sh("printf '1\\n2\\n3\\n4\\033[2;1H\\033[L\\033[4;1H\\033[M'", 10, 4);
    finish(&mut t);
    assert_eq!(lines(&t), ["1", "", "2", ""]);
}

#[test]
fn scroll_region() {
    let mut t = sh("printf '1\\n2\\n3\\n4\\n5\\033[2;4r\\033[4;1H\\nX\\033[r'", 10, 5);
    finish(&mut t);
    assert_eq!(lines(&t), ["1", "3", "4", "X", "5"]);
    // 範囲の上端が画面の上端でなければスクロールバックに入れない
    assert_eq!(t.screen.scrollback_len(), 0);
    // SU / SD・逆改行
    let mut t = sh("printf '1\\n2\\n3\\033[S\\033[1;1H\\033MA'", 10, 3);
    finish(&mut t);
    assert_eq!(lines(&t), ["A", "2", "3"]);
}

#[test]
fn alt_screen_preserves_main() {
    let mut t = sh("printf 'main\\n\\033[?1049h\\033[HALT'; read x; printf '\\033[?1049lback'", 10, 4);
    assert!(pump(&mut t, 5, |t| text(t, 0) == "ALT"));
    assert_eq!(text(&t, 1), "");
    // 代替画面ではスクロールバックをさかのぼらない
    t.scroll_view(5);
    assert_eq!(text(&t, 0), "ALT");
    t.send_text("\n").unwrap();
    finish(&mut t);
    assert_eq!(text(&t, 0), "main");
    assert_eq!(text(&t, 1), "back");
}

#[test]
fn autowrap_edge() {
    let mut t = sh("printf 'abcde\\rX\\n'; printf 'abcdefg'", 5, 4);
    finish(&mut t);
    assert_eq!(lines(&t), ["Xbcde", "abcde", "fg", ""]);
    // 自動折り返しを切ると右端で上書き
    let mut t = sh("printf '\\033[?7labcdefg'", 5, 2);
    finish(&mut t);
    assert_eq!(lines(&t), ["abcdg", ""]);
}

#[test]
fn resize_seen_by_stty() {
    let mut t = sh("stty size; read x; stty size", 40, 10);
    assert!(pump(&mut t, 5, |t| text(t, 0) == "10 40"));
    t.resize(50, 12).unwrap();
    assert_eq!(t.size(), (50, 12));
    t.send_text("\n").unwrap();
    finish(&mut t);
    assert!(lines(&t).iter().any(|l| l == "12 50"), "{:?}", lines(&t));
}

#[test]
fn sigint_stops_sleep() {
    let mut t = sh("sleep 10", 20, 3);
    std::thread::sleep(Duration::from_millis(200));
    let t0 = Instant::now();
    t.signal(libc::SIGINT).unwrap();
    finish(&mut t);
    let st = wait_exit(&mut t);
    assert!(matches!(st, ExitStatus::Signal(libc::SIGINT) | ExitStatus::Code(130)), "{st:?}");
    assert!(t0.elapsed() < Duration::from_secs(3));
}

#[test]
fn ctrl_c_key_stops_sleep() {
    let mut t = sh("sleep 10", 20, 3);
    std::thread::sleep(Duration::from_millis(200));
    t.send_key(&KeyEvent::new(Key::Char('c'), Mods::CTRL)).unwrap();
    finish(&mut t);
    let st = wait_exit(&mut t);
    assert!(matches!(st, ExitStatus::Signal(libc::SIGINT) | ExitStatus::Code(130)), "{st:?}");
}

#[test]
fn command_exit_osc_keeps_shell() {
    let mut t = bash(
        "trap : INT; (exit 7); printf '\\e]777;zcode-exit;%d\\a' $?; printf '\\e]777;zcode-exit;3\\e\\\\\\e]0;my title\\a\\a\\a'; echo done; exec sleep 5",
        30,
        4,
    );
    assert!(pump(&mut t, 5, |t| text(t, 0) == "done"));
    assert_eq!(
        t.take_events(),
        [TermEvent::CommandExit(7), TermEvent::CommandExit(3), TermEvent::Title("my title".into()), TermEvent::Bell]
    );
    assert!(t.take_events().is_empty());
    assert_eq!(t.try_wait(), None, "シェルは生きている");
}

#[test]
fn command_exit_after_sigint() {
    // F5 実行の形: Ctrl+C で止めても終了コードが届き、シェルが残る
    let mut t = bash("trap : INT; sleep 10; printf '\\e]777;zcode-exit;%d\\a' $?; exec sleep 5", 30, 4);
    std::thread::sleep(Duration::from_millis(300));
    t.signal(libc::SIGINT).unwrap();
    let mut evs = Vec::new();
    assert!(pump(&mut t, 5, |t| {
        evs.extend(t.take_events());
        !evs.is_empty()
    }));
    assert_eq!(evs, [TermEvent::CommandExit(130)]);
    assert_eq!(t.try_wait(), None);
}

#[test]
fn scrollback_cap_and_view() {
    let mut t = sh("seq 1 1500", 10, 10);
    finish(&mut t);
    assert_eq!(t.screen.scrollback_len(), 1000);
    assert_eq!(text(&t, 8), "1500");
    assert_eq!(t.cursor(), Some((0, 9)));
    t.take_dirty_rows();
    t.scroll_view(5);
    assert_eq!(t.take_dirty_rows(), (1 << 10) - 1);
    assert_eq!(text(&t, 0), "1487");
    assert_eq!(t.cursor(), None);
    t.scroll_view(100_000);
    // 1501 行（最後は空行）のうち、画面の 10 行の前の 1000 行が残る
    assert_eq!(text(&t, 0), "492");
    t.scroll_view(-100_000);
    assert_eq!(text(&t, 0), "1492");
    t.scroll_view(3);
    // 入力で最新へ戻る
    t.send_key(&KeyEvent::char('a')).unwrap();
    assert_eq!(text(&t, 0), "1492");
    assert_eq!(t.cursor(), Some((0, 9)));
}

#[test]
fn scroll_view_snaps_on_output() {
    let mut t = sh("stty -echo; seq 1 30; read x; echo new", 10, 5);
    assert!(pump(&mut t, 5, |t| text(t, 3) == "30"));
    t.scroll_view(10);
    assert_eq!(text(&t, 0), "17");
    t.send_text("\n").unwrap();
    // send_text でも戻るので、もう一度さかのぼってから出力を待つ
    t.scroll_view(10);
    finish(&mut t);
    assert_eq!(text(&t, 3), "new");
}

#[test]
fn dirty_rows() {
    let mut t = sh("stty -echo; printf a; read x; printf '\\033[4;1Hb'; read y", 10, 5);
    assert!(pump(&mut t, 5, |t| text(t, 0) == "a"));
    // 起動直後は全部
    assert_eq!(t.take_dirty_rows(), 0b11111);
    assert_eq!(t.take_dirty_rows(), 0);
    t.send_text("\n").unwrap();
    assert!(pump(&mut t, 5, |t| text(t, 3) == "b"));
    // 書いた行 3 と、カーソルが居た行 0
    assert_eq!(t.take_dirty_rows(), 0b1001);
    t.resize(12, 5).unwrap();
    assert_eq!(t.take_dirty_rows(), 0b11111);
}

#[test]
fn read_budget_reports_more() {
    let mut t = sh("head -c 300000 /dev/zero | tr '\\0' x", 80, 10);
    let budget = ReadBudget { max_bytes: 4096, max_time: Duration::from_secs(1) };
    let mut total = 0;
    let mut saw_more = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline);
        let r = t.read(budget).unwrap();
        assert!(r.bytes <= 4096);
        total += r.bytes;
        saw_more |= r.more;
        if r.closed {
            break;
        }
        if r.bytes == 0 {
            wait_fd(t.fd(), 20);
        }
    }
    assert!(saw_more);
    assert_eq!(total, 300_000);
    assert_eq!(t.screen.scrollback_len(), 1000);
    // 時間の上限 0 なら、1 回の read(2) ぶん（CHUNK まで）だけ読む
    let mut t = sh("head -c 100000 /dev/zero | tr '\\0' y", 80, 10);
    let budget = ReadBudget { max_bytes: 1 << 20, max_time: Duration::ZERO };
    let deadline = Instant::now() + Duration::from_secs(5);
    let r = loop {
        assert!(Instant::now() < deadline);
        wait_fd(t.fd(), 100);
        let r = t.read(budget).unwrap();
        if r.bytes > 0 {
            break r;
        }
    };
    assert!(r.bytes <= CHUNK);
}

#[test]
fn no_leaked_fds() {
    // CLOEXEC の付いていない fd を開いておく（zcode の evdev などの代わり）
    let leak = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
    assert!(leak >= 0);
    let mut t = sh("exec ls /proc/self/fd", 80, 4);
    finish(&mut t);
    unsafe { libc::close(leak) };
    let fds: BTreeSet<String> = lines(&t).join(" ").split_whitespace().map(str::to_string).collect();
    // 0〜2 と、ls が /proc/self/fd を読むのに開いた 3 だけ
    assert_eq!(fds, ["0", "1", "2", "3"].into_iter().map(String::from).collect(), "{:?}", lines(&t));
}

#[test]
fn no_zombie_after_drop() {
    let t = Term::spawn(&cmd("sleep", &["10"]), 20, 3).unwrap();
    let pid = t.pid();
    drop(t);
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(pid) {
        assert!(Instant::now() < deadline, "回収されていない");
        std::thread::sleep(Duration::from_millis(20));
    }
    // SIGHUP を無視していても、少し待って SIGKILL して回収する
    let t = sh("trap '' HUP; sleep 10", 20, 3);
    std::thread::sleep(Duration::from_millis(200));
    let pid = t.pid();
    drop(t);
    let deadline = Instant::now() + Duration::from_secs(6);
    while alive(pid) {
        assert!(Instant::now() < deadline, "回収されていない");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn environment_cwd_and_oom() {
    let mut c = cmd("/bin/sh", &["-c", "echo \"$TERM/${COLORTERM-unset}/$FOO\"; pwd; cat /proc/self/oom_score_adj"]);
    c.env = vec![("FOO".into(), "bar".into()), ("COLORTERM".into(), "truecolor".into())];
    c.cwd = Some("/tmp".into());
    let mut t = Term::spawn(&c, 40, 5).unwrap();
    finish(&mut t);
    // cmd.env で明示したものは渡す
    assert_eq!(&lines(&t)[..3], ["xterm-256color/truecolor/bar", "/tmp", "500"]);
    let mut c = cmd("/bin/sh", &["-c", "echo \"${COLORTERM-unset}\""]);
    c.env = vec![];
    let mut t = Term::spawn(&c, 40, 5).unwrap();
    finish(&mut t);
    assert_eq!(text(&t, 0), "unset");
}

#[test]
fn login_shell_by_default() {
    let c = TermCommand { program: None, args: vec![], cwd: None, env: vec![("SHELL".into(), "/bin/sh".into())] };
    let mut t = Term::spawn(&c, 40, 6).unwrap();
    t.send_text("echo \"[$0]\"; exit 4\n").unwrap();
    finish(&mut t);
    // 打鍵のエコーがプロンプトより先に出ることがあるので、行末で見る
    assert!(lines(&t).iter().any(|l| l.ends_with("[-sh]")), "{:?}", lines(&t));
    assert_eq!(wait_exit(&mut t), ExitStatus::Code(4));
}

#[test]
fn spawn_errors() {
    let e = Term::spawn(&cmd("no-such-command-zc", &[]), 10, 3).err().unwrap();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
    let mut c = cmd("/bin/sh", &["-c", "true"]);
    c.cwd = Some("/no/such/dir".into());
    let e = Term::spawn(&c, 10, 3).err().unwrap();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
}

#[test]
fn keys_reach_child_in_app_mode() {
    // DECCKM を有効にした子には ESC O A が届く
    let mut t = sh("stty raw -echo; printf '\\033[?1h'; dd bs=1 count=3 2>/dev/null | od -An -c", 40, 4);
    assert!(pump(&mut t, 5, |t| t.screen.app_cursor));
    t.send_key(&KeyEvent::new(Key::Up, Mods::NONE)).unwrap();
    finish(&mut t);
    let s = lines(&t).join(" ");
    assert!(s.contains("033   O   A"), "{s}");
}

#[test]
fn bracketed_paste() {
    let mut t = sh("stty raw -echo; printf '\\033[?2004h'; dd bs=1 count=17 2>/dev/null | od -An -c -w32", 80, 4);
    assert!(pump(&mut t, 5, |t| t.screen.bracketed_paste));
    // 制御文字の無い文字列（SKK の確定）はそのまま、改行入りは囲む
    t.send_text("ab").unwrap();
    t.send_text("c\nd").unwrap();
    finish(&mut t);
    let s = lines(&t).join(" ");
    assert!(s.contains("a   b 033   [   2   0   0   ~   c  \\r   d 033   [   2   0   1   ~"), "{s}");
}

#[test]
fn utf8_split_across_reads() {
    // 字の途中で読みが切れても化けない
    let mut t = sh("printf '\\346\\274'; sleep 0.2; printf '\\242ok'", 10, 2);
    finish(&mut t);
    assert_eq!(text(&t, 0), "漢ok");
}
