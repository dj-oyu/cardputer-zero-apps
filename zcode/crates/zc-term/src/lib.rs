//! 統合ターミナル（design.md §9、モック 3・4）。
//!
//! - openpty・fork/exec で子プロセスを作り、`vte` でエスケープシーケンスを解釈して、
//!   画面（[`Cell`] の格子）とスクロールバック（1000 行）を持つ
//! - 読み取りと解釈はメインで行う。pty の fd（[`Term::fd`]）をイベントループの
//!   `poll()` に足し、読めるようになったら [`Term::read`] を呼ぶ。1 回の起床で読む量
//!   には上限（64KB か 2ms）を設け、先に入力を見る（design.md §4.3）
//! - 子プロセスは `oom_score_adj` を上げ、メモリ不足のとき zcode より先に落ちるようにする
//! - 描くのは `zc-ui`。この crate は `zc-font` を知らない（大きさは桁と行で受け取る）
//!
//! # 使い方（統合側へ）
//!
//! - `poll()` には [`Term::fd`] を `POLLIN` で入れる。起きたら [`Term::read`] を呼び、
//!   `more` なら次の起床でも続けて読む（入力を先に処理してから）。`closed` が来たら
//!   **fd を `poll()` から外す**（閉じた pty は POLLHUP を出し続けるので空回りする）。
//!   そのあと [`Term::try_wait`] で終わり方を取る（`closed` の直後はまだ終わっていない
//!   ことがあるので、取れるまでタイマーでときどき呼ぶ）
//! - `read` の結果が `changed` なら、[`Term::take_dirty_rows`] の行と [`Term::cursor`] で描く。
//!   [`Term::take_events`] で終了コード（F5 実行）・タイトル・BEL を受け取る
//! - 書き込み（[`Term::send_key`] / [`Term::send_text`]）は待たない。pty が詰まっていれば
//!   内部の列に残し、次の `read` か書き込みで続きを送る
//! - [`Term`] を捨てると子のグループに SIGHUP を送る。すぐ終わらなければ、別スレッドが
//!   少し待ってから SIGKILL して回収する（ゾンビを残さない）
//!
//! # F5 実行の終了コード
//!
//! 独自の OSC `ESC ] 777 ; zcode-exit ; <n> BEL`（ST 終端も可）を受けると
//! [`TermEvent::CommandExit`] を出す。実行は例えば次の形にすると、終了コードを
//! 受け取ったあともシェルが残る:
//!
//! ```text
//! bash -c 'trap : INT; <コマンド>; printf "\e]777;zcode-exit;%d\a" $?; exec bash -i'
//! ```
//!
//! `trap : INT` が無いと、Ctrl+C で止めたときに外側の `bash -c` も SIGINT で終わり、
//! 終了コードもシェルも残らない（`trap '' INT` は子にも無視が引き継がれるので不可）。
//!
//! # 画面の約束
//!
//! - 全角の字は左のマスが `wide: true`。右隣のマスは `' '` の詰め物（同じ見た目）なので、
//!   描くときは飛ばす
//! - 既定の背景は `Style.bg = None`（パネルの地の色で塗る）。反転は `Attrs::REVERSE`
//!   のまま渡す（描く側で前景と背景を入れ替える）。暗い（SGR 2）は文字色を背景へ
//!   半分寄せた色、隠す（SGR 8）は文字色を背景と同じ色にして渡す

use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zc_core::{KeyEvent, Style};

mod keys;
mod pty;
mod screen;

use screen::Screen;

/// 起動するコマンド。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermCommand {
    /// プログラム（`None` ならログインシェル）。
    pub program: Option<String>,
    pub args: Vec<String>,
    /// 作業フォルダ。
    pub cwd: Option<PathBuf>,
    /// 追加の環境変数（`TERM` は自動で入れる）。
    pub env: Vec<(String, String)>,
}

/// 画面の 1 マス。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
    /// 全角の左半分なら `true`（右隣のマスは空）。
    pub wide: bool,
}

/// 1 回の [`Term::read`] で読む量の上限。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadBudget {
    pub max_bytes: usize,
    pub max_time: Duration,
}

impl Default for ReadBudget {
    fn default() -> Self {
        ReadBudget { max_bytes: 64 * 1024, max_time: Duration::from_millis(2) }
    }
}

/// [`Term::read`] の結果。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ReadResult {
    /// 読んだバイト数。
    pub bytes: usize,
    /// 画面のどこかが変わった（描き直しが要る）。変わった行は [`Term::take_dirty_rows`]。
    pub changed: bool,
    /// 上限に達して、まだ読めるものが残っている（次の起床で続きを読む）。
    pub more: bool,
    /// 子プロセス側が閉じた（終了した）。
    pub closed: bool,
}

/// 子プロセスの終わり方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// 終了コード。
    Code(i32),
    /// シグナルで終わった。
    Signal(i32),
}

/// 端末から届いた知らせ（[`Term::take_events`] で取り出す）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    /// F5 実行のコマンドが終わった（独自の OSC `ESC ] 777 ; zcode-exit ; <n> BEL`）。
    /// シェルはまだ生きている。
    CommandExit(i32),
    /// ウィンドウのタイトル（OSC 0 / 2）。
    Title(String),
    /// BEL（続けて鳴ったものは 1 つにまとめる）。
    Bell,
}

/// 1 回の `read(2)` で読む量（時間の上限を細かく見るため、小さめに区切る）。
const CHUNK: usize = 8 * 1024;

/// 送りきれずに溜めておく量の上限。超えた分は捨ててエラーを返す。
const MAX_PENDING: usize = 1 << 20;

/// 端末 1 つ（pty・子プロセス・画面）。メインスレッドが持つ。
pub struct Term {
    master: OwnedFd,
    pid: libc::pid_t,
    status: Option<ExitStatus>,
    closed: bool,
    parser: vte::Parser,
    screen: Screen,
    /// まだ送れていないバイト。
    out: Vec<u8>,
    buf: Box<[u8]>,
}

impl Term {
    /// 子プロセスを起動する。`cols`×`rows` は画面の大きさ（マス）。
    ///
    /// `program` が `None` なら `$SHELL`（無ければ `/bin/bash`）をログインシェルとして
    /// 起動する。環境変数は zcode のものを引き継ぎ、`TERM=xterm-256color` にして
    /// `COLORTERM` を消し、`cmd.env` で上書きする。ロケール（`LC_ALL`・`LC_CTYPE`・`LANG`）が
    /// どれも無ければ `LC_CTYPE=C.UTF-8` を足す（無いと readline が日本語の字幅を数え違える）。
    /// 子は新しいセッションの先頭で、pty が制御端末。zcode の fd は 0〜2 以外すべて閉じる。
    /// プログラムが見つからない・
    /// `cwd` に入れないときはエラーを返す。
    pub fn spawn(cmd: &TermCommand, cols: u16, rows: u16) -> io::Result<Term> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let sp = pty::spawn(cmd, cols, rows)?;
        Ok(Term {
            master: sp.master,
            pid: sp.pid,
            status: None,
            closed: false,
            parser: vte::Parser::new(),
            screen: Screen::new(cols, rows),
            out: Vec::new(),
            buf: vec![0u8; CHUNK].into_boxed_slice(),
        })
    }
    /// pty の master の fd（`poll()` に渡す）。
    pub fn fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }
    /// 読めるぶんを読んで解釈する（上限つき）。待たない。
    ///
    /// 子プロセスが終わって pty が閉じると `closed` を返す（以降も `closed` のまま）。
    /// 新しい出力があれば、さかのぼっていた表示は最新の位置へ戻る。
    pub fn read(&mut self, budget: ReadBudget) -> io::Result<ReadResult> {
        let mut res = ReadResult { closed: self.closed, ..ReadResult::default() };
        self.flush()?;
        if self.closed {
            return Ok(res);
        }
        let start = Instant::now();
        let cursor_before = self.screen.cursor_state();
        let fd = self.master.as_raw_fd();
        loop {
            if res.bytes >= budget.max_bytes {
                res.more = readable(fd);
                break;
            }
            let want = self.buf.len().min(budget.max_bytes - res.bytes);
            // SAFETY: buf は want バイト以上ある。
            let n = unsafe { libc::read(fd, self.buf.as_mut_ptr().cast(), want) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.raw_os_error() {
                    Some(libc::EAGAIN) => break,
                    Some(libc::EINTR) => continue,
                    // slave がすべて閉じた（子が終わった）
                    Some(libc::EIO) => {
                        self.closed = true;
                        break;
                    }
                    _ => return Err(e),
                }
            }
            if n == 0 {
                self.closed = true;
                break;
            }
            let n = n as usize;
            if res.bytes == 0 {
                self.screen.snap_to_bottom();
            }
            self.parser.advance(&mut self.screen, &self.buf[..n]);
            res.bytes += n;
            if start.elapsed() >= budget.max_time {
                res.more = readable(fd);
                break;
            }
        }
        if !self.screen.replies.is_empty() {
            let r = std::mem::take(&mut self.screen.replies);
            self.queue(&r)?;
        }
        if self.closed {
            self.out.clear();
            if self.status.is_none() {
                self.status = pty::try_wait(self.pid).ok().flatten();
            }
        }
        res.closed = self.closed;
        res.changed = self.screen.changed || self.screen.cursor_state() != cursor_before;
        self.screen.changed = false;
        Ok(res)
    }
    /// 打鍵を子プロセスに送る（矢印キーなどはエスケープシーケンスに直す）。
    ///
    /// 子が終わったあとの打鍵は捨てる（エラーにしない）。さかのぼっていた表示は最新へ戻る。
    pub fn send_key(&mut self, ev: &KeyEvent) -> io::Result<()> {
        let mut b = Vec::with_capacity(8);
        keys::encode_key(ev, self.screen.app_cursor, &mut b);
        self.screen.snap_to_bottom();
        self.queue(&b)
    }
    /// 文字列を子プロセスに送る（SKK の確定文字列、貼り付け）。
    ///
    /// 改行は `\r` に直す（端末の貼り付けと同じ）。子が括弧付き貼り付けモード
    /// （`CSI ? 2004 h`。bash の readline や Python の REPL）を有効にしていて、文字列に
    /// 制御文字（改行・タブなど）が入っているときだけ `ESC [ 200 ~` 〜 `ESC [ 201 ~` で
    /// 囲む（貼り付けた改行で実行されないように）。制御文字の無い文字列（SKK の確定）は
    /// そのまま送る。
    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        let mut b = Vec::with_capacity(text.len() + 12);
        let paste = self.screen.bracketed_paste && text.chars().any(|c| c.is_control());
        if paste {
            b.extend_from_slice(b"\x1b[200~");
        }
        let t = text.replace("\r\n", "\r").replace('\n', "\r");
        if paste {
            // 中身で貼り付けを終わらせられないように、終わりの印は取り除く
            b.extend_from_slice(t.replace("\x1b[201~", "").as_bytes());
            b.extend_from_slice(b"\x1b[201~");
        } else {
            b.extend_from_slice(t.as_bytes());
        }
        self.screen.snap_to_bottom();
        self.queue(&b)
    }
    /// 画面の大きさを変える（パネルの開閉・全画面）。
    ///
    /// 折り返しは付け直さない。行が減るときはカーソルより下の空き行から削り、足りなければ
    /// 上の行をスクロールバックへ送る。行が増えるときはスクロールバックから戻す。
    /// 子プロセスにはカーネルが SIGWINCH を送る。
    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if (cols, rows) == self.screen.size() {
            return Ok(());
        }
        self.screen.resize(cols, rows);
        if self.closed {
            return Ok(());
        }
        pty::set_winsize(self.master.as_raw_fd(), cols, rows)
    }
    /// 画面の大きさ（桁, 行）。
    pub fn size(&self) -> (u16, u16) {
        self.screen.size()
    }
    /// 画面の 1 行（スクロール位置を反映済み）。長さは桁数。範囲外の行は空。
    pub fn row(&self, row: u16) -> &[Cell] {
        self.screen.row(row)
    }
    /// カーソルの位置（桁, 行）。隠れていれば `None`（さかのぼって表示しているときも）。
    pub fn cursor(&self) -> Option<(u16, u16)> {
        self.screen.cursor()
    }
    /// 前回から変わった行（ビット i が i 行目）を取り出して、印を消す。
    ///
    /// カーソルが動いた・出た・消えたときは、前の行と今の行も入る。スクロール・大きさの
    /// 変更・さかのぼりの変更は全部の行。64 行目以降が変わったときも全部の行
    /// （`u64::MAX`。行数ぶんのビットで切っていない）。
    pub fn take_dirty_rows(&mut self) -> u64 {
        self.screen.take_dirty_rows()
    }
    /// スクロールバックをさかのぼる（正で過去へ）。最大でスクロールバックの行数まで。
    /// 代替画面（`less`・`top` など）の表示中は何もしない。
    pub fn scroll_view(&mut self, delta: i32) {
        self.screen.scroll_view(delta);
    }
    /// 代替画面（`less`・`vim`・`top` など）を表示中か。
    pub fn alt_screen(&self) -> bool {
        self.screen.alt_screen()
    }
    /// 子プロセスが終わっていれば、その終わり方（待たない）。
    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        if self.status.is_none() {
            self.status = pty::try_wait(self.pid).ok().flatten();
        }
        self.status
    }
    /// 子プロセスのグループにシグナルを送る（実行の停止）。
    ///
    /// 送り先は端末の前景プロセスグループ（Ctrl+C を打ったときにカーネルが送る先と同じ。
    /// 対話シェルのジョブ制御で別グループになったコマンドにも届く）。`SIGTERM`・`SIGKILL`・
    /// `SIGHUP` は、子（シェル）のグループにも送る。子が終わっていれば何もしない。
    pub fn signal(&mut self, signal: i32) -> io::Result<()> {
        if self.try_wait().is_some() {
            return Ok(());
        }
        let fg = if self.closed { None } else { pty::foreground_pgrp(self.master.as_raw_fd()) };
        let fg = fg.unwrap_or(self.pid);
        pty::kill_group(fg, signal)?;
        if fg != self.pid && matches!(signal, libc::SIGTERM | libc::SIGKILL | libc::SIGHUP) {
            let _ = pty::kill_group(self.pid, signal);
        }
        Ok(())
    }
    /// 溜まった知らせ（終了コード・タイトル・BEL）を取り出す。
    pub fn take_events(&mut self) -> Vec<TermEvent> {
        std::mem::take(&mut self.screen.events)
    }

    /// 送る列に足して、送れるだけ送る。
    fn queue(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.closed || bytes.is_empty() {
            return Ok(());
        }
        if self.out.len() + bytes.len() > MAX_PENDING {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "端末への書き込みが詰まっている"));
        }
        self.out.extend_from_slice(bytes);
        self.flush()
    }

    /// 溜まっているバイトを送れるだけ送る（待たない）。
    fn flush(&mut self) -> io::Result<()> {
        let mut done = 0;
        while done < self.out.len() {
            let rest = &self.out[done..];
            // SAFETY: rest の長さを渡している。
            let n = unsafe { libc::write(self.master.as_raw_fd(), rest.as_ptr().cast(), rest.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.raw_os_error() {
                    Some(libc::EAGAIN) => break,
                    Some(libc::EINTR) => continue,
                    Some(libc::EIO) => {
                        // 子が終わった。送れないものは捨てる（closed は read で知らせる）
                        self.out.clear();
                        return Ok(());
                    }
                    _ => {
                        self.out.drain(..done);
                        return Err(e);
                    }
                }
            }
            done += n as usize;
        }
        self.out.drain(..done);
        Ok(())
    }

    /// 子プロセスの pid（試験用）。
    #[cfg(test)]
    fn pid(&self) -> libc::pid_t {
        self.pid
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        if self.try_wait().is_some() {
            return;
        }
        let fg = pty::foreground_pgrp(self.master.as_raw_fd());
        let _ = pty::kill_group(self.pid, libc::SIGHUP);
        let _ = pty::kill_group(self.pid, libc::SIGCONT);
        if let Some(g) = fg.filter(|&g| g != self.pid) {
            let _ = pty::kill_group(g, libc::SIGHUP);
            let _ = pty::kill_group(g, libc::SIGCONT);
        }
        if let Ok(Some(_)) = pty::try_wait(self.pid) {
            return;
        }
        pty::reap_later(self.pid);
    }
}

/// fd にまだ読めるものがあるか（待たない）。
fn readable(fd: RawFd) -> bool {
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: p は 1 要素の配列として渡す。待ち時間 0。
    let r = unsafe { libc::poll(&mut p, 1, 0) };
    r > 0 && p.revents & libc::POLLIN != 0
}

/// 解析の速さを測るための入口（`examples/termbench.rs`）。pty を使わずに画面へ流し込む。
/// 公開 API ではない。
#[doc(hidden)]
pub mod bench {
    use super::{Cell, Screen};

    /// pty の無い端末（解析と画面だけ）。
    pub struct Emulator {
        parser: vte::Parser,
        screen: Screen,
    }

    impl Emulator {
        pub fn new(cols: u16, rows: u16) -> Emulator {
            Emulator { parser: vte::Parser::new(), screen: Screen::new(cols, rows) }
        }
        pub fn feed(&mut self, bytes: &[u8]) {
            self.parser.advance(&mut self.screen, bytes);
            self.screen.replies.clear();
            self.screen.events.clear();
        }
        pub fn row(&self, r: u16) -> &[Cell] {
            self.screen.row(r)
        }
        pub fn scrollback_len(&self) -> usize {
            self.screen.scrollback_len()
        }
    }
}

#[cfg(test)]
mod tests;
