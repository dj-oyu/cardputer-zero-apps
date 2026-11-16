//! ハイライト（design.md §7）。**段階 2 で実装する。ここは公開 API の雛形。**
//!
//! - 規則は `zcode/languages/<言語>/language.toml`（テキスト）。`build.rs` が DFA に
//!   変換して実行ファイルに埋め込む。実行時にはコンパイルもパースもしない
//! - 外には「行 → 色付きの区間（[`Span`]）」の形でしか見せない。色は
//!   `zc_core::Theme` がトークンの種類から決める
//! - 行ごとに「行頭の文脈」（[`LineState`]）を覚え、編集した行から下は、行頭の文脈が
//!   前回と同じになったところで再計算を打ち切る
//! - 見えている行と、編集した行から下の一定量（目安 2,000 行）はメインで同期して
//!   計算する。残りは [`Highlighter::compute_rest`] を batch スレッドで走らせ、
//!   結果を [`Highlighter::apply_rest`] で取り込む（版が古ければ捨てる）
//!
//! この crate は `zc-buffer` を知らない。テキストは `zc_core::LineSource` 越しに読む。

use std::path::Path;
use zc_core::{CancelToken, Cancelled, LineChange, LineSource, Span, Version, Versioned};

/// 言語の番号（[`Registry`] の中での位置）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LanguageId(pub u16);

/// 言語の設定（`language.toml` の、規則以外の部分）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageInfo {
    /// 表示名（ステータスバーに出す）。
    pub name: &'static str,
    /// 拡張子（ドットなし）。
    pub extensions: &'static [&'static str],
    /// 行コメントの記号（Ctrl+/ 用）。
    pub line_comment: Option<&'static str>,
    /// 字下げ 1 段ぶんの文字列。
    pub indent: &'static str,
    /// F5 で実行するコマンド（`{file}` をパスに置き換える）。
    pub run: Option<&'static str>,
    /// 保存時の構文チェックのコマンド（§10.2）。
    pub check: Option<&'static str>,
    /// チェックの出力からエラー行を拾う正規表現。
    pub check_pattern: Option<&'static str>,
}

/// 埋め込んだ言語の一覧。
pub struct Registry {
    _private: (),
}

impl Registry {
    /// 実行ファイルに埋め込んだ言語。
    pub fn builtin() -> &'static Registry {
        static R: Registry = Registry { _private: () };
        &R
    }
    /// 全部の言語。
    pub fn languages(&self) -> impl Iterator<Item = LanguageId> + '_ {
        // 段階 2: build.rs が作った表から
        core::iter::empty()
    }
    /// 言語の設定。
    pub fn info(&self, _id: LanguageId) -> &LanguageInfo {
        todo!("段階 2")
    }
    /// ファイル名（拡張子）と 1 行目（shebang）から言語を決める。
    pub fn detect(&self, _path: &Path, _first_line: &str) -> Option<LanguageId> {
        // 段階 2。それまでは「ハイライトなし」
        None
    }
}

/// 行頭の文脈（文脈スタックを表す番号）。同じ値なら、その行以降の結果は同じ。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct LineState(pub u32);

/// [`Highlighter::update`] の結果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateResult {
    /// 行頭の文脈が確定している行数（先頭からこの行まで）。
    pub valid_lines: u32,
    /// 末尾まで確定したか。`false` なら残りを batch に出す。
    pub complete: bool,
}

/// batch スレッドで計算した「残りの行頭の文脈」。
pub struct RestStates {
    /// 最初の行。
    pub from_line: u32,
    /// `from_line` 行目以降の行頭の文脈。
    pub states: Vec<LineState>,
}

/// 1 つの文書のハイライトの状態（行頭の文脈のキャッシュ）。メインスレッドが持つ。
pub struct Highlighter {
    lang: Option<LanguageId>,
    // 段階 2: 行頭の文脈の列、確定している行数
}

impl Highlighter {
    /// `lang` が `None` なら色を付けない。
    pub fn new(lang: Option<LanguageId>) -> Highlighter {
        Highlighter { lang }
    }
    pub fn language(&self) -> Option<LanguageId> {
        self.lang
    }
    /// 編集を知らせる（その行から下の文脈を無効にする）。
    pub fn edited(&mut self, _change: LineChange) {
        todo!("段階 2")
    }
    /// 無効になった行頭の文脈を、最大 `budget_lines` 行ぶん計算し直す（メインで呼ぶ）。
    /// 行頭の文脈が前回と同じになったら、そこで打ち切る。
    pub fn update(&mut self, _src: &dyn LineSource, _budget_lines: u32) -> UpdateResult {
        todo!("段階 2")
    }
    /// `line` 行目のトークンを `out` に入れる（先に `out` を空にする）。
    /// その行の文脈がまだ確定していなければ、色なしの 1 区間を返す。
    pub fn spans(&self, _line: u32, _text: &str, _out: &mut Vec<Span>) {
        todo!("段階 2")
    }
    /// batch に出す依頼を作る: 確定していない最初の行と、その行頭の文脈。
    /// 全部確定していれば `None`。
    pub fn rest_request(&self) -> Option<(u32, LineState)> {
        todo!("段階 2")
    }
    /// **batch スレッドで呼ぶ。** `from_line` 行目（行頭の文脈は `state`）から末尾までの
    /// 行頭の文脈を計算する。数百行ごとに `cancel` を見て、古ければやめる。
    pub fn compute_rest(
        _lang: LanguageId,
        _src: &dyn LineSource,
        _from_line: u32,
        _state: LineState,
        _cancel: &CancelToken,
    ) -> Result<Versioned<RestStates>, Cancelled> {
        todo!("段階 2")
    }
    /// batch の結果を取り込む。`current` と版が違えば捨てて `false`。
    pub fn apply_rest(&mut self, _rest: Versioned<RestStates>, _current: Version) -> bool {
        todo!("段階 2")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_defaults() {
        assert!(Registry::builtin().detect(Path::new("a.py"), "").is_none());
        assert_eq!(Registry::builtin().languages().count(), 0);
        assert_eq!(Highlighter::new(None).language(), None);
    }
}
