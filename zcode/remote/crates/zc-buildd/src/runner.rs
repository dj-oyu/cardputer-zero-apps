//! ジョブの実行の口（docs/stage3-design.md §4.3）。形は雛形で確定。
//!
//! 台帳（[`crate::jobs`]）は [`Runner::run`] を、ジョブごとのスレッドで呼ぶ。実行する側は
//! 道具を [`spawn_tool`] で起動し、出力を [`JobOutput`] に流し、終わり方を返す。
//!
//! 言語ごとの中身: [`rust`]（課題 B3）、[`c`]・[`node`]（課題 B4）。

pub mod c;
pub mod node;
pub mod rust;

use crate::pack::Packed;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use zc_wire::msg::{FileDiags, JobOpts, Kind, Lang, Status, TestCase};

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

/// 道具に渡す環境変数を絞る: `PATH`・`HOME`・`LANG`・`CARGO_HOME`・`RUSTUP_HOME`・
/// `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER`・`RUSTC_WRAPPER`・`SCCACHE_*`・
/// `npm_config_cache` だけを残し、あとは消す（受付の環境に何かあっても、`build.rs` や
/// `postinstall` に渡さない）。`CARGO_TERM_COLOR=never`・`NO_COLOR=1`・`CI=1` を足す。
pub fn tool_env(cmd: &mut Command) {
    let _ = cmd;
    todo!("B3")
}

/// 道具を起動して、終わるまで待つ。標準出力と標準エラーを 1 行ずつ `on_stdout` /
/// [`JobOutput::log`] に流す（`on_stdout` が `false` を返した行はログに出さない =
/// cargo の JSON の行）。新しいプロセスグループで起動し、[`JobOutput::set_process_group`]
/// で台帳に知らせる。標準入力は `/dev/null`。戻り値は終了コード（シグナルで終わったら
/// 128 + 番号）。
pub fn spawn_tool(cmd: Command, out: &dyn JobOutput, on_stdout: &mut dyn FnMut(&[u8]) -> bool) -> std::io::Result<i32> {
    let _ = (cmd, out, on_stdout);
    todo!("B3")
}

/// 道具の場所を `PATH` から探す（無ければ [`JobFailure`]: 「cargo が無い」）。
pub fn which(name: &str) -> Result<PathBuf, JobFailure> {
    let _ = name;
    todo!("B3")
}
