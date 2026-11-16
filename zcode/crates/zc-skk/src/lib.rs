//! SKK 日本語入力（design.md §9、モック 7〜9）。
//!
//! - [`Skk`]: 状態機械（直接入力 / ▽ 読み入力 / ▼ 候補選択 / 辞書登録）。メインで動く。
//!   `KeyEvent` を受けて「確定文字列・未確定の表示・候補一覧」を返す。UI にも
//!   バッファにも依存しない
//! - [`SystemDict`]: システム辞書（`/usr/share/skk/SKK-JISYO.L`、EUC-JP）を mmap して
//!   二分探索する。**探索は assist スレッドで行う**（mmap に触るため）。状態機械は
//!   [`SkkOutput::lookup`] で探索を頼み、結果を [`Skk::candidates_ready`] で受け取る
//! - [`UserDict`]: 個人辞書（`~/.skk-uim-jisyo`、uim-skk と同じ形式・同じ並び）。
//!   起動時に読む。書き戻しは storage スレッドの zc-store が行う（design.md §8.3:
//!   ロックファイル、5 分おきと終了時、読み直して重ねて丸ごと書き直し）
//!
//! # エディタからの使い方
//!
//! 1. 起動時に `~/.skk-uim-jisyo` を読んで [`UserDict::parse`]（無ければ [`UserDict::new`]）、
//!    [`Skk::new`]。システム辞書は assist スレッドで [`SystemDict::open`] する
//!    （開けなくても動く。そのときは探索の依頼に空の結果を返す）
//! 2. エディタ・ターミナルの打鍵は、まず [`Skk::handle_key`] に渡す。返ってきた
//!    [`SkkOutput::commit`] を（`consumed` に関わらず）先に挿入し、`consumed` が `false`
//!    ならその打鍵をふつうのキーとして処理する
//! 3. `lookup` が `Some` なら assist スレッドで [`SystemDict::lookup`] を呼び、結果を
//!    `Versioned::new(req.version, 候補)` にして [`Skk::candidates_ready`] に渡す。
//!    **結果を渡すまで、続く打鍵は `handle_key` に渡さずに取っておき、渡した後に順に流す**
//!    （「Space の直後の a」のような打鍵を正しく扱うため。往復は 1ms 未満）。取っておかずに
//!    渡しても壊れはしない（Skk が取っておいて結果の後に処理する）が、その打鍵で確定した
//!    文字列は次の打鍵の `commit` まで遅れて出る
//! 4. `learned` が `true` なら [`Skk::take_changes`] を `zc_store::DictChange::Learn
//!    { reading, candidate: word }` に直して storage に渡す（5 分おき・終了時にまとめて書く）
//! 5. 入力モードが英数からかなに変わったら（[`Skk::mode`] を見る）、assist で
//!    [`SystemDict::prefetch`] を呼ぶ。フォーカスが移ったら [`Skk::reset`]
//! 6. 表示: [`Skk::preedit`]（本文のカーソル位置。▽ / ▼ は UI が付ける）、
//!    [`Skk::candidates`]（▼ の候補一覧。登録欄の中で変換しているときは、その候補）、
//!    [`Skk::registration`] と [`Skk::registration_preedit`]（登録欄）
//!
//! # キーの扱い（かなモード。英数モードでは Ctrl+J 以外は何も使わない）
//!
//! | キー | 直接入力 | ▽ 読み | ▽ 送り仮名の途中 | ▼ 候補選択 | 辞書登録欄 |
//! |---|---|---|---|---|---|
//! | 英小文字・記号 | ローマ字 → かな。かなにならない字（数字・`/` など）は通す | 読みに足す | 送り仮名に足す。そろったら探索 | 確定してから、その字を直接入力として処理 | 欄の中の SKK へ |
//! | 英大文字 | ▽ を始める | 送り仮名の始まり（読みが空ならふつうの字） | 送り仮名に足す | 確定してから ▽ を始める | 欄の中の SKK へ |
//! | Space | 通す（`z ` は全角空白） | 探索して ▼ | — | 次の候補（最後の次は登録欄） | 欄の中の SKK へ |
//! | `x` | ローマ字（`xa` など） | 読みに足す | 送り仮名に足す | 前の候補（最初なら ▽ へ） | 欄の中の SKK へ |
//! | `a s d f j k l` | ローマ字 | 読みに足す | 送り仮名に足す | 今のページの候補を選んで確定 | 欄の中の SKK へ |
//! | `l` | 英数モードへ | 読みを確定して英数モードへ | 無視 | 確定して英数モードへ | 欄の中の SKK へ |
//! | `q` | ひらがな ⇔ カタカナ | 読みを反対のかなで確定 | 無視 | 確定してモード切替 | 欄の中の SKK へ |
//! | `Q` | 空の ▽ を始める | 無視 | 無視 | 確定して空の ▽ | 欄の中の SKK へ |
//! | `L` | 無視（全角英数モードは無い） | 無視 | 無視 | 確定 | 欄の中の SKK へ |
//! | Enter | 通す | 読みを確定（改行しない） | 読み＋送り仮名を確定 | 確定（改行しない） | 欄の中で確定、何も無ければ登録して本文に確定 |
//! | Ctrl+J | 何もしない（かなモードのまま） | Enter と同じ | Enter と同じ | Enter と同じ | 欄の中の SKK へ |
//! | Ctrl+G / Esc | ローマ字の途中を捨てる。無ければ通す | ▽ を捨てる | ▽ を捨てる | ▽ に戻る | 欄の中で取り消し、何も無ければ登録をやめて ▼（候補が無ければ ▽）へ |
//! | Backspace | ローマ字の途中を消す。無ければ通す | ローマ字・読みを 1 字消す（空の ▽ で消すと ▽ も消える） | 送り仮名をやめて ▽ へ | ▽ に戻る | 欄の中で消す、何も無ければ欄の字を消す |
//! | ほかのキー（Tab・矢印など） | 未確定のローマ字を片づけて通す | 読みを確定して通す | 確定して通す | 確定して通す | 使わない（Ctrl / Alt つきは通す） |
//! | Ctrl / Alt つき（Ctrl+S など） | 通す | 確定して通す | 確定して通す | 確定して通す | 通す（欄は開いたまま） |
//!
//! 「通す」は `consumed: false`。確定した語は個人辞書の先頭に上がる（学習）。
//!
//! # 本物の SKK との違い
//!
//! - 候補一覧は最初の Space から出す（モック 8）。Space / `x` は 1 候補ずつ動き、
//!   ページは選んでいる候補に合わせて変わる
//! - 全角英数モード（`L`）、abbrev モード（`/`）、数値変換（`#`）、接頭辞・接尾辞（`>`）、
//!   候補の削除（`X`）、送り仮名ごとの候補（`[く/書/]` の塊）は無い
//! - Lisp の式の候補は `(concat "…")` だけ展開し、ほかは出さない
//! - ▼ で Backspace は ▽ に戻る（ddskk は確定して 1 字消す）。Esc は Ctrl+G と同じ
//! - 登録欄の入力は、いつもひらがなモードで始まる。登録欄の中でさらに登録欄を開ける
//!   （入れ子）。表示するのはいちばん内側の登録欄

mod dict;
mod engine;
mod romaji;
#[cfg(test)]
mod tests;

pub use dict::{SystemDict, UserDict};
pub use romaji::{to_hiragana, to_katakana};

use engine::{classify, Ctx, Engine, K};
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
    /// ▼ 候補を選択中（辞書登録欄が開いている間も、本文の側はこれ）。
    Converting,
}

/// 表示用の未確定文字列。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Preedit {
    pub state: PreeditState,
    /// 表示する文字列（▽ / ▼ の記号を含まない）。▽ では読み（カタカナモードならカタカナ）と
    /// 未確定のローマ字、▼ では選んでいる候補と送り仮名。
    pub text: String,
    /// 送り仮名の始まる位置（`text` のバイト位置）。▽ で送り仮名を入力中か、送りありの ▼。
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
    /// 登録しようとしている読み（ひらがな。送りありは `か*く` のように `*` の後ろが送り仮名）。
    pub reading: String,
    /// 登録欄の中の確定済みの文字列（未確定の部分は [`Skk::registration_preedit`]）。
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
    /// SKK がこの打鍵を使ったか。`false` なら、呼ぶ側がふつうのキーとして処理する
    /// （`commit` を入れた後で）。
    pub consumed: bool,
    /// 確定した文字列（空なら無し）。エディタ・ターミナルに送る。
    pub commit: String,
    /// 辞書の探索が要るなら、その依頼（assist スレッドに出す）。
    pub lookup: Option<LookupRequest>,
    /// 個人辞書が変わった（学習・登録）。storage への書き戻しの予約に使う。
    /// 中身は [`Skk::take_changes`]。
    pub learned: bool,
}

/// 個人辞書への変更 1 つ（学習・登録）。エディタが `zc_store::DictChange::Learn
/// { reading, candidate: word }` に直して storage に渡す。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LearnedWord {
    /// 読み。送りありなら `おくr` のように最後が英小文字 1 字。
    pub reading: String,
    /// 辞書に書く形の候補（注釈なし。`/` や `;` を含む語は `(concat "…")` に包んだ形）。
    pub word: String,
}

/// SKK の状態機械。メインスレッドが持つ。
pub struct Skk {
    engine: Engine,
    user: UserDict,
    next_version: u64,
    changes: Vec<LearnedWord>,
    /// 結果待ちの間に届いた打鍵（結果が来たら順に処理する）。
    queued: Vec<KeyEvent>,
    /// 取っておいた打鍵を処理して出た確定文字列など（次の `handle_key` で返す）。
    deferred: SkkOutput,
}

impl Skk {
    pub fn new(user: UserDict) -> Skk {
        Skk {
            engine: Engine::new(InputMode::Ascii),
            user,
            next_version: 0,
            changes: Vec::new(),
            queued: Vec::new(),
            deferred: SkkOutput::default(),
        }
    }

    /// 入力モード（登録欄が開いていれば、その欄のモード）。
    pub fn mode(&self) -> InputMode {
        self.engine.active().mode
    }

    /// かなモードへ（Ctrl+J）/ 英数へ。英数にすると未確定の文字列は捨てる
    /// （ひらがな ⇔ カタカナでは残す）。登録欄が開いていれば、その欄のモードを変える。
    pub fn set_mode(&mut self, mode: InputMode) {
        let e = self.engine.active_mut();
        if mode == InputMode::Ascii && e.mode != InputMode::Ascii {
            *e = Engine::new(InputMode::Ascii);
            self.queued.clear();
        }
        e.mode = mode;
        self.engine.refresh();
    }

    /// 打鍵を 1 つ処理する。
    pub fn handle_key(&mut self, ev: &KeyEvent) -> SkkOutput {
        let mut out = self.handle_raw(ev);
        let d = std::mem::take(&mut self.deferred);
        if !d.commit.is_empty() {
            out.commit = d.commit + &out.commit;
        }
        if out.lookup.is_none() {
            out.lookup = d.lookup;
        }
        out.learned |= d.learned;
        out
    }

    fn handle_raw(&mut self, ev: &KeyEvent) -> SkkOutput {
        let k = classify(ev);
        if self.engine.waiting() {
            return match k {
                K::CtrlG | K::Esc => {
                    self.engine.cancel_wait();
                    self.queued.clear();
                    self.engine.refresh();
                    SkkOutput { consumed: true, ..Default::default() }
                }
                K::Other { .. } => SkkOutput::default(),
                _ => {
                    self.queued.push(*ev);
                    SkkOutput { consumed: true, ..Default::default() }
                }
            };
        }
        let mut cx = Ctx { user: &mut self.user, next_version: &mut self.next_version, changes: &mut self.changes, learned: false };
        let mut out = self.engine.handle(ev, k, &mut cx);
        out.learned |= cx.learned;
        self.engine.refresh();
        out
    }

    /// 辞書の探索結果を受け取る。版が古ければ捨てて `false`。個人辞書の候補を先に並べ、
    /// 同じ候補は 1 つにまとめる。候補が無ければ登録欄が開く。
    pub fn candidates_ready(&mut self, result: Versioned<Vec<Candidate>>) -> bool {
        if !self.engine.deliver(result.version, result.value, &self.user) {
            return false;
        }
        // 結果待ちの間に届いた打鍵を処理する（出たものは次の handle_key で返す）
        for ev in std::mem::take(&mut self.queued) {
            let o = self.handle_raw(&ev);
            self.deferred.commit.push_str(&o.commit);
            if o.lookup.is_some() {
                self.deferred.lookup = o.lookup;
            }
            self.deferred.learned |= o.learned;
        }
        self.engine.refresh();
        true
    }

    /// [`Skk::handle_key`] と同じだが、探索の依頼をその場で `dict` に引いて結果を渡す
    /// （試験用。システム辞書が無いときは `None` で、個人辞書だけで変換する。**実機の
    /// エディタでは `Some` を渡さない**: mmap にメインで触ることになる）。
    pub fn handle_key_with(&mut self, ev: &KeyEvent, dict: Option<&SystemDict>) -> SkkOutput {
        let mut out = self.handle_key(ev);
        while let Some(req) = out.lookup.take() {
            let sys = dict.map(|d| d.lookup(&req.reading, req.okuri)).unwrap_or_default();
            self.candidates_ready(Versioned::new(req.version, sys));
            let d = std::mem::take(&mut self.deferred);
            out.commit.push_str(&d.commit);
            out.lookup = d.lookup;
            out.learned |= d.learned;
        }
        out
    }

    /// 未確定を捨てる（フォーカスが移ったとき）。登録欄も閉じる。モードはそのまま。
    /// 出ている探索の結果は、後で届いても捨てる。
    pub fn reset(&mut self) {
        self.engine = Engine::new(self.engine.mode);
        self.queued.clear();
        self.deferred = SkkOutput::default();
        self.engine.refresh();
    }

    /// 表示用の未確定文字列（本文の側。登録欄の中は [`Skk::registration_preedit`]）。
    pub fn preedit(&self) -> &Preedit {
        &self.engine.preedit
    }

    /// 候補一覧（▼ のとき。登録欄の中で変換しているときは、その候補）。
    pub fn candidates(&self) -> Option<&CandidateList> {
        self.engine.candidates()
    }

    /// 辞書登録欄（開いているとき。入れ子なら、いちばん内側）。
    pub fn registration(&self) -> Option<&Registration> {
        self.engine.innermost_reg().map(|r| &r.view)
    }

    /// 辞書登録欄の中の未確定文字列（[`Skk::registration`] と同じ欄）。
    pub fn registration_preedit(&self) -> Option<&Preedit> {
        self.engine.innermost_reg().map(|r| &r.inner.preedit)
    }

    /// 個人辞書（学習した分を含む）。
    pub fn user_dict(&self) -> &UserDict {
        &self.user
    }

    /// 前回から後に学習・登録した語（古い順）。エディタが zc-store に渡す。
    pub fn take_changes(&mut self) -> Vec<LearnedWord> {
        std::mem::take(&mut self.changes)
    }
}
