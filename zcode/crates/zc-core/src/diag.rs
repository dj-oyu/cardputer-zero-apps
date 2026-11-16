//! 診断（波線・ステータスバー・問題一覧）の形（docs/stage3-design.md §9）。
//!
//! 診断を作る側は 2 つある: 実機の構文チェック（`zc-intel` の `run_check`）と、ビルド係の
//! check / build / test（`zc-remote` 経由）。どちらも [`DiagnosticSet`] 1 つにまとめて
//! メインへ届け、置き換えは **出どころ（[`DiagSource`]）ごと** に行う。
//!
//! - 行・桁は 0 始まり。桁は行の中の UTF-8 のバイト位置（[`crate::Span`] と同じ）
//! - パスはプロジェクトの根からの相対・`/` 区切り（実機の構文チェックは、そのファイルの
//!   絶対パスでもよい。照合は統合側が行う）
//! - 版（[`FileDiagnostics::base`]）は「診断の位置が指している中身の版」。届くまでの
//!   編集は、タブ側の記録（`zc_intel::ShiftLog`）でずらしてから付ける

use crate::Version;
use std::path::Path;
use std::sync::Arc;

/// 診断の重さ。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
}

/// 診断 1 件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// 0 始まりの行。
    pub line: u32,
    /// 桁（分かれば。0 始まり、行の中の UTF-8 のバイト位置）。
    pub col: Option<u32>,
    /// 範囲の終わり（行, 桁）。分かれば。終わりは含まない。
    pub end: Option<(u32, u32)>,
    pub severity: Severity,
    /// 診断の番号（`E0308`・`TS2322` など）。
    pub code: Option<Box<str>>,
    pub message: String,
}

impl Diagnostic {
    /// 行だけ分かっている診断（桁・範囲・番号なし）。
    pub fn at_line(line: u32, severity: Severity, message: impl Into<String>) -> Diagnostic {
        Diagnostic { line, col: None, end: None, severity, code: None, message: message.into() }
    }
}

/// 1 ファイルぶんの診断。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiagnostics {
    /// プロジェクトの根からの相対・`/` 区切り（[`DiagSource::Syntax`] は絶対パスでもよい）。
    pub path: Arc<Path>,
    /// 診断の位置が指している中身の版。依頼したときにそのファイルを開いていなければ
    /// `None`（ディスクの中身に対する位置。あとで開いたら、そのまま付ける）。
    pub base: Option<Version>,
    /// 行の順に並んでいる。
    pub diags: Vec<Diagnostic>,
}

/// 診断の出どころ。置き換えの単位。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagSource {
    /// 実機の構文チェック（`language.toml` の `check`）。1 ファイルだけを見る。
    Syntax,
    /// ビルド係の check / build / test（プロジェクト全体を見る）。
    Remote,
}

/// 1 回の依頼の結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticSet {
    pub source: DiagSource,
    /// `true` のときだけ、「同じ出どころで、今回出てこなかったファイルの診断」を消す。
    /// 取り消し・失敗・途中で切れた結果は `false`（波線を消さない）。
    pub complete: bool,
    pub files: Vec<FileDiagnostics>,
}
