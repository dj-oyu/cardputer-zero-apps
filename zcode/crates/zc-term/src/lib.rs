//! 統合ターミナル（design.md §9、モック 3・4）。**段階 3 で実装する。ここは公開 API の雛形。**
//!
//! - openpty・fork/exec で子プロセスを作り、`vte` でエスケープシーケンスを解釈して、
//!   画面（[`Cell`] の格子）とスクロールバック（1000 行）を持つ
//! - 読み取りと解釈はメインで行う。pty の fd（[`Term::fd`]）をイベントループの
//!   `poll()` に足し、読めるようになったら [`Term::read`] を呼ぶ。1 回の起床で読む量
//!   には上限（64KB か 2ms）を設け、先に入力を見る（design.md §4.3）
//! - 子プロセスは `oom_score_adj` を上げ、メモリ不足のとき zcode より先に落ちるようにする
//! - 描くのは `zc-ui`。この crate は `zc-font` を知らない（大きさは桁と行で受け取る）

use std::io;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::time::Duration;
use zc_core::{KeyEvent, Style};

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

/// 端末 1 つ（pty・子プロセス・画面）。メインスレッドが持つ。
pub struct Term {
    _private: (),
}

impl Term {
    /// 子プロセスを起動する。`cols`×`rows` は画面の大きさ（マス）。
    pub fn spawn(_cmd: &TermCommand, _cols: u16, _rows: u16) -> io::Result<Term> {
        todo!("段階 3")
    }
    /// pty の master の fd（`poll()` に渡す）。
    pub fn fd(&self) -> RawFd {
        todo!("段階 3")
    }
    /// 読めるぶんを読んで解釈する（上限つき）。
    pub fn read(&mut self, _budget: ReadBudget) -> io::Result<ReadResult> {
        todo!("段階 3")
    }
    /// 打鍵を子プロセスに送る（矢印キーなどはエスケープシーケンスに直す）。
    pub fn send_key(&mut self, _ev: &KeyEvent) -> io::Result<()> {
        todo!("段階 3")
    }
    /// 文字列を子プロセスに送る（SKK の確定文字列、貼り付け）。
    pub fn send_text(&mut self, _text: &str) -> io::Result<()> {
        todo!("段階 3")
    }
    /// 画面の大きさを変える（パネルの開閉・全画面）。
    pub fn resize(&mut self, _cols: u16, _rows: u16) -> io::Result<()> {
        todo!("段階 3")
    }
    /// 画面の大きさ（桁, 行）。
    pub fn size(&self) -> (u16, u16) {
        todo!("段階 3")
    }
    /// 画面の 1 行（スクロール位置を反映済み）。
    pub fn row(&self, _row: u16) -> &[Cell] {
        todo!("段階 3")
    }
    /// カーソルの位置（桁, 行）。隠れていれば `None`。
    pub fn cursor(&self) -> Option<(u16, u16)> {
        todo!("段階 3")
    }
    /// 前回から変わった行（ビット i が i 行目）を取り出して、印を消す。
    pub fn take_dirty_rows(&mut self) -> u64 {
        todo!("段階 3")
    }
    /// スクロールバックをさかのぼる（正で過去へ）。
    pub fn scroll_view(&mut self, _delta: i32) {
        todo!("段階 3")
    }
    /// 子プロセスが終わっていれば、その終わり方（待たない）。
    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        todo!("段階 3")
    }
    /// 子プロセスのグループにシグナルを送る（実行の停止）。
    pub fn signal(&mut self, _signal: i32) -> io::Result<()> {
        todo!("段階 3")
    }
}
