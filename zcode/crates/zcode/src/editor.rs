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
use crate::save::{AtomicFileSaver, SaveRequest, Saver};
use std::path::{Path, PathBuf};
use std::time::Duration;
use zc_core::{palette, CancelToken, Color, Command, Damage, DocId, KeyEvent, LineChange, Mods, Motion, Pos, Rect, Span, Version};
use zc_font::TextStyle;
use zc_highlight::{Highlighter, LanguageId, Registry};
use zc_input::{Keyboard, RawRecord};
use zc_ui::layout::{Regions, SIDE_W, UI_PX};
use zc_ui::{EditorStatus, EditorView, FileTree, ImeMode, RenderCx, SpanSource, StatusBar, Tab, TabBar, Widget};

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
        let mut t = DocTab { id, path, title, doc, view: EditorView::code(), lang: Lang::detect(None, ""), hl: None, custom_spans: None, loading: false };
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
    }
    /// 編集のあと: ハイライトとビューに、変わった行を知らせる。
    fn edited(&mut self, change: LineChange) {
        if let Some(h) = self.hl.as_mut() {
            h.hl.edited(change);
            h.dirty = true;
        }
        self.view.lines_changed(change);
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
    confirm: Option<Confirm>,
    mods: Mods,
    /// 次の描画でステータスバーの中身を作り直す。
    status_due: bool,
    /// タブバーに出した内容（変わったときだけ作り直す）。
    tabs_shown: Vec<(DocId, bool)>,
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
            saver: Box::new(AtomicFileSaver),
            clipboard: String::new(),
            prompt: None,
            last_find: String::new(),
            message: None,
            confirm: None,
            mods: Mods::NONE,
            status_due: true,
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

    /// 保存の出口を差し替える（既定は [`AtomicFileSaver`]）。
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
    /// 選択中の文書のパス。
    pub fn active_path(&self) -> Option<&Path> {
        self.tabs.get(self.active).and_then(|t| t.path.as_deref())
    }
    /// ステータスバーに出ている知らせ。
    pub fn message(&self) -> Option<&str> {
        self.message.as_ref().map(|m| m.0.as_str())
    }

    fn set_focus(&mut self, focus: Focus) {
        if self.focus == focus {
            return;
        }
        self.focus = focus;
        self.tree.set_focused(focus == Focus::Tree);
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.view.set_focused(focus == Focus::Editor);
            t.doc.break_undo_group();
        }
        self.status_due = true;
    }

    fn say(&mut self, text: impl Into<String>, error: bool) {
        self.message = Some((text.into(), error));
        self.status_due = true;
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
        cx.workers.batch.submit(move || {
            let result = match backend.load(&path, &CancelToken::never()) {
                Ok(doc) => Ok(doc),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(backend.empty()),
                Err(e) => Err(e.to_string()),
            };
            tx.send(AppEvent::Opened { doc: id, result });
        });
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
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
        for k in keys {
            self.handle_key(k, cx);
        }
        self.pump_highlight(Some(cx));
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

    fn handle_key(&mut self, ev: &KeyEvent, cx: &mut LoopCx<'_>) {
        if self.message.take().is_some() {
            self.status_due = true;
        }
        if self.prompt.is_some() {
            self.prompt_key(ev, cx);
            return;
        }
        let Some(cmd) = commands::lookup(ev) else { return };
        if ev.repeat && !cmd.repeatable() {
            return;
        }
        self.exec(cmd, cx);
    }

    /// 操作を実行する。
    pub fn exec(&mut self, cmd: Command, cx: &mut LoopCx<'_>) {
        let confirm = self.confirm.take();
        match cmd {
            Command::Quit => {
                if confirm != Some(Confirm::Quit) && self.tabs.iter().any(|t| t.doc.is_modified()) {
                    self.confirm = Some(Confirm::Quit);
                    self.say("未保存のファイルがある。もう一度 Ctrl+Q で終了", true);
                } else {
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

    // --- ステータスバー ---

    fn update_status(&mut self) {
        self.status_due = false;
        let mut bg = palette::STATUS;
        if let Some(p) = &self.prompt {
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
                        // 段階 4（SKK）までは英数のまま
                        ime: Some(ImeMode::Ascii),
                    };
                    st.apply(&mut self.status);
                }
                None => {
                    self.status.set_left("zcode");
                    self.status.set_right("Ctrl+E ツリー  Ctrl+N 新規  Ctrl+Q 終了");
                    self.status.set_chips(Vec::new());
                }
            }
            if let Some((m, error)) = &self.message {
                self.status.set_left(m);
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
                    Ok(d) => {
                        let t = &mut self.tabs[i];
                        t.doc = d;
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
                    Ok(()) => {
                        // 保存を頼んだあとに編集していたら、未保存のまま
                        if t.doc.version() == version {
                            t.doc.mark_saved();
                        }
                        let title = t.title.clone();
                        let path = t.path.clone();
                        self.tree.refresh();
                        if self.tabs.get(self.active).is_some_and(|a| a.id == doc) {
                            self.tree.set_current(path.as_deref());
                        }
                        self.say(format!("保存した: {title}"), false);
                    }
                    Err(e) => {
                        let title = t.title.clone();
                        self.say(format!("{title} を保存できない: {e}"), true);
                    }
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
            AppEvent::Flushed(_) => {}
        }
        self.pump_highlight(Some(cx));
    }

    fn on_timer(&mut self, id: TimerId, cx: &mut LoopCx<'_>) {
        match id {
            TimerId::StatusDelay => self.status_due = true,
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
            }
            _ => {}
        }
    }

    fn on_glyphs_ready(&mut self) {
        self.tabbar.invalidate();
        self.tree.invalidate();
        self.status.invalidate();
        self.empty_drawn = None;
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.view.invalidate();
        }
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) -> Damage {
        let regions = Regions::compute(self.tree_open, 0);
        let mut damage = Damage::none();
        // タブバー（未保存の印が変わったときも作り直す）
        let changed = self.tabs_dirty || self.tabs.len() != self.tabs_shown.len() || self.tabs.iter().zip(&self.tabs_shown).any(|(t, s)| (t.id, t.doc.is_modified()) != *s);
        if changed {
            self.tabs_dirty = false;
            self.tabs_shown = self.tabs.iter().map(|t| (t.id, t.doc.is_modified())).collect();
            let tabs = self.tabs.iter().map(|t| Tab { doc: t.id, title: t.title.clone(), modified: t.doc.is_modified() }).collect();
            self.tabbar.set_tabs(tabs, self.active);
            self.tabbar.set_inset(if self.tree_open { SIDE_W } else { 0 });
            self.status_due = true;
        }
        damage |= self.tabbar.render(cx, regions.tabs);
        if self.tree_open {
            damage |= self.tree.render(cx, regions.side);
        }
        self.pump_highlight(None);
        match self.tabs.get_mut(self.active) {
            Some(t) => {
                let mut spans = TabSpans { custom: t.custom_spans.as_deref_mut(), hl: t.hl.as_ref().map(|h| &h.hl) };
                damage |= t.view.render(cx, regions.editor, t.doc.as_lines(), &mut spans);
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
        if self.status_due {
            self.update_status();
        }
        damage |= self.status.render(cx, regions.status);
        damage
    }

    fn wants_raw(&self) -> bool {
        // モディファイアだけの打鍵（ロックのタップ）でも起こしてもらい、札を更新する
        true
    }
}
