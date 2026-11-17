//! エディタ画面（段階 2）。タブ・ファイルツリー・エディタ本体・ステータスバーを
//! 並べ、打鍵を操作（`Command`）に直して文書に渡す。
//!
//! ```text
//!  打鍵 → commands::lookup → exec ─┬→ 文書（TextDoc = zc-buffer）を編集
//!                                  │    └ 変わった行 → ハイライトとビューに知らせる
//!                                  ├→ ツリー / タブ / 入力欄
//!                                  └→ 保存: Saver に依頼（storage スレッド）
//!  描画: TabBar / FileTree / EditorView / StatusBar がそれぞれ変わった所だけ描く
//! ```
//!
//! - 文書は [`TextDoc`]（本番は `zc_buffer::Buffer`）。ファイルを開くのは batch
//!   スレッド、保存は [`Saver`]（storage スレッド）。メインでは読み書きしない
//! - ステータスバーの行・桁は、打鍵の 50ms 後にまとめて更新する（§5.2）
//! - カーソルの点滅は、最後の打鍵から約 10 秒で止める（点いたまま。待機中の CPU を 0% に）

use crate::app::{AppEvent, LoopCx, Screen, TimerId};
use crate::commands;
use crate::doc::{DocBackend, TextDoc};
use crate::intel;
use crate::save::{SaveOk, SaveRequest, Saver, StoreSaver};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use zc_core::time::now_us;
use zc_core::{palette, CancelToken, Color, Command, Damage, DocId, Key, KeyEvent, LineChange, Mods, Motion, Pos, Rect, Span, Version};
use zc_font::TextStyle;
use zc_highlight::{Highlighter, LanguageId, LanguageInfo, Registry};
use zc_intel::{Completion, CompletionKind, Diagnostic, Severity, WordIndex};
use zc_input::{Keyboard, RawRecord};
use zc_ui::layout::{Regions, SIDE_W, UI_PX};
use std::sync::{Arc, OnceLock};
use zc_skk::{InputMode, LearnedWord, Preedit, PreeditState, Skk, SystemDict, UserDict};
use zc_term::{ReadBudget, Term, TermCommand, TermEvent};
use zc_markdown::MdDocument;
use zc_ui::{CandidatePopup, CandidateRow, CodeLayouter, MdLayouter, MiniBuffer, PreeditStyle, PreeditView, MINI_H};
use zc_ui::{CompletionPopup, EditorStatus, EditorView, FileTree, ImeMode, PickerList, PopupItem, RenderCx, SpanSource, StatusBar, Tab, TabBar, TerminalPanel, Widget, TERM_PANEL_H};

/// ステータスバーを遅らせる時間（§5.2）。
const STATUS_DELAY: Duration = Duration::from_millis(50);
/// カーソルの点滅の半周期。
const BLINK: Duration = Duration::from_millis(530);
/// 最後の打鍵のあと、点滅を続ける回数（点く・消えるを 1 回ずつ数える）。
const BLINKS_AFTER_KEY: u32 = 20;
/// メインで同期してハイライトする行数の上限（§7.3）。
const HL_BUDGET: u32 = 2000;
/// エラーを知らせるときのステータスバーの色（モック 4）。
const STATUS_ERROR: Color = Color::hex(0xa1260d);
/// 実行が終了コード 0 で終わったときのステータスバーの色（モック 4 の「緑系」）。
const STATUS_OK: Color = Color::hex(0x16825d);
/// SKK のシステム辞書（Debian の skkdic。uim-skk と同じもの）。
const SKK_SYSTEM_DICT: &str = "/usr/share/skk/SKK-JISYO.L";
/// システム辞書の場所（環境変数 `ZCODE_SKK_DICT` があればそちら。試験で小さな辞書を使う）。
fn skk_system_dict_path() -> std::path::PathBuf {
    std::env::var_os("ZCODE_SKK_DICT").map(std::path::PathBuf::from).unwrap_or_else(|| SKK_SYSTEM_DICT.into())
}

/// 学習した語をためてから個人辞書に書くまでの時間（SD の書き込みを減らす。§8.3）。
const SKK_FLUSH: Duration = Duration::from_secs(300);
/// 候補一覧で直接選ぶキー（1 ページ 7 件）。
const SKK_KEYS: [char; 7] = ['a', 's', 'd', 'f', 'j', 'k', 'l'];

/// 個人辞書 `~/.skk-uim-jisyo` を読む（数 KB なので起動時にそのまま読む。無ければ空）。
fn load_skk_user_dict() -> UserDict {
    std::env::var_os("HOME")
        .and_then(|h| std::fs::read(Path::new(&h).join(".skk-uim-jisyo")).ok())
        .map(|b| UserDict::parse(&b))
        .unwrap_or_default()
}

/// SKK の未確定 → 行に差し込んで見せる形（モック 7・8）。▽ の送り仮名は `*` の後ろに薄く。
fn preedit_view(p: &Preedit) -> Option<PreeditView> {
    match p.state {
        PreeditState::Direct if p.text.is_empty() => None,
        // ローマ字の途中だけ
        PreeditState::Direct => Some(PreeditView::new().with(PreeditStyle::Pending, p.text.clone())),
        PreeditState::Reading => {
            let (reading, okuri) = match p.okuri_at {
                Some(i) if i <= p.text.len() => p.text.split_at(i),
                _ => (p.text.as_str(), ""),
            };
            let v = PreeditView::reading(format!("▽{reading}"));
            Some(if okuri.is_empty() && p.okuri_at.is_none() { v } else { v.with(PreeditStyle::Pending, format!("*{okuri}")) })
        }
        PreeditState::Converting => Some(PreeditView::converting(format!("▼{}", p.text))),
    }
}

/// 未確定を 1 行の文字にする（ターミナル・登録欄に出すとき）。
fn preedit_text(p: &Preedit) -> Option<(String, bool)> {
    match p.state {
        PreeditState::Direct if p.text.is_empty() => None,
        PreeditState::Direct => Some((p.text.clone(), false)),
        PreeditState::Reading => Some((format!("▽{}", p.text), false)),
        PreeditState::Converting => Some((format!("▼{}", p.text), true)),
    }
}

/// 実行中の経過時間の表示を更新する間隔。
const RUN_TICK: Duration = Duration::from_secs(1);

/// F5 の実行で、新しい端末の bash に渡す台本。実行するコマンドは環境変数 `ZCODE_RUN` で
/// 渡す（引用符の付け直しを避ける）。プロンプト風の 1 行（フォルダとコマンド）を出してから実行し、終了コードを
/// 画面に出して、独自の OSC（`zc_term::TermEvent::CommandExit`）で zcode にも知らせ、
/// そのまま対話のシェルになる（モック 3・4: 続けてコマンドも打てる）。`trap : INT` は、
/// Ctrl+C で実行中のプログラムだけを止め、外側の bash を道連れにしないため（`trap '' INT`
/// だと無視が子に引き継がれて止められない）。
const RUN_SCRIPT: &str = r#"printf '\033[94m%s\033[0m$ %s\n' "$(pwd | sed "s|^$HOME|~|")" "$ZCODE_RUN"; trap : INT; eval "$ZCODE_RUN"; ec=$?; trap - INT; printf '\033[90m[終了コード %d]\033[0m\n' "$ec"; printf '\033]777;zcode-exit;%d\007' "$ec"; unset ZCODE_RUN; exec bash -i"#;

/// エディタ画面の指定。
#[derive(Clone, Debug)]
pub struct EditorConfig {
    /// 文書の実装。
    pub backend: DocBackend,
    /// ツリーの根（作業フォルダ）。
    pub root: PathBuf,
    /// 起動時に開くファイル。
    pub files: Vec<PathBuf>,
}

impl EditorConfig {
    /// 既定の作業フォルダ: `~/code` があればそこ、無ければ今いるフォルダ。
    pub fn default_root() -> PathBuf {
        if let Some(home) = std::env::var_os("HOME") {
            let code = Path::new(&home).join("code");
            if code.is_dir() {
                return code;
            }
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

/// 言語の設定（表示名・字下げ・行コメント）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Lang {
    id: Option<LanguageId>,
    name: String,
    indent: String,
    comment: Option<String>,
}

impl Lang {
    /// `zc-highlight` に登録のある言語なら、その設定。
    fn info(&self) -> Option<&'static LanguageInfo> {
        self.id.map(|id| Registry::builtin().info(id))
    }
    /// ファイル名と 1 行目から決める。`zc-highlight` に登録が無ければ、拡張子の小さな
    /// 表で名前と行コメントだけ決める（色は付かない）。
    fn detect(path: Option<&Path>, first_line: &str) -> Lang {
        if let Some(p) = path {
            if let Some(id) = Registry::builtin().detect(p, first_line) {
                let info = Registry::builtin().info(id);
                return Lang { id: Some(id), name: info.name.into(), indent: info.indent.into(), comment: info.line_comment.map(Into::into) };
            }
        }
        let ext = path.and_then(|p| p.extension()).and_then(|e| e.to_str()).unwrap_or("");
        let (name, comment) = match ext {
            "py" | "pyw" => ("Python", Some("#")),
            "sh" | "bash" => ("Shell", Some("#")),
            "rs" => ("Rust", Some("//")),
            "c" | "h" => ("C", Some("//")),
            "toml" => ("TOML", Some("#")),
            "json" => ("JSON", None),
            "md" => ("Markdown", None),
            _ => ("Plain Text", None),
        };
        Lang { id: None, name: name.into(), indent: "    ".into(), comment: comment.map(Into::into) }
    }
}

/// ハイライトの状態（文書ごと）。
struct Hl {
    lang: LanguageId,
    hl: Highlighter,
    /// 行頭の文脈を計算し直す必要がある。
    dirty: bool,
    /// batch に残りを頼んだ版。
    rest_pending: Option<Version>,
}

/// 開いている文書 1 つ。
struct DocTab {
    id: DocId,
    path: Option<PathBuf>,
    title: String,
    doc: Box<dyn TextDoc + Send>,
    view: EditorView,
    lang: Lang,
    hl: Option<Hl>,
    /// 外から差し込んだトークン（render-test・計測用）。あればこちらを使う。
    custom_spans: Option<Box<dyn SpanSource>>,
    /// batch で読み込み中（中身はまだ空）。
    loading: bool,
    /// 単語補完の索引（§10.2）。
    words: WordIndex,
    /// 保存時の構文チェックの結果（編集に合わせて行をずらす）。
    diags: Vec<Diagnostic>,
    /// Markdown のライブプレビュー（.md のときだけ。モック 10・11）。
    md: Option<MdState>,
}

/// この行数までの Markdown は、全体の解析をメインで行う（超えたら batch）。実機で
/// 1 行約 4.5µs なので 400 行で約 2ms（`docs/measurements.md` §12）。
const MD_MAIN_PARSE_LINES: u32 = 400;

/// Markdown の文書ごとの状態。解析結果はプレビューの [`MdLayouter`] と共有する。
struct MdState {
    doc: Rc<RefCell<MdDocument>>,
    /// プレビュー中か（`false` ならソース表示。Ctrl+Shift+V で切り替え）。
    preview: bool,
    /// 全体の解析をやり直す必要がある（ブロックの境目が変わったかもしれない）。
    stale: bool,
    /// batch に全体の解析を頼んだ版。
    pending: Option<Version>,
}

impl MdState {
    fn layouter(&self) -> Box<dyn zc_ui::LineLayouter> {
        if self.preview {
            Box::new(MdLayouter::new(self.doc.clone()))
        } else {
            Box::new(CodeLayouter::standard())
        }
    }
}

/// 描画に渡すトークンの出どころ。
struct TabSpans<'a> {
    custom: Option<&'a mut (dyn SpanSource + 'static)>,
    hl: Option<&'a Highlighter>,
}

impl SpanSource for TabSpans<'_> {
    fn spans(&mut self, line: u32, text: &str, out: &mut Vec<Span>) {
        out.clear();
        if let Some(c) = self.custom.as_mut() {
            c.spans(line, text, out);
        } else if let Some(h) = self.hl {
            h.spans(line, text, out);
        }
    }
}

impl DocTab {
    fn new(id: DocId, path: Option<PathBuf>, title: String, doc: Box<dyn TextDoc + Send>) -> DocTab {
        let words = WordIndex::build(doc.as_lines(), &CancelToken::never()).unwrap_or_else(|_| WordIndex::new());
        let mut t = DocTab { id, path, title, doc, view: EditorView::code(), lang: Lang::detect(None, ""), hl: None, custom_spans: None, loading: false, words, diags: Vec::new(), md: None };
        t.detect_language();
        t
    }
    /// 言語を決め直し、ハイライトを最初から作る。
    fn detect_language(&mut self) {
        let mut first = String::new();
        self.doc.line(0, &mut first);
        self.lang = Lang::detect(self.path.as_deref(), &first);
        self.hl = self.lang.id.map(|lang| Hl { lang, hl: Highlighter::new(Some(lang)), dirty: true, rest_pending: None });
        self.view.recheck_all();
        // Markdown はライブプレビューで開く（ソース表示にしていたら、そのまま）
        if self.lang.name == "Markdown" {
            let preview = self.md.as_ref().is_none_or(|m| m.preview);
            let md = MdState { doc: Rc::new(RefCell::new(MdDocument::new())), preview, stale: true, pending: None };
            self.view.set_layouter(md.layouter());
            self.md = Some(md);
            self.parse_markdown_here();
        } else if self.md.take().is_some() {
            self.view.set_layouter(Box::new(CodeLayouter::standard()));
        }
    }
    /// 小さな Markdown なら、全体をここ（メイン）で解析し直す。大きければ何もしない
    /// （`stale` のまま。`pump_markdown` が batch に出す）。
    fn parse_markdown_here(&mut self) -> bool {
        let Some(m) = self.md.as_mut() else { return false };
        if !m.stale || self.loading || self.doc.as_lines().len_lines() > MD_MAIN_PARSE_LINES {
            return false;
        }
        let token = self.doc.token();
        let Ok(parsed) = MdDocument::parse(self.doc.as_lines(), &token) else { return false };
        if m.doc.borrow_mut().replace(parsed, self.doc.version()) {
            m.stale = false;
            if m.preview {
                self.view.recheck_all();
            }
        }
        true
    }
    /// 編集のあと: ハイライトとビューに、変わった行を知らせる。
    fn edited(&mut self, change: LineChange) {
        if let Some(h) = self.hl.as_mut() {
            h.hl.edited(change);
            h.dirty = true;
        }
        self.view.lines_changed(change);
        if let Some(m) = self.md.as_mut() {
            // 編集した行のブロックだけ直す。境目が変わったかもしれなければ、全体をやり直す
            if m.doc.borrow_mut().edited(self.doc.as_lines(), change) {
                m.stale = true;
            }
            if m.preview {
                self.view.recheck_all();
            }
        }
        self.words.edited(self.doc.as_lines(), change);
        if !self.diags.is_empty() {
            intel::shift_diagnostics(&mut self.diags, change);
            self.view.set_marks(intel::marks_of(&self.diags));
        }
    }
    /// 構文チェックでエラーがあるか（タブの名前を赤くする）。
    fn has_error(&self) -> bool {
        self.diags.iter().any(|d| d.severity == Severity::Error)
    }
    /// カーソル行の診断。
    fn diag_at_cursor(&self) -> Option<&Diagnostic> {
        let line = self.doc.selection().1.line;
        self.diags.iter().find(|d| d.line == line)
    }
    /// 文書の選択範囲をビューに写す。
    fn sync_cursor(&mut self) {
        let (a, h) = self.doc.selection();
        self.view.set_selection(a, h);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Editor,
    Tree,
    /// ターミナルのパネル（打鍵は、決まったショートカットを除いて端末へ送る）。
    Terminal,
}

/// 端末（パネルに出しているもの）。
struct TermState {
    term: Term,
    /// 見出しに出す名前（実行したコマンド、またはシェル）。
    title: String,
    /// F5 の実行中ならその中身。
    running: Option<RunInfo>,
}

/// F5 で実行しているもの。
struct RunInfo {
    file: String,
    started_us: u64,
}

/// 文字列をシェルの単語 1 つにする。特別な文字が無ければそのまま、あれば `'...'` で囲む。
fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-+/@%:,=".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// ステータスバーの上に出す 1 行の入力（段階 4 の入力欄ができるまでの簡易版）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptKind {
    GotoLine,
    Find,
    SaveAs,
}

struct Prompt {
    kind: PromptKind,
    text: String,
}

/// 開いている補完の候補（モック 12）。
struct CompState {
    items: Vec<Completion>,
    sel: usize,
    /// 置き換える単語の先頭部分（打った文字）。
    prefix: String,
}

/// 開いているアウトライン（モック 14）。
struct OutlineState {
    items: Vec<zc_intel::OutlineItem>,
    query: String,
    /// 絞り込んだ行と、選べる行の行番号。
    rows: Vec<intel::OutlineRow>,
    sel: usize,
}

/// 二度押しで確かめる操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    Quit,
    Close(DocId),
}

/// エディタ画面。
pub struct EditorScreen {
    cfg: EditorConfig,
    tabs: Vec<DocTab>,
    active: usize,
    next_id: u32,
    tabbar: TabBar,
    tree: FileTree,
    tree_open: bool,
    focus: Focus,
    status: StatusBar,
    saver: Box<dyn Saver>,
    clipboard: String,
    prompt: Option<Prompt>,
    last_find: String,
    /// ステータスバーに出す知らせ（次の打鍵で消える）と、エラーかどうか。
    message: Option<(String, bool)>,
    /// 知らせをステータスバーの右に出す（左のファイル名・行・桁と件数は残す。モック 13）。
    message_right: bool,
    confirm: Option<Confirm>,
    mods: Mods,
    /// 次の描画でステータスバーの中身を作り直す。
    status_due: bool,
    /// 単語補完の候補と小窓。
    completion: Option<CompState>,
    popup: CompletionPopup,
    /// 小窓の下の行を描き直してから、小窓を置き直す。
    popup_stale: bool,
    /// アウトライン（開いているあいだは本文の全体を覆う）。
    outline: Option<OutlineState>,
    picker: PickerList,
    /// SKK（段階 4。モック 7〜9）。エディタとターミナルの打鍵は、まずここを通る。
    skk: Skk,
    /// システム辞書（assist スレッドで最初に引くときに開く。開けなければ `None`）。
    skk_dict: Arc<OnceLock<Option<SystemDict>>>,
    /// 結果を待っている辞書引きの数。待っている間の打鍵は `skk_held` に取っておく。
    skk_waiting: u32,
    skk_held: Vec<KeyEvent>,
    /// まだ個人辞書に書いていない学習。
    skk_learned: Vec<LearnedWord>,
    /// 前回見た入力モード（英数 → かなで辞書を先読みする）。
    skk_mode: InputMode,
    cand_popup: CandidatePopup,
    cand_stale: bool,
    mini: MiniBuffer,
    /// 登録欄を出していた（開閉で領域が変わる）。
    mini_open: bool,
    /// 統合ターミナル（段階 3）。
    term: Option<TermState>,
    panel: TerminalPanel,
    panel_open: bool,
    panel_full: bool,
    /// 端末の大きさ（パネルの寸法から決めたもの。最初の描画までは見積もり）。
    term_grid: (u16, u16),
    /// 保存が終わったら実行する文書（F5 で未保存だったとき）。
    pending_run: Option<DocId>,
    /// 実行の結果（ステータスバーの文と色。次の打鍵まで出す。モック 4）。
    run_status: Option<(String, Color)>,
    /// タブバーに出した内容（変わったときだけ作り直す）: 文書・未保存・エラー。
    tabs_shown: Vec<(DocId, bool, bool)>,
    tabs_dirty: bool,
    blinks_left: u32,
    cursor_on: bool,
    /// 文書が無いときの案内を描いた領域。
    empty_drawn: Option<Rect>,
    /// 計測用: 打鍵のたびにすぐステータスバーを更新する（遅らせない）。
    pub status_immediate: bool,
}

impl EditorScreen {
    pub fn new(cfg: EditorConfig) -> EditorScreen {
        let tree = FileTree::new(&cfg.root);
        let no_files = cfg.files.is_empty();
        let mut s = EditorScreen {
            cfg,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            tabbar: TabBar::new(),
            tree,
            tree_open: no_files,
            focus: if no_files { Focus::Tree } else { Focus::Editor },
            status: StatusBar::new(),
            saver: Box::new(StoreSaver::from_env()),
            clipboard: String::new(),
            prompt: None,
            last_find: String::new(),
            message: None,
            message_right: false,
            confirm: None,
            mods: Mods::NONE,
            status_due: true,
            completion: None,
            popup: CompletionPopup::new("Tab 確定  ↑↓ 選ぶ  Esc 閉じる"),
            popup_stale: false,
            outline: None,
            picker: PickerList::new("アウトライン"),
            skk: Skk::new(load_skk_user_dict()),
            skk_dict: Arc::new(OnceLock::new()),
            skk_waiting: 0,
            skk_held: Vec::new(),
            skk_learned: Vec::new(),
            skk_mode: InputMode::Ascii,
            cand_popup: CandidatePopup::new(),
            cand_stale: false,
            mini: MiniBuffer::new(),
            mini_open: false,
            term: None,
            panel: TerminalPanel::new(),
            panel_open: false,
            panel_full: false,
            term_grid: (56, 6),
            pending_run: None,
            run_status: None,
            tabs_shown: Vec::new(),
            tabs_dirty: true,
            blinks_left: 0,
            cursor_on: true,
            empty_drawn: None,
            status_immediate: false,
        };
        s.tree.set_focused(s.focus == Focus::Tree);
        s
    }

    /// 保存の出口を差し替える（既定は [`StoreSaver`]）。
    pub fn set_saver(&mut self, saver: Box<dyn Saver>) {
        self.saver = saver;
    }
    /// ファイルツリー。
    pub fn tree_mut(&mut self) -> &mut FileTree {
        &mut self.tree
    }
    /// ツリーを開く / 閉じる。
    pub fn set_tree_open(&mut self, open: bool) {
        if self.tree_open != open {
            self.tree_open = open;
            self.tree.invalidate();
            self.tabs_dirty = true;
            self.completion = None;
            self.popup.forget();
            if !open {
                self.set_focus(Focus::Editor);
            }
        }
    }
    /// 押している / ロック中のモディファイア（ステータスバーの札）。
    pub fn set_mods(&mut self, mods: Mods) {
        let shown = |m: Mods| m.0 & (Mods::SHIFT.0 | Mods::SYM.0 | Mods::FN.0);
        if shown(mods) != shown(self.mods) {
            self.status_due = true;
        }
        self.mods = mods;
    }
    /// 開いている文書の数。
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }
    /// 選択中の文書（テストと計測用）。
    pub fn active_doc(&self) -> Option<&dyn TextDoc> {
        self.tabs.get(self.active).map(|t| &*t.doc as &dyn TextDoc)
    }
    /// 選択中の文書のビュー（テストと計測用）。
    pub fn active_view(&self) -> Option<&EditorView> {
        self.tabs.get(self.active).map(|t| &t.view)
    }
    /// 選択中の文書の番号。
    pub fn active_doc_id(&self) -> Option<DocId> {
        self.tabs.get(self.active).map(|t| t.id)
    }
    /// 選択中の文書のパス。
    pub fn active_path(&self) -> Option<&Path> {
        self.tabs.get(self.active).and_then(|t| t.path.as_deref())
    }
    /// 選択中の文書が Markdown なら、プレビュー中か（`false` はソース表示）。
    pub fn markdown_preview(&self) -> Option<bool> {
        self.tabs.get(self.active).and_then(|t| t.md.as_ref()).map(|m| m.preview)
    }
    /// ステータスバーに出ている知らせ。
    pub fn message(&self) -> Option<&str> {
        self.message.as_ref().map(|m| m.0.as_str())
    }
    /// 開いている補完の候補と、選んでいる位置（テスト用）。
    pub fn completion(&self) -> Option<(Vec<String>, usize)> {
        self.completion.as_ref().map(|c| (c.items.iter().map(|i| i.label.clone()).collect(), c.sel))
    }
    /// 開いているアウトラインの行（名前、親として残しただけの行か）と選んでいる位置（テスト用）。
    pub fn outline_rows(&self) -> Option<(Vec<(String, bool)>, usize)> {
        self.outline.as_ref().map(|o| (o.rows.iter().map(|r| (r.row.name.clone(), r.line.is_none())).collect(), o.sel))
    }
    /// ターミナルの画面の文字（行ごと、右の空白は落とす。テスト用）。端末が無ければ `None`。
    pub fn terminal_text(&self) -> Option<Vec<String>> {
        let t = self.term.as_ref()?;
        let (_, rows) = t.term.size();
        let text = |cells: &[zc_term::Cell]| {
            let mut s = String::new();
            let mut skip = false;
            for c in cells {
                // 全角の右半分の空白は飛ばす
                if !std::mem::take(&mut skip) {
                    s.push(c.ch);
                }
                skip = c.wide;
            }
            s.trim_end().to_string()
        };
        Some((0..rows).map(|r| text(t.term.row(r))).collect())
    }
    /// パネルが開いているか・全画面か・入力がターミナルにあるか（テスト用）。
    pub fn terminal_state(&self) -> (bool, bool, bool) {
        (self.panel_open, self.panel_full, self.focus == Focus::Terminal)
    }
    /// 実行中か、と最後の実行の結果の文（テスト用）。
    pub fn run_state(&self) -> (bool, Option<&str>) {
        (self.term.as_ref().is_some_and(|t| t.running.is_some()), self.run_status.as_ref().map(|s| s.0.as_str()))
    }
    /// SKK（テスト用）: 入力モード、本文の未確定、候補（いまのページ）、登録欄の読み。
    pub fn skk_state(&self) -> (InputMode, String, Vec<String>, Option<String>) {
        let p = self.skk.preedit();
        let cands = self.skk_candidate_rows().map(|(r, _, _)| r.into_iter().map(|c| c.text).collect()).unwrap_or_default();
        (self.skk.mode(), p.text.clone(), cands, self.skk.registration().map(|r| r.reading.clone()))
    }
    /// まだ個人辞書に書いていない学習の数（テスト用）。
    pub fn skk_pending_learned(&self) -> usize {
        self.skk_learned.len()
    }
    /// 選択中の文書の診断（テスト用）。
    pub fn diagnostics(&self) -> &[Diagnostic] {
        self.tabs.get(self.active).map_or(&[], |t| &t.diags)
    }

    fn set_focus(&mut self, focus: Focus) {
        if self.focus == focus {
            return;
        }
        self.focus = focus;
        self.skk.reset();
        self.tree.set_focused(focus == Focus::Tree);
        self.panel.set_focused(focus == Focus::Terminal);
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.view.set_focused(focus == Focus::Editor);
            t.doc.break_undo_group();
        }
        self.status_due = true;
    }

    fn say(&mut self, text: impl Into<String>, error: bool) {
        self.message = Some((text.into(), error));
        self.message_right = false;
        self.status_due = true;
    }
    /// 右に出す知らせ（構文チェックの結果）。
    fn say_right(&mut self, text: impl Into<String>, error: bool) {
        self.say(text, error);
        self.message_right = true;
    }

    fn alloc_id(&mut self) -> DocId {
        let id = DocId(self.next_id);
        self.next_id += 1;
        id
    }

    fn activate(&mut self, index: usize) {
        if self.tabs.is_empty() {
            self.active = 0;
            self.tree.set_current(None);
        } else {
            self.active = index.min(self.tabs.len() - 1);
            let focused = self.focus == Focus::Editor;
            let t = &mut self.tabs[self.active];
            // 画面には別の文書が描いてあるので、全部描き直す
            t.view.invalidate();
            t.view.set_focused(focused);
            t.view.set_cursor_on(true);
            let path = t.path.clone();
            self.tree.set_current(path.as_deref());
        }
        self.empty_drawn = None;
        self.tabs_dirty = true;
        self.status_due = true;
        // 別の文書の上に出ていた小窓は消す（ビューは全部描き直す）
        self.completion = None;
        self.popup.forget();
        self.skk.reset();
        self.cand_popup.forget();
    }

    /// 読み込み済みの文書をタブに足して、選ぶ（render-test・計測・テスト用。実行中に
    /// ファイルを開くのは [`EditorScreen::open_path`]）。`spans` を渡すと、ハイライトの
    /// 代わりにそれで色を付ける。
    pub fn open_doc(&mut self, path: Option<PathBuf>, title: &str, doc: Box<dyn TextDoc + Send>, spans: Option<Box<dyn SpanSource>>) -> DocId {
        let id = self.alloc_id();
        let mut tab = DocTab::new(id, path, title.into(), doc);
        tab.custom_spans = spans;
        tab.sync_cursor();
        self.tabs.push(tab);
        self.activate(self.tabs.len() - 1);
        id
    }

    /// ファイルを開く（もう開いていればそのタブへ）。読み込みは batch スレッドで行い、
    /// 終わるまでは空のタブを出しておく。無いファイルは、その名前の新しい文書になる。
    pub fn open_path(&mut self, path: &Path, cx: &mut LoopCx<'_>) {
        let path = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().map(|d| d.join(path)).unwrap_or_else(|_| path.to_path_buf()) };
        if let Some(i) = self.tabs.iter().position(|t| t.path.as_deref() == Some(path.as_path())) {
            self.activate(i);
            return;
        }
        let id = self.alloc_id();
        let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "?".into());
        let mut tab = DocTab::new(id, Some(path.clone()), title, self.cfg.backend.empty());
        tab.loading = true;
        self.tabs.push(tab);
        self.activate(self.tabs.len() - 1);
        let (backend, tx) = (self.cfg.backend, cx.results.clone());
        let note = self.saver.load_note();
        cx.workers.batch.submit(move || {
            let result = match backend.load(&path, &CancelToken::never()) {
                Ok(doc) => {
                    let words = WordIndex::build(doc.as_lines(), &CancelToken::never()).unwrap_or_else(|_| WordIndex::new());
                    // 書き込み層に読んだ中身を知らせる（保存時の「変わっていなければ書かない」
                    // 「ほかのプログラムが変えた」の判定）。読んだ直後なのでページキャッシュから読める
                    if let (Some(note), Ok(bytes)) = (note, std::fs::read(&path)) {
                        note(&path, &bytes);
                    }
                    Ok((doc, words))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((backend.empty(), WordIndex::new())),
                Err(e) => Err(e.to_string()),
            };
            tx.send(AppEvent::Opened { doc: id, result });
        });
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        if let Some(p) = &tab.path {
            self.saver.closed(p);
        }
        let next = if index < self.active || (index == self.active && index == self.tabs.len()) { self.active.saturating_sub(1) } else { self.active };
        self.activate(next);
        if self.tabs.is_empty() && self.tree_open {
            self.set_focus(Focus::Tree);
        }
    }

    // --- 打鍵 ---

    /// 打鍵 1 回ぶんをまとめて処理する。`mods` はいま押している / ロック中の
    /// モディファイア。
    pub fn keys(&mut self, keys: &[KeyEvent], mods: Mods, cx: &mut LoopCx<'_>) {
        self.set_mods(mods);
        if keys.is_empty() {
            return;
        }
        self.feed_keys(keys.to_vec(), cx);
        self.pump_highlight(Some(cx));
        self.pump_markdown(cx);
        // 打鍵の直後はカーソルを点けたままにして、点滅をやり直す
        self.cursor_on = true;
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.view.set_cursor_on(true);
        }
        self.blinks_left = BLINKS_AFTER_KEY;
        cx.timers.set_after(TimerId::CursorBlink, BLINK);
        // 行・桁の表示は、打鍵が 50ms 止まってから（知らせや入力欄はすぐ出す）
        if self.status_immediate {
            self.status_due = true;
        } else if !self.status_due {
            cx.timers.set_after(TimerId::StatusDelay, STATUS_DELAY);
        }
    }

    /// 打鍵を順に処理する。SKK の辞書引きの結果を待っている間に来た打鍵は取っておき、
    /// 結果が届いてから流す（「Space の直後の a」を正しく扱うため。zc-skk の使い方 3）。
    fn feed_keys(&mut self, keys: Vec<KeyEvent>, cx: &mut LoopCx<'_>) {
        let mut it = keys.into_iter();
        for k in it.by_ref() {
            if self.skk_waiting > 0 {
                self.skk_held.push(k);
                break;
            }
            self.handle_key(&k, cx);
        }
        self.skk_held.extend(it);
    }

    fn handle_key(&mut self, ev: &KeyEvent, cx: &mut LoopCx<'_>) {
        if self.message.take().is_some() {
            self.status_due = true;
        }
        // SKK: エディタとターミナルの打鍵はまず SKK へ（入力欄・アウトラインは英数のまま）
        if matches!(self.focus, Focus::Editor | Focus::Terminal) && self.prompt.is_none() && self.outline.is_none() {
            let out = self.skk.handle_key(ev);
            if self.cand_popup.drawn().is_some() {
                self.cand_stale = true;
            }
            self.after_skk(out.commit, out.lookup, out.learned, cx);
            if out.consumed {
                self.completion = None;
                self.status_due = true;
                return;
            }
        }
        if self.focus == Focus::Terminal {
            self.terminal_key(ev, cx);
            return;
        }
        if self.run_status.take().is_some() {
            self.status_due = true;
        }
        if self.prompt.is_some() {
            self.prompt_key(ev, cx);
            return;
        }
        if self.outline.is_some() {
            self.outline_key(ev);
            return;
        }
        let Some(cmd) = commands::lookup(ev) else { return };
        if ev.repeat && !cmd.repeatable() {
            return;
        }
        if self.popup.drawn().is_some() {
            // 小窓の下の行を描き直してから置き直す（カーソルも中身も変わりうる）
            self.popup_stale = true;
        }
        if let Some(c) = self.completion.as_mut() {
            match cmd {
                Command::Indent => {
                    self.accept_completion();
                    return;
                }
                Command::Move { motion: m @ (Motion::Up | Motion::Down), select: false } => {
                    let n = c.items.len();
                    c.sel = if m == Motion::Up { (c.sel + n - 1) % n } else { (c.sel + 1) % n };
                    return;
                }
                Command::Cancel => {
                    self.completion = None;
                    return;
                }
                _ => {}
            }
        }
        self.exec(cmd, cx);
        // 単語を打っている間・消している間は候補を出し直す。ほかの操作では閉じる
        match cmd {
            Command::InsertChar(c) if c.is_alphanumeric() || c == '_' => self.update_completion(false),
            Command::Backspace if self.completion.is_some() => self.update_completion(false),
            Command::Complete => self.update_completion(true),
            _ => self.completion = None,
        }
    }

    // --- SKK（段階 4。モック 7〜9） ---

    /// SKK の 1 打鍵の後始末: 確定した文字を入れる、辞書を引く、学習をためる、先読み。
    fn after_skk(&mut self, commit: String, lookup: Option<zc_skk::LookupRequest>, learned: bool, cx: &mut LoopCx<'_>) {
        if !commit.is_empty() {
            self.commit_text(&commit);
        }
        if let Some(req) = lookup {
            self.skk_waiting += 1;
            let (dict, tx) = (self.skk_dict.clone(), cx.results.clone());
            cx.workers.assist.submit(move || {
                let d = dict.get_or_init(|| SystemDict::open(&skk_system_dict_path()).ok());
                let cands = d.as_ref().map(|d| d.lookup(&req.reading, req.okuri)).unwrap_or_default();
                tx.send(AppEvent::SkkCandidates(zc_core::Versioned::new(req.version, cands)));
            });
        }
        if learned {
            self.skk_learned.extend(self.skk.take_changes());
            if cx.timers.deadline(TimerId::SkkFlush).is_none() {
                cx.timers.set_after(TimerId::SkkFlush, SKK_FLUSH);
            }
        }
        let mode = self.skk.mode();
        if mode != self.skk_mode {
            if self.skk_mode == InputMode::Ascii {
                // かなモードに入った: 辞書を先読みしておく（最初の変換を待たせない）
                let dict = self.skk_dict.clone();
                cx.workers.assist.submit(move || {
                    if let Some(d) = dict.get_or_init(|| SystemDict::open(&skk_system_dict_path()).ok()) {
                        d.prefetch();
                    }
                });
            }
            self.skk_mode = mode;
            self.status_due = true;
        }
    }

    /// 確定した文字列を、入力が向いている所（文書かターミナル）に入れる。
    fn commit_text(&mut self, text: &str) {
        if self.focus == Focus::Terminal {
            if let Some(t) = self.term.as_mut() {
                if let Err(e) = t.term.send_text(text) {
                    self.say(format!("ターミナルに送れない: {e}"), true);
                }
            }
            return;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if tab.loading {
            return;
        }
        if let Some(c) = tab.doc.insert(text) {
            tab.edited(c);
        }
        tab.sync_cursor();
    }

    /// 学習した語を個人辞書に書き戻す（storage スレッドで）。
    fn flush_skk(&mut self, cx: &mut LoopCx<'_>) {
        if self.skk_learned.is_empty() {
            return;
        }
        let changes = self.skk_learned.drain(..).map(|w| zc_store::DictChange::Learn { reading: w.reading, candidate: w.word }).collect();
        self.saver.skk_sync(changes, cx);
    }

    /// 候補一覧の中身（いまのページ）。
    fn skk_candidate_rows(&self) -> Option<(Vec<CandidateRow>, usize, String)> {
        let list = self.skk.candidates()?;
        if list.items.is_empty() {
            return None;
        }
        let start = list.page_start.min(list.items.len());
        let rows = list.items[start..(start + SKK_KEYS.len()).min(list.items.len())]
            .iter()
            .zip(SKK_KEYS)
            .map(|(c, key)| CandidateRow { key, text: c.text.clone(), annotation: c.annotation.clone() })
            .collect();
        let pages = list.items.len().div_ceil(SKK_KEYS.len());
        let page = format!("{}/{}", start / SKK_KEYS.len() + 1, pages);
        Some((rows, list.selected.saturating_sub(start), page))
    }

    // --- ターミナル（段階 3。モック 3・4） ---

    /// ターミナルに入力があるときの打鍵。ターミナルの開閉・全画面・実行・終了だけは
    /// zcode が受け、ほかは全部端末へ送る（Ctrl+C は端末の中で実行を止める）。
    fn terminal_key(&mut self, ev: &KeyEvent, cx: &mut LoopCx<'_>) {
        if let Some(c @ (Command::ToggleTerminal | Command::ToggleTerminalFullscreen | Command::Quit | Command::Run)) = commands::lookup(ev) {
            self.exec(c, cx);
            return;
        }
        let Some(t) = self.term.as_mut() else {
            self.set_focus(Focus::Editor);
            return;
        };
        // Shift+PgUp / PgDn でスクロールバックを見る
        if ev.mods.shift() && matches!(ev.key, Key::PageUp | Key::PageDown) {
            let (_, rows) = t.term.size();
            let page = rows.saturating_sub(1).max(1) as i32;
            t.term.scroll_view(if ev.key == Key::PageUp { page } else { -page });
            return;
        }
        if let Err(e) = t.term.send_key(ev) {
            self.say(format!("ターミナルに送れない: {e}"), true);
        }
    }

    /// パネルを開いて端末に入力を向ける。端末が無ければシェルを起動する。
    fn open_terminal(&mut self) {
        if self.term.is_none() {
            let cmd = TermCommand { program: None, args: Vec::new(), cwd: Some(self.cfg.root.clone()), env: Vec::new() };
            match Term::spawn(&cmd, self.term_grid.0, self.term_grid.1) {
                Ok(term) => {
                    let title = std::env::var("SHELL").ok().and_then(|s| s.rsplit('/').next().map(str::to_string)).unwrap_or_else(|| "bash".into());
                    self.term = Some(TermState { term, title, running: None });
                }
                Err(e) => {
                    self.say(format!("シェルを起動できない: {e}"), true);
                    return;
                }
            }
        }
        self.set_panel(true, self.panel_full);
        self.set_focus(Focus::Terminal);
    }

    /// パネルの開閉・全画面を変える（エディタとツリーの大きさが変わる）。
    fn set_panel(&mut self, open: bool, full: bool) {
        if (self.panel_open, self.panel_full) == (open, full) {
            return;
        }
        self.panel_open = open;
        self.panel_full = full;
        self.panel.invalidate();
        self.tree.invalidate();
        self.tabbar.invalidate();
        self.empty_drawn = None;
        self.completion = None;
        self.popup.forget();
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.view.invalidate();
        }
        if !open && self.focus == Focus::Terminal {
            self.set_focus(Focus::Editor);
        }
        self.status_due = true;
    }

    /// F12: 閉じていれば開いて端末へ、端末にいれば閉じてエディタへ、エディタにいれば端末へ。
    fn toggle_terminal(&mut self) {
        if !self.panel_open {
            self.open_terminal();
        } else if self.focus == Focus::Terminal {
            self.set_panel(false, false);
        } else {
            self.set_focus(Focus::Terminal);
        }
    }

    /// F5: 保存してから、言語の `run` を新しい端末で実行する。
    fn run_active(&mut self, cx: &mut LoopCx<'_>) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        if tab.loading {
            return;
        }
        if tab.lang.info().and_then(|i| i.run).is_none() {
            let msg = format!("{} は F5 で実行できない（language.toml に run が無い）", tab.lang.name);
            self.say(msg, false);
            return;
        }
        if tab.path.is_none() || tab.doc.is_modified() {
            // 保存が終わってから実行する（storage スレッドが書き終える前に読ませない）
            self.pending_run = Some(tab.id);
            self.save_active(cx);
            return;
        }
        self.start_run(cx);
    }

    /// 選択中の文書を、いまのディスクの中身で実行する。
    fn start_run(&mut self, cx: &mut LoopCx<'_>) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let (Some(path), Some(run)) = (tab.path.clone(), tab.lang.info().and_then(|i| i.run)) else { return };
        let file = tab.title.clone();
        // 作業フォルダはファイルのあるフォルダなので、ファイル名だけで渡す（見出しも短くなる）
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned());
        let cmd = run.replace("{file}", &shell_quote(&name));
        let cwd = path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.cfg.root.clone());
        let tc = TermCommand {
            program: Some("bash".into()),
            args: vec!["-c".into(), RUN_SCRIPT.into()],
            cwd: Some(cwd),
            env: vec![("ZCODE_RUN".into(), cmd.clone())],
        };
        // 前の端末（前の実行やシェル）は閉じる（子プロセスのグループに SIGHUP）
        self.term = None;
        match Term::spawn(&tc, self.term_grid.0, self.term_grid.1) {
            Ok(term) => {
                self.term = Some(TermState { term, title: cmd, running: Some(RunInfo { file, started_us: cx.now_us }) });
                self.run_status = None;
                self.panel.invalidate();
                self.set_panel(true, self.panel_full);
                self.set_focus(Focus::Terminal);
                cx.timers.set_after(TimerId::Tick, RUN_TICK);
            }
            Err(e) => self.say(format!("実行できない: {e}"), true),
        }
    }

    /// 端末の出力を読む。まだ読み残しがあれば `true`。
    fn pump_terminal(&mut self) -> bool {
        let Some(t) = self.term.as_mut() else { return false };
        let r = match t.term.read(ReadBudget::default()) {
            Ok(r) => r,
            Err(e) => {
                self.say(format!("ターミナルを読めない: {e}"), true);
                self.term = None;
                return false;
            }
        };
        t.term.take_dirty_rows();
        for ev in t.term.take_events() {
            if let TermEvent::CommandExit(code) = ev {
                if let Some(run) = t.running.take() {
                    self.run_status = Some(if code == 0 {
                        (format!("✓ 終了コード 0  {}", run.file), STATUS_OK)
                    } else {
                        (format!("✕ 終了コード {code}  {}", run.file), STATUS_ERROR)
                    });
                    self.status_due = true;
                }
            }
        }
        if r.closed {
            // シェルが終わった（exit など）。パネルは閉じる
            self.term = None;
            self.set_panel(false, false);
            self.say("ターミナルを閉じた", false);
            return false;
        }
        r.more
    }

    /// パネルの見出し（モック 3・4）。
    fn panel_header(&self) -> (String, &'static str) {
        let Some(t) = self.term.as_ref() else { return (String::new(), "F12 戻る") };
        let title = if self.panel_full { format!("全画面  {}", t.title) } else { t.title.clone() };
        let hint = if t.running.is_some() { "Ctrl+C 停止" } else { "F12 戻る" };
        (title, hint)
    }

    // --- 単語補完（モック 12） ---

    /// カーソルの前の単語から候補を作り直す。`manual`（Ctrl+Space）なら 1 文字からでも出し、
    /// 候補が無ければそう知らせる。
    fn update_completion(&mut self, manual: bool) {
        self.completion = None;
        if self.focus != Focus::Editor {
            return;
        }
        let Some(tab) = self.tabs.get(self.active) else { return };
        let (a, h) = tab.doc.selection();
        if tab.loading || a != h {
            return;
        }
        let Some(prefix) = zc_intel::prefix_at(tab.doc.as_lines(), h) else {
            if manual {
                self.say("補完: カーソルの前に単語が無い", false);
            }
            return;
        };
        if !manual && prefix.chars().count() < intel::AUTO_MIN_CHARS {
            return;
        }
        let mut text = String::new();
        tab.doc.line(h.line, &mut text);
        // 単語の途中（カーソルの後ろにも単語の文字が続く）では自動で出さない
        if !manual && text.chars().nth(h.col as usize).is_some_and(|c| c.is_alphanumeric() || c == '_') {
            return;
        }
        // 文字列・コメントの中では出さない
        if let Some(hl) = tab.hl.as_ref() {
            let mut spans = Vec::new();
            hl.hl.spans(h.line, &text, &mut spans);
            let byte = text.char_indices().nth(h.col as usize).map_or(text.len(), |(b, _)| b);
            if intel::in_string_or_comment(&spans, byte) {
                return;
            }
        }
        let keywords = tab.lang.info().map_or(&[][..], |i| i.keywords);
        let items = zc_intel::complete(&tab.words, keywords, &prefix, h, intel::MAX_ITEMS);
        if items.is_empty() {
            if manual {
                self.say(format!("「{prefix}」の候補は無い"), false);
            }
            return;
        }
        self.completion = Some(CompState { items, sel: 0, prefix });
    }

    /// 選んでいる候補で、打っている単語を置き換える（1 回の取り消しで戻せる）。
    fn accept_completion(&mut self) {
        let Some(c) = self.completion.take() else { return };
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Some(item) = c.items.get(c.sel) else { return };
        let (_, h) = tab.doc.selection();
        let start = Pos::new(h.line, h.col.saturating_sub(c.prefix.chars().count() as u32));
        tab.doc.break_undo_group();
        if let Some(ch) = tab.doc.replace_range(start, h, &item.label) {
            tab.edited(ch);
        }
        tab.doc.break_undo_group();
        tab.sync_cursor();
    }

    // --- アウトライン（モック 14） ---

    fn open_outline(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let Some(spec) = tab.lang.info().and_then(|i| i.outline.as_ref()) else {
            let msg = format!("{} にはアウトラインが無い", tab.lang.name);
            self.say(msg, false);
            return;
        };
        let hl = tab.hl.as_ref().map(|h| &h.hl);
        let items = intel::with_outline_rules(spec, |rules| {
            zc_intel::outline(tab.doc.as_lines(), rules, &mut |l, text, out| {
                out.clear();
                if let Some(h) = hl {
                    h.spans(l, text, out);
                }
            })
        });
        // カーソルのいる項目（カーソルより前で最後のもの）を選んでおく
        let cur = tab.doc.selection().1.line;
        let rows = intel::filter_outline(&items, "");
        let sel = rows.iter().rposition(|r| r.line.is_some_and(|l| l <= cur)).unwrap_or(0);
        let title = format!("アウトライン — {}", tab.title);
        self.picker.set_title(&title);
        self.picker.set_empty_text("項目が無い");
        self.outline = Some(OutlineState { items, query: String::new(), rows, sel });
        self.completion = None;
        self.sync_picker();
        self.picker.invalidate();
        self.status_due = true;
    }

    fn close_outline(&mut self) {
        self.outline = None;
        self.panel.invalidate();
        self.tree.invalidate();
        self.tabbar.invalidate();
        self.empty_drawn = None;
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.view.invalidate();
        }
        self.status_due = true;
    }

    fn sync_picker(&mut self) {
        if let Some(o) = &self.outline {
            self.picker.set(&o.query, o.rows.iter().map(|r| r.row.clone()).collect(), o.sel);
        }
    }

    fn outline_key(&mut self, ev: &KeyEvent) {
        self.status_due = true;
        let Some(o) = self.outline.as_mut() else { return };
        // 選べる行（親として残しただけの行は飛ばす）の間を動く
        fn step(o: &OutlineState, dir: i32) -> usize {
            let n = o.rows.len() as i32;
            let mut i = o.sel as i32;
            for _ in 0..n {
                i = (i + dir).rem_euclid(n.max(1));
                if o.rows[i as usize].line.is_some() {
                    return i as usize;
                }
            }
            o.sel
        }
        match ev.key {
            Key::Esc => {
                self.close_outline();
                return;
            }
            Key::Enter => {
                let line = o.rows.get(o.sel).and_then(|r| r.line);
                self.close_outline();
                if let (Some(line), Some(tab)) = (line, self.tabs.get_mut(self.active)) {
                    let pos = Pos::new(line, 0);
                    tab.doc.break_undo_group();
                    tab.doc.set_selection(pos, pos);
                    tab.sync_cursor();
                }
                return;
            }
            Key::Up if !o.rows.is_empty() => o.sel = step(o, -1),
            Key::Down if !o.rows.is_empty() => o.sel = step(o, 1),
            Key::Backspace => {
                o.query.pop();
                o.rows = intel::filter_outline(&o.items, &o.query);
                o.sel = o.rows.iter().position(|r| r.line.is_some()).unwrap_or(0);
            }
            _ => {
                let Some(c) = ev.text.filter(|c| !c.is_control()) else { return };
                if ev.mods.ctrl() {
                    return;
                }
                o.query.push(c);
                o.rows = intel::filter_outline(&o.items, &o.query);
                o.sel = o.rows.iter().position(|r| r.line.is_some()).unwrap_or(0);
            }
        }
        self.sync_picker();
    }

    /// 操作を実行する。
    pub fn exec(&mut self, cmd: Command, cx: &mut LoopCx<'_>) {
        let confirm = self.confirm.take();
        match cmd {
            Command::Quit => {
                let running = self.term.as_ref().is_some_and(|t| t.running.is_some());
                if confirm != Some(Confirm::Quit) && (running || self.tabs.iter().any(|t| t.doc.is_modified())) {
                    self.confirm = Some(Confirm::Quit);
                    let what = if running { "実行中のプログラムがある" } else { "未保存のファイルがある" };
                    self.say(format!("{what}。もう一度 Ctrl+Q で終了"), true);
                } else {
                    self.flush_skk(cx);
                    cx.quit = true;
                }
            }
            Command::ToggleTree => {
                let open = !self.tree_open;
                self.set_tree_open(open);
            }
            Command::FocusTree => {
                self.set_tree_open(true);
                self.set_focus(Focus::Tree);
            }
            Command::FocusEditor | Command::Cancel => {
                if !self.tabs.is_empty() {
                    self.set_focus(Focus::Editor);
                }
                // 選択を外す
                if let Some(t) = self.tabs.get_mut(self.active) {
                    let (_, h) = t.doc.selection();
                    t.doc.set_selection(h, h);
                    t.sync_cursor();
                }
            }
            Command::TreeOpen => {
                if let Some(path) = self.tree.activate() {
                    self.open_path(&path, cx);
                    self.set_focus(Focus::Editor);
                }
            }
            Command::NextTab | Command::PrevTab if !self.tabs.is_empty() => {
                let n = self.tabs.len();
                let i = if cmd == Command::NextTab { (self.active + 1) % n } else { (self.active + n - 1) % n };
                self.tabs[self.active].doc.break_undo_group();
                self.activate(i);
            }
            Command::CloseTab if !self.tabs.is_empty() => {
                let t = &self.tabs[self.active];
                if t.doc.is_modified() && confirm != Some(Confirm::Close(t.id)) {
                    self.confirm = Some(Confirm::Close(t.id));
                    self.say("未保存。もう一度 Ctrl+W で閉じる", true);
                } else {
                    self.close_tab(self.active);
                }
            }
            Command::NewFile => {
                let n = self.tabs.iter().filter(|t| t.path.is_none()).count();
                let title = if n == 0 { "untitled".to_string() } else { format!("untitled-{}", n + 1) };
                let doc = self.cfg.backend.empty();
                self.open_doc(None, &title, doc, None);
                self.set_focus(Focus::Editor);
            }
            Command::Save => self.save_active(cx),
            // 候補は打鍵の処理の最後で出す（handle_key）
            Command::Complete => {}
            Command::Run => self.run_active(cx),
            Command::ToggleTerminal => self.toggle_terminal(),
            Command::ToggleTerminalFullscreen => {
                if self.panel_open {
                    let full = !self.panel_full;
                    self.set_panel(true, full);
                } else {
                    self.panel_full = true;
                    self.open_terminal();
                }
            }
            Command::ShowOutline if !self.tabs.is_empty() => self.open_outline(),
            Command::TogglePreview if self.focus == Focus::Editor => self.toggle_preview(),
            Command::ToggleCheckbox if self.focus == Focus::Editor => self.toggle_checkbox(),
            Command::Bold if self.focus == Focus::Editor => self.wrap_selection("**"),
            Command::Italic if self.focus == Focus::Editor => self.wrap_selection("*"),
            Command::GotoLine | Command::Find if !self.tabs.is_empty() => {
                let kind = if cmd == Command::Find { PromptKind::Find } else { PromptKind::GotoLine };
                let text = if kind == PromptKind::Find { self.last_find.clone() } else { String::new() };
                self.prompt = Some(Prompt { kind, text });
                self.status_due = true;
            }
            Command::FindNext | Command::FindPrev => {
                let needle = self.last_find.clone();
                self.find(&needle, cmd == Command::FindPrev);
            }
            _ if self.focus == Focus::Tree => self.exec_tree(cmd, cx),
            _ => self.exec_edit(cmd),
        }
    }

    /// 入力がツリーにあるときの操作。
    fn exec_tree(&mut self, cmd: Command, cx: &mut LoopCx<'_>) {
        match cmd {
            Command::Move { motion, .. } => match motion {
                Motion::Up => self.tree.move_sel(-1),
                Motion::Down => self.tree.move_sel(1),
                Motion::PageUp => self.tree.move_sel(-10),
                Motion::PageDown => self.tree.move_sel(10),
                Motion::DocStart | Motion::LineStart => self.tree.move_sel(i32::MIN / 2),
                Motion::DocEnd | Motion::LineEnd => self.tree.move_sel(i32::MAX / 2),
                Motion::Right => self.tree.expand_selected(),
                Motion::Left => self.tree.collapse_selected(),
                _ => {}
            },
            Command::Newline => self.exec(Command::TreeOpen, cx),
            _ => {}
        }
    }

    /// 文書を編集する操作・カーソルを動かす操作。
    fn exec_edit(&mut self, cmd: Command) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if tab.loading {
            return;
        }
        let page = tab.view.page_lines();
        let doc = &mut tab.doc;
        let change = match cmd {
            Command::InsertChar(c) => doc.insert(c.encode_utf8(&mut [0u8; 4])),
            Command::Newline => {
                // 前の行の字下げを引き継ぐ。開き括弧や `:` で終わっていれば 1 段深くする
                let (a, h) = doc.selection();
                let at = a.min(h);
                let mut line = String::new();
                doc.line(at.line, &mut line);
                let before: String = line.chars().take(at.col as usize).collect();
                let mut s = String::from("\n");
                s.extend(before.chars().take_while(|c| *c == ' ' || *c == '\t'));
                if before.trim_end().ends_with([':', '{', '[', '(']) {
                    s.push_str(&tab.lang.indent);
                }
                doc.insert(&s)
            }
            Command::Backspace => doc.delete_backward(),
            Command::DeleteForward => doc.delete_forward(),
            Command::Undo => doc.undo(),
            Command::Redo => doc.redo(),
            Command::Copy | Command::Cut => {
                let (a, h) = doc.selection();
                if a == h {
                    None
                } else {
                    self.clipboard = doc.selected_text();
                    if cmd == Command::Cut {
                        doc.break_undo_group();
                        doc.insert("")
                    } else {
                        None
                    }
                }
            }
            Command::Paste if !self.clipboard.is_empty() => {
                doc.break_undo_group();
                let c = doc.insert(&self.clipboard);
                doc.break_undo_group();
                c
            }
            Command::SelectAll => {
                doc.select_all();
                None
            }
            Command::Indent => {
                let (a, h) = doc.selection();
                if a.line != h.line {
                    doc.indent_lines(&tab.lang.indent, false)
                } else {
                    // Tab は直前の文字入力と別の取り消し単位にする
                    let s = if tab.lang.indent.starts_with('\t') {
                        "\t".to_string()
                    } else {
                        // 次の字下げ位置まで空白を入れる
                        let w = tab.lang.indent.chars().count().max(1) as u32;
                        " ".repeat((w - a.min(h).col % w) as usize)
                    };
                    doc.break_undo_group();
                    let c = doc.insert(&s);
                    doc.break_undo_group();
                    c
                }
            }
            Command::Outdent => doc.indent_lines(&tab.lang.indent, true),
            Command::DeleteLine => doc.delete_lines(),
            Command::ToggleLineComment => match tab.lang.comment.as_deref() {
                Some(m) => doc.toggle_line_comment(m),
                None => None,
            },
            Command::Move { motion, select } => {
                doc.break_undo_group();
                doc.move_cursor(motion, select, page);
                // 1 画面ぶんの移動は、表示位置も 1 画面ぶん動かす（カーソルの画面上の高さを保つ）
                match motion {
                    Motion::PageDown => tab.view.scroll_lines(page as i32),
                    Motion::PageUp => tab.view.scroll_lines(-(page as i32)),
                    _ => {}
                }
                None
            }
            _ => {
                // 段階 3 以降の操作（実行・ターミナル・Markdown・日本語入力・一覧）
                self.message = Some(("この操作はまだ使えない".into(), false));
                self.status_due = true;
                return;
            }
        };
        if let Some(c) = change {
            tab.edited(c);
        }
        tab.sync_cursor();
    }

    fn save_active(&mut self, cx: &mut LoopCx<'_>) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if tab.loading {
            return;
        }
        let Some(path) = tab.path.clone() else {
            self.prompt = Some(Prompt { kind: PromptKind::SaveAs, text: String::new() });
            self.status_due = true;
            return;
        };
        tab.doc.break_undo_group();
        let req = SaveRequest { doc: tab.id, path, version: tab.doc.version(), write: tab.doc.save_writer() };
        self.saver.save(req, cx);
    }

    fn find(&mut self, needle: &str, backward: bool) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if needle.is_empty() {
            return;
        }
        let (a, h) = tab.doc.selection();
        // 前へ探すときは選択の先頭から、後ろへ探すときは選択の末尾から
        let from = if backward { a.min(h) } else { a.max(h) };
        match tab.doc.find(needle, from, backward) {
            Some((s, e)) => {
                tab.doc.set_selection(s, e);
                tab.sync_cursor();
            }
            None => self.say(format!("「{needle}」は見つからない"), false),
        }
    }

    /// 入力欄が開いているあいだの打鍵。
    fn prompt_key(&mut self, ev: &KeyEvent, cx: &mut LoopCx<'_>) {
        use zc_core::Key;
        self.status_due = true;
        let Some(p) = self.prompt.as_mut() else { return };
        match ev.key {
            Key::Esc => self.prompt = None,
            Key::Backspace => {
                p.text.pop();
            }
            Key::Enter => {
                let (kind, text) = (p.kind, p.text.clone());
                match kind {
                    PromptKind::GotoLine => {
                        self.prompt = None;
                        if let (Ok(n), Some(tab)) = (text.trim().parse::<u32>(), self.tabs.get_mut(self.active)) {
                            let pos = Pos::new(n.saturating_sub(1), 0);
                            tab.doc.break_undo_group();
                            tab.doc.set_selection(pos, pos);
                            tab.sync_cursor();
                        }
                    }
                    PromptKind::Find => {
                        // Enter は次を探す（欄は開いたまま。Esc で閉じる）
                        self.last_find = text.clone();
                        self.find(&text, ev.mods.shift());
                    }
                    PromptKind::SaveAs => {
                        self.prompt = None;
                        let name = text.trim();
                        if name.is_empty() {
                            return;
                        }
                        let path = if Path::new(name).is_absolute() { PathBuf::from(name) } else { self.cfg.root.join(name) };
                        if let Some(tab) = self.tabs.get_mut(self.active) {
                            tab.title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| name.into());
                            tab.path = Some(path);
                            tab.detect_language();
                            self.tabs_dirty = true;
                            self.save_active(cx);
                        }
                    }
                }
            }
            _ => {
                if let Some(c) = ev.text {
                    if p.kind != PromptKind::GotoLine || c.is_ascii_digit() {
                        p.text.push(c);
                    }
                }
            }
        }
    }

    // --- ハイライト ---

    /// 編集で無効になった行頭の文脈を計算し直す。1 回の予算を超える分は batch に出す
    /// （`cx` があるときだけ）。
    fn pump_highlight(&mut self, cx: Option<&mut LoopCx<'_>>) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Some(h) = tab.hl.as_mut() else { return };
        if !h.dirty || tab.loading {
            return;
        }
        h.dirty = false;
        let r = h.hl.update(tab.doc.as_lines(), HL_BUDGET);
        if r.complete {
            return;
        }
        let Some(cx) = cx else {
            h.dirty = true; // 次に cx があるときにやり直す
            return;
        };
        let version = tab.doc.version();
        if h.rest_pending == Some(version) {
            return;
        }
        if let Some((from, state)) = h.hl.rest_request() {
            h.rest_pending = Some(version);
            let (snap, token, tx, lang, id) = (tab.doc.snapshot_lines(), tab.doc.token(), cx.results.clone(), h.lang, tab.id);
            cx.workers.batch.submit(move || {
                if let Ok(rest) = Highlighter::compute_rest(lang, &*snap, from, state, &token) {
                    tx.send(AppEvent::HighlightRest { doc: id, rest });
                }
            });
        }
    }

    // --- Markdown ---

    /// 全体の解析が要る Markdown を解析する。小さければメインで、大きければ batch で。
    fn pump_markdown(&mut self, cx: &mut LoopCx<'_>) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Some(m) = tab.md.as_ref() else { return };
        if !m.stale || tab.loading {
            return;
        }
        if tab.parse_markdown_here() {
            return;
        }
        let version = tab.doc.version();
        let m = tab.md.as_mut().expect("md");
        if m.pending == Some(version) {
            return;
        }
        m.pending = Some(version);
        let (snap, token, tx, id) = (tab.doc.snapshot_lines(), tab.doc.token(), cx.results.clone(), tab.id);
        cx.workers.batch.submit(move || {
            if let Ok(parsed) = MdDocument::parse(&*snap, &token) {
                tx.send(AppEvent::MarkdownParsed { doc: id, parsed });
            }
        });
    }

    /// プレビュー ⇔ ソース表示（Ctrl+Shift+V。同じバッファ、同じカーソル位置。モック 10・11）。
    fn toggle_preview(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Some(m) = tab.md.as_mut() else {
            self.say("プレビューは Markdown だけ", false);
            return;
        };
        m.preview = !m.preview;
        tab.view.set_layouter(m.layouter());
        tab.sync_cursor();
        // 補完の小窓などは描き直したビューの上に置き直す
        self.popup.forget();
        self.cand_popup.forget();
        self.status_due = true;
    }

    /// カーソル行のチェックボックス（`- [ ]` / `- [x]`）を切り替える（Ctrl+Enter）。
    fn toggle_checkbox(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Some(m) = tab.md.as_ref() else { return };
        let (a, h) = tab.doc.selection();
        let mut text = String::new();
        tab.doc.line(h.line, &mut text);
        let Some(new) = m.doc.borrow().toggle_checkbox(h.line, &text) else {
            self.say("チェックボックスの行ではない", false);
            return;
        };
        let end = Pos::new(h.line, text.chars().count() as u32);
        tab.doc.break_undo_group();
        if let Some(c) = tab.doc.replace_range(Pos::new(h.line, 0), end, &new) {
            tab.edited(c);
        }
        tab.doc.break_undo_group();
        // 記号の長さは変わらないので、カーソルはそのまま
        let n = new.chars().count() as u32;
        let clamp = |p: Pos| if p.line == h.line { Pos::new(p.line, p.col.min(n)) } else { p };
        tab.doc.set_selection(clamp(a), clamp(h));
        tab.sync_cursor();
    }

    /// 選択範囲を `mark` で囲む（Alt+B は `**`、Alt+I は `*`）。選択が無ければ記号の組を
    /// 入れて、その間にカーソルを置く。
    fn wrap_selection(&mut self, mark: &str) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if tab.loading || tab.md.is_none() {
            self.say("太字・斜体は Markdown だけ", false);
            return;
        }
        let (a, h) = tab.doc.selection();
        let (s, e) = (a.min(h), a.max(h));
        let inner = tab.doc.selected_text();
        let k = mark.chars().count() as u32;
        tab.doc.break_undo_group();
        if let Some(c) = tab.doc.replace_range(s, e, &format!("{mark}{inner}{mark}")) {
            tab.edited(c);
        }
        tab.doc.break_undo_group();
        // 囲んだ中身を選び直す（1 行の中なら）。複数行なら閉じの記号の後ろ
        if s.line == e.line {
            let (ns, ne) = (Pos::new(s.line, s.col + k), Pos::new(e.line, e.col + k));
            if a <= h {
                tab.doc.set_selection(ns, ne);
            } else {
                tab.doc.set_selection(ne, ns);
            }
        }
        tab.sync_cursor();
    }

    // --- ステータスバー ---

    fn update_status(&mut self) {
        self.status_due = false;
        let mut bg = palette::STATUS;
        if self.outline.is_some() {
            self.status.set_left("アウトライン");
            self.status.set_right("Enter 移動  Esc 閉じる");
            self.status.set_chips(Vec::new());
        } else if let Some(p) = &self.prompt {
            let label = match p.kind {
                PromptKind::GotoLine => "行へ移動",
                PromptKind::Find => "検索",
                PromptKind::SaveAs => "保存する名前",
            };
            self.status.set_left(&format!("{label}: {}_", p.text));
            self.status.set_right("Enter 確定  Esc 閉じる");
            self.status.set_chips(Vec::new());
        } else {
            match self.tabs.get(self.active) {
                Some(t) => {
                    let (_, h) = t.doc.selection();
                    let st = EditorStatus {
                        file: &t.title,
                        modified: t.doc.is_modified(),
                        line: h.line,
                        col: h.col,
                        language: &t.lang.name,
                        extra: "UTF-8",
                        mods: self.mods,
                        ime: Some(match self.skk.mode() {
                            InputMode::Ascii => ImeMode::Ascii,
                            InputMode::Hiragana => ImeMode::Kana,
                            InputMode::Katakana => ImeMode::Kata,
                        }),
                    };
                    st.apply(&mut self.status);
                    if let Some(m) = &t.md {
                        let left = format!("{}  {}", self.status.left(), if m.preview { "プレビュー" } else { "ソース" });
                        self.status.set_left(&left);
                    }
                    if !t.diags.is_empty() {
                        let left = format!("{}  ⊗ {}", self.status.left(), t.diags.len());
                        self.status.set_left(&left);
                    }
                    // カーソルがエラーの行にあれば、そのメッセージを右に出す（モック 13）
                    if let Some(d) = t.diag_at_cursor() {
                        let right = format!("{} 行: {}", d.line + 1, d.message);
                        self.status.set_right(&right);
                    }
                }
                None => {
                    self.status.set_left("zcode");
                    self.status.set_right("Ctrl+E ツリー  Ctrl+N 新規  Ctrl+Q 終了");
                    self.status.set_chips(Vec::new());
                }
            }
            // 辞書登録中（モック 9）・ターミナルでの未確定
            if self.skk.registration().is_some() {
                self.status.set_left("Enter 登録  Ctrl+G やめる");
            }
            // 実行中は橙、終わったら終了コード（モック 3・4）
            if let Some(run) = self.term.as_ref().and_then(|t| t.running.as_ref()) {
                let secs = now_us().saturating_sub(run.started_us) / 1_000_000;
                self.status.set_left(&format!("▶ 実行中 {}  {secs}s", run.file));
                bg = palette::STATUS_RUN;
            } else if let Some((s, c)) = &self.run_status {
                self.status.set_left(s);
                bg = *c;
            }
            if let Some((m, error)) = &self.message {
                if self.message_right && !self.tabs.is_empty() {
                    self.status.set_right(m);
                } else {
                    self.status.set_left(m);
                }
                if *error {
                    bg = STATUS_ERROR;
                }
            }
        }
        self.status.set_bg(bg);
    }
}

impl Screen for EditorScreen {
    fn start(&mut self, cx: &mut LoopCx<'_>) {
        for f in self.cfg.files.clone() {
            self.open_path(&f, cx);
        }
        self.pump_highlight(Some(cx));
        self.blinks_left = BLINKS_AFTER_KEY;
        cx.timers.set_after(TimerId::CursorBlink, BLINK);
    }

    fn on_keys(&mut self, keys: &[KeyEvent], _raw: &[RawRecord], kb: &Keyboard, cx: &mut LoopCx<'_>) {
        self.keys(keys, kb.translator().mods(), cx);
    }

    fn on_event(&mut self, ev: AppEvent, cx: &mut LoopCx<'_>) {
        match ev {
            AppEvent::Opened { doc, result } => {
                let Some(i) = self.tabs.iter().position(|t| t.id == doc) else { return };
                match result {
                    Ok((d, words)) => {
                        let t = &mut self.tabs[i];
                        t.doc = d;
                        t.words = words;
                        t.loading = false;
                        t.detect_language();
                        t.view.invalidate();
                        t.sync_cursor();
                        self.status_due = true;
                    }
                    Err(e) => {
                        let title = self.tabs[i].title.clone();
                        self.close_tab(i);
                        self.say(format!("{title} を開けない: {e}"), true);
                    }
                }
            }
            AppEvent::Saved { doc, version, result } => {
                let Some(t) = self.tabs.iter_mut().find(|t| t.id == doc) else { return };
                match result {
                    Ok(ok) => {
                        // 保存を頼んだあとに編集していたら、未保存のまま
                        if t.doc.version() == version {
                            t.doc.mark_saved();
                        }
                        // F5 で保存を待っていたら実行する（保存した版のままのときだけ）
                        let run_now = self.pending_run == Some(doc) && t.doc.version() == version;
                        if self.pending_run == Some(doc) {
                            self.pending_run = None;
                        }
                        // 保存した中身で構文チェック（batch で使い捨て。§10.2）
                        if let (Some((cmd, pat)), Some(path)) = (t.lang.info().and_then(intel::check_command), t.path.clone()) {
                            let tx = cx.results.clone();
                            cx.workers.batch.submit(move || {
                                let result = zc_intel::run_check(cmd, pat, &path, &CancelToken::never()).map_err(|e| e.to_string());
                                tx.send(AppEvent::Checked { doc, result });
                            });
                        }
                        let title = t.title.clone();
                        let path = t.path.clone();
                        self.tree.refresh();
                        if self.tabs.get(self.active).is_some_and(|a| a.id == doc) {
                            self.tree.set_current(path.as_deref());
                        }
                        match ok {
                            SaveOk::Written { overwrote_external_change: false } => self.say(format!("保存した: {title}"), false),
                            SaveOk::Written { overwrote_external_change: true } => {
                                self.say(format!("保存した: {title}（ほかで変更されていた内容を上書きした）"), true)
                            }
                            SaveOk::Unchanged => self.say(format!("変更なし: {title}"), false),
                        }
                        if run_now && self.tabs.get(self.active).is_some_and(|a| a.id == doc) {
                            self.start_run(cx);
                        }
                    }
                    Err(e) => {
                        let title = t.title.clone();
                        self.say(format!("{title} を保存できない: {e}"), true);
                    }
                }
            }
            AppEvent::SkkCandidates(v) => {
                self.skk_waiting = self.skk_waiting.saturating_sub(1);
                self.skk.candidates_ready(v);
                if self.cand_popup.drawn().is_some() {
                    self.cand_stale = true;
                }
                self.status_due = true;
                // 待っていた間の打鍵を流す
                let held = std::mem::take(&mut self.skk_held);
                if self.skk_waiting == 0 && !held.is_empty() {
                    self.feed_keys(held, cx);
                } else {
                    self.skk_held = held;
                }
            }
            AppEvent::Checked { doc, result } => {
                let Some(t) = self.tabs.iter_mut().find(|t| t.id == doc) else { return };
                match result {
                    Ok(diags) => {
                        let first = diags.first().map(|d| format!("{} 行: {}", d.line + 1, d.message));
                        let had = !t.diags.is_empty();
                        t.diags = diags;
                        t.view.set_marks(intel::marks_of(&t.diags));
                        self.tabs_dirty = true;
                        self.status_due = true;
                        if let Some(m) = first {
                            self.say_right(m, true);
                        } else if had {
                            self.say("構文エラーは無くなった", false);
                        }
                    }
                    // チェックの道具が無い・時間切れ: 波線はそのまま、知らせるだけ
                    Err(e) => self.say(format!("構文チェックできない: {e}"), false),
                }
            }
            AppEvent::HighlightRest { doc, rest } => {
                let Some(t) = self.tabs.iter_mut().find(|t| t.id == doc) else { return };
                let version = t.doc.version();
                if let Some(h) = t.hl.as_mut() {
                    h.rest_pending = None;
                    if h.hl.apply_rest(rest, version) {
                        t.view.recheck_all();
                    }
                }
            }
            AppEvent::MarkdownParsed { doc, parsed } => {
                let Some(t) = self.tabs.iter_mut().find(|t| t.id == doc) else { return };
                let version = t.doc.version();
                if let Some(m) = t.md.as_mut() {
                    m.pending = None;
                    // 頼んだ後に編集していたら捨てる（stale のまま。pump_markdown が頼み直す）
                    if m.doc.borrow_mut().replace(parsed, version) {
                        m.stale = false;
                        if m.preview {
                            t.view.recheck_all();
                        }
                    }
                }
            }
            AppEvent::Flushed(_) | AppEvent::Store(_) => {}
        }
        self.pump_highlight(Some(cx));
        self.pump_markdown(cx);
    }

    fn on_timer(&mut self, id: TimerId, cx: &mut LoopCx<'_>) {
        match id {
            TimerId::StatusDelay => self.status_due = true,
            TimerId::SkkFlush => self.flush_skk(cx),
            TimerId::Tick => {
                if self.term.as_ref().is_some_and(|t| t.running.is_some()) {
                    self.status_due = true;
                    cx.timers.set_after(TimerId::Tick, RUN_TICK);
                }
            }
            TimerId::CursorBlink => {
                if self.blinks_left > 0 {
                    self.blinks_left -= 1;
                    self.cursor_on = !self.cursor_on;
                    cx.timers.set_after(TimerId::CursorBlink, BLINK);
                } else {
                    // 点いた状態で止める（次の打鍵まで何もしない）
                    self.cursor_on = true;
                }
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.view.set_cursor_on(self.cursor_on);
                }
                self.panel.set_cursor_on(self.cursor_on);
            }
            _ => {}
        }
    }

    fn on_glyphs_ready(&mut self) {
        self.tabbar.invalidate();
        self.tree.invalidate();
        self.status.invalidate();
        self.picker.invalidate();
        self.popup.invalidate();
        self.panel.invalidate();
        self.empty_drawn = None;
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.view.invalidate();
        }
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) -> Damage {
        let panel_h = match (self.panel_open, self.panel_full) {
            (false, _) => 0,
            (true, false) => TERM_PANEL_H,
            (true, true) => u16::MAX,
        };
        let mut regions = Regions::compute(self.tree_open, panel_h);
        // SKK の辞書登録欄（モック 9）: ステータスバーの上に 1 行。その分ほかを縮める
        let mini_area = self.skk.registration().map(|_| Rect::new(0, (regions.status.y as i32 - MINI_H as i32) as i16, Rect::SCREEN.w, MINI_H));
        if let Some(m) = mini_area {
            for r in [&mut regions.editor, &mut regions.side, &mut regions.panel] {
                if r.bottom() > m.y as i32 {
                    r.h = (m.y as i32 - r.y as i32).max(0) as u16;
                }
            }
        }
        if self.mini_open != mini_area.is_some() {
            self.mini_open = mini_area.is_some();
            self.tree.invalidate();
            self.panel.invalidate();
            self.mini.invalidate();
            self.cand_popup.forget();
            self.status_due = true;
        }
        let cand = if matches!(self.focus, Focus::Editor | Focus::Terminal) { self.skk_candidate_rows() } else { None };
        // ターミナルに入力があるときの候補一覧は、パネルを描いた後に、端末のカーソルの所へ置く
        let cand_on_panel = self.focus == Focus::Terminal && mini_area.is_none() && self.panel_open;
        let mut damage = Damage::none();
        // タブバー（未保存の印が変わったときも作り直す）
        let changed = self.tabs_dirty || self.tabs.len() != self.tabs_shown.len() || self.tabs.iter().zip(&self.tabs_shown).any(|(t, s)| (t.id, t.doc.is_modified(), t.has_error()) != *s);
        if changed {
            self.tabs_dirty = false;
            self.tabs_shown = self.tabs.iter().map(|t| (t.id, t.doc.is_modified(), t.has_error())).collect();
            let tabs = self.tabs.iter().map(|t| Tab { doc: t.id, title: t.title.clone(), modified: t.doc.is_modified(), error: t.has_error() }).collect();
            self.tabbar.set_tabs(tabs, self.active);
            self.tabbar.set_inset(if self.tree_open { SIDE_W } else { 0 });
            self.status_due = true;
        }
        damage |= self.tabbar.render(cx, regions.tabs);
        if self.outline.is_some() {
            // 本文（ツリーとエディタ）の全体を覆う（モック 14）
            let body = Rect::new(0, regions.tabs.bottom() as i16, Rect::SCREEN.w, (regions.status.y as i32 - regions.tabs.bottom()) as u16);
            damage |= self.picker.render(cx, body);
            if self.status_due {
                self.update_status();
            }
            damage |= self.status.render(cx, regions.status);
            return damage;
        }
        if self.tree_open {
            damage |= self.tree.render(cx, regions.side);
        }
        self.pump_highlight(None);
        let want_popup = self.completion.is_some() && self.focus == Focus::Editor;
        match self.tabs.get_mut(self.active) {
            Some(t) => {
                // 小窓を閉じる・置き直すときは、下にあった行を描き直させる
                if let Some(old) = self.popup.drawn() {
                    if !want_popup || std::mem::take(&mut self.popup_stale) {
                        t.view.invalidate_rows(old.y as i32, old.bottom());
                        self.popup.forget();
                    }
                }
                if let Some(old) = self.cand_popup.drawn() {
                    if cand.is_none() || std::mem::take(&mut self.cand_stale) {
                        t.view.invalidate_rows(old.y as i32, old.bottom());
                        if old.bottom() > regions.panel.y as i32 {
                            self.panel.invalidate();
                        }
                        self.cand_popup.forget();
                    }
                }
                // SKK の未確定は、カーソルの位置に差し込んで見せる（文書は変えない。モック 7・8）
                let pre = if self.focus == Focus::Editor && mini_area.is_none() { preedit_view(self.skk.preedit()) } else { None };
                let pre = pre.or_else(|| {
                    // 登録欄を開いている間、本文には変換しようとしていた読みを ▼ で残す（モック 9）
                    (self.focus == Focus::Editor && mini_area.is_some()).then(|| PreeditView::converting(format!("▼{}", self.skk.registration().map_or("", |r| r.reading.as_str()))))
                });
                t.view.set_preedit(pre);
                let mut spans = TabSpans { custom: t.custom_spans.as_deref_mut(), hl: t.hl.as_ref().map(|h| &h.hl) };
                let view_damage = t.view.render(cx, regions.editor, t.doc.as_lines(), &mut spans);
                damage |= view_damage;
                if let (true, Some(c), Some(caret)) = (want_popup, self.completion.as_ref(), t.view.caret_rect()) {
                    let items = c
                        .items
                        .iter()
                        .map(|i| {
                            let (tag, tag_color) = match i.kind {
                                CompletionKind::Word => ("ab", palette::STATUS),
                                CompletionKind::Keyword => ("kw", palette::CONTROL),
                            };
                            PopupItem { tag, tag_color, label: i.label.clone(), hit_chars: c.prefix.chars().count() }
                        })
                        .collect();
                    self.popup.set(items, c.sel);
                    let (_, h) = t.doc.selection();
                    let word = Pos::new(h.line, h.col.saturating_sub(c.prefix.chars().count() as u32));
                    let word_x = t.view.point_of(word).map_or(caret.x as i32, |r| r.x as i32);
                    let rect = self.popup.place(cx, caret, word_x, regions.editor);
                    damage |= self.popup.render_at(cx, rect, !view_damage.is_empty());
                }
                // SKK の候補一覧（モック 8）。▼ の語の下（登録欄の中の変換なら登録欄の上）
                if let (false, Some((rows, sel, page))) = (cand_on_panel, cand.clone()) {
                    let anchor = match mini_area {
                        Some(m) => Some(Rect::new(m.x + 4, m.y, 1, m.h)),
                        None => t.view.preedit_rect().or_else(|| t.view.caret_rect()),
                    };
                    if let Some(anchor) = anchor {
                        self.cand_popup.set(rows, sel, &page);
                        let bounds = Rect::new(regions.editor.x, regions.editor.y, regions.editor.w, (mini_area.map_or(regions.editor.bottom(), |m| m.y as i32) - regions.editor.y as i32).max(0) as u16);
                        let rect = self.cand_popup.place(cx, anchor, bounds);
                        damage |= self.cand_popup.render_at(cx, rect, !view_damage.is_empty());
                    }
                }
            }
            None => {
                if self.empty_drawn != Some(regions.editor) {
                    self.empty_drawn = Some(regions.editor);
                    cx.canvas.fill_rect(regions.editor, palette::BG);
                    let ts = TextStyle::mono(UI_PX);
                    let r = regions.editor;
                    for (i, s) in ["Enter  ツリーで選んだファイルを開く", "Ctrl+N  新しいファイル", "Ctrl+B  ツリーを開く / 閉じる"].iter().enumerate() {
                        let row = Rect::new(r.x + 10, r.y + 10 + i as i16 * 13, r.w.saturating_sub(12), 13);
                        cx.text_in(row, s, &ts, palette::DIM, palette::BG);
                    }
                    damage |= Damage::rect(r);
                }
            }
        }
        // ターミナルのパネル（大きさが変わったら端末にも知らせる）
        if self.panel_open {
            let (title, hint) = self.panel_header();
            self.panel.set_header(&title, hint);
            let grid = TerminalPanel::grid_size(cx, regions.panel);
            self.term_grid = grid;
            // SKK の未確定はカーソルの位置に重ねて見せる（端末にはまだ送っていない）
            let pre = if self.focus == Focus::Terminal && mini_area.is_none() { preedit_text(self.skk.preedit()) } else { None };
            self.panel.set_preedit(pre);
            let mut panel_damage = Damage::none();
            match self.term.as_mut() {
                Some(t) => {
                    if t.term.size() != grid {
                        let _ = t.term.resize(grid.0, grid.1);
                    }
                    panel_damage |= self.panel.render_term(cx, regions.panel, &t.term);
                }
                None => panel_damage |= self.panel.render_empty(cx, regions.panel, ""),
            }
            damage |= panel_damage;
            // 候補一覧（ターミナル）。パネルの行が描き直されたら重ね直す
            if let (true, Some((rows, sel, page)), Some(anchor)) = (cand_on_panel, cand.clone(), self.panel.preedit_rect()) {
                // 文書が無いとき（上の描き直しを通らない）は、ここで置き直しの前にパネルを描き直させる
                if self.tabs.is_empty() && self.cand_popup.drawn().is_some() && std::mem::take(&mut self.cand_stale) {
                    self.panel.invalidate();
                    self.empty_drawn = None;
                    self.cand_popup.forget();
                }
                self.cand_popup.set(rows, sel, &page);
                let bounds = Rect::new(0, regions.tabs.bottom() as i16, Rect::SCREEN.w, (regions.status.y as i32 - regions.tabs.bottom()) as u16);
                let rect = self.cand_popup.place(cx, anchor, bounds);
                damage |= self.cand_popup.render_at(cx, rect, !panel_damage.is_empty());
            }
        }
        // 辞書登録欄（モック 9）
        if let (Some(area), Some(reg)) = (mini_area, self.skk.registration()) {
            let label = format!("[辞書登録] {}：", reg.reading);
            let pre = self.skk.registration_preedit().and_then(preedit_text);
            self.mini.set(&label, &reg.text, pre.as_ref().map(|(s, c)| (s.as_str(), *c)));
            damage |= self.mini.render(cx, area);
        }
        if self.status_due {
            self.update_status();
        }
        damage |= self.status.render(cx, regions.status);
        damage
    }

    fn poll_fd(&self) -> Option<std::os::fd::RawFd> {
        self.term.as_ref().map(|t| t.term.fd())
    }

    fn on_poll_fd(&mut self, _cx: &mut LoopCx<'_>) -> bool {
        self.pump_terminal()
    }

    fn wants_raw(&self) -> bool {
        // モディファイアだけの打鍵（ロックのタップ）でも起こしてもらい、札を更新する
        true
    }
}
