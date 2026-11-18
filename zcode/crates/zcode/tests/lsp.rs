//! 型の情報（LSP。docs/lsp-design.md）のエディタ側: Ctrl+H・Ctrl+D・Ctrl+R・Ctrl+T・F8、補完に
//! rust-analyzer の候補を足す、古い答えを捨てる、失敗を 1 回だけ知らせる。ビルド係は偽物
//! （頼まれた問い合わせを覚えるだけ。答えは試験が `RemoteEvent::LspResult` で流す）。

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use zc_core::rt::{main_channel, MainReceiver, MainSender};
use zc_core::time::now_us;
use zc_core::{DiagSource, Diagnostic, DiagnosticSet, FileDiagnostics, Key, KeyEvent, Mods, Pos, Severity};
use zc_font::FontConfig;
use zc_remote_ipc::{DoneStatus, LspItem, LspKind, LspState};
use zcode::app::{AppEvent, LoopCx, Screen, TimerId, Timers, Workers};
use zcode::doc::DocBackend;
use zcode::editor::{EditorConfig, EditorScreen, RemoteLink};
use zcode::remote::{LspAsk, RemoteEvent, RemoteJob, RemoteReq};

struct Loop {
    timers: Timers,
    workers: Workers,
    tx: MainSender<AppEvent>,
    rx: MainReceiver<AppEvent>,
}

impl Loop {
    fn new() -> Loop {
        let (tx, rx) = main_channel().unwrap();
        Loop { timers: Timers::new().unwrap(), workers: Workers::new(), tx, rx }
    }
    fn with<R>(&mut self, f: impl FnOnce(&mut LoopCx<'_>) -> R) -> R {
        let mut cx = LoopCx { timers: &mut self.timers, workers: &self.workers, results: &self.tx, now_us: now_us(), quit: false, force_present: false };
        f(&mut cx)
    }
    fn keys(&mut self, ed: &mut EditorScreen, keys: &[KeyEvent]) {
        self.with(|cx| ed.keys(keys, Mods::NONE, cx));
    }
    fn type_str(&mut self, ed: &mut EditorScreen, s: &str) {
        for c in s.chars() {
            self.keys(ed, &[KeyEvent::char(c)]);
        }
    }
    fn pump(&mut self, ed: &mut EditorScreen) {
        self.workers.batch.wait_idle();
        self.workers.storage.wait_idle();
        self.workers.check.wait_idle();
        let evs: Vec<AppEvent> = self.rx.drain().collect();
        for ev in evs {
            self.with(|cx| ed.on_event(ev, cx));
        }
    }
    fn answer(&mut self, ed: &mut EditorScreen, req: RemoteReq, status: DoneStatus, message: &str, text: &str, items: Vec<LspItem>) {
        let ev = RemoteEvent::LspResult { req, status, message: message.into(), text: text.into(), items };
        self.with(|cx| ed.on_event(AppEvent::Remote(ev), cx));
    }
    fn fire(&mut self, ed: &mut EditorScreen, id: TimerId) {
        assert!(self.timers.deadline(id).is_some(), "{id:?} のタイマーが仕掛かっていない");
        self.timers.cancel(id);
        self.with(|cx| ed.on_timer(id, cx));
    }
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(Key::Char(c), Mods::CTRL)
}
fn key(k: Key) -> KeyEvent {
    KeyEvent::new(k, Mods::NONE)
}

/// 覚えた問い合わせ（種類・ファイル・行・桁・文字列・重ねたファイル）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Asked {
    kind: LspKind,
    file: PathBuf,
    line: u32,
    col: u32,
    text: String,
    overlay: Vec<PathBuf>,
}

#[derive(Clone, Default)]
struct FakeLsp {
    asks: Arc<Mutex<Vec<Asked>>>,
    warmed: Arc<Mutex<Vec<PathBuf>>>,
    /// 設定が無い人（`lsp` が `None` を返す）。
    off: bool,
}

impl RemoteLink for FakeLsp {
    fn submit(&mut self, _job: RemoteJob, _cx: &mut LoopCx<'_>) -> RemoteReq {
        RemoteReq(9999)
    }
    fn lsp(&mut self, ask: LspAsk, _cx: &mut LoopCx<'_>) -> Option<RemoteReq> {
        if self.off {
            return None;
        }
        let mut v = self.asks.lock().unwrap();
        v.push(Asked { kind: ask.kind, file: ask.file, line: ask.line, col: ask.col, text: ask.text, overlay: ask.overlay.iter().map(|o| o.path.clone()).collect() });
        Some(RemoteReq(v.len() as u32))
    }
    fn warm(&mut self, file: PathBuf, _cx: &mut LoopCx<'_>) {
        self.warmed.lock().unwrap().push(file);
    }
}

impl FakeLsp {
    fn last(&self) -> (RemoteReq, Asked) {
        let v = self.asks.lock().unwrap();
        (RemoteReq(v.len() as u32), v.last().cloned().expect("問い合わせが無い"))
    }
    fn count(&self) -> usize {
        self.asks.lock().unwrap().len()
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zcode-lsp-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

const MAIN: &str = "mod util;\nfn main() {\n    let s = \"日本\"; let n = util::helper(1);\n    println!(\"{s}{n}\");\n}\n";
const UTIL: &str = "pub fn helper(a: i32) -> i32 {\n    a\n}\n";

/// `proj/`（Cargo.toml・src/main.rs・src/util.rs）を作り、main.rs を開く。
fn project(tag: &str, fake: &FakeLsp) -> (PathBuf, PathBuf, EditorScreen, Loop) {
    let dir = tmp_dir(tag);
    let proj = dir.join("proj");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::write(proj.join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
    fs::write(proj.join("src/main.rs"), MAIN).unwrap();
    fs::write(proj.join("src/util.rs"), UTIL).unwrap();
    fs::write(proj.join("notes.py"), "x = 1\n").unwrap();
    fs::write(proj.join("notes.md"), "# x\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Buffer, root: dir.clone(), files: vec![proj.join("src/main.rs")] });
    ed.set_remote(Box::new(fake.clone()));
    lp.with(|cx| ed.start(cx));
    lp.pump(&mut ed);
    (dir, proj, ed, lp)
}

/// カーソルを置く（行と文字の位置）。
fn place(ed: &mut EditorScreen, lp: &mut Loop, line: u32, col: u32) {
    lp.keys(ed, &[KeyEvent::new(Key::Home, Mods::CTRL)]);
    for _ in 0..line {
        lp.keys(ed, &[key(Key::Down)]);
    }
    // Home は行頭の空白の後ろへ行くので使わない（Ctrl+Home の後の ↓ は桁 0 のまま）
    for _ in 0..col {
        lp.keys(ed, &[key(Key::Right)]);
    }
    assert_eq!(ed.active_doc().unwrap().selection().1, Pos::new(line, col));
}

#[test]
fn opening_a_file_warms_its_language_server() {
    let fake = FakeLsp::default();
    let (dir, proj, mut ed, mut lp) = project("warm", &fake);
    assert_eq!(*fake.warmed.lock().unwrap(), [proj.join("src/main.rs")]);
    // Rust 以外でも言語サーバーのある言語（Python）は起こす
    lp.with(|cx| ed.open_path(&proj.join("notes.py"), cx));
    lp.pump(&mut ed);
    assert_eq!(*fake.warmed.lock().unwrap(), [proj.join("src/main.rs"), proj.join("notes.py")]);
    // ビルド係を使わない言語（Markdown）は起こさない
    lp.with(|cx| ed.open_path(&proj.join("notes.md"), cx));
    lp.pump(&mut ed);
    assert_eq!(fake.warmed.lock().unwrap().len(), 2);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn hover_shows_a_popup_and_drops_stale_answers() {
    let fake = FakeLsp::default();
    let (dir, proj, mut ed, mut lp) = project("hover", &fake);
    // 3 行目の「日本」の後ろの helper（文字の位置 30 → UTF-8 のバイト位置 34）
    let line = MAIN.split('\n').nth(2).unwrap();
    let ch = line[..line.find("helper").unwrap()].chars().count() as u32 + 2;
    place(&mut ed, &mut lp, 2, ch);
    lp.keys(&mut ed, &[ctrl('h')]);
    let (req, asked) = fake.last();
    assert_eq!(asked, Asked { kind: LspKind::Hover, file: proj.join("src/main.rs"), line: 2, col: line.find("helper").unwrap() as u32 + 2, text: String::new(), overlay: vec![] });
    lp.answer(&mut ed, req, DoneStatus::Ok, "", "p::util\n\n\npub fn helper(a: i32) -> i32\n\n\n説明", vec![]);
    assert_eq!(ed.hover(), Some(vec!["p::util".into(), "".into(), "pub fn helper(a: i32) -> i32".into(), "".into(), "説明".into()]));
    check_golden(&mut ed, "lsp-hover.png");
    // 次の打鍵で閉じる（Esc は閉じるだけで、ほかに何もしない）
    lp.keys(&mut ed, &[key(Key::Esc)]);
    assert_eq!(ed.hover(), None);
    // 頼んだ後に打った: 答えは捨てる。未保存の文書は重ねて頼む
    lp.keys(&mut ed, &[ctrl('h')]);
    let (req, _) = fake.last();
    lp.type_str(&mut ed, "x");
    lp.answer(&mut ed, req, DoneStatus::Ok, "", "古い", vec![]);
    assert_eq!(ed.hover(), None);
    lp.keys(&mut ed, &[ctrl('h')]);
    assert_eq!(fake.last().1.overlay, [proj.join("src/main.rs")]);
    // 何も無い
    let (req, _) = fake.last();
    lp.answer(&mut ed, req, DoneStatus::Ok, "", "", vec![]);
    assert_eq!(ed.message(), Some("型の情報は無い"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn definition_jumps_inside_and_tells_outside() {
    let fake = FakeLsp::default();
    let (dir, proj, mut ed, mut lp) = project("def", &fake);
    place(&mut ed, &mut lp, 2, 30);
    lp.keys(&mut ed, &[ctrl('d')]);
    let (req, asked) = fake.last();
    assert_eq!(asked.kind, LspKind::Definition);
    let util = proj.join("src/util.rs");
    lp.answer(&mut ed, req, DoneStatus::Ok, "", "", vec![LspItem { path: util.clone(), line: 0, col: 7, end: Some((0, 13)), detail: "pub fn helper(a: i32) -> i32 {".into(), ..Default::default() }]);
    lp.pump(&mut ed);
    assert_eq!(ed.active_path(), Some(util.as_path()));
    assert_eq!(ed.active_doc().unwrap().selection().1, Pos::new(0, 7));
    // プロジェクトの外: ステータスバーに「定義は …」
    lp.keys(&mut ed, &[ctrl('d')]);
    let (req, _) = fake.last();
    lp.answer(&mut ed, req, DoneStatus::Ok, "", "", vec![LspItem { label: "core/src/num/mod.rs".into(), line: 570, col: 4, detail: "pub struct i32".into(), ..Default::default() }]);
    assert_eq!(ed.message(), Some("定義は core/src/num/mod.rs:571  pub struct i32"));
    // 見つからない
    lp.keys(&mut ed, &[ctrl('d')]);
    let (req, _) = fake.last();
    lp.answer(&mut ed, req, DoneStatus::Ok, "", "", vec![]);
    assert_eq!(ed.message(), Some("定義が見つからない"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn references_list_dims_outside_and_jumps_on_enter() {
    let fake = FakeLsp::default();
    let (dir, proj, mut ed, mut lp) = project("refs", &fake);
    place(&mut ed, &mut lp, 2, 30);
    lp.keys(&mut ed, &[ctrl('r')]);
    let (req, asked) = fake.last();
    assert_eq!(asked.kind, LspKind::References);
    let items = vec![
        LspItem { label: "alloc/src/vec/mod.rs".into(), line: 9, ..Default::default() },
        LspItem { path: proj.join("src/main.rs"), line: 2, col: 34, detail: "let s = \"日本\"; let n = util::helper(1);".into(), ..Default::default() },
        LspItem { path: proj.join("src/util.rs"), line: 0, col: 7, detail: "pub fn helper(a: i32) -> i32 {".into(), ..Default::default() },
    ];
    lp.answer(&mut ed, req, DoneStatus::Ok, "", "", items);
    let (title, rows, sel) = ed.locs_rows().unwrap();
    assert_eq!(title, "使っている所 — 3 件（プロジェクトの外 1 件）");
    assert_eq!(rows[0], ("alloc/src/vec/mod.rs:10  ".into(), true));
    assert_eq!(rows[2], ("proj/src/util.rs:1  pub fn helper(a: i32) -> i32 {".into(), false));
    assert_eq!(sel, 1, "最初の選べる行");
    check_golden(&mut ed, "lsp-references.png");
    // ↓ で外の行を飛ばして回る。Enter でその場所へ
    lp.keys(&mut ed, &[key(Key::Down)]);
    assert_eq!(ed.locs_rows().unwrap().2, 2);
    lp.keys(&mut ed, &[key(Key::Down)]);
    assert_eq!(ed.locs_rows().unwrap().2, 1);
    lp.keys(&mut ed, &[key(Key::Down), key(Key::Enter)]);
    lp.pump(&mut ed);
    assert_eq!(ed.locs_rows(), None);
    assert_eq!(ed.active_path(), Some(proj.join("src/util.rs").as_path()));
    assert_eq!(ed.active_doc().unwrap().selection().1, Pos::new(0, 7));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn symbol_search_asks_again_when_typing_stops() {
    let fake = FakeLsp::default();
    let (dir, proj, mut ed, mut lp) = project("sym", &fake);
    // helper の「he」の後ろ
    place(&mut ed, &mut lp, 2, 34);
    lp.keys(&mut ed, &[ctrl('t')]);
    // カーソルの単語の前半（カーソルまで）で探し始める
    let (req, asked) = fake.last();
    assert_eq!((asked.kind, asked.text.as_str()), (LspKind::Symbols, "he"));
    lp.answer(&mut ed, req, DoneStatus::Ok, "", "", vec![LspItem { path: proj.join("src/util.rs"), name: "helper".into(), kind: 12, line: 0, col: 7, ..Default::default() }]);
    let (_, rows, _) = ed.locs_rows().unwrap();
    assert_eq!(rows, [("helper".to_string(), false)]);
    // 打つと、止まったところで頼み直す（打鍵ごとには頼まない）
    lp.type_str(&mut ed, "lp");
    assert_eq!(fake.count(), 1);
    lp.fire(&mut ed, TimerId::LspSymbols);
    let (req2, asked) = fake.last();
    assert_eq!(asked.text, "help");
    // 前の検索の遅れた答えは捨てる
    lp.answer(&mut ed, req, DoneStatus::Ok, "", "", vec![]);
    lp.answer(&mut ed, req2, DoneStatus::Ok, "", "", vec![LspItem { path: proj.join("src/util.rs"), name: "helper".into(), kind: 12, ..Default::default() }, LspItem { label: "x/y.rs".into(), name: "help_ext".into(), ..Default::default() }]);
    let (_, rows, _) = ed.locs_rows().unwrap();
    assert_eq!(rows, [("helper".to_string(), false), ("help_ext".to_string(), true)]);
    lp.keys(&mut ed, &[key(Key::Esc)]);
    assert_eq!(ed.locs_rows(), None);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn f8_walks_the_local_diagnostics_and_wraps() {
    // ビルド係を使わない人（設定が無い）でも効く
    let fake = FakeLsp { off: true, ..Default::default() };
    let (dir, proj, mut ed, mut lp) = project("f8", &fake);
    lp.keys(&mut ed, &[key(Key::F(8))]);
    assert_eq!(ed.message(), Some("診断は無い"));
    let diag = |line, col| Diagnostic { line, col: Some(col), end: None, severity: Severity::Error, code: None, message: format!("e{line}") };
    let set = DiagnosticSet { source: DiagSource::Syntax, complete: false, files: vec![FileDiagnostics { path: Arc::from(proj.join("src/main.rs").as_path()), base: None, diags: vec![diag(3, 4), diag(2, 34)] }] };
    let doc = ed.active_doc_id().unwrap();
    lp.with(|cx| ed.on_event(AppEvent::Checked { doc, result: Ok(set) }, cx));
    place(&mut ed, &mut lp, 0, 0);
    lp.keys(&mut ed, &[key(Key::F(8))]);
    // UTF-8 の 34 バイト目 = 「日本」の後ろの文字の位置 30
    assert_eq!(ed.active_doc().unwrap().selection().1, Pos::new(2, 30));
    lp.keys(&mut ed, &[key(Key::F(8))]);
    assert_eq!(ed.active_doc().unwrap().selection().1, Pos::new(3, 4));
    lp.keys(&mut ed, &[key(Key::F(8))]);
    assert_eq!(ed.active_doc().unwrap().selection().1, Pos::new(2, 30), "最後の次は最初へ");
    lp.keys(&mut ed, &[KeyEvent::new(Key::F(8), Mods::SHIFT)]);
    assert_eq!(ed.active_doc().unwrap().selection().1, Pos::new(3, 4), "最初の前は最後へ");
    // 設定が無いときの Ctrl+H は 1 回だけ知らせる
    lp.keys(&mut ed, &[ctrl('h')]);
    assert!(ed.message().unwrap().contains("設定が無い"), "{:?}", ed.message());
    lp.keys(&mut ed, &[ctrl('h')]);
    assert_eq!(ed.message(), None);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn failures_are_told_once_and_loading_every_time() {
    let fake = FakeLsp::default();
    let (dir, proj, mut ed, mut lp) = project("fail", &fake);
    let ask = |ed: &mut EditorScreen, lp: &mut Loop| {
        lp.keys(ed, &[ctrl('h')]);
        fake.last().0
    };
    let r = ask(&mut ed, &mut lp);
    lp.answer(&mut ed, r, DoneStatus::Limit, "今月のビルド係の費用が上限に達した", "", vec![]);
    assert!(ed.message().unwrap().contains("上限"), "{:?}", ed.message());
    let r = ask(&mut ed, &mut lp);
    lp.answer(&mut ed, r, DoneStatus::Limit, "今月のビルド係の費用が上限に達した", "", vec![]);
    assert_eq!(ed.message(), None, "同じ理由は繰り返さない");
    let r = ask(&mut ed, &mut lp);
    lp.answer(&mut ed, r, DoneStatus::Offline, "ネットワークが無い", "", vec![]);
    assert!(ed.message().is_some());
    // 準備中は毎回。準備できたら 1 回だけ知らせる
    for _ in 0..2 {
        let r = ask(&mut ed, &mut lp);
        lp.answer(&mut ed, r, DoneStatus::Error, "rust-analyzer が準備中（プロジェクトを読んでいる）", "", vec![]);
        assert_eq!(ed.message(), Some("型の情報: rust-analyzer が準備中（プロジェクトを読んでいる）"));
        lp.keys(&mut ed, &[key(Key::Right)]);
    }
    lp.with(|cx| ed.on_event(AppEvent::Remote(RemoteEvent::LspState { root: proj.clone(), state: LspState::Ready, message: String::new() }), cx));
    assert_eq!(ed.message(), Some("型の情報: 準備できた"));
    // 言語サーバーの無いファイル（Markdown）は頼まずに知らせる
    lp.with(|cx| ed.open_path(&proj.join("notes.md"), cx));
    lp.pump(&mut ed);
    let n = fake.count();
    lp.keys(&mut ed, &[ctrl('h')]);
    assert_eq!(fake.count(), n);
    assert!(ed.message().unwrap().contains("対象ではない"), "{:?}", ed.message());
    // Python は頼む
    lp.with(|cx| ed.open_path(&proj.join("notes.py"), cx));
    lp.pump(&mut ed);
    lp.keys(&mut ed, &[ctrl('h')]);
    assert_eq!(fake.count(), n + 1);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn completion_adds_rust_analyzer_items_when_typing_stops() {
    let fake = FakeLsp::default();
    let (dir, _proj, mut ed, mut lp) = project("comp", &fake);
    place(&mut ed, &mut lp, 3, 0);
    lp.keys(&mut ed, &[key(Key::End)]);
    lp.type_str(&mut ed, " n.");
    // 「.」の直後: 単語補完は出ない。打鍵が止まったら rust-analyzer に頼む
    assert_eq!(ed.completion(), None);
    assert_eq!(fake.count(), 0, "打鍵ごとには頼まない");
    lp.fire(&mut ed, TimerId::LspComplete);
    let (req, asked) = fake.last();
    assert_eq!(asked.kind, LspKind::Completion);
    let col = asked.col;
    let item = |name: &str| LspItem { line: 3, col, name: name.into(), insert: name.into(), kind: 2, ..Default::default() };
    lp.answer(&mut ed, req, DoneStatus::Ok, "", "", vec![item("abs"), item("pow")]);
    assert_eq!(ed.completion(), Some((vec!["abs".to_string(), "pow".to_string()], 0)));
    // 打つと絞り込む（rust-analyzer の候補は残し、単語補完も足す）
    lp.type_str(&mut ed, "p");
    let (labels, sel) = ed.completion().unwrap();
    assert_eq!((labels[0].as_str(), sel), ("pow", 0), "{labels:?}");
    assert!(!labels.contains(&"abs".to_string()));
    lp.keys(&mut ed, &[key(Key::Tab)]);
    let mut text = String::new();
    zc_core::LineSource::line(ed.active_doc().unwrap().as_lines(), 3, &mut text);
    assert!(text.ends_with(" n.pow"), "{text:?}");
    // 古い答え（頼んだ後に打った）は捨てる
    lp.type_str(&mut ed, "(); n.");
    lp.fire(&mut ed, TimerId::LspComplete);
    let (req, _) = fake.last();
    lp.type_str(&mut ed, "z");
    lp.answer(&mut ed, req, DoneStatus::Ok, "", "", vec![item("zzz")]);
    assert!(ed.completion().is_none_or(|c| !c.0.contains(&"zzz".to_string())));
    // 補完の失敗は何も出さない
    lp.fire(&mut ed, TimerId::LspComplete);
    let (req, _) = fake.last();
    lp.answer(&mut ed, req, DoneStatus::Offline, "ネットワークが無い", "", vec![]);
    assert_eq!(ed.message(), None);
    let _ = fs::remove_dir_all(dir);
}

fn check_golden(ed: &mut EditorScreen, name: &str) {
    let fonts = FontConfig::default();
    if !fonts.available() {
        return;
    }
    let canvas = zcode::render_screen(ed, &fonts).unwrap();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden").join(name);
    if let Err(e) = zc_display::png::check_golden(&canvas, path) {
        panic!("{e}");
    }
}

