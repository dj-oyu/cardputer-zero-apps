//! SKK 日本語入力（design.md §9、モック 7〜9）。**段階 4 で実装する。ここは公開 API の雛形。**
//!
//! - [`Skk`]: 状態機械（直接入力 / ▽ 読み入力 / ▼ 候補選択 / 辞書登録）。メインで動く。
//!   `KeyEvent` を受けて「確定文字列・未確定の表示・候補一覧」を返す。UI にも
//!   バッファにも依存しない
//! - [`SystemDict`]: システム辞書（`/usr/share/skk/SKK-JISYO.L`、EUC-JP）を mmap して
//!   二分探索する。**探索は assist スレッドで行う**（mmap に触るため）。状態機械は
//!   [`SkkOutput::lookup`] で探索を頼み、結果を [`Skk::candidates_ready`] で受け取る
//! - [`UserDict`]: 個人辞書（`~/.skk-uim-jisyo`、uim-skk と同じ形式・同じ並び）。
//!   起動時に読み、書き戻しは storage スレッドが行う（design.md §8.3: ロックファイル、
//!   5 分おきと終了時、丸ごと書き直し）

use std::io;
use std::path::Path;
use zc_core::{KeyEvent, Version, Versioned};

/// 入力モード（ステータスバー右端に出す）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InputMode {
    /// 英数（起動時）。キーはそのまま通す。
    #[default]
    Ascii,
    /// ひらがな。
    Hiragana,
    /// カタカナ。
    Katakana,
}

/// 未確定文字列の状態。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PreeditState {
    /// 未確定なし（ローマ字の途中だけがありうる）。
    #[default]
    Direct,
    /// ▽ 読みを入力中。
    Reading,
    /// ▼ 候補を選択中。
    Converting,
}

/// 表示用の未確定文字列。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Preedit {
    pub state: PreeditState,
    /// 表示する文字列（▽ / ▼ の記号を含まない）。
    pub text: String,
    /// 送り仮名の始まる位置（`text` のバイト位置）。
    pub okuri_at: Option<usize>,
}

/// 変換候補。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    /// 注釈（辞書の `;` 以降）。
    pub annotation: Option<String>,
}

/// 候補一覧（モック 8）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CandidateList {
    pub items: Vec<Candidate>,
    /// 選んでいる候補。
    pub selected: usize,
    /// いま見せているページの最初の候補（`asdfjkl` で選べるのは 7 件）。
    pub page_start: usize,
}

/// 辞書登録欄（モック 9）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    /// 登録しようとしている読み。
    pub reading: String,
    /// 登録欄の中の確定済みの文字列。
    pub text: String,
}

/// assist スレッドへの辞書探索の依頼。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupRequest {
    /// 読み（送りありなら末尾にローマ字 1 字。例: `"うごk"`）。
    pub reading: String,
    pub okuri: bool,
    /// 依頼の版。結果に付けて返す（読みが変わったら古い結果を捨てる）。
    pub version: Version,
}

/// [`Skk::handle_key`] の結果。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SkkOutput {
    /// SKK がこの打鍵を使ったか。`false` なら、呼ぶ側がふつうのキーとして処理する。
    pub consumed: bool,
    /// 確定した文字列（空なら無し）。エディタ・ターミナルに送る。
    pub commit: String,
    /// 辞書の探索が要るなら、その依頼（assist スレッドに出す）。
    pub lookup: Option<LookupRequest>,
    /// 個人辞書が変わった（学習・登録）。storage への書き戻しの予約に使う。
    pub learned: bool,
}

/// SKK の状態機械。メインスレッドが持つ。
pub struct Skk {
    mode: InputMode,
    preedit: Preedit,
    // 段階 4: ローマ字の途中、候補、登録欄の入れ子、個人辞書
}

impl Skk {
    pub fn new(_user: UserDict) -> Skk {
        Skk { mode: InputMode::Ascii, preedit: Preedit::default() }
    }
    pub fn mode(&self) -> InputMode {
        self.mode
    }
    /// かなモードへ（Ctrl+J）/ 英数へ。
    pub fn set_mode(&mut self, _mode: InputMode) {
        todo!("段階 4")
    }
    /// 打鍵を 1 つ処理する。
    pub fn handle_key(&mut self, _ev: &KeyEvent) -> SkkOutput {
        todo!("段階 4")
    }
    /// 辞書の探索結果を受け取る。版が古ければ捨てて `false`。候補が無ければ
    /// 登録欄が開く。
    pub fn candidates_ready(&mut self, _result: Versioned<Vec<Candidate>>) -> bool {
        todo!("段階 4")
    }
    /// 未確定を捨てる（フォーカスが移ったとき）。
    pub fn reset(&mut self) {
        todo!("段階 4")
    }
    /// 表示用の未確定文字列。
    pub fn preedit(&self) -> &Preedit {
        &self.preedit
    }
    /// 候補一覧（▼ のとき）。
    pub fn candidates(&self) -> Option<&CandidateList> {
        None
    }
    /// 辞書登録欄（開いているとき）。
    pub fn registration(&self) -> Option<&Registration> {
        None
    }
    /// 個人辞書（書き戻し用）。
    pub fn user_dict(&self) -> &UserDict {
        todo!("段階 4")
    }
}

/// システム辞書（mmap、読むだけ）。`Send + Sync`。**メインスレッドで探索しない。**
pub struct SystemDict {
    _private: (),
}

impl SystemDict {
    /// 辞書を mmap する。**assist スレッドで呼ぶ。**
    pub fn open(_path: &Path) -> io::Result<SystemDict> {
        todo!("段階 4")
    }
    /// 辞書全体に `madvise(WILLNEED)` をかける（かなモードに入ったときの先読み）。
    pub fn prefetch(&self) {
        todo!("段階 4")
    }
    /// 読みを探す。**assist スレッドで呼ぶ。**
    pub fn lookup(&self, _reading: &str, _okuri: bool) -> Vec<Candidate> {
        todo!("段階 4")
    }
}

/// 個人辞書（メモリ上に持つ。uim-skk と同じ形式）。
#[derive(Default)]
pub struct UserDict {
    _private: (),
}

impl UserDict {
    /// 空の辞書（ファイルが無いとき）。
    pub fn new() -> UserDict {
        UserDict::default()
    }
    /// EUC-JP のバイト列から読む。壊れた行は読み飛ばす。
    pub fn parse(_euc_jp: &[u8]) -> UserDict {
        todo!("段階 4")
    }
    pub fn lookup(&self, _reading: &str, _okuri: bool) -> Vec<Candidate> {
        todo!("段階 4")
    }
    /// 確定した語を先頭に上げる（無ければ足す）。
    pub fn learn(&mut self, _reading: &str, _okuri: bool, _word: &str) {
        todo!("段階 4")
    }
    /// 前回の [`UserDict::to_bytes`] から変わったか。
    pub fn is_dirty(&self) -> bool {
        todo!("段階 4")
    }
    /// 書き戻す内容（EUC-JP、uim-skk と同じ並び）。storage スレッドがロックファイルを
    /// 取って丸ごと書く。
    pub fn to_bytes(&mut self) -> Vec<u8> {
        todo!("段階 4")
    }
}
