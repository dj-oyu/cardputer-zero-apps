//! エディタの「型の情報」（LSP。docs/lsp-design.md）。`editor.rs` の子モジュール
//! （`EditorScreen` の中身に触るため）。
//!
//! 実機では LSP を動かさない（design.md §10）。問い合わせは専用スレッド（`RemoteLink::lsp`）→
//! `zc-remote serve` → ビルド係の言語サーバー（Rust は rust-analyzer、Python は pyright など。
//! どれを使うかはビルド係が拡張子で決める）へ。メインは頼むだけで待たない。対象の言語は
//! `RemoteLang::has_lsp`（`language.toml` の `remote` がある言語のうち、ビルド係にサーバーがあるもの）。
//!
//! | キー | 操作 | 答えの出し方 |
//! |---|---|---|
//! | Ctrl+H | 型と説明 | カーソルの下の小窓（補完の小窓と同じ見た目）。次の打鍵で閉じる |
//! | Ctrl+D | 定義へ | 1 つでプロジェクトの中ならそこへ移る。外（std・crates.io）なら「定義は …」をステータスバーに。いくつもあれば一覧 |
//! | Ctrl+R | 使っている所 | 一覧（アウトラインと同じ部品）。外の場所は薄く出して選べない |
//! | Ctrl+T | 名前を探す | 一覧。検索欄の打鍵が止まったら頼み直す |
//! | F8 / Shift+F8 | 次 / 前の診断 | 手元の診断（波線）だけ。オフラインでも効く。端まで行ったら反対の端へ |
//! | （補完） | 補完の小窓に言語サーバーの候補を足す | 打鍵が止まってから頼み、届いたら足す。単語補完はそのまますぐ出る |
//!
//! - **古い答えは捨てる**: 頼んだときの文書の版（補完と hover はカーソルの位置も）が変わって
//!   いたら、何もしない
//! - 「上限」「オフライン」「認証」「設定が無い」は、同じ理由が続く間 1 回だけ知らせる（補完では
//!   何も出さない）。答えが 1 つ届いたら忘れる
//! - 位置: 文書の `Pos` の桁は文字の数、ビルド係とは UTF-8 のバイト位置で話す

use super::{EditorScreen, Focus};
use crate::app::{LoopCx, TimerId};
use crate::remote::{LspAsk, OverlaySource, RemoteReq};
use std::path::PathBuf;
use std::time::Duration;
use zc_core::{palette, DocId, Key, KeyEvent, Pos, Version};
use zc_remote_ipc::{DoneStatus, LspItem, LspKind, LspState, RemoteLang};
use zc_ui::{PickRow, PopupItem, Widget};

/// 型の情報（言語サーバー）を頼める言語か。
pub(super) fn has_lsp(lang: Option<RemoteLang>) -> bool {
    lang.is_some_and(RemoteLang::has_lsp)
}

/// 補完を言語サーバーに頼むまで、打鍵が止まるのを待つ時間。
pub const LSP_COMPLETE_IDLE: Duration = Duration::from_millis(300);
/// 名前の検索を頼み直すまで、検索欄の打鍵が止まるのを待つ時間。
pub const LSP_SYMBOLS_IDLE: Duration = Duration::from_millis(300);
/// hover の小窓に出す行の数。
pub const HOVER_LINES: usize = 8;
/// 補完の小窓に足す言語サーバーの候補の数。
pub const LSP_COMPLETIONS: usize = 6;

/// 行の中の文字の位置 → UTF-8 のバイト位置。
pub(super) fn byte_col(line: &str, char_col: u32) -> u32 {
    line.char_indices().nth(char_col as usize).map_or(line.len(), |(b, _)| b) as u32
}

/// 行の中の UTF-8 のバイト位置 → 文字の位置（文字の途中なら前の文字の頭）。
pub(super) fn char_col(line: &str, byte_col: u32) -> u32 {
    let mut b = (byte_col as usize).min(line.len());
    while !line.is_char_boundary(b) {
        b -= 1;
    }
    line[..b].chars().count() as u32
}

/// hover の文を小窓の行にする（空行は続けて 2 つ以上並べない。長ければ「…」）。
pub(super) fn hover_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in text.lines().map(str::trim_end) {
        if l.is_empty() && out.last().is_none_or(|p| p.is_empty()) {
            continue;
        }
        out.push(l.to_string());
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    if out.len() > HOVER_LINES {
        let rest = out.len() - (HOVER_LINES - 1);
        out.truncate(HOVER_LINES - 1);
        out.push(format!("…（あと {rest} 行）"));
    }
    out
}

/// 場所 1 つ（一覧の行の元）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Loc {
    /// プロジェクトの中なら絶対パス（開ける）。外なら `None`。
    pub path: Option<PathBuf>,
    /// 見せる場所（根からの相対、外なら `core/src/option.rs` の形）。
    pub place: String,
    pub line: u32,
    /// UTF-8 のバイト位置。
    pub col: u32,
    /// 名前（symbols）。
    pub name: String,
    /// 行の中身・入れ物の名前。
    pub detail: String,
    pub kind: u32,
}

impl Loc {
    pub(super) fn from_item(it: &LspItem, root: Option<&std::path::Path>) -> Loc {
        let path = (!it.path.as_os_str().is_empty()).then(|| it.path.clone());
        let place = match &path {
            Some(p) => root.and_then(|r| p.strip_prefix(r).ok()).unwrap_or(p).to_string_lossy().into_owned(),
            None => it.label.clone(),
        };
        Loc { path, place, line: it.line, col: it.col, name: it.name.clone(), detail: it.detail.clone(), kind: it.kind }
    }
}

/// SymbolKind の印と色（アウトラインと同じ見た目）。
fn symbol_icon(kind: u32) -> (&'static str, zc_core::Color) {
    match kind {
        6 | 9 | 12 => ("ƒ", palette::FUNCTION),
        5 | 10 | 11 | 23 | 26 => ("C", palette::CLASS),
        2..=4 => ("M", palette::KEYWORD),
        14 | 13 | 8 => ("v", palette::STATUS),
        _ => ("·", palette::DIM),
    }
}

/// 一覧（使っている所・名前の検索・定義がいくつもあるとき）。
pub(super) struct LocsState {
    pub title: String,
    pub items: Vec<Loc>,
    pub query: String,
    /// 絞り込んだ行（`items` の添字。`None` は選べない行 = プロジェクトの外）。
    pub rows: Vec<(PickRow, Option<usize>)>,
    pub sel: usize,
    /// 名前の検索（検索欄の打鍵で頼み直す）。
    pub symbols: bool,
    /// 名前の検索で、今待っている問い合わせ。
    pub waiting: Option<RemoteReq>,
}

impl LocsState {
    pub(super) fn new(title: String, items: Vec<Loc>, symbols: bool, query: String) -> LocsState {
        let mut s = LocsState { title, items, query, rows: Vec::new(), sel: 0, symbols, waiting: None };
        s.refilter();
        s
    }

    /// 絞り込み直す（名前と場所の部分一致。大文字小文字は見ない）。
    pub(super) fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        self.rows.clear();
        for (i, l) in self.items.iter().enumerate() {
            let name = if self.symbols { l.name.clone() } else { format!("{}:{}  {}", l.place, l.line + 1, l.detail) };
            let hay = format!("{} {}", name, l.place).to_lowercase();
            // 名前の検索はビルド係が絞り込み済み。ここでは打ちかけの分だけ
            if !q.is_empty() && !hay.contains(&q) {
                continue;
            }
            // 小文字にして長さの変わる文字があっても、元の名前の文字の境目に無ければ色を付けない
            let hit = name.to_lowercase().find(&q).filter(|_| !q.is_empty()).map(|s| s..s + q.len()).filter(|r| r.end <= name.len() && name.is_char_boundary(r.start) && name.is_char_boundary(r.end));
            let hits = hit.into_iter().collect();
            let (icon, icon_color) = if self.symbols { symbol_icon(l.kind) } else if l.path.is_some() { ("→", palette::STATUS) } else { ("·", palette::DIM) };
            let right = if self.symbols { format!("{}:{}", l.place, l.line + 1) } else { String::new() };
            let selectable = l.path.is_some();
            let row = PickRow { depth: 0, icon, icon_color, name, hits, right, context: !selectable };
            self.rows.push((row, selectable.then_some(i)));
        }
        self.sel = self.rows.iter().position(|(_, i)| i.is_some()).unwrap_or(0);
    }

    /// 選べる行の間を動く。
    pub(super) fn step(&mut self, dir: i32) {
        let n = self.rows.len() as i32;
        let mut i = self.sel as i32;
        for _ in 0..n {
            i = (i + dir).rem_euclid(n.max(1));
            if self.rows[i as usize].1.is_some() {
                self.sel = i as usize;
                return;
            }
        }
    }

    pub(super) fn selected(&self) -> Option<&Loc> {
        self.rows.get(self.sel).and_then(|(_, i)| *i).map(|i| &self.items[i])
    }
}

/// 言語サーバーの補完の候補 1 つ（補完の小窓に足す）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LspCand {
    pub label: String,
    pub insert: String,
    /// 置き換えの始まり（カーソルの行の、文字の位置）。
    pub start: u32,
}

/// 頼んで、答えを待っている問い合わせ。
pub(super) struct Pending {
    pub req: RemoteReq,
    pub kind: LspKind,
    pub doc: DocId,
    pub version: Version,
    /// 頼んだときのカーソル（文字の位置）。
    pub pos: Pos,
}

/// 型の情報の状態。
#[derive(Default)]
pub(super) struct LspUi {
    pub pending: Vec<Pending>,
    /// 出している hover（文書・版・行）。
    pub hover: Option<(DocId, Version, Vec<String>)>,
    pub locs: Option<LocsState>,
    /// 最後に知らせた失敗の種類（同じ理由を繰り返さない）。
    pub said: Option<DoneStatus>,
    /// 「準備中」と知らせた（準備できたら 1 回だけ知らせる）。
    pub told_loading: bool,
    /// 定義へ飛んだ先が読み込み中: 読み終わったら移る桁（文書・行・UTF-8 のバイト位置）。
    pub goto_col: Option<(DocId, u32, u32)>,
}

impl EditorScreen {
    /// 型の情報を使えるタブか（言語サーバーのある言語のファイルで、保存先がある）。使えなければ理由を知らせる。
    fn lsp_target(&mut self, quiet: bool) -> Option<(DocId, Version, PathBuf, Pos, u32)> {
        let t = self.tabs.get(self.active)?;
        if t.loading {
            return None;
        }
        let ok = has_lsp(t.remote_lang());
        let Some(path) = t.path.clone().filter(|_| ok) else {
            if !quiet {
                let msg = if ok { "型の情報: 先に保存する（ファイルの名前が要る）".to_string() } else { format!("型の情報: {} は対象ではない（ビルド係に言語サーバーが無い）", t.lang.name) };
                self.say(msg, false);
            }
            return None;
        };
        let (_, h) = t.doc.selection();
        let mut line = String::new();
        t.doc.line(h.line, &mut line);
        Some((t.id, t.doc.version(), path, h, byte_col(&line, h.col)))
    }

    /// 未保存の文書の写し（打鍵が止まったときの check と同じ重ね）。
    fn lsp_overlay(&self) -> Vec<OverlaySource> {
        self.tabs.iter().filter(|t| !t.loading && t.doc.is_modified()).filter_map(|t| t.path.as_ref().map(|p| OverlaySource::new(p.clone(), t.doc.snapshot_lines()))).collect()
    }

    /// 頼む。`quiet` なら（補完）使えなくても何も出さない。
    pub(super) fn lsp_ask(&mut self, kind: LspKind, text: String, quiet: bool, cx: &mut LoopCx<'_>) -> Option<RemoteReq> {
        let (doc, version, file, pos, col) = self.lsp_target(quiet)?;
        let ask = LspAsk { kind, file, line: pos.line, col, text, overlay: self.lsp_overlay() };
        let Some(req) = self.remote.lsp(ask, cx) else {
            if !quiet && self.lsp.said != Some(DoneStatus::Auth) {
                self.lsp.said = Some(DoneStatus::Auth);
                self.say("型の情報: ビルド係の設定が無い（~/.config/zcode/remote.json）", false);
            }
            return None;
        };
        // 同じ種類の前の問い合わせ（答えはまだ）は捨てる（serve も、まだ送っていなければ取り消す）
        self.lsp.pending.retain(|p| p.kind != kind);
        self.lsp.pending.push(Pending { req, kind, doc, version, pos });
        Some(req)
    }

    /// 言語サーバーのある言語のファイルを開いた: サーバーを先に起こしておく（ビルド係の設定がある人だけ）。
    pub(super) fn lsp_warm(&mut self, doc: DocId, cx: &mut LoopCx<'_>) {
        let Some(t) = self.tabs.iter().find(|t| t.id == doc) else { return };
        if !has_lsp(t.remote_lang()) {
            return;
        }
        if let Some(p) = t.path.clone() {
            self.remote.warm(p, cx);
        }
    }

    /// 操作（Ctrl+H・Ctrl+D・Ctrl+R・Ctrl+T・F8・Shift+F8）。
    pub(super) fn exec_lsp(&mut self, cmd: zc_core::Command, cx: &mut LoopCx<'_>) {
        use zc_core::Command as C;
        match cmd {
            C::Hover => {
                self.lsp_ask(LspKind::Hover, String::new(), false, cx);
            }
            C::GotoDefinition => {
                self.lsp_ask(LspKind::Definition, String::new(), false, cx);
            }
            C::FindReferences => {
                self.lsp_ask(LspKind::References, String::new(), false, cx);
            }
            C::FindSymbol => self.open_symbols(cx),
            C::NextDiagnostic => self.goto_diag(true),
            C::PrevDiagnostic => self.goto_diag(false),
            _ => {}
        }
    }

    /// F8 / Shift+F8: 今のタブの次（前）の診断へ。端まで行ったら反対の端へ。
    pub(super) fn goto_diag(&mut self, forward: bool) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        if t.loading {
            return;
        }
        let mut at: Vec<(u32, u32)> = t.diags.iter().map(|d| (d.line, d.col.unwrap_or(0))).collect();
        if at.is_empty() {
            self.say("診断は無い", false);
            return;
        }
        at.sort_unstable();
        at.dedup();
        let (_, h) = t.doc.selection();
        let mut line = String::new();
        t.doc.line(h.line, &mut line);
        let cur = (h.line, byte_col(&line, h.col));
        let next = if forward { at.iter().find(|p| **p > cur).or(at.first()) } else { at.iter().rev().find(|p| **p < cur).or(at.last()) };
        let Some(&(l, c)) = next else { return };
        let l = l.min(t.doc.len_lines().saturating_sub(1));
        t.doc.line(l, &mut line);
        let pos = Pos::new(l, char_col(&line, c));
        t.doc.break_undo_group();
        t.doc.set_selection(pos, pos);
        t.sync_cursor();
        self.status_due = true;
    }

    /// Ctrl+T: 名前を探す一覧を開く（カーソルの単語があれば、それで探し始める）。
    fn open_symbols(&mut self, cx: &mut LoopCx<'_>) {
        if self.lsp_target(false).is_none() {
            return;
        }
        let word = self.tabs.get(self.active).and_then(|t| zc_intel::prefix_at(t.doc.as_lines(), t.doc.selection().1)).unwrap_or_default();
        let mut s = LocsState::new("名前を探す".into(), Vec::new(), true, word.clone());
        if !word.is_empty() {
            s.waiting = self.lsp_ask(LspKind::Symbols, word, false, cx);
        }
        self.open_locs(s);
    }

    fn open_locs(&mut self, s: LocsState) {
        self.picker.set_title(&s.title);
        self.picker.set_empty_text(if s.symbols { "名前を打つと探す" } else { "見つからない" });
        self.lsp.locs = Some(s);
        self.completion = None;
        self.close_hover();
        self.sync_picker();
        self.picker.invalidate();
        self.status_due = true;
    }

    pub(super) fn close_locs(&mut self) {
        self.lsp.locs = None;
        self.restore_body();
    }

    /// 一覧が開いているあいだの打鍵。
    pub(super) fn locs_key(&mut self, ev: &KeyEvent, cx: &mut LoopCx<'_>) {
        self.status_due = true;
        let Some(s) = self.lsp.locs.as_mut() else { return };
        match ev.key {
            Key::Esc => return self.close_locs(),
            Key::Enter => {
                let target = s.selected().cloned();
                if let Some(loc) = target {
                    self.close_locs();
                    self.goto_loc(&loc, cx);
                }
                return;
            }
            Key::Up if !s.rows.is_empty() => s.step(-1),
            Key::Down if !s.rows.is_empty() => s.step(1),
            Key::Backspace => {
                s.query.pop();
                s.refilter();
                if s.symbols {
                    cx.timers.set_after(TimerId::LspSymbols, LSP_SYMBOLS_IDLE);
                }
            }
            _ => {
                let Some(c) = ev.text.filter(|c| !c.is_control()) else { return };
                if ev.mods.ctrl() {
                    return;
                }
                s.query.push(c);
                s.refilter();
                if s.symbols {
                    cx.timers.set_after(TimerId::LspSymbols, LSP_SYMBOLS_IDLE);
                }
            }
        }
        self.sync_picker();
    }

    /// 名前の検索欄の打鍵が止まった: 今の文字で頼み直す。
    pub(super) fn lsp_symbols_timer(&mut self, cx: &mut LoopCx<'_>) {
        let Some(q) = self.lsp.locs.as_ref().filter(|s| s.symbols).map(|s| s.query.clone()) else { return };
        if q.is_empty() {
            return;
        }
        let req = self.lsp_ask(LspKind::Symbols, q, true, cx);
        if let Some(s) = self.lsp.locs.as_mut() {
            s.waiting = req.or(s.waiting);
        }
    }

    /// 場所へ移る（プロジェクトの中だけ）。
    fn goto_loc(&mut self, loc: &Loc, cx: &mut LoopCx<'_>) {
        let Some(path) = &loc.path else { return };
        self.goto_file_line(path, loc.line, cx);
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        if t.loading {
            self.lsp.goto_col = Some((t.id, loc.line, loc.col));
            return;
        }
        let mut line = String::new();
        let l = loc.line.min(t.doc.len_lines().saturating_sub(1));
        t.doc.line(l, &mut line);
        let pos = Pos::new(l, char_col(&line, loc.col));
        t.doc.set_selection(pos, pos);
        t.sync_cursor();
    }

    /// 読み込みが終わった文書: 定義へ飛んだ先なら、その桁へ。
    pub(super) fn lsp_opened(&mut self, doc: DocId) {
        let Some((_, l, c)) = self.lsp.goto_col.take_if(|(d, _, _)| *d == doc) else { return };
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == doc) else { return };
        let mut line = String::new();
        let l = l.min(t.doc.len_lines().saturating_sub(1));
        t.doc.line(l, &mut line);
        let pos = Pos::new(l, char_col(&line, c));
        t.doc.set_selection(pos, pos);
        t.sync_cursor();
    }

    pub(super) fn close_hover(&mut self) {
        if self.lsp.hover.take().is_some() {
            self.hover_stale = true;
        }
    }

    /// 補完の小窓を開いた・打っている: 打鍵が止まったら言語サーバーにも頼む。
    pub(super) fn arm_lsp_completion(&mut self, now: bool, cx: &mut LoopCx<'_>) {
        if self.focus != Focus::Editor || self.tabs.get(self.active).is_none_or(|t| !has_lsp(t.remote_lang())) {
            return;
        }
        if now {
            self.lsp_complete_timer(cx);
        } else {
            cx.timers.set_after(TimerId::LspComplete, LSP_COMPLETE_IDLE);
        }
    }

    /// 打鍵が止まった: 補完の場面（単語を打っている・`.` / `::` の直後）なら頼む。
    pub(super) fn lsp_complete_timer(&mut self, cx: &mut LoopCx<'_>) {
        if self.focus != Focus::Editor {
            return;
        }
        let Some(t) = self.tabs.get(self.active) else { return };
        let (a, h) = t.doc.selection();
        if a != h || t.loading {
            return;
        }
        let mut line = String::new();
        t.doc.line(h.line, &mut line);
        let before: String = line.chars().take(h.col as usize).collect();
        let word = before.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').count();
        let head = &before[..before.len() - before.chars().rev().take(word).map(char::len_utf8).sum::<usize>()];
        let after_dot = head.ends_with('.') || head.ends_with("::");
        if word == 0 && !after_dot {
            return;
        }
        if word > 0 && word < crate::intel::AUTO_MIN_CHARS && !after_dot && self.completion.is_none() {
            return;
        }
        self.lsp_ask(LspKind::Completion, String::new(), true, cx);
    }

    /// 答えが届いた。
    pub(super) fn on_lsp_result(&mut self, req: RemoteReq, status: DoneStatus, message: String, text: String, items: Vec<LspItem>, cx: &mut LoopCx<'_>) {
        let Some(i) = self.lsp.pending.iter().position(|p| p.req == req) else { return };
        let p = self.lsp.pending.remove(i);
        let quiet = p.kind == LspKind::Completion || (p.kind == LspKind::Symbols && self.lsp.locs.as_ref().is_none_or(|s| s.waiting != Some(req)));
        if status != DoneStatus::Ok {
            if status == DoneStatus::Cancelled || quiet {
                return;
            }
            // 準備中は毎回（自分で押した操作の返事）。上限・オフライン・認証・時計は 1 回だけ
            let loading = message.contains("準備中");
            if loading {
                self.lsp.told_loading = true;
                self.say(format!("型の情報: {message}"), false);
            } else if self.lsp.said != Some(status) {
                self.lsp.said = Some(status);
                let m = crate::intel::done_text(status, &message).unwrap_or(message);
                self.say(format!("型の情報: {m}"), false);
            }
            return;
        }
        self.lsp.said = None;
        // 古い答え（頼んだ後に編集した・カーソルを動かした・タブを替えた）は捨てる
        let Some(t) = self.tabs.get(self.active) else { return };
        let same_doc = t.id == p.doc && t.doc.version() == p.version;
        let same_pos = t.doc.selection().1 == p.pos;
        // 見せる場所は作業フォルダからの相対（メインでファイルを調べない）
        let root = Some(self.cfg.root.clone());
        match p.kind {
            LspKind::Hover => {
                if !same_doc || !same_pos || self.focus != Focus::Editor {
                    return;
                }
                let lines = hover_lines(&text);
                if lines.is_empty() {
                    self.say("型の情報は無い", false);
                    return;
                }
                self.hover_stale = true;
                self.lsp.hover = Some((p.doc, p.version, lines));
            }
            LspKind::Definition => {
                if !same_doc || !same_pos {
                    return;
                }
                let locs: Vec<Loc> = items.iter().map(|it| Loc::from_item(it, root.as_deref())).collect();
                match locs.as_slice() {
                    [] => self.say("定義が見つからない", false),
                    [one] if one.path.is_some() => {
                        let one = one.clone();
                        self.goto_loc(&one, cx);
                    }
                    [one] => {
                        let detail = if one.detail.is_empty() { String::new() } else { format!("  {}", one.detail) };
                        self.say(format!("定義は {}:{}{detail}", one.place, one.line + 1), false);
                    }
                    _ => self.open_locs(LocsState::new(format!("定義 — {} 件", locs.len()), locs, false, String::new())),
                }
            }
            LspKind::References => {
                if !same_doc {
                    return;
                }
                let locs: Vec<Loc> = items.iter().map(|it| Loc::from_item(it, root.as_deref())).collect();
                if locs.is_empty() {
                    self.say("使っている所は無い", false);
                    return;
                }
                let inside = locs.iter().filter(|l| l.path.is_some()).count();
                let title = format!("使っている所 — {} 件（プロジェクトの外 {} 件）", locs.len(), locs.len() - inside);
                self.open_locs(LocsState::new(title, locs, false, String::new()));
            }
            LspKind::Symbols => {
                let Some(s) = self.lsp.locs.as_mut().filter(|s| s.symbols && s.waiting == Some(req)) else { return };
                s.waiting = None;
                s.items = items.iter().map(|it| Loc::from_item(it, root.as_deref())).collect();
                s.refilter();
                self.sync_picker();
            }
            LspKind::Completion => {
                if !same_doc || !same_pos || self.focus != Focus::Editor {
                    return;
                }
                self.merge_lsp_completion(&items);
            }
        }
        self.status_due = true;
    }

    /// 言語サーバーの候補を補完の小窓に足す（小窓が無ければ開く）。
    fn merge_lsp_completion(&mut self, items: &[LspItem]) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let (_, h) = t.doc.selection();
        let mut line = String::new();
        t.doc.line(h.line, &mut line);
        let mut cands: Vec<LspCand> = Vec::new();
        for it in items {
            if it.line != h.line || it.insert.is_empty() {
                continue;
            }
            let start = char_col(&line, it.col).min(h.col);
            if cands.iter().any(|c| c.insert == it.insert) {
                continue;
            }
            cands.push(LspCand { label: it.name.clone(), insert: it.insert.clone(), start });
            if cands.len() >= LSP_COMPLETIONS {
                break;
            }
        }
        if cands.is_empty() {
            return;
        }
        if self.popup.drawn().is_some() {
            self.popup_stale = true;
        }
        match self.completion.as_mut() {
            Some(c) => {
                c.lsp = cands;
                c.sel = 0;
            }
            None => self.completion = Some(super::CompState { items: Vec::new(), sel: 0, prefix: String::new(), lsp: cands }),
        }
    }

    /// 言語サーバーの状態が変わった。
    pub(super) fn on_lsp_state(&mut self, state: LspState, message: String) {
        match state {
            LspState::Ready if std::mem::take(&mut self.lsp.told_loading) => {
                let m = if message.is_empty() { "型の情報: 準備できた".to_string() } else { format!("型の情報: 準備できた（{message}）") };
                self.say(m, false);
            }
            // 上限・起動できない: 知らせるのは問い合わせの答えのとき（ファイルを開くたびに出さない）
            _ => {}
        }
    }

    /// hover の小窓の項目。
    pub(super) fn hover_items(&self) -> Option<Vec<PopupItem>> {
        let (doc, version, lines) = self.lsp.hover.as_ref()?;
        let t = self.tabs.get(self.active)?;
        if t.id != *doc || t.doc.version() != *version || self.focus != Focus::Editor {
            return None;
        }
        Some(lines.iter().map(|l| PopupItem { tag: "", tag_color: palette::STATUS, label: l.clone(), hit_chars: 0 }).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_convert_between_chars_and_bytes() {
        let l = "let あ = 1; // 日本";
        assert_eq!(byte_col(l, 4), 4);
        assert_eq!(byte_col(l, 5), 7);
        assert_eq!(char_col(l, 7), 5);
        assert_eq!(char_col(l, 5), 4, "文字の途中は前の文字の頭");
        assert_eq!(byte_col(l, 999), l.len() as u32);
        assert_eq!(char_col(l, 999), l.chars().count() as u32);
    }

    #[test]
    fn hover_text_is_squeezed_and_cut() {
        assert_eq!(hover_lines("a\n\n\n\nb\n\n"), ["a", "", "b"]);
        assert!(hover_lines("").is_empty());
        let long: String = (0..20).map(|i| format!("l{i}\n")).collect();
        let v = hover_lines(&long);
        assert_eq!(v.len(), HOVER_LINES);
        assert_eq!(v.last().unwrap(), "…（あと 13 行）");
    }

    #[test]
    fn loc_rows_dim_outside_and_skip_them() {
        let root = std::path::Path::new("/p");
        let a = LspItem { path: "/p/src/a.rs".into(), line: 3, col: 4, detail: "foo();".into(), ..Default::default() };
        let b = LspItem { label: "core/src/option.rs".into(), line: 9, ..Default::default() };
        let c = LspItem { path: "/p/src/b.rs".into(), line: 0, detail: "fn foo() {}".into(), ..Default::default() };
        let items: Vec<Loc> = [b, a, c].iter().map(|i| Loc::from_item(i, Some(root))).collect();
        let mut s = LocsState::new("t".into(), items, false, String::new());
        assert_eq!(s.rows.len(), 3);
        assert!(s.rows[0].0.context, "外の場所は薄く");
        assert_eq!(s.rows[1].0.name, "src/a.rs:4  foo();");
        assert_eq!(s.sel, 1, "最初の選べる行");
        s.step(1);
        assert_eq!(s.sel, 2);
        s.step(1);
        assert_eq!(s.sel, 1, "外の行を飛ばして回る");
        assert_eq!(s.selected().unwrap().place, "src/a.rs");
        s.query = "b.rs".into();
        s.refilter();
        assert_eq!(s.rows.len(), 1);
        assert_eq!(s.selected().unwrap().line, 0);
    }
}
