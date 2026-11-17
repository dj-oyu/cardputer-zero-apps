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
//! - 診断（波線・タブ名の赤・`⊗ n`）は、作業フォルダに 1 つの置き場（`zc_intel::DiagStore`）に
//!   出どころごとに持つ。保存したら、言語に `remote` があればビルド係の check を専用スレッド
//!   （[`RemoteLink`]）に頼み、対象でない・届かないときは実機の構文チェックを `check`
//!   ワーカーで走らせる。遅れて届いた結果は、タブごとの編集の記録（`ShiftLog`）で今の行へ
//!   ずらしてから取り込む（docs/stage3-design.md §8・§9）
//! - 打鍵が止まったときの check（`check_on_save = idle`）: `remote` のある言語で編集したら
//!   [`AUTO_CHECK_IDLE`] のタイマーを仕掛け直すだけ（打鍵ごとの仕事はこれだけ）。鳴ったら、
//!   未保存の文書の写しを重ねて（[`crate::remote::OverlaySource`]）ビルド係の check を頼む。
//!   診断の基準は頼んだときの文書の版。断られた・届かないときは何も出さず、次の保存まで休む
//! - カーソルの点滅は、最後の打鍵から約 10 秒で止める（点いたまま。待機中の CPU を 0% に）

use crate::app::{AppEvent, LoopCx, Screen, TimerId};
use crate::commands;
use crate::doc::{DocBackend, TextDoc};
use crate::intel::{self, CheckFail};
use crate::remote::{LspAsk, OverlaySource, RemoteClient, RemoteEvent, RemoteJob, RemoteReq};
use crate::save::{SaveOk, SaveRequest, Saver, StoreSaver};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::rc::Rc;
use std::time::Duration;
use zc_core::time::now_us;
use zc_core::{palette, CancelToken, Color, Command, Damage, DiagSource, DiagnosticSet, DocId, FileDiagnostics, Key, KeyEvent, LineChange, Mods, Motion, Pos, Rect, Span, Version};
use zc_font::TextStyle;
use zc_highlight::{Highlighter, LanguageId, LanguageInfo, Registry};
use zc_intel::{Completion, CompletionKind, DiagStore, Diagnostic, Severity, ShiftLog, WordIndex};
use zc_remote_ipc::{DoneStatus, FileDiags, JobKind, JobTag, Phase, RemoteLang, SubmitFlags, TestReport};
use zc_input::{Keyboard, RawRecord};
use zc_ui::layout::{Regions, SIDE_W, UI_PX};
use std::sync::OnceLock;
use zc_skk::{InputMode, LearnedWord, Preedit, PreeditState, Skk, SystemDict, UserDict};
use zc_term::{ReadBudget, Term, TermCommand, TermEvent};
use zc_markdown::MdDocument;
use zc_ui::{ShortcutList, ShortcutRow};
use zc_store::{BackupPolicy, DocBackup, Restore, Snapshot};
use zc_ui::{CandidatePopup, CandidateRow, CodeLayouter, MdLayouter, MiniBuffer, PreeditStyle, PreeditView, MINI_H};
use zc_ui::{CompletionPopup, EditorStatus, EditorView, FileTree, ImeMode, PickerList, PopupItem, RenderCx, SpanSource, StatusBar, Tab, TabBar, TerminalPanel, Widget, TERM_PANEL_H};

#[path = "editor_lsp.rs"]
mod lsp;

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
/// 控えておく「ジョブ → 保存済みの版」の数の上限（終わったジョブの分は消す。これは、
/// 終わりの知らせが届かなかったときの歯止め）。
const MAX_BINDINGS: usize = 16;
/// 最後の編集から、打鍵が止まったときの check を頼むまでの時間。
pub const AUTO_CHECK_IDLE: Duration = Duration::from_millis(1200);
/// ほかの相手（F5 の `zc-remote run` など）のジョブを、最後に知らせを見てから「動いている」と
/// みなす上限（serve が死んで終わりの知らせが届かなかったときの歯止め）。
const FOREIGN_JOB_MAX_US: u64 = 10 * 60 * 1_000_000;

/// ビルド係（`zc-remote` の相手をする専用スレッド）への口。画面は依頼を渡すだけで待たない。
/// 結果は [`AppEvent::Remote`] で届く。既定は本物の [`RemoteClient`]（最初の依頼で作る）。
/// 試験では偽物に差し替える（[`EditorScreen::set_remote`]）。
pub trait RemoteLink {
    /// ジョブを頼む。最後に必ず [`RemoteEvent::Done`] が 1 回届く。
    fn submit(&mut self, job: RemoteJob, cx: &mut LoopCx<'_>) -> RemoteReq;
    /// 打鍵が止まったときの check を頼んでよいか（ビルド係を使わない人に、スレッドや serve を
    /// 作らないため）。設定が `idle` かどうかは serve が決める（違えば断られる）。
    fn auto_check_ready(&mut self) -> bool {
        true
    }
    /// LSP の問い合わせ（docs/lsp-design.md）。最後に必ず [`RemoteEvent::LspResult`] が 1 回届く。
    /// ビルド係を使わない人（設定が無い）には `None`（何も作らない）。
    fn lsp(&mut self, ask: LspAsk, cx: &mut LoopCx<'_>) -> Option<RemoteReq> {
        let _ = (ask, cx);
        None
    }
    /// 先につないでおく（Rust のファイルを開いた。serve が rust-analyzer も起こす）。
    fn warm(&mut self, file: PathBuf, cx: &mut LoopCx<'_>) {
        let _ = (file, cx);
    }
}

/// 本物の専用スレッド。最初の依頼まで作らない（ビルド係を使わない人には、スレッドも
/// `zc-remote serve` も増えない）。
#[derive(Default)]
struct LazyRemote(Option<RemoteClient>);

impl RemoteLink for LazyRemote {
    fn submit(&mut self, job: RemoteJob, cx: &mut LoopCx<'_>) -> RemoteReq {
        self.0.get_or_insert_with(|| RemoteClient::new(cx.results.clone(), None)).submit(job)
    }
    fn auto_check_ready(&mut self) -> bool {
        // 保存時の check でもう作ってあるか、zc-remote の設定がある人だけ
        self.0.is_some() || crate::remote::remote_config_exists()
    }
    fn lsp(&mut self, ask: LspAsk, cx: &mut LoopCx<'_>) -> Option<RemoteReq> {
        if !self.auto_check_ready() {
            return None;
        }
        Some(self.0.get_or_insert_with(|| RemoteClient::new(cx.results.clone(), None)).lsp(ask))
    }
    fn warm(&mut self, file: PathBuf, cx: &mut LoopCx<'_>) {
        if self.auto_check_ready() {
            self.0.get_or_insert_with(|| RemoteClient::new(cx.results.clone(), None)).warm(file);
        }
    }
}

/// 自分が専用スレッドに頼んだジョブ。
struct MyJob {
    req: RemoteReq,
    kind: JobKind,
    doc: DocId,
    /// 頼んだときの「保存済みの版」（check が実機に回るとき、まだその版のままかを見る）。
    version: Version,
    started_us: u64,
    /// 試験の結果（`RemoteEvent::Tests`）を受け取った。
    got_report: bool,
    /// 打鍵が止まったときの check（未保存の中身を重ねた）。何があっても知らせない。
    auto: bool,
    /// 打鍵が止まったときの check で、頼んだときの文書の版（診断の基準。保存済みの版の代わり）。
    bases: Vec<(DocId, Version)>,
}

/// ジョブの走査が済んだ時点の、開いていたタブの「保存済みの版」（§9.3）。ビルド係の診断の
/// 位置が指しているのは、この版の中身。
struct Binding {
    tag: JobTag,
    root: PathBuf,
    /// 届いた順の通し番号（serve が再起動すると `tag` は振り直されるので、新旧はこれで比べる）。
    seq: u64,
    bases: Vec<(DocId, Version)>,
}

/// ステータスバーの右に出している、自分のジョブの進み具合。
struct Progress {
    req: RemoteReq,
    kind: JobKind,
    phase: Phase,
    done: u32,
    total: u32,
    started_us: u64,
}

/// 「1 回だけ知らせる」の控え（§11.1）。同じ理由が続く間は繰り返さない。ビルド係の
/// ジョブが 1 つ普通に終わったら、ビルド係の分は忘れる（次に同じ理由が起きたらまた知らせる）。
#[derive(Default)]
struct Notices {
    unavailable: bool,
    /// 打鍵が止まったときの check の間に分かった「使えない」理由（その場では出さず、次の保存で出す）。
    unavailable_later: Option<String>,
    auth: bool,
    clock: bool,
    error: bool,
    /// 実機の `check` の道具が無いと知らせた言語。
    check_missing: Vec<String>,
}

/// 開いている試験の結果の一覧（モック 15）。
struct TestsState {
    root: PathBuf,
    report: TestReport,
    query: String,
    rows: Vec<intel::TestRow>,
    sel: usize,
}

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
    /// 診断の置き場の鍵（パスがあるとき。作業フォルダからの相対）。
    key: Option<Arc<Path>>,
    /// 置き場にあるこのファイルの診断の写し（全部の出どころを合わせたもの。表示用）。
    /// 置き場が変わるたびに [`DocTab::refresh_diags`] で作り直す。
    diags: Vec<Diagnostic>,
    /// 行の増減の記録（遅れて届いた診断を、今の行へずらす）。
    shift: ShiftLog,
    /// ディスクの中身と同じだった最後の版（開いた・保存した版）。
    saved: Option<Version>,
    /// 最後にビルド係の check を頼んだ版（保存時・打鍵が止まったとき。同じ版を重ねて頼まない）。
    checked: Option<Version>,
    /// Markdown のライブプレビュー（.md のときだけ。モック 10・11）。
    md: Option<MdState>,
    /// 読み込みが終わったら入れる控えの内容とカーソル（起動時の復元）。
    pending_restore: Option<(String, Pos)>,
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
        let saved = path.is_some().then(|| doc.version());
        let mut t = DocTab {
            id,
            path,
            title,
            doc,
            view: EditorView::code(),
            lang: Lang::detect(None, ""),
            hl: None,
            custom_spans: None,
            loading: false,
            words,
            key: None,
            diags: Vec::new(),
            shift: ShiftLog::new(),
            saved,
            checked: None,
            md: None,
            pending_restore: None,
        };
        t.detect_language();
        t
    }
    /// 控えの内容を入れる（今の中身を 1 回の編集で置き換える。未保存の印が付き、
    /// 1 回の取り消しでディスクの中身に戻る）。
    fn apply_restore(&mut self, text: &str, cursor: Pos, store: &mut DiagStore) {
        let last = self.doc.as_lines().len_lines().saturating_sub(1);
        let end = Pos::new(last, self.doc.line_len(last));
        self.doc.break_undo_group();
        if let Some(c) = self.doc.replace_range(Pos::new(0, 0), end, text) {
            self.edited(c, store);
        }
        self.doc.break_undo_group();
        let line = cursor.line.min(self.doc.as_lines().len_lines().saturating_sub(1));
        let at = Pos::new(line, cursor.col.min(self.doc.line_len(line)));
        self.doc.set_selection(at, at);
        self.sync_cursor();
    }
    /// 置き場の鍵を決め直して（パスが決まった・変わった）、診断を置き場から写す。
    fn rekey(&mut self, workspace: &Path, store: &DiagStore) {
        self.key = self.path.as_deref().map(|p| intel::diag_key(workspace, p));
        self.refresh_diags(store);
    }
    /// 置き場から診断を写し直し、波線を付け直す。
    fn refresh_diags(&mut self, store: &DiagStore) {
        self.diags.clear();
        if let Some(k) = &self.key {
            store.file(k, &mut self.diags);
        }
        if !self.loading {
            // 置き場は文書の長さを知らない。行は文書の中に収める
            let last = self.doc.len_lines().saturating_sub(1);
            for d in &mut self.diags {
                d.line = d.line.min(last);
            }
        }
        self.view.set_marks(intel::marks_of(&self.diags, self.doc.len_lines()));
    }
    /// ビルド係での扱い（`language.toml` の `remote`）。
    fn remote_lang(&self) -> Option<RemoteLang> {
        self.lang.info().and_then(|i| i.remote).and_then(RemoteLang::parse)
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
    /// 編集のあと: ハイライトとビューに、変わった行を知らせる。診断は置き場の中でずらす。
    fn edited(&mut self, change: LineChange, store: &mut DiagStore) {
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
        // 編集の後の版で記録する（遅れて届く診断を、今の行へずらすため。O(1)）
        self.shift.record(self.doc.version(), change);
        // 診断のあるファイルだけ、置き場の分をずらして写し直す（無ければ何もしない）
        if !self.diags.is_empty() {
            if let Some(k) = &self.key {
                store.shift_file(k, change);
            }
            self.refresh_diags(store);
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
    /// rust-analyzer の候補（届いたら足す。小窓では単語補完より先に出す。docs/lsp-design.md）。
    lsp: Vec<lsp::LspCand>,
}

/// 補完の小窓の 1 行（単語補完と rust-analyzer の候補を合わせたもの）。
struct CompEntry {
    label: String,
    tag: &'static str,
    tag_color: Color,
    insert: String,
    /// 置き換えの始まり（カーソルの行の、文字の位置）。
    start: u32,
    /// 打った文字と一致している先頭の文字数（青で示す）。
    hit: usize,
}

impl CompState {
    /// 小窓に出す候補（rust-analyzer の分が先。単語補完の同じ語は除く）。`col` はカーソルの桁。
    fn entries(&self, col: u32) -> Vec<CompEntry> {
        let mut out: Vec<CompEntry> = self
            .lsp
            .iter()
            .map(|c| CompEntry { label: c.label.clone(), tag: "rs", tag_color: palette::FUNCTION, insert: c.insert.clone(), start: c.start, hit: col.saturating_sub(c.start) as usize })
            .collect();
        let n = self.prefix.chars().count();
        for i in &self.items {
            if out.iter().any(|e| e.insert == i.label) {
                continue;
            }
            let (tag, tag_color) = match i.kind {
                CompletionKind::Word => ("ab", palette::STATUS),
                CompletionKind::Keyword => ("kw", palette::CONTROL),
            };
            out.push(CompEntry { label: i.label.clone(), tag, tag_color, insert: i.label.clone(), start: col.saturating_sub(n as u32), hit: n });
        }
        out
    }
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
    /// 型の情報（LSP。`editor_lsp.rs`）。
    lsp: lsp::LspUi,
    /// hover の小窓（補完の小窓と同じ部品）。
    hover_popup: CompletionPopup,
    /// hover の小窓の下の行を描き直してから、置き直す（閉じたときも）。
    hover_stale: bool,
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
    /// 控えを書く時機（§8.4）。
    backup: BackupPolicy,
    /// 起動時に見つけた控え（復元するか尋ねている間）。
    restore_offer: Option<Restore>,
    /// Ctrl+Q で終わる（控えを消す）。シグナルでの終了なら控えを書いてから終わる。
    quit_clean: bool,
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
    /// 実行・試験の結果（ステータスバーの文と色。次の打鍵まで出す。モック 4・15）。
    run_status: Option<(String, Color)>,
    /// 診断の置き場（作業フォルダに 1 つ。開いていないファイルの分も持つ）。
    diag_store: DiagStore,
    /// ビルド係への口。
    remote: Box<dyn RemoteLink>,
    /// 自分が頼んで、まだ終わっていないジョブ。
    jobs: Vec<MyJob>,
    /// 走査が済んだジョブと、そのときの保存済みの版（届いた順）。
    bindings: Vec<Binding>,
    bind_seq: u64,
    /// プロジェクトの根ごとの、最後に取り込んだビルド係の結果の通し番号（古い結果を捨てる）。
    applied: Vec<(PathBuf, u64)>,
    notices: Notices,
    /// 打鍵が止まったときの check を、次の保存まで休む（断られた・届かなかった）。
    auto_paused: bool,
    /// ほかの相手（F5 の `zc-remote run`・CLI）の動いているジョブ（tag・根・最初に見た時刻）。
    /// 同じプロジェクトの中では、打鍵が止まったときの check を頼まない。
    foreign: Vec<(JobTag, PathBuf, u64)>,
    /// ステータスバーの右に出す進み具合。
    progress: Option<Progress>,
    /// 試験の結果の一覧（開いているあいだは本文の全体を覆う。モック 15）。
    tests: Option<TestsState>,
    /// ショートカット一覧（F1。コマンドパレットを兼ねる。モック 5・6）。
    shortcuts: Option<ShortcutList>,
    /// まだ一覧で見ていない試験の結果（F6 で開く）。
    test_unseen: Option<(PathBuf, TestReport)>,
    /// 保存が終わったら試験を頼む文書（F6 で未保存だったとき）。
    pending_test: Option<DocId>,
    /// F6 のあと打鍵していない（結果が届いたら、そのまま一覧を出してよい）。
    test_waiting: bool,
    /// 読み込みが終わったら移動する行（試験の一覧から、開いていないファイルへ飛んだとき）。
    pending_goto: Option<(DocId, u32)>,
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
            lsp: lsp::LspUi::default(),
            hover_popup: CompletionPopup::new("Esc 閉じる"),
            hover_stale: false,
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
            backup: BackupPolicy::default(),
            restore_offer: None,
            quit_clean: false,
            mini: MiniBuffer::new(),
            mini_open: false,
            term: None,
            panel: TerminalPanel::new(),
            panel_open: false,
            panel_full: false,
            term_grid: (56, 6),
            pending_run: None,
            run_status: None,
            diag_store: DiagStore::new(),
            remote: Box::<LazyRemote>::default(),
            jobs: Vec::new(),
            bindings: Vec::new(),
            bind_seq: 0,
            applied: Vec::new(),
            notices: Notices::default(),
            auto_paused: false,
            foreign: Vec::new(),
            progress: None,
            tests: None,
            shortcuts: None,
            test_unseen: None,
            pending_test: None,
            test_waiting: false,
            pending_goto: None,
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
    /// ビルド係への口を差し替える（既定は本物の専用スレッド。試験用）。
    pub fn set_remote(&mut self, remote: Box<dyn RemoteLink>) {
        self.remote = remote;
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
    /// ショートカット一覧が開いていれば、その絞り込みの文字列（テスト用）。
    pub fn shortcuts_query(&self) -> Option<&str> {
        self.shortcuts.as_ref().map(|l| l.query())
    }
    /// 選択中の文書が Markdown なら、プレビュー中か（`false` はソース表示）。
    pub fn markdown_preview(&self) -> Option<bool> {
        self.tabs.get(self.active).and_then(|t| t.md.as_ref()).map(|m| m.preview)
    }
    /// 起動時の控えの復元を尋ねている間は、その未保存の文書の数（テスト用）。
    pub fn restore_pending(&self) -> Option<usize> {
        self.restore_offer.as_ref().map(|r| r.unsaved_docs().count())
    }
    /// ステータスバーに出ている知らせ。
    pub fn message(&self) -> Option<&str> {
        self.message.as_ref().map(|m| m.0.as_str())
    }
    /// 開いている補完の候補と、選んでいる位置（テスト用）。
    pub fn completion(&self) -> Option<(Vec<String>, usize)> {
        let col = self.tabs.get(self.active).map_or(0, |t| t.doc.selection().1.col);
        self.completion.as_ref().map(|c| (c.entries(col).into_iter().map(|e| e.label).collect(), c.sel))
    }
    /// 出している hover の行（テスト用）。
    pub fn hover(&self) -> Option<Vec<String>> {
        self.hover_items().map(|v| v.into_iter().map(|i| i.label).collect())
    }
    /// 開いている「使っている所」・「名前を探す」の一覧の行（名前、選べない行か）と選んでいる位置（テスト用）。
    pub fn locs_rows(&self) -> Option<(String, Vec<(String, bool)>, usize)> {
        self.lsp.locs.as_ref().map(|s| (s.title.clone(), s.rows.iter().map(|(r, i)| (r.name.clone(), i.is_none())).collect(), s.sel))
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
    /// 選択中の文書の診断（全部の出どころを合わせたもの。テスト用）。
    pub fn diagnostics(&self) -> &[Diagnostic] {
        self.tabs.get(self.active).map_or(&[], |t| &t.diags)
    }
    /// 開いている試験の一覧の行（名前、通ったか）と選んでいる位置（テスト用）。
    pub fn test_list(&self) -> Option<(Vec<(String, bool)>, usize)> {
        self.tests.as_ref().map(|t| (t.rows.iter().map(|r| (r.row.name.clone(), t.report.cases[r.case].passed)).collect(), t.sel))
    }
    /// ステータスバーの右に出している、ビルド係の進み具合（テスト用）。
    pub fn remote_progress(&self) -> Option<String> {
        self.progress.as_ref().map(|p| intel::progress_text(p.kind, p.phase, p.done, p.total, 0))
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
        self.lsp.hover = None;
        self.hover_popup.forget();
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
        // 開いていない間に届いていた診断を付ける
        tab.rekey(&self.cfg.root, &self.diag_store);
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
        tab.rekey(&self.cfg.root, &self.diag_store);
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
        // 閉じた文書の「保存済みの版」は、もう診断の基準にしない
        for b in &mut self.bindings {
            b.bases.retain(|(d, _)| *d != tab.id);
        }
        // 未保存の編集に合わせてずらした診断は、ディスクの中身に合わない。捨てる
        // （保存済みのまま閉じたファイルの分は残す。開き直したときに付く）
        if let (true, Some(k)) = (tab.doc.is_modified(), &tab.key) {
            self.diag_store.forget(k);
        }
        if self.pending_goto.is_some_and(|(d, _)| d == tab.id) {
            self.pending_goto = None;
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
        if !self.tabs.is_empty() {
            self.backup.note_edit(cx.now_us);
            self.arm_backup(cx);
        }
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
        let before = self.tabs.get(self.active).map(|t| (t.id, t.doc.version()));
        let mut it = keys.into_iter();
        for k in it.by_ref() {
            if self.skk_waiting > 0 {
                self.skk_held.push(k);
                break;
            }
            self.handle_key(&k, cx);
        }
        self.skk_held.extend(it);
        self.arm_auto_check(before, cx);
    }

    /// 編集したら、打鍵が止まったときの check のタイマーを仕掛け直す（打鍵ごとの仕事はこれだけ。
    /// 版を比べて、タイマーを置き直す）。
    fn arm_auto_check(&mut self, before: Option<(DocId, Version)>, cx: &mut LoopCx<'_>) {
        if self.auto_paused {
            return;
        }
        let Some(t) = self.tabs.get(self.active) else { return };
        if before == Some((t.id, t.doc.version())) || t.loading || t.path.is_none() || t.remote_lang().is_none() {
            return;
        }
        cx.timers.set_after(TimerId::AutoCheck, AUTO_CHECK_IDLE);
    }

    /// 打鍵が止まった: 今のタブの未保存の中身でビルド係の check を頼む。前に頼んだ版のまま・
    /// 同じプロジェクトでビルド・試験が動いている・休んでいる、なら頼まない（動いているなら
    /// 少し後にもう一度見る）。
    fn auto_check(&mut self, cx: &mut LoopCx<'_>) {
        if self.auto_paused {
            return;
        }
        let Some(t) = self.tabs.get(self.active) else { return };
        if t.loading {
            return;
        }
        let (Some(path), Some(lang)) = (t.path.clone(), t.remote_lang()) else { return };
        let (doc, version) = (t.id, t.doc.version());
        if t.checked == Some(version) {
            return;
        }
        let now = now_us();
        self.foreign.retain(|(_, _, at)| now.saturating_sub(*at) < FOREIGN_JOB_MAX_US);
        let busy = self.jobs.iter().any(|j| j.kind == JobKind::Test) || self.foreign.iter().any(|(_, root, _)| path.starts_with(root));
        if busy {
            cx.timers.set_after(TimerId::AutoCheck, AUTO_CHECK_IDLE);
            return;
        }
        if !self.remote.auto_check_ready() {
            return;
        }
        // 重ねるのは未保存の文書（写しを取るだけ。バイト列にするのは専用スレッド）。基準の版は
        // 全部のタブの今の版（保存済みのタブは、ディスクの中身 = 今の中身）
        let mut overlay = Vec::new();
        let mut bases = Vec::new();
        for t in self.tabs.iter().filter(|t| !t.loading) {
            let Some(p) = &t.path else { continue };
            if t.doc.is_modified() {
                overlay.push(OverlaySource::new(p.clone(), t.doc.snapshot_lines()));
            }
            bases.push((t.id, t.doc.version()));
        }
        let job = RemoteJob { kind: JobKind::Check, lang, flags: SubmitFlags::AUTO, file: path, overlay };
        let req = self.remote.submit(job, cx);
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == doc) {
            t.checked = Some(version);
        }
        self.jobs.push(MyJob { req, kind: JobKind::Check, doc, version, started_us: now, got_report: false, auto: true, bases });
    }

    fn handle_key(&mut self, ev: &KeyEvent, cx: &mut LoopCx<'_>) {
        if self.restore_offer.is_some() {
            self.restore_key(ev, cx);
            return;
        }
        if self.shortcuts.is_some() {
            self.shortcuts_key(ev, cx);
            return;
        }
        if self.message.take().is_some() {
            self.status_due = true;
        }
        // hover の小窓は次の打鍵で閉じる（Esc は閉じるだけ）
        if self.lsp.hover.is_some() {
            self.close_hover();
            if ev.key == Key::Esc {
                return;
            }
        }
        // 打鍵があったら、試験の結果が届いても一覧で打鍵を奪わない（F6 は exec で立て直す）
        self.test_waiting = false;
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
        if self.tests.is_some() {
            self.tests_key(ev, cx);
            return;
        }
        if self.lsp.locs.is_some() {
            self.locs_key(ev, cx);
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
                    let n = (c.items.len() + c.lsp.len()).max(1);
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
            Command::InsertChar(c) if c.is_alphanumeric() || c == '_' => {
                self.update_completion(false);
                self.arm_lsp_completion(false, cx);
            }
            // rust-analyzer はメソッド・パスの続きも出せる（単語補完には無い）
            Command::InsertChar('.' | ':') => {
                self.completion = None;
                self.arm_lsp_completion(false, cx);
            }
            Command::Backspace if self.completion.is_some() => {
                self.update_completion(false);
                self.arm_lsp_completion(false, cx);
            }
            Command::Complete => {
                self.update_completion(true);
                self.arm_lsp_completion(true, cx);
            }
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
            tab.edited(c, &mut self.diag_store);
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
        // rust-analyzer の候補は、打った文字で絞って残す（新しい答えが届けば替える）
        let old_lsp = self.completion.take().map(|c| c.lsp).unwrap_or_default();
        if self.focus != Focus::Editor {
            return;
        }
        let Some(tab) = self.tabs.get(self.active) else { return };
        let (a, h) = tab.doc.selection();
        if tab.loading || a != h {
            return;
        }
        let lsp: Vec<lsp::LspCand> = {
            let mut line = String::new();
            tab.doc.line(h.line, &mut line);
            old_lsp
                .into_iter()
                .filter(|c| {
                    let typed: String = line.chars().skip(c.start as usize).take(h.col.saturating_sub(c.start) as usize).collect();
                    c.start <= h.col && c.insert.to_lowercase().starts_with(&typed.to_lowercase()) && c.insert != typed
                })
                .collect()
        };
        let Some(prefix) = zc_intel::prefix_at(tab.doc.as_lines(), h) else {
            if manual {
                self.say("補完: カーソルの前に単語が無い", false);
            }
            return;
        };
        // rust-analyzer の候補が残っていれば、1 文字目から絞って出す
        if !manual && lsp.is_empty() && prefix.chars().count() < intel::AUTO_MIN_CHARS {
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
        if items.is_empty() && lsp.is_empty() {
            if manual && tab.remote_lang() != Some(RemoteLang::Rust) {
                self.say(format!("「{prefix}」の候補は無い"), false);
            }
            return;
        }
        self.completion = Some(CompState { items, sel: 0, prefix, lsp });
    }

    /// 選んでいる候補で、打っている単語を置き換える（1 回の取り消しで戻せる）。
    fn accept_completion(&mut self) {
        let Some(c) = self.completion.take() else { return };
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let (_, h) = tab.doc.selection();
        let Some(item) = c.entries(h.col).into_iter().nth(c.sel) else { return };
        let start = Pos::new(h.line, item.start.min(h.col));
        tab.doc.break_undo_group();
        if let Some(ch) = tab.doc.replace_range(start, h, &item.insert) {
            tab.edited(ch, &mut self.diag_store);
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
        self.restore_body();
    }

    /// 本文を覆っていた一覧（アウトライン・試験の結果）を閉じたあと、下の画面を描き直させる。
    fn restore_body(&mut self) {
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
        if let Some(s) = &self.lsp.locs {
            self.picker.set(&s.query, s.rows.iter().map(|(r, _)| r.clone()).collect(), s.sel);
        } else if let Some(o) = &self.outline {
            self.picker.set(&o.query, o.rows.iter().map(|r| r.row.clone()).collect(), o.sel);
        } else if let Some(t) = &self.tests {
            self.picker.set(&t.query, t.rows.iter().map(|r| r.row.clone()).collect(), t.sel);
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
                    self.quit_clean = true;
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
                // タブを切り替えたら、段 2（SD）の控えをすぐ書く（変化があれば。§8.4）
                self.backup.note_tab_switch();
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
            Command::RunTests => self.run_tests(cx),
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
            Command::Hover | Command::GotoDefinition | Command::FindReferences | Command::FindSymbol | Command::NextDiagnostic | Command::PrevDiagnostic => {
                if !self.tabs.is_empty() {
                    if self.focus == Focus::Tree {
                        self.set_focus(Focus::Editor);
                    }
                    self.exec_lsp(cmd, cx);
                }
            }
            Command::ShowShortcuts => self.open_shortcuts(),
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
            tab.edited(c, &mut self.diag_store);
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
            Key::Esc => {
                // 名前を付けずにやめた: 保存を待っていた実行・試験もやめる
                if p.kind == PromptKind::SaveAs {
                    self.pending_run = None;
                    self.pending_test = None;
                }
                self.prompt = None;
            }
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
                            tab.rekey(&self.cfg.root, &self.diag_store);
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

    // --- 診断・ビルド係（docs/stage3-design.md §8・§9。モック 13・15） ---

    /// 置き場の中身が変わったファイルを開いているタブの、波線・タブ名・件数を作り直す。
    fn diags_changed(&mut self, changed: &[Arc<Path>]) {
        if changed.is_empty() {
            return;
        }
        for t in self.tabs.iter_mut().filter(|t| t.key.as_ref().is_some_and(|k| changed.contains(k))) {
            t.refresh_diags(&self.diag_store);
        }
        self.tabs_dirty = true;
        self.status_due = true;
    }

    /// 保存した中身を検査する（§8.3）。言語に `remote` があればビルド係の check を頼み、
    /// 無ければ実機の構文チェック。ビルド係が「対象ではない・届かない」と返したら、
    /// そのとき実機に回す（[`EditorScreen::check_done`]）。
    fn check_after_save(&mut self, doc: DocId, cx: &mut LoopCx<'_>) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == doc) else { return };
        let (Some(path), Some(version)) = (t.path.clone(), t.saved) else { return };
        match t.remote_lang() {
            Some(lang) => {
                t.checked = Some(version);
                // 保存したら、休んでいた打鍵が止まったときの check もまた試す（設定を直したかもしれない）
                self.auto_paused = false;
                // 同じプロジェクトの前の check は serve が取り消す（その終わりの知らせで控えから消える）
                let req = self.remote.submit(RemoteJob::new(JobKind::Check, lang, SubmitFlags::NONE, path), cx);
                self.jobs.push(MyJob { req, kind: JobKind::Check, doc, version, started_us: now_us(), got_report: false, auto: false, bases: Vec::new() });
            }
            None => self.start_local_check(doc, cx),
        }
    }

    /// 実機の構文チェック（`language.toml` の `check`）を `check` ワーカーで走らせる。
    /// 数秒かかる道具（`tsc`）もあるので、ファイルを開く batch には載せない。
    fn start_local_check(&mut self, doc: DocId, cx: &mut LoopCx<'_>) {
        let Some(t) = self.tabs.iter().find(|t| t.id == doc) else { return };
        let (Some((cmd, pat)), Some(path)) = (t.lang.info().and_then(intel::check_command), t.path.clone()) else { return };
        let (base, tx) = (t.saved, cx.results.clone());
        // 同じ文書のチェックがまだ実行待ちなら、新しい方に置き換える（保存を続けても溜まらない）
        cx.workers.check.submit_keyed(doc.0 as u64, move || {
            let result = intel::run_local_check(cmd, pat, &path, &CancelToken::never())
                .map(|diags| DiagnosticSet { source: DiagSource::Syntax, complete: false, files: vec![FileDiagnostics { path: Arc::from(path.as_path()), base, diags }] });
            tx.send(AppEvent::Checked { doc, result });
        });
    }

    /// 実機の構文チェックの結果を取り込む。パスを置き場の鍵に直し、保存した版からの編集の
    /// ぶんをずらしてから。構文チェックは 1 ファイルしか見ないので、ほかのファイルの分も、
    /// ビルド係の分も消さない。
    fn apply_syntax(&mut self, doc: DocId, mut set: DiagnosticSet) {
        let had = self.tabs.iter().find(|t| t.id == doc).is_some_and(|t| !t.diags.is_empty());
        set.complete = false;
        for f in &mut set.files {
            let abs = f.path.clone();
            if let Some(t) = self.tabs.iter().find(|t| !t.loading && t.path.as_deref() == Some(&*abs)) {
                if let Some(base) = f.base {
                    t.shift.shift(base, t.doc.len_lines(), &mut f.diags);
                }
            }
            f.path = intel::diag_key(&self.cfg.root, &abs);
        }
        let changed = self.diag_store.apply(set);
        self.diags_changed(&changed);
        // 最初のエラーを右に出す（モック 13）。無くなったら、そう知らせる
        let first = self.tabs.iter().find(|t| t.id == doc).and_then(|t| t.diags.first()).map(|d| format!("{} 行: {}", d.line + 1, d.message));
        if let Some(m) = first {
            self.say_right(m, true);
        } else if had {
            self.say("構文エラーは無くなった", false);
        }
    }

    /// 専用スレッドからの知らせ。
    fn on_remote(&mut self, ev: RemoteEvent, cx: &mut LoopCx<'_>) {
        match ev {
            // zc-remote が入っていない・版が違う。1 回だけ、静かに知らせる。このあとの依頼は
            // 「対象ではない」で返るので、実機の構文チェックに回る
            // 打鍵が止まったときの check しか待っていなければ、その場では出さない（次の保存で出す）
            RemoteEvent::Unavailable(why) => {
                if !self.jobs.iter().any(|j| !j.auto) {
                    self.notices.unavailable_later = Some(why);
                } else if !std::mem::replace(&mut self.notices.unavailable, true) {
                    self.say(format!("ビルド係を使えない: {why}"), false);
                }
            }
            RemoteEvent::Phase { req, tag, root, phase, done, total } => {
                let mine = req.and_then(|r| self.jobs.iter().position(|j| j.req == r));
                if req.is_none() && tag != JobTag(0) {
                    // ほかの相手のジョブ（F5 のビルドなど）が動いている（見た時刻は、知らせのたびに更新）
                    match self.foreign.iter_mut().find(|(t, _, _)| *t == tag) {
                        Some(f) => (f.1, f.2) = (root.clone(), now_us()),
                        None => {
                            if self.foreign.len() >= MAX_BINDINGS {
                                self.foreign.remove(0);
                            }
                            self.foreign.push((tag, root.clone(), now_us()));
                        }
                    }
                }
                if phase == Phase::Scanned {
                    self.bind(tag, root, mine);
                }
                // 進み具合を出すのは自分の依頼だけ（F5 のビルドは端末に流れている）。打鍵が止まった
                // ときの check は出さない（ステータスバーを打鍵のたびに変えない）
                if let Some(job) = mine.map(|i| &self.jobs[i]).filter(|j| !j.auto) {
                    self.progress = Some(Progress { req: job.req, kind: job.kind, phase, done, total, started_us: job.started_us });
                    self.status_due = true;
                    if cx.timers.deadline(TimerId::Tick).is_none() {
                        cx.timers.set_after(TimerId::Tick, RUN_TICK);
                    }
                }
            }
            RemoteEvent::Diagnostics { req, tag, root, complete, files } => self.remote_diagnostics(req, tag, &root, complete, files),
            RemoteEvent::Tests { req, root, report } => self.tests_arrived(req, root, report),
            RemoteEvent::Done { req, tag, status, message, .. } => self.remote_done(req, tag, status, &message, cx),
            RemoteEvent::LspResult { req, status, message, text, items } => self.on_lsp_result(req, status, message, text, items, cx),
            RemoteEvent::LspState { state, message, .. } => self.on_lsp_state(state, message),
            // ログは頼んでいない。成果物は F5（端末の zc-remote run）が自分で受け取る
            RemoteEvent::Log { .. } | RemoteEvent::Artifact { .. } => {}
        }
    }

    /// ジョブの走査が済んだ: `root` の下の開いているタブの「保存済みの版」を控える（§9.3）。
    /// このジョブの診断の位置が指すのは、その版の中身。
    /// `mine` が打鍵が止まったときの check なら、基準は頼んだときの文書の版（重ねた中身の版）。
    fn bind(&mut self, tag: JobTag, root: PathBuf, mine: Option<usize>) {
        let overrides: &[(DocId, Version)] = mine.map_or(&[], |i| &self.jobs[i].bases);
        let mut bases = Vec::new();
        for t in self.tabs.iter().filter(|t| !t.loading) {
            let base = overrides.iter().find(|(d, _)| *d == t.id).map(|(_, v)| *v).or(t.saved);
            if let (Some(p), Some(v)) = (t.path.as_deref(), base) {
                if p.starts_with(&root) {
                    bases.push((t.id, v));
                }
            }
        }
        self.bind_seq += 1;
        // 同じジョブが走査し直した（受付が忘れていて、目録からやり直した）ら、控えも取り直す
        self.bindings.retain(|b| b.tag != tag);
        if self.bindings.len() >= MAX_BINDINGS {
            self.bindings.remove(0);
        }
        self.bindings.push(Binding { tag, root, seq: self.bind_seq, bases });
    }

    /// ビルド係の診断を取り込む。
    ///
    /// - 置き換わった古いジョブの結果は捨てる: 同じ根で、後から走査したジョブの結果をもう
    ///   取り込んでいたら捨てる。取り消されて途中までの結果（`complete` でない）も、後から
    ///   走査したジョブがあれば捨てる（置き場は順番を見ないので、ここで見る）
    /// - 開いているファイルの分は、控えた「保存済みの版」から今までの編集のぶんをずらす
    /// - `complete` のときは、**この根の下で**今回出てこなかったファイルのビルド係の分を消す
    ///   （置き場は作業フォルダ全体で 1 つ。ほかのプロジェクトの分を消さない）
    /// - 打鍵が止まったときの check の結果は、ステータスバーに知らせを出さない（波線と件数だけ。
    ///   カーソル行のメッセージはふだんどおり右に出る）
    fn remote_diagnostics(&mut self, req: Option<RemoteReq>, tag: JobTag, root: &Path, complete: bool, files: Vec<FileDiags>) {
        let quiet = req.is_some_and(|r| self.jobs.iter().any(|j| j.req == r && j.auto));
        let bound = self.bindings.iter().position(|b| b.tag == tag && b.root == root);
        // 走査の知らせを受けていないジョブ（途中からつないだ）は、いちばん新しい扱い
        let seq = match bound {
            Some(i) => self.bindings[i].seq,
            None => {
                self.bind_seq += 1;
                self.bind_seq
            }
        };
        let newer_applied = self.applied.iter().any(|(r, s)| r == root && *s > seq);
        let newer_scanned = self.bindings.iter().any(|b| b.root == root && b.seq > seq);
        if newer_applied || (!complete && newer_scanned) {
            return;
        }
        let mut set = DiagnosticSet { source: DiagSource::Remote, complete: false, files: Vec::with_capacity(files.len()) };
        // 位置なしの診断（パスが空）は、ステータスバーにだけ出す（§8.6）
        let mut loose: Option<String> = None;
        for FileDiags { path, mut diags } in files {
            if path.is_empty() {
                loose = loose.or_else(|| diags.first().map(|d| d.message.clone()));
                continue;
            }
            let abs = root.join(&path);
            let mut base = None;
            if let Some(t) = self.tabs.iter().find(|t| !t.loading && t.path.as_deref() == Some(abs.as_path())) {
                // 走査のあとで開いたファイルは、開いたとき（= ディスクの中身）の版が基準
                base = bound.and_then(|i| self.bindings[i].bases.iter().find(|(d, _)| *d == t.id)).map(|(_, v)| *v).or(t.saved);
                if let Some(b) = base {
                    t.shift.shift(b, t.doc.len_lines(), &mut diags);
                }
            }
            set.files.push(FileDiagnostics { path: intel::diag_key(&self.cfg.root, &abs), base, diags });
        }
        if complete {
            for (p, _) in self.diag_store.files() {
                // 鍵は作業フォルダからの相対（外のファイルは絶対パスで、join してもそのまま）
                if self.cfg.root.join(&p).starts_with(root) && !set.files.iter().any(|f| f.path == p) {
                    set.files.push(FileDiagnostics { path: p, base: None, diags: Vec::new() });
                }
            }
        }
        let active_key = self.tabs.get(self.active).and_then(|t| t.key.clone());
        let had = self.tabs.get(self.active).is_some_and(|t| !t.diags.is_empty());
        let changed = self.diag_store.apply(set);
        self.diags_changed(&changed);
        match self.applied.iter_mut().find(|(r, _)| r == root) {
            Some(slot) => slot.1 = seq,
            None => {
                if self.applied.len() >= MAX_BINDINGS {
                    self.applied.remove(0);
                }
                self.applied.push((root.to_path_buf(), seq));
            }
        }
        // 今のタブの最初の診断を右に出す（モック 13）。今のタブに関係なければ黙っている
        if quiet {
            self.status_due = true;
        } else if let Some(m) = loose {
            self.say_right(m, true);
        } else if active_key.is_some_and(|k| changed.contains(&k)) {
            let first = self.tabs.get(self.active).and_then(|t| t.diags.first()).map(|d| format!("{} 行: {}", d.line + 1, d.message));
            if let Some(m) = first {
                self.say_right(m, true);
            } else if had {
                self.say("エラーは無くなった", false);
            }
        }
    }

    /// ジョブが終わった。
    fn remote_done(&mut self, req: Option<RemoteReq>, tag: JobTag, status: DoneStatus, message: &str, cx: &mut LoopCx<'_>) {
        // つなげなかったときの終わりの知らせは tag が 0（どのジョブでもない）。控えは消さない
        if tag != JobTag(0) {
            self.bindings.retain(|b| b.tag != tag);
            self.foreign.retain(|(t, _, _)| *t != tag);
        }
        let Some(i) = req.and_then(|r| self.jobs.iter().position(|j| j.req == r)) else { return };
        let job = self.jobs.remove(i);
        if self.progress.as_ref().is_some_and(|p| p.req == job.req) {
            self.progress = None;
            self.status_due = true;
        }
        if matches!(status, DoneStatus::Ok | DoneStatus::Failed) {
            // ビルド係は動いている。前に知らせた理由は終わったので、次に起きたらまた知らせる
            self.notices.auth = false;
            self.notices.clock = false;
            self.notices.error = false;
        }
        match job.kind {
            JobKind::Check => self.check_done(&job, status, message, cx),
            JobKind::Test => self.tests_done(&job, status, message),
            JobKind::Build => {}
        }
    }

    /// 保存時の check が終わった（§8.3・§11.1）。打鍵が止まったときの check は何も知らせず、
    /// 実機の構文チェックにも回さない。断られた・届かないなら、次の保存まで休む。
    fn check_done(&mut self, job: &MyJob, status: DoneStatus, message: &str, cx: &mut LoopCx<'_>) {
        if job.auto && !matches!(status, DoneStatus::Ok | DoneStatus::Failed | DoneStatus::Cancelled) {
            self.auto_paused = true;
            return;
        }
        // 打鍵が止まったときの check の間に分かった「使えない」は、保存時の check の終わりに出す
        if let Some(why) = self.notices.unavailable_later.take_if(|_| !job.auto) {
            if !std::mem::replace(&mut self.notices.unavailable, true) {
                self.say(format!("ビルド係を使えない: {why}"), false);
            }
        }
        match status {
            DoneStatus::Ok | DoneStatus::Failed => {
                // この保存の検査はビルド係がした。前に実機の構文チェックに回したときの結果は
                // 古い中身のものなので、このファイルの分だけ消す（残すと波線が消えなくなる）
                if let Some(key) = self.tabs.iter().find(|t| t.id == job.doc).and_then(|t| t.key.clone()) {
                    let stale = DiagnosticSet { source: DiagSource::Syntax, complete: false, files: vec![FileDiagnostics { path: key, base: None, diags: Vec::new() }] };
                    let changed = self.diag_store.apply(stale);
                    self.diags_changed(&changed);
                }
            }
            // 新しい保存に置き換わった・上限: 何も出さない（波線はそのまま）
            DoneStatus::Cancelled | DoneStatus::Limit => {}
            DoneStatus::NotApplicable | DoneStatus::Offline | DoneStatus::Auth | DoneStatus::Clock | DoneStatus::Error => {
                // 認証・時計・ビルド係の失敗は 1 回だけ知らせる。対象でない・オフラインは黙っている
                let flag = match status {
                    DoneStatus::Auth => Some(&mut self.notices.auth),
                    DoneStatus::Clock => Some(&mut self.notices.clock),
                    DoneStatus::Error => Some(&mut self.notices.error),
                    _ => None,
                };
                if flag.is_some_and(|f| !std::mem::replace(f, true)) {
                    if let Some(m) = intel::done_text(status, message) {
                        self.say(m, false);
                    }
                }
                // 実機の構文チェックに回す。そのあと保存し直していたら、新しい保存の依頼に任せる
                if self.tabs.iter().any(|t| t.id == job.doc && t.saved == Some(job.version)) {
                    self.start_local_check(job.doc, cx);
                }
            }
        }
    }

    // --- 試験を実行（§8.5。モック 15） ---

    /// F6: 保存してから、ビルド係で試験を走らせる。まだ見ていない結果があれば、その一覧を開く。
    fn run_tests(&mut self, cx: &mut LoopCx<'_>) {
        if let Some((root, report)) = self.test_unseen.take() {
            self.open_tests(root, report);
            return;
        }
        let Some(tab) = self.tabs.get(self.active) else { return };
        if tab.loading {
            return;
        }
        if tab.remote_lang().is_none() {
            let msg = format!("{} は試験を実行できない（language.toml に remote が無い）", tab.lang.name);
            self.say(msg, false);
            return;
        }
        // 結果が届くまで打鍵が無ければ、届いたときにそのまま一覧を出す
        self.test_waiting = true;
        if self.jobs.iter().any(|j| j.kind == JobKind::Test) {
            self.say("試験は実行中", false);
            return;
        }
        if tab.path.is_none() || tab.doc.is_modified() {
            self.pending_test = Some(tab.id);
            self.save_active(cx);
            return;
        }
        self.start_tests(cx);
    }

    /// 選択中の文書のプロジェクトの試験を、いまのディスクの中身で頼む。
    fn start_tests(&mut self, cx: &mut LoopCx<'_>) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let (Some(path), Some(lang)) = (tab.path.clone(), tab.remote_lang()) else { return };
        let (doc, version) = (tab.id, tab.saved.unwrap_or_else(|| tab.doc.version()));
        let req = self.remote.submit(RemoteJob::new(JobKind::Test, lang, SubmitFlags::NONE, path), cx);
        let started_us = now_us();
        self.jobs.push(MyJob { req, kind: JobKind::Test, doc, version, started_us, got_report: false, auto: false, bases: Vec::new() });
        self.test_unseen = None;
        self.progress = Some(Progress { req, kind: JobKind::Test, phase: Phase::Connecting, done: 0, total: 0, started_us });
        self.status_due = true;
        cx.timers.set_after(TimerId::Tick, RUN_TICK);
    }

    /// 試験の結果が届いた。全部通っていれば、ステータスバーに出すだけ。落ちた試験があれば
    /// 一覧を出す。ただし、F6 のあとに打鍵していたら（書いている途中に打鍵を奪わない）
    /// 知らせるだけにして、次の F6 で開く。
    fn tests_arrived(&mut self, req: RemoteReq, root: PathBuf, report: TestReport) {
        let Some(job) = self.jobs.iter_mut().find(|j| j.req == req && j.kind == JobKind::Test) else { return };
        job.got_report = true;
        self.status_due = true;
        let summary = intel::test_summary(&report);
        if report.failed == 0 {
            self.run_status = Some((format!("✓ 試験 {summary}"), STATUS_OK));
        } else if self.test_waiting && self.focus != Focus::Terminal && self.prompt.is_none() && self.outline.is_none() {
            self.open_tests(root, report);
        } else {
            self.run_status = Some((format!("✕ 試験 {summary}  F6 で一覧"), STATUS_ERROR));
            self.test_unseen = Some((root, report));
        }
    }

    /// 試験のジョブが終わった。結果（[`RemoteEvent::Tests`]）が届いていれば、もう出してある。
    fn tests_done(&mut self, job: &MyJob, status: DoneStatus, message: &str) {
        match status {
            DoneStatus::Ok | DoneStatus::Failed if job.got_report => {}
            DoneStatus::Ok => self.say("試験: 結果が無い", false),
            DoneStatus::Failed => {
                // 試験をコンパイルできなかった（診断は波線になっている）
                let why = message.lines().next().map(str::trim).filter(|m| !m.is_empty()).unwrap_or("ビルドできなかった");
                self.run_status = Some((format!("✕ 試験: {why}"), STATUS_ERROR));
                self.status_due = true;
            }
            // 自分で頼んだ操作なので、理由は毎回出す
            _ => {
                if let Some(m) = intel::done_text(status, message) {
                    self.say(m, status != DoneStatus::Cancelled);
                }
            }
        }
    }

    fn open_tests(&mut self, root: PathBuf, report: TestReport) {
        let rows = intel::test_rows(&report, "");
        let title = format!("試験 — {}", intel::test_summary(&report));
        self.picker.set_title(&title);
        self.picker.set_empty_text("一致する試験が無い");
        self.tests = Some(TestsState { root, report, query: String::new(), rows, sel: 0 });
        self.completion = None;
        self.sync_picker();
        self.picker.invalidate();
        self.status_due = true;
    }

    /// ショートカット一覧を開く（一覧はキー割り当てと同じ表 `commands::BINDINGS` から作る）。
    fn open_shortcuts(&mut self) {
        let rows = commands::BINDINGS.iter().map(|b| ShortcutRow { category: b.category, title: b.title, keys: b.keys }).collect();
        self.shortcuts = Some(ShortcutList::new(rows));
        self.completion = None;
        self.tabs_dirty = true;
        self.status_due = true;
    }

    fn close_shortcuts(&mut self) {
        self.shortcuts = None;
        self.tabs_dirty = true;
        self.restore_body();
    }

    /// ショートカット一覧が開いているあいだの打鍵: 文字で絞り込み、↑↓ で選び、Enter で実行。
    fn shortcuts_key(&mut self, ev: &KeyEvent, cx: &mut LoopCx<'_>) {
        self.status_due = true;
        if commands::lookup(ev) == Some(Command::ShowShortcuts) {
            self.close_shortcuts();
            return;
        }
        let Some(list) = self.shortcuts.as_mut() else { return };
        match ev.key {
            Key::Esc => self.close_shortcuts(),
            Key::Enter => {
                let cmd = list.selected().and_then(|i| commands::BINDINGS.get(i)).map(|b| b.command);
                self.close_shortcuts();
                if let Some(cmd) = cmd {
                    // 一覧から選んだ操作は、エディタにいるものとして実行する（ツリー・端末の操作は
                    // それぞれの中で効く）
                    if self.focus == Focus::Tree && !self.tabs.is_empty() && !matches!(cmd, Command::Move { .. } | Command::Newline) {
                        self.set_focus(Focus::Editor);
                    }
                    self.exec(cmd, cx);
                }
            }
            Key::Up => list.move_sel(-1),
            Key::Down => list.move_sel(1),
            Key::PageUp => list.page(false),
            Key::PageDown => list.page(true),
            Key::Backspace => {
                list.backspace();
            }
            _ => {
                if ev.mods.ctrl() || ev.mods.contains(Mods::ALT) {
                    return;
                }
                if let Some(c) = ev.text.filter(|c| !c.is_control()) {
                    list.push_char(c);
                }
            }
        }
    }

    fn close_tests(&mut self) {
        self.tests = None;
        self.restore_body();
    }

    /// 試験の一覧が開いているあいだの打鍵。
    fn tests_key(&mut self, ev: &KeyEvent, cx: &mut LoopCx<'_>) {
        self.status_due = true;
        // 一覧の中の F6 は、もう一度実行
        if commands::lookup(ev) == Some(Command::RunTests) {
            self.close_tests();
            self.run_tests(cx);
            return;
        }
        let Some(t) = self.tests.as_mut() else { return };
        let n = t.rows.len();
        match ev.key {
            Key::Esc => {
                self.close_tests();
                return;
            }
            Key::Enter => {
                // 落ちた場所が分かっていれば、その行へ（通った試験では何もしない）
                let target = t.rows.get(t.sel).and_then(|r| t.report.cases[r.case].location.as_ref()).map(|(p, l)| (t.root.join(p), *l));
                if let Some((path, line)) = target {
                    self.close_tests();
                    self.goto_file_line(&path, line, cx);
                }
                return;
            }
            Key::Up if n > 0 => t.sel = (t.sel + n - 1) % n,
            Key::Down if n > 0 => t.sel = (t.sel + 1) % n,
            Key::Backspace => {
                t.query.pop();
                t.rows = intel::test_rows(&t.report, &t.query);
                t.sel = 0;
            }
            _ => {
                let Some(c) = ev.text.filter(|c| !c.is_control()) else { return };
                if ev.mods.ctrl() {
                    return;
                }
                t.query.push(c);
                t.rows = intel::test_rows(&t.report, &t.query);
                t.sel = 0;
            }
        }
        self.sync_picker();
    }

    /// ファイルを開いて（開いていればそのタブへ）、`line` 行目（0 始まり）へ移る。
    fn goto_file_line(&mut self, path: &Path, line: u32, cx: &mut LoopCx<'_>) {
        self.open_path(path, cx);
        self.set_focus(Focus::Editor);
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        if t.loading {
            // 読み込みが終わってから移る
            self.pending_goto = Some((t.id, line));
            return;
        }
        let pos = Pos::new(line.min(t.doc.len_lines().saturating_sub(1)), 0);
        t.doc.break_undo_group();
        t.doc.set_selection(pos, pos);
        t.sync_cursor();
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

    // --- 控え（§8.4） ---

    /// いまのセッションと未保存の内容。未保存の文書が無ければ `None`（控えは要らない）。
    fn snapshot(&self) -> Option<Snapshot> {
        let mut docs = Vec::with_capacity(self.tabs.len());
        for t in &self.tabs {
            let unsaved = (t.doc.is_modified() && !t.loading).then(|| {
                let mut v = Vec::new();
                let _ = (t.doc.save_writer())(&mut v);
                v
            });
            docs.push(DocBackup {
                path: t.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                title: t.title.clone(),
                cursor: t.doc.selection().1,
                scroll_line: t.view.viewport().top,
                unsaved,
            });
        }
        if docs.iter().all(|d| d.unsaved.is_none()) {
            return None;
        }
        Some(Snapshot { saved_at_ms: 0, active: Some(self.active as u32), docs })
    }

    /// 控えのタイマーを、次に書く時刻に合わせる。
    fn arm_backup(&mut self, cx: &mut LoopCx<'_>) {
        match self.backup.next_deadline() {
            Some(t) => cx.timers.set_after(TimerId::Backup, Duration::from_micros(t.saturating_sub(cx.now_us).max(1_000))),
            None => cx.timers.cancel(TimerId::Backup),
        }
    }

    /// 書く時機になった段の控えを書く（未保存の文書が無ければ控えを消す）。
    fn write_backups(&mut self, cx: &mut LoopCx<'_>) {
        // 復元を尋ねている間は、前回の控えを上書きしない
        if self.restore_offer.is_some() {
            return;
        }
        let due = self.backup.due(cx.now_us);
        if !due.is_empty() {
            match self.snapshot() {
                Some(snap) => {
                    for tier in due {
                        self.saver.backup(tier, snap.clone(), cx);
                        self.backup.note_written(tier);
                    }
                }
                None => {
                    self.saver.discard_backups(cx);
                    self.backup.note_clean();
                }
            }
        }
        self.arm_backup(cx);
    }

    /// 起動時に控えが見つかった。未保存の内容があれば、復元するか尋ねる。
    fn offer_restore(&mut self, found: Option<Restore>, cx: &mut LoopCx<'_>) {
        let Some(r) = found else { return };
        for n in &r.notes {
            self.say(n.clone(), true);
        }
        if r.unsaved_docs().next().is_none() {
            // セッションだけ（未保存の内容が無い）なら、尋ねずに片づける
            self.saver.discard_backups(cx);
            return;
        }
        self.restore_offer = Some(r);
        self.status_due = true;
    }

    /// 復元の問いへの答え: Enter で復元、Esc で捨てる。ほかのキーは問いを出したまま。
    fn restore_key(&mut self, ev: &KeyEvent, cx: &mut LoopCx<'_>) {
        if ev.mods.contains(Mods::CTRL) || ev.mods.contains(Mods::ALT) {
            return;
        }
        match ev.key {
            Key::Enter => {
                let r = self.restore_offer.take().expect("restore_offer");
                self.restore(r, cx);
            }
            Key::Esc => {
                self.restore_offer = None;
                self.saver.discard_backups(cx);
                self.say("前回の未保存の内容を捨てた", false);
            }
            _ => {}
        }
        self.status_due = true;
    }

    /// 控えの未保存の内容をタブに戻す（開いていなければ開く）。保存はしない（未保存の印が付く）。
    fn restore(&mut self, r: Restore, cx: &mut LoopCx<'_>) {
        let mut n = 0;
        let mut last = None;
        for d in r.snapshot.docs.into_iter() {
            let Some(bytes) = d.unsaved else { continue };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let i = match &d.path {
                Some(p) => {
                    let path = PathBuf::from(p);
                    self.open_path(&path, cx);
                    self.tabs.iter().position(|t| t.path.as_deref() == Some(path.as_path()))
                }
                None => {
                    let doc = self.cfg.backend.empty();
                    self.open_doc(None, &d.title, doc, None);
                    Some(self.tabs.len() - 1)
                }
            };
            let Some(i) = i else { continue };
            let t = &mut self.tabs[i];
            if t.loading {
                t.pending_restore = Some((text, d.cursor));
            } else {
                t.apply_restore(&text, d.cursor, &mut self.diag_store);
            }
            n += 1;
            last = Some(i);
        }
        if let Some(i) = last {
            self.activate(i);
            self.set_focus(Focus::Editor);
        }
        self.tabs_dirty = true;
        // 戻した内容はまだ未保存なので、控えはこの後の打鍵・タイマーで書き直される
        self.backup.note_edit(cx.now_us);
        self.arm_backup(cx);
        self.say(format!("前回の未保存の内容を {n} 件戻した（まだ保存していない）"), false);
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
            tab.edited(c, &mut self.diag_store);
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
            tab.edited(c, &mut self.diag_store);
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
        if let Some(r) = &self.restore_offer {
            let n = r.unsaved_docs().count();
            let names: Vec<&str> = r.unsaved_docs().map(|d| d.title.as_str()).take(2).collect();
            self.status.set_left(&format!("前回の未保存 {n} 件: {}{}", names.join(" "), if n > 2 { " …" } else { "" }));
            self.status.set_right("Enter 戻す  Esc 捨てる");
            self.status.set_chips(Vec::new());
            self.status.set_bg(palette::STATUS_RUN);
            return;
        }
        if let Some(list) = &self.shortcuts {
            self.status.set_left(&list.status_left());
            self.status.set_right(&list.status_right());
            self.status.set_chips(Vec::new());
            self.status.set_bg(palette::STATUS);
            return;
        }
        if let Some(s) = &self.lsp.locs {
            let left = match (s.symbols, s.waiting.is_some()) {
                (true, true) => "名前を探す: 問い合わせ中".to_string(),
                (true, false) => format!("名前を探す  {} 件", s.items.len()),
                (false, _) => s.selected().map(|l| format!("{}:{}", l.place, l.line + 1)).unwrap_or_else(|| s.title.clone()),
            };
            self.status.set_left(&left);
            self.status.set_right("Enter 移動  Esc 閉じる");
            self.status.set_chips(Vec::new());
        } else if self.outline.is_some() {
            self.status.set_left("アウトライン");
            self.status.set_right("Enter 移動  Esc 閉じる");
            self.status.set_chips(Vec::new());
        } else if let Some(t) = &self.tests {
            // 選んでいる試験が落ちていれば、そのメッセージ（モック 15）
            let failed = t.rows.get(t.sel).map(|r| &t.report.cases[r.case]).filter(|c| !c.passed);
            let left = match failed {
                Some(c) => {
                    let mut lines = c.message.lines().map(str::trim).filter(|l| !l.is_empty());
                    // Rust の「thread '…' panicked at …」の行より、その次の理由の行を出す
                    let first = lines.clone().find(|l| !l.starts_with("thread '")).or_else(|| lines.next());
                    first.unwrap_or("失敗（メッセージなし）").to_string()
                }
                None => format!("試験  {}", intel::test_summary(&t.report)),
            };
            self.status.set_left(&left);
            self.status.set_right("Enter 移動  Esc 閉じる");
            self.status.set_chips(Vec::new());
            if t.report.failed > 0 {
                bg = STATUS_ERROR;
            }
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
                    // 件数は今のタブの分（重なりは 1 つに数える。§8.6）
                    let counts = t.key.as_deref().map(|k| self.diag_store.counts(k)).unwrap_or_default();
                    if counts.errors + counts.warnings > 0 {
                        let left = format!("{}  ⊗ {}", self.status.left(), counts.errors + counts.warnings);
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
            // ビルド係の進み具合は右に（§8.6）
            if let Some(p) = &self.progress {
                let secs = now_us().saturating_sub(p.started_us) / 1_000_000;
                self.status.set_right(&intel::progress_text(p.kind, p.phase, p.done, p.total, secs));
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
        // 前回の控え（§8.4）。storage スレッドは依頼の順に処理するので、この後の控えの
        // 書き込みより先に読まれる
        if let Some(job) = self.saver.restore_job() {
            let tx = cx.results.clone();
            cx.workers.storage.submit(move || {
                tx.send(AppEvent::RestoreFound(job()));
            });
        }
        self.pump_highlight(Some(cx));
        self.blinks_left = BLINKS_AFTER_KEY;
        cx.timers.set_after(TimerId::CursorBlink, BLINK);
    }

    fn stop(&mut self) {
        // 復元を尋ねている途中なら、前回の控えに手を付けない
        if self.restore_offer.is_some() {
            return;
        }
        // Ctrl+Q（未保存なら確かめてから）は控えを消す。シグナル（ランチャーの Esc 長押しなど）
        // なら、未保存の内容を控えに書いてから終わる
        let snap = if self.quit_clean { None } else { self.snapshot() };
        self.saver.finish_backups(snap);
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
                        if let Some((text, cursor)) = t.pending_restore.take() {
                            t.apply_restore(&text, cursor, &mut self.diag_store);
                        }
                        t.view.invalidate();
                        // 読んだ中身がディスクの中身。空の仮の文書のときの記録と控えは捨てる
                        t.saved = Some(t.doc.version());
                        t.shift = ShiftLog::new();
                        let id = t.id;
                        for b in &mut self.bindings {
                            b.bases.retain(|(d, _)| *d != id);
                        }
                        let t = &mut self.tabs[i];
                        t.refresh_diags(&self.diag_store);
                        if let Some((_, line)) = self.pending_goto.take_if(|(d, _)| *d == id) {
                            let pos = Pos::new(line.min(t.doc.len_lines().saturating_sub(1)), 0);
                            t.doc.set_selection(pos, pos);
                        }
                        t.sync_cursor();
                        self.status_due = true;
                        // 定義へ飛んだ先なら、その桁へ。Rust なら rust-analyzer を先に起こしておく
                        self.lsp_opened(id);
                        self.lsp_warm(id, cx);
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
                        let current = t.doc.version() == version;
                        if current {
                            t.doc.mark_saved();
                        }
                        // ディスクの中身はこの版（遅れて届く診断の基準。§9.3）
                        t.saved = Some(version);
                        // F5・F6 で保存を待っていたら実行する（保存した版のままのときだけ）
                        let run_now = self.pending_run == Some(doc) && current;
                        if self.pending_run == Some(doc) {
                            self.pending_run = None;
                        }
                        let test_now = self.pending_test == Some(doc) && current;
                        if self.pending_test == Some(doc) {
                            self.pending_test = None;
                        }
                        let builds_remotely = t.lang.info().and_then(|i| i.run).is_some_and(|r| r.starts_with("zc-remote run"));
                        let title = t.title.clone();
                        let path = t.path.clone();
                        let active = self.tabs.get(self.active).is_some_and(|a| a.id == doc);
                        // 保存した中身を検査する（§8.3）。このあとすぐビルド係でビルド・試験をするなら、
                        // その診断が届くので、同じ木の check を重ねて頼まない
                        if !(active && (test_now || (run_now && builds_remotely))) {
                            self.check_after_save(doc, cx);
                        }
                        self.tree.refresh();
                        if active {
                            self.tree.set_current(path.as_deref());
                        }
                        match ok {
                            SaveOk::Written { overwrote_external_change: false } => self.say(format!("保存した: {title}"), false),
                            SaveOk::Written { overwrote_external_change: true } => {
                                self.say(format!("保存した: {title}（ほかで変更されていた内容を上書きした）"), true)
                            }
                            SaveOk::Unchanged => self.say(format!("変更なし: {title}"), false),
                        }
                        if run_now && active {
                            self.start_run(cx);
                        }
                        if test_now && active {
                            self.start_tests(cx);
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
            AppEvent::Checked { doc, result } => match result {
                Ok(set) => self.apply_syntax(doc, set),
                // チェックの道具が無い: 波線はそのまま。同じ言語では 1 回だけ知らせる
                Err(CheckFail::Unavailable(why)) => {
                    let lang = self.tabs.iter().find(|t| t.id == doc).map(|t| t.lang.name.clone()).unwrap_or_default();
                    if !self.notices.check_missing.contains(&lang) {
                        self.notices.check_missing.push(lang);
                        self.say(format!("構文チェックできない: {why}"), false);
                    }
                }
                // 時間切れ・書き方の誤り: 波線はそのまま、知らせるだけ
                Err(CheckFail::Other(e)) => self.say(format!("構文チェックできない: {e}"), false),
            },
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
            AppEvent::Remote(ev) => self.on_remote(ev, cx),
            AppEvent::RestoreFound(found) => self.offer_restore(found, cx),
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
            TimerId::Backup => self.write_backups(cx),
            TimerId::AutoCheck => self.auto_check(cx),
            TimerId::LspComplete => self.lsp_complete_timer(cx),
            TimerId::LspSymbols => self.lsp_symbols_timer(cx),
            TimerId::Tick => {
                // 実行中の経過時間と、ビルド係の「実行中 n 秒」
                if self.term.as_ref().is_some_and(|t| t.running.is_some()) || self.progress.is_some() {
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
        self.hover_popup.invalidate();
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
            let mut tabs: Vec<Tab> = self.tabs.iter().map(|t| Tab { doc: t.id, title: t.title.clone(), modified: t.doc.is_modified(), error: t.has_error() }).collect();
            let mut active = self.active;
            if self.shortcuts.is_some() {
                active = tabs.len();
                tabs.push(Tab { doc: DocId(u32::MAX), title: "ショートカット".into(), modified: false, error: false });
            }
            self.tabbar.set_tabs(tabs, active);
            self.tabbar.set_inset(if self.tree_open { SIDE_W } else { 0 });
            self.status_due = true;
        }
        damage |= self.tabbar.render(cx, regions.tabs);
        if let Some(list) = self.shortcuts.as_mut() {
            // 本文（ツリーとエディタとパネル）の全体を覆う（モック 5・6）
            let body = Rect::new(0, regions.tabs.bottom() as i16, Rect::SCREEN.w, (regions.status.y as i32 - regions.tabs.bottom()) as u16);
            damage |= list.render(cx, body);
            if self.status_due {
                self.update_status();
            }
            damage |= self.status.render(cx, regions.status);
            return damage;
        }
        if self.outline.is_some() || self.tests.is_some() || self.lsp.locs.is_some() {
            // 本文（ツリーとエディタ）の全体を覆う（モック 14・15）
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
        let hover = if want_popup { None } else { self.hover_items() };
        match self.tabs.get_mut(self.active) {
            Some(t) => {
                // 小窓を閉じる・置き直すときは、下にあった行を描き直させる
                if let Some(old) = self.popup.drawn() {
                    if !want_popup || std::mem::take(&mut self.popup_stale) {
                        t.view.invalidate_rows(old.y as i32, old.bottom());
                        self.popup.forget();
                    }
                }
                if let Some(old) = self.hover_popup.drawn() {
                    if hover.is_none() || std::mem::take(&mut self.hover_stale) {
                        t.view.invalidate_rows(old.y as i32, old.bottom());
                        self.hover_popup.forget();
                    }
                }
                self.hover_stale = false;
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
                    let (_, h) = t.doc.selection();
                    let entries = c.entries(h.col);
                    let start = entries.iter().map(|e| e.start).min().unwrap_or(h.col);
                    let items = entries.into_iter().map(|e| PopupItem { tag: e.tag, tag_color: e.tag_color, label: e.label, hit_chars: e.hit }).collect();
                    self.popup.set(items, c.sel);
                    let word = Pos::new(h.line, start);
                    let word_x = t.view.point_of(word).map_or(caret.x as i32, |r| r.x as i32);
                    let rect = self.popup.place(cx, caret, word_x, regions.editor);
                    damage |= self.popup.render_at(cx, rect, !view_damage.is_empty());
                }
                // hover（Ctrl+H）: カーソルの下の小窓（補完の小窓と同じ見た目。選んでいる行は無い）
                if let (Some(items), Some(caret)) = (hover, t.view.caret_rect()) {
                    self.hover_popup.set(items, usize::MAX);
                    let rect = self.hover_popup.place(cx, caret, caret.x as i32, regions.editor);
                    damage |= self.hover_popup.render_at(cx, rect, !view_damage.is_empty());
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
