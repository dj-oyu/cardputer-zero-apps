//! エディタ画面（`EditorScreen`）の動き: 打鍵 → 操作 → 文書、ツリー、保存、
//! ステータスバーの遅延、描き変える範囲。実機もイベントループも要らない
//! （ワーカーと、結果を受ける口だけ本物を使う）。動きの試験は、文書を `SimpleDoc` と
//! 本物の `zc_buffer::Buffer` の両方で回す（`both!`）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use zc_core::rt::{main_channel, MainReceiver, MainSender};
use zc_core::time::now_us;
use zc_core::{Canvas, Damage, DiagSource, Diagnostic, DiagnosticSet, FileDiagnostics, Key, KeyEvent, Mods, Pos, Priority, RowRange, Severity, Theme};
use zc_font::{FontConfig, Fonts, RasterMode};
use zc_ui::layout::CODE_LINE_H;
use zc_ui::RenderCx;
use zcode::app::{AppEvent, LoopCx, Screen, TimerId, Timers, Workers};
use zcode::doc::{DocBackend, SimpleDoc};
use zcode::edbench::{doc_text, EdBenchKind, EditorBench};
use zc_remote_ipc::{Backend, DoneStatus, FileDiags, JobKind, JobTag, Phase, RemoteLang, SubmitFlags, TestCase, TestReport};
use zcode::editor::{EditorConfig, EditorScreen, RemoteLink};
use zcode::remote::{RemoteEvent, RemoteJob, RemoteReq};

/// イベントループの代わり。
struct Loop {
    timers: Timers,
    workers: Workers,
    tx: MainSender<AppEvent>,
    rx: MainReceiver<AppEvent>,
    quit: bool,
}

impl Loop {
    fn new() -> Loop {
        let (tx, rx) = main_channel().unwrap();
        Loop { timers: Timers::new().unwrap(), workers: Workers::new(), tx, rx, quit: false }
    }
    fn with<R>(&mut self, f: impl FnOnce(&mut LoopCx<'_>) -> R) -> R {
        let mut cx = LoopCx { timers: &mut self.timers, workers: &self.workers, results: &self.tx, now_us: now_us(), quit: false, force_present: false };
        let r = f(&mut cx);
        self.quit |= cx.quit;
        r
    }
    /// 打鍵を流す。
    fn keys(&mut self, ed: &mut EditorScreen, keys: &[KeyEvent]) {
        self.with(|cx| ed.keys(keys, Mods::NONE, cx));
    }
    /// 文字列を 1 字ずつ打つ（`\n` は Enter）。
    fn type_str(&mut self, ed: &mut EditorScreen, s: &str) {
        for c in s.chars() {
            let ev = if c == '\n' { key(Key::Enter) } else { KeyEvent::char(c) };
            self.keys(ed, &[ev]);
        }
    }
    /// ワーカーが終わるのを待って、結果を画面に届ける。
    fn pump(&mut self, ed: &mut EditorScreen) {
        self.workers.batch.wait_idle();
        self.workers.storage.wait_idle();
        self.workers.check.wait_idle();
        let evs: Vec<AppEvent> = self.rx.drain().collect();
        for ev in evs {
            self.with(|cx| ed.on_event(ev, cx));
        }
    }
}

fn key(k: Key) -> KeyEvent {
    KeyEvent::new(k, Mods::NONE)
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(Key::Char(c), Mods::CTRL)
}

/// テストごとに別のフォルダ（同じ tag の simple / buffer 版が並列に走っても衝突しない）。
fn tmp_dir(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("zcode-editor-{tag}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// 文書を 1 つ開いたエディタ。
fn editor_with(backend: DocBackend, text: &str) -> EditorScreen {
    let cfg = EditorConfig { backend, root: PathBuf::from("/nonexistent"), files: vec![PathBuf::from("a.py")] };
    let mut ed = EditorScreen::new(cfg);
    let doc: Box<dyn zcode::doc::TextDoc + Send> = match backend {
        DocBackend::Simple => Box::new(SimpleDoc::from_text(text)),
        DocBackend::Buffer => Box::new(zc_buffer::Buffer::from_text(text)),
    };
    ed.open_doc(Some(PathBuf::from("/nonexistent/a.py")), "a.py", doc, None);
    ed
}

/// 末尾に改行のあるファイルを開いたときの文書の中身。本物のバッファは末尾の改行を
/// 「空の最終行」として保つ（VS Code と同じ）。`SimpleDoc` は読むときに落とし、保存で足す。
fn loaded(backend: DocBackend, s: &str) -> String {
    match backend {
        DocBackend::Buffer => format!("{s}\n"),
        DocBackend::Simple => s.to_string(),
    }
}

/// 新しい文書を保存したときのファイルの中身。本物のバッファは書いたとおりに保存する
/// （末尾の改行を自動では足さない。VS Code の既定と同じ）。`SimpleDoc` は必ず足す。
fn saved_new(backend: DocBackend, s: &str) -> String {
    match backend {
        DocBackend::Buffer => s.to_string(),
        DocBackend::Simple => format!("{s}\n"),
    }
}

/// 試験を `SimpleDoc` と本物の `zc_buffer::Buffer` の両方で回す。
macro_rules! both {
    ($name:ident, $simple:ident, $buffer:ident) => {
        #[test]
        fn $simple() {
            $name(DocBackend::Simple)
        }
        #[test]
        fn $buffer() {
            $name(DocBackend::Buffer)
        }
    };
}

fn text(ed: &EditorScreen) -> String {
    doc_text(ed.active_doc().unwrap())
}
fn cursor(ed: &EditorScreen) -> Pos {
    ed.active_doc().unwrap().selection().1
}

/// 描画の道具（フォントが無ければ `None`）。
struct Gfx {
    fonts: Fonts,
    theme: Theme,
    canvas: Canvas,
}

impl Gfx {
    fn new() -> Option<Gfx> {
        let cfg = FontConfig::default();
        if !cfg.available() {
            eprintln!("フォントが無いので飛ばす");
            return None;
        }
        Some(Gfx { fonts: Fonts::load(&cfg, RasterMode::Inline).unwrap(), theme: Theme::dark_plus(), canvas: Canvas::new() })
    }
    fn render(&mut self, s: &mut dyn Screen) -> Damage {
        s.render(&mut RenderCx::new(&mut self.canvas, &mut self.fonts, &self.theme))
    }
}

fn ranges(d: &Damage) -> Vec<(u16, u16, Priority)> {
    let mut v: Vec<_> = d.iter().map(|(r, p)| (r.start, r.end, p)).collect();
    v.sort();
    v
}

both!(typing_newline_indent_and_undo, typing_newline_indent_and_undo_simple, typing_newline_indent_and_undo_buffer);
fn typing_newline_indent_and_undo(backend: DocBackend) {
    let mut lp = Loop::new();
    let mut ed = editor_with(backend, "");
    lp.type_str(&mut ed, "def f():\nx = 1");
    // `:` で終わる行のあとは 1 段深くなる
    assert_eq!(text(&ed), "def f():\n    x = 1");
    lp.type_str(&mut ed, "\ny");
    assert_eq!(text(&ed), "def f():\n    x = 1\n    y");
    // Tab は次の字下げ位置まで、Shift+Tab で戻す
    lp.keys(&mut ed, &[key(Key::Tab)]);
    assert_eq!(cursor(&ed), Pos::new(2, 8));
    lp.keys(&mut ed, &[KeyEvent::new(Key::Tab, Mods::SHIFT)]);
    assert_eq!(text(&ed), "def f():\n    x = 1\ny   ");
    lp.keys(&mut ed, &[ctrl('z'), ctrl('z')]);
    assert_eq!(text(&ed), "def f():\n    x = 1\n    y");
    // BackSpace / Delete / 矢印
    lp.keys(&mut ed, &[key(Key::Backspace), key(Key::Up), key(Key::End), key(Key::Home), key(Key::Delete)]);
    assert_eq!(text(&ed), "def f():\n     = 1\n    ");
    // 行コメント（.py なので #）
    lp.keys(&mut ed, &[ctrl('/')]);
    assert_eq!(text(&ed), "def f():\n     # = 1\n    ");
    // 全部選んで切り取り → 貼り付け
    lp.keys(&mut ed, &[ctrl('a'), ctrl('x')]);
    assert_eq!(text(&ed), "");
    lp.keys(&mut ed, &[ctrl('v'), ctrl('v')]);
    assert_eq!(text(&ed), "def f():\n     # = 1\n    def f():\n     # = 1\n    ");
    // Ctrl+Shift+K で行を消す
    lp.keys(&mut ed, &[KeyEvent::new(Key::Char('k'), Mods::CTRL | Mods::SHIFT)]);
    assert_eq!(text(&ed), "def f():\n     # = 1\n    def f():\n     # = 1");
    // リピートの打鍵は、繰り返してよい操作にだけ渡す
    let mut rep = ctrl('a');
    rep.repeat = true;
    lp.keys(&mut ed, &[key(Key::Esc), rep]);
    let (a, h) = ed.active_doc().unwrap().selection();
    assert_eq!(a, h);
}

both!(quit_and_close_ask_when_modified, quit_and_close_ask_when_modified_simple, quit_and_close_ask_when_modified_buffer);
fn quit_and_close_ask_when_modified(backend: DocBackend) {
    let mut lp = Loop::new();
    let mut ed = editor_with(backend, "x");
    lp.keys(&mut ed, &[ctrl('q')]);
    assert!(lp.quit, "未保存が無ければすぐ終わる");
    let mut lp = Loop::new();
    lp.type_str(&mut ed, "y");
    lp.keys(&mut ed, &[ctrl('q')]);
    assert!(!lp.quit);
    assert!(ed.message().unwrap().contains("未保存"));
    // ほかの打鍵をはさむと、確認はやり直し
    lp.keys(&mut ed, &[key(Key::Left), ctrl('q')]);
    assert!(!lp.quit);
    lp.keys(&mut ed, &[ctrl('w')]);
    assert_eq!(ed.tab_count(), 1);
    lp.keys(&mut ed, &[ctrl('w')]);
    assert_eq!(ed.tab_count(), 0);
    lp.keys(&mut ed, &[ctrl('q')]);
    assert!(lp.quit);
}

both!(tabs_new_file_and_switching, tabs_new_file_and_switching_simple, tabs_new_file_and_switching_buffer);
fn tabs_new_file_and_switching(backend: DocBackend) {
    let mut lp = Loop::new();
    let mut ed = editor_with(backend, "first");
    lp.keys(&mut ed, &[ctrl('n')]);
    assert_eq!(ed.tab_count(), 2);
    lp.type_str(&mut ed, "second");
    assert_eq!(text(&ed), "second");
    lp.keys(&mut ed, &[KeyEvent::new(Key::Tab, Mods::CTRL)]);
    assert_eq!(text(&ed), "first");
    lp.keys(&mut ed, &[KeyEvent::new(Key::Tab, Mods::CTRL | Mods::SHIFT)]);
    assert_eq!(text(&ed), "second");
    assert_eq!(ed.active_path(), None);
}

both!(goto_line_and_find_prompts, goto_line_and_find_prompts_simple, goto_line_and_find_prompts_buffer);
fn goto_line_and_find_prompts(backend: DocBackend) {
    let mut lp = Loop::new();
    let body: String = (1..=50).map(|i| format!("line {i} foo\n")).collect();
    let mut ed = editor_with(backend, &body);
    lp.keys(&mut ed, &[ctrl('g')]);
    lp.type_str(&mut ed, "3x7\n"); // 数字だけ入る
    assert_eq!(cursor(&ed), Pos::new(36, 0));
    assert_eq!(text(&ed).lines().count(), 50, "入力欄に打った字は文書に入らない");
    // 検索: Enter のたびに次へ。Esc で閉じる
    lp.keys(&mut ed, &[ctrl('f')]);
    lp.type_str(&mut ed, "foo\n");
    assert_eq!(ed.active_doc().unwrap().selection(), (Pos::new(36, 8), Pos::new(36, 11)));
    lp.type_str(&mut ed, "\n");
    assert_eq!(cursor(&ed), Pos::new(37, 11));
    lp.keys(&mut ed, &[key(Key::Esc), key(Key::F(3))]);
    assert_eq!(cursor(&ed), Pos::new(38, 11));
    lp.keys(&mut ed, &[KeyEvent::new(Key::F(3), Mods::SHIFT)]);
    assert_eq!(ed.active_doc().unwrap().selection(), (Pos::new(37, 8), Pos::new(37, 11)));
    // 見つからない
    lp.keys(&mut ed, &[ctrl('f')]);
    lp.keys(&mut ed, &[key(Key::Backspace), key(Key::Backspace), key(Key::Backspace)]);
    lp.type_str(&mut ed, "zzz\n");
    assert!(ed.message().unwrap().contains("見つからない"));
}

both!(open_from_tree_edit_and_save, open_from_tree_edit_and_save_simple, open_from_tree_edit_and_save_buffer);
fn open_from_tree_edit_and_save(backend: DocBackend) {
    let dir = tmp_dir("save");
    fs::create_dir_all(dir.join("blink")).unwrap();
    fs::write(dir.join("blink/main.py"), "import time\n").unwrap();
    fs::write(dir.join("notes.txt"), "memo\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend, root: dir.clone(), files: vec![] });
    lp.with(|cx| ed.start(cx));
    // ファイルを渡さずに起動: ツリーに入力がある。blink を開いて main.py を選ぶ
    assert_eq!(ed.tab_count(), 0);
    lp.keys(&mut ed, &[key(Key::Enter), key(Key::Down), key(Key::Enter)]);
    assert_eq!(ed.tab_count(), 1);
    assert_eq!(ed.active_path(), Some(dir.join("blink/main.py").as_path()));
    // 読み込みは batch。終わるまでは空で、打鍵も入らない
    lp.type_str(&mut ed, "zzz");
    lp.pump(&mut ed);
    assert_eq!(text(&ed), loaded(backend, "import time"));
    // 編集して保存（storage スレッドが書く）
    lp.keys(&mut ed, &[key(Key::End)]);
    lp.type_str(&mut ed, "\nprint(1)");
    assert!(ed.active_doc().unwrap().is_modified());
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    assert_eq!(fs::read_to_string(dir.join("blink/main.py")).unwrap(), "import time\nprint(1)\n");
    assert!(!ed.active_doc().unwrap().is_modified());
    assert!(ed.message().unwrap().contains("保存した"));
    // 保存を頼んだあとに打った分は、未保存のまま残る
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.type_str(&mut ed, "!");
    lp.pump(&mut ed);
    assert_eq!(fs::read_to_string(dir.join("blink/main.py")).unwrap(), "import time\nprint(1)\n");
    assert!(ed.active_doc().unwrap().is_modified());
    // 同じファイルをもう一度開くと、同じタブへ
    lp.keys(&mut ed, &[ctrl('e'), key(Key::Enter)]);
    assert_eq!(ed.tab_count(), 1);
    // 新しいファイル: 名前を聞いてから保存し、ツリーにも出る
    lp.keys(&mut ed, &[ctrl('n')]);
    lp.type_str(&mut ed, "x = 1");
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.type_str(&mut ed, "blink/new.py\n");
    lp.pump(&mut ed);
    assert_eq!(fs::read_to_string(dir.join("blink/new.py")).unwrap(), saved_new(backend, "x = 1"));
    assert_eq!(ed.active_path(), Some(dir.join("blink/new.py").as_path()));
    assert!(ed.tree_mut().nodes().iter().any(|n| n.name == "new.py"));
    // 保存できない場所
    lp.keys(&mut ed, &[ctrl('n')]);
    lp.type_str(&mut ed, "y");
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.type_str(&mut ed, "no/such/dir/y.py\n");
    lp.pump(&mut ed);
    assert!(ed.message().unwrap().contains("保存できない"));
    assert!(ed.active_doc().unwrap().is_modified());
    // 無いファイルを開くと、その名前の新しい文書
    lp.with(|cx| ed.open_path(&dir.join("fresh.py"), cx));
    lp.pump(&mut ed);
    assert_eq!((text(&ed), ed.active_path()), (String::new(), Some(dir.join("fresh.py").as_path())));
    // UTF-8 でないファイルは開かない
    fs::write(dir.join("bin.dat"), [0xffu8, 0x00, 0xfe]).unwrap();
    let n = ed.tab_count();
    lp.with(|cx| ed.open_path(&dir.join("bin.dat"), cx));
    lp.pump(&mut ed);
    assert_eq!(ed.tab_count(), n);
    assert!(ed.message().unwrap().contains("開けない"));
    let _ = fs::remove_dir_all(&dir);
}

both!(keystroke_damage_is_cursor_line_and_status_is_deferred, keystroke_damage_is_cursor_line_and_status_is_deferred_simple, keystroke_damage_is_cursor_line_and_status_is_deferred_buffer);
fn keystroke_damage_is_cursor_line_and_status_is_deferred(backend: DocBackend) {
    let Some(mut g) = Gfx::new() else { return };
    let mut lp = Loop::new();
    let body: String = (1..=40).map(|i| format!("value_{i} = {i}\n")).collect();
    let mut ed = editor_with(backend, &body);
    let d = g.render(&mut ed);
    assert_eq!(d.bounds(), RowRange::new(0, 170));
    assert!(g.render(&mut ed).is_empty());
    // 3 行目へ動いて 1 字打つ
    lp.keys(&mut ed, &[key(Key::Down), key(Key::Down)]);
    g.render(&mut ed);
    lp.with(|cx| ed.on_timer(TimerId::StatusDelay, cx));
    g.render(&mut ed);
    lp.type_str(&mut ed, "x");
    let d = g.render(&mut ed);
    let y = 13 + 2 * CODE_LINE_H;
    // 1 回目の編集: カーソル行と、タブバーの ●（未保存の印）。ステータスバーの ● も
    // このとき一緒に変わる
    assert_eq!(ranges(&d), vec![(0, 13, Priority::Normal), (y, y + CODE_LINE_H, Priority::Cursor), (159, 170, Priority::Status)]);
    assert_eq!(ed.active_view().unwrap().stats().lines_painted, 1);
    // 2 回目からはカーソル行だけ。行・桁の表示は 50ms 後
    lp.type_str(&mut ed, "y");
    assert!(lp.timers.deadline(TimerId::StatusDelay).is_some());
    let d = g.render(&mut ed);
    assert_eq!(ranges(&d), vec![(y, y + CODE_LINE_H, Priority::Cursor)]);
    lp.with(|cx| ed.on_timer(TimerId::StatusDelay, cx));
    let d = g.render(&mut ed);
    assert_eq!(ranges(&d), vec![(159, 170, Priority::Status)]);
    // 何も無ければ描かない
    assert!(g.render(&mut ed).is_empty());
}

both!(cursor_blink_stops_when_idle, cursor_blink_stops_when_idle_simple, cursor_blink_stops_when_idle_buffer);
fn cursor_blink_stops_when_idle(backend: DocBackend) {
    let Some(mut g) = Gfx::new() else { return };
    let mut lp = Loop::new();
    let mut ed = editor_with(backend, "abc\ndef\n");
    lp.keys(&mut ed, &[key(Key::Right)]);
    g.render(&mut ed);
    assert!(lp.timers.deadline(TimerId::CursorBlink).is_some());
    let mut toggles = 0;
    for _ in 0..100 {
        if lp.timers.deadline(TimerId::CursorBlink).is_none() {
            break;
        }
        lp.timers.cancel(TimerId::CursorBlink);
        lp.with(|cx| ed.on_timer(TimerId::CursorBlink, cx));
        let d = g.render(&mut ed);
        if !d.is_empty() {
            // 点滅で変わるのはカーソル行だけ
            assert_eq!(ranges(&d), vec![(13, 13 + CODE_LINE_H, Priority::Cursor)]);
            toggles += 1;
        }
    }
    assert_eq!(toggles, 20);
    // 止まったあとはタイマーが無い（待機中は起きない）。カーソルは点いたまま
    assert!(lp.timers.deadline(TimerId::CursorBlink).is_none());
    let lit = (13..13 + CODE_LINE_H).any(|y| g.canvas.row(y).contains(&zc_core::palette::CURSOR.0));
    assert!(lit);
    // 打鍵でまた点滅を始める
    lp.keys(&mut ed, &[key(Key::Right)]);
    assert!(lp.timers.deadline(TimerId::CursorBlink).is_some());
}

both!(tree_toggle_and_focus, tree_toggle_and_focus_simple, tree_toggle_and_focus_buffer);
fn tree_toggle_and_focus(backend: DocBackend) {
    let Some(mut g) = Gfx::new() else { return };
    let dir = tmp_dir("tree");
    fs::write(dir.join("a.py"), "a = 1\n").unwrap();
    fs::write(dir.join("b.py"), "b = 2\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend, root: dir.clone(), files: vec![dir.join("a.py")] });
    lp.with(|cx| ed.start(cx));
    lp.pump(&mut ed);
    g.render(&mut ed);
    // ファイルを渡して起動: ツリーは閉じていて、エディタは全幅
    assert_eq!(ed.active_view().unwrap().point_of(Pos::new(0, 0)).unwrap().x, 23);
    // Ctrl+B で開くと、ツリーへ移る。文字は文書に入らない。↓ Enter で b.py
    lp.keys(&mut ed, &[ctrl('b')]);
    let d = g.render(&mut ed);
    assert_eq!(d.bounds(), RowRange::new(0, 170));
    assert_eq!(ed.active_view().unwrap().point_of(Pos::new(0, 0)).unwrap().x, 84 + 23);
    assert_eq!(ed.tree_state(), (true, true));
    lp.type_str(&mut ed, "zz");
    lp.keys(&mut ed, &[key(Key::Down), key(Key::Enter)]);
    lp.pump(&mut ed);
    assert_eq!((ed.tab_count(), text(&ed)), (2, loaded(backend, "b = 2")));
    // 開いたらエディタに入力が戻る
    lp.type_str(&mut ed, "#");
    assert_eq!(text(&ed), loaded(backend, "#b = 2"));
    // ツリーを閉じると全幅に戻る
    lp.keys(&mut ed, &[ctrl('b')]);
    g.render(&mut ed);
    assert_eq!(ed.active_view().unwrap().point_of(Pos::new(0, 0)).unwrap().x, 23);
    assert!(g.render(&mut ed).is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn bench_screens_follow_the_real_path() {
    let Some(mut g) = Gfx::new() else { return };
    let mut lp = Loop::new();
    // 1 字入力: 描くのは 1 行だけ
    let mut b = EditorBench::new(EdBenchKind::Type, 10);
    g.render(&mut b);
    lp.with(|cx| b.step(cx));
    g.render(&mut b);
    lp.with(|cx| b.step(cx));
    let d = g.render(&mut b);
    assert_eq!(b.editor().active_view().unwrap().stats().lines_painted, 1);
    assert_eq!(ranges(&d), vec![(13 + 7 * CODE_LINE_H, 13 + 8 * CODE_LINE_H, Priority::Cursor)]);
    // スクロール: memmove でずらして、描くのは 2 行（新しく見えた行と、カーソルが抜けた行）
    let mut b = EditorBench::new(EdBenchKind::Scroll, 10);
    g.render(&mut b);
    for _ in 0..3 {
        lp.with(|cx| b.step(cx));
        g.render(&mut b);
        let s = b.editor().active_view().unwrap().stats();
        assert_eq!((s.scrolled, s.lines_painted), (-(CODE_LINE_H as i32), 2));
    }
}

/// 打鍵 1 回ぶんのメインの処理時間の目安（計測用。ふだんは走らせない）:
/// `cargo test --release -p zcode --test editor -- --ignored --nocapture`
///
/// 開発機での値なので、実機の値は「全画面の描き直し」（段階 1 の実測: 600MHz で
/// 約 400µs）との比で見積もる。実機での計測は `zcode --bench editor-type --stats`。
#[test]
#[ignore]
fn perf_keystroke() {
    use std::time::Instant;
    let Some(mut g) = Gfx::new() else { return };
    let mut lp = Loop::new();
    let median = |v: &mut Vec<u128>| {
        v.sort();
        v[v.len() / 2]
    };
    for kind in [EdBenchKind::Type, EdBenchKind::Scroll] {
        let mut b = EditorBench::new(kind, 0);
        g.render(&mut b);
        let (mut input, mut draw) = (Vec::new(), Vec::new());
        for _ in 0..400 {
            let t0 = Instant::now();
            lp.with(|cx| b.step(cx));
            let t1 = Instant::now();
            g.render(&mut b);
            input.push((t1 - t0).as_nanos());
            draw.push(t1.elapsed().as_nanos());
        }
        eprintln!("{kind:?}: 打鍵の処理 p50 {} ns、描画 p50 {} ns", median(&mut input), median(&mut draw));
    }
    // 比べる相手: 段階 1 の「エディタ風の全画面を描き直す」
    let ts = zc_font::TextStyle::mono(zc_ui::layout::CODE_PX);
    let mut full = Vec::new();
    for i in 0..400 {
        let t = Instant::now();
        let mut cx = RenderCx::new(&mut g.canvas, &mut g.fonts, &g.theme);
        zcode::sample::draw_editor(&mut cx, zc_core::Rect::new(0, 13, 320, 146), &ts, CODE_LINE_H, i % 12);
        full.push(t.elapsed().as_nanos());
    }
    eprintln!("全画面の描き直し（段階 1 の描き方）: p50 {} ns", median(&mut full));
}

both!(save_skips_unchanged_and_warns_on_external_change, save_skips_unchanged_simple, save_skips_unchanged_buffer);
fn save_skips_unchanged_and_warns_on_external_change(backend: DocBackend) {
    let dir = tmp_dir("store");
    let p = dir.join("a.py");
    fs::write(&p, "a = 1\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend, root: dir.clone(), files: vec![p.clone()] });
    lp.with(|cx| ed.start(cx));
    lp.pump(&mut ed);
    // 何も変えずに保存: 書き込み層が「前回と同じ」と判断して書かない（SD に書かない）
    let before = fs::metadata(&p).unwrap().modified().unwrap();
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    assert!(ed.message().unwrap().contains("変更なし"), "{:?}", ed.message());
    assert_eq!(fs::metadata(&p).unwrap().modified().unwrap(), before);
    // ほかのプログラムが書き換えた後に保存すると、上書きしたことを知らせる
    fs::write(&p, "a = 2  # outside\n").unwrap();
    lp.keys(&mut ed, &[key(Key::End)]);
    lp.type_str(&mut ed, "  # mine");
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    assert!(fs::read_to_string(&p).unwrap().starts_with("a = 1  # mine"));
    assert!(ed.message().unwrap().contains("上書き"), "{:?}", ed.message());
    // その後の保存は、ふつうの「保存した」
    lp.type_str(&mut ed, "!");
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    assert!(ed.message().unwrap().starts_with("保存した"), "{:?}", ed.message());
    let _ = fs::remove_dir_all(&dir);
}

/// 描き直しだけで得た画面が、全部を描き直した画面と同じか（小窓の消し残しが無いか）。
fn assert_screen_is_clean(g: &mut Gfx, ed: &mut EditorScreen) {
    g.render(ed);
    let partial = g.canvas.clone();
    ed.on_glyphs_ready();
    g.render(ed);
    assert!(partial == g.canvas, "部分的な描き直しが、全部を描き直した画面と違う（消し残し）");
}

both!(word_completion_popup, word_completion_popup_simple, word_completion_popup_buffer);
fn word_completion_popup(backend: DocBackend) {
    let mut lp = Loop::new();
    let mut ed = editor_with(backend, "led_blink = 1\nled = 2\nname = \"lemon\"\n");
    let mut g = Gfx::new();
    if let Some(g) = g.as_mut() {
        g.render(&mut ed);
    }
    lp.keys(&mut ed, &[key(Key::Down), key(Key::Down), key(Key::End), key(Key::Enter)]);
    // 1 文字では出ない。2 文字で出る（近い行の語が先: lemon は 1 行上、led は 2 行上。キーワードは後）
    lp.type_str(&mut ed, "l");
    assert!(ed.completion().is_none());
    lp.type_str(&mut ed, "e");
    let (items, sel) = ed.completion().expect("候補が出る");
    assert_eq!((items.as_slice(), sel), (&["lemon".to_string(), "led".into(), "led_blink".into(), "len".into()][..], 0));
    if let Some(g) = g.as_mut() {
        let d = g.render(&mut ed);
        assert!(!d.is_empty());
    }
    // ↓ で次の候補、Tab で置き換え（打った "le" ごと）
    let second = items[1].clone();
    lp.keys(&mut ed, &[key(Key::Down)]);
    assert_eq!(ed.completion().unwrap().1, 1);
    lp.keys(&mut ed, &[key(Key::Tab)]);
    assert!(ed.completion().is_none());
    assert_eq!(text(&ed).trim_end().lines().last().unwrap(), second);
    // 取り消すと打った "le" に戻る
    lp.keys(&mut ed, &[ctrl('z')]);
    assert_eq!(text(&ed).trim_end().lines().last().unwrap(), "le");
    if let Some(g) = g.as_mut() {
        assert_screen_is_clean(g, &mut ed);
    }
    // 打ち足すと絞り込み直す。Esc で閉じる。閉じた後の画面に消し残しが無い
    lp.type_str(&mut ed, "d_");
    assert_eq!(ed.completion().unwrap().0, vec!["led_blink".to_string()]);
    if let Some(g) = g.as_mut() {
        g.render(&mut ed);
    }
    lp.keys(&mut ed, &[key(Key::Esc)]);
    assert!(ed.completion().is_none());
    if let Some(g) = g.as_mut() {
        assert_screen_is_clean(g, &mut ed);
    }
    // 文字列の中・単語の途中では自動で出ない。Ctrl+Space なら出る
    lp.keys(&mut ed, &[key(Key::Enter)]);
    lp.type_str(&mut ed, "s = \"le");
    assert!(ed.completion().is_none());
    lp.keys(&mut ed, &[key(Key::Enter)]);
    lp.type_str(&mut ed, "na");
    assert!(ed.completion().is_some());
    lp.keys(&mut ed, &[key(Key::Left)]);
    assert!(ed.completion().is_none());
    lp.keys(&mut ed, &[KeyEvent::new(Key::Char(' '), Mods::CTRL)]);
    assert!(ed.completion().is_some_and(|c| c.0.contains(&"name".to_string())));
}

both!(outline_filter_and_jump, outline_filter_and_jump_simple, outline_filter_and_jump_buffer);
fn outline_filter_and_jump(backend: DocBackend) {
    let mut lp = Loop::new();
    let src = "import time\n\nclass Blinker:\n    def __init__(self):\n        pass\n\n    def make_pattern(self):\n        return [1, 0]\n\n\ndef main():\n    # def not_this(): コメントの中は出さない\n    pass\n\n\ndef run_formatted():\n    pass\n";
    let mut ed = editor_with(backend, src);
    let g = Gfx::new();
    lp.keys(&mut ed, &[ctrl('o')]);
    let (rows, sel) = ed.outline_rows().expect("Ctrl+O で開く");
    let names: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(names, ["Blinker", "__init__", "make_pattern", "main", "run_formatted"]);
    assert_eq!(sel, 0);
    // 絞り込み: 一致した項目の親は残す（選べない）
    lp.type_str(&mut ed, "ma");
    let (rows, sel) = ed.outline_rows().unwrap();
    assert_eq!(rows, vec![("Blinker".into(), true), ("make_pattern".into(), false), ("main".into(), false), ("run_formatted".into(), false)]);
    assert_eq!(sel, 1);
    if let Some(mut g) = g {
        assert!(!g.render(&mut ed).is_empty());
        // ↓ で main、Enter でその行へ。閉じたあとの画面に一覧が残らない
        lp.keys(&mut ed, &[key(Key::Down), key(Key::Enter)]);
        assert!(ed.outline_rows().is_none());
        assert_eq!(cursor(&ed), Pos::new(10, 0));
        assert_screen_is_clean(&mut g, &mut ed);
    } else {
        lp.keys(&mut ed, &[key(Key::Down), key(Key::Enter)]);
        assert_eq!(cursor(&ed), Pos::new(10, 0));
    }
    // Esc は何もせず閉じる
    lp.keys(&mut ed, &[ctrl('o'), key(Key::Esc)]);
    assert!(ed.outline_rows().is_none());
    assert_eq!(cursor(&ed), Pos::new(10, 0));
}

both!(save_runs_syntax_check, save_runs_syntax_check_simple, save_runs_syntax_check_buffer);
fn save_runs_syntax_check(backend: DocBackend) {
    if std::process::Command::new("python3").arg("-c").arg("pass").status().map_or(true, |s| !s.success()) {
        eprintln!("python3 が無いので飛ばす");
        return;
    }
    let dir = tmp_dir("check");
    let p = dir.join("bad.py");
    fs::write(&p, "def main():\n    for i in range(3)\n        print(i)\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend, root: dir.clone(), files: vec![p.clone()] });
    // Python は remote = "python"（2026-11-17）。ビルド係が「対象ではない」と返したら実機の check に回る
    let fake = FakeRemote::default();
    ed.set_remote(Box::new(fake.clone()));
    lp.with(|cx| ed.start(cx));
    lp.pump(&mut ed);
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed); // 保存 → ビルド係に check を頼む
    assert_eq!(fake.jobs().iter().map(|j| (j.kind, j.lang)).collect::<Vec<_>>(), [(JobKind::Check, RemoteLang::Python)]);
    lp.remote(&mut ed, done(Some(1), 0, DoneStatus::NotApplicable));
    lp.pump(&mut ed); // 実機の check の結果
    let diags = ed.diagnostics();
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].line, 1);
    assert!(ed.message().unwrap().starts_with("2 行:"), "{:?}", ed.message());
    assert_eq!(ed.active_view().unwrap().marks().len(), 1);
    // 上に 1 行足すと、波線もずれる
    lp.keys(&mut ed, &[KeyEvent::new(Key::Home, Mods::CTRL)]);
    lp.type_str(&mut ed, "import os\n");
    assert_eq!(ed.diagnostics()[0].line, 2);
    // 直して保存すると消える（カーソルは 2 行目の頭 → 3 行目の for の行末へ）
    lp.keys(&mut ed, &[key(Key::Down), key(Key::End)]);
    lp.type_str(&mut ed, ":");
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    // ビルド係に届かない（オフライン）も実機の check に回る
    lp.remote(&mut ed, done(Some(2), 0, DoneStatus::Offline));
    lp.pump(&mut ed);
    assert!(ed.diagnostics().is_empty(), "{:?}", ed.diagnostics());
    assert!(ed.active_view().unwrap().marks().is_empty());
    assert!(fs::read_to_string(&p).unwrap().contains("range(3):"));
    let _ = fs::remove_dir_all(&dir);
}

// --- 診断の置き場とビルド係（docs/stage3-design.md §13、課題 Z3 の受け入れ） ---

/// 偽物のビルド係: 頼まれたジョブを覚えるだけ。結果は試験が `AppEvent::Remote` で流す。
/// 依頼の番号は 1 から順。
#[derive(Clone, Default)]
struct FakeRemote(Arc<Mutex<Vec<RemoteJob>>>);

impl RemoteLink for FakeRemote {
    fn submit(&mut self, job: RemoteJob, _cx: &mut LoopCx<'_>) -> RemoteReq {
        let mut v = self.0.lock().unwrap();
        v.push(job);
        RemoteReq(v.len() as u32)
    }
}

impl FakeRemote {
    fn jobs(&self) -> Vec<RemoteJob> {
        self.0.lock().unwrap().clone()
    }
}

impl Loop {
    /// 専用スレッドからの知らせを流す。
    fn remote(&mut self, ed: &mut EditorScreen, ev: RemoteEvent) {
        self.with(|cx| ed.on_event(AppEvent::Remote(ev), cx));
    }
    /// 実機の構文チェックの結果（絶対パス・版なし）を流す。
    fn syntax(&mut self, ed: &mut EditorScreen, file: &Path, diags: &[(u32, &str)]) {
        let doc = ed.active_doc_id().unwrap();
        let set = DiagnosticSet { source: DiagSource::Syntax, complete: false, files: vec![FileDiagnostics { path: Arc::from(file), base: None, diags: diags.iter().map(|&(l, m)| err(l, m)).collect() }] };
        self.with(|cx| ed.on_event(AppEvent::Checked { doc, result: Ok(set) }, cx));
    }
}

fn err(line: u32, msg: &str) -> Diagnostic {
    Diagnostic::at_line(line, Severity::Error, msg)
}
fn scanned(tag: u32, root: &Path) -> RemoteEvent {
    RemoteEvent::Phase { req: None, tag: JobTag(tag), root: root.to_path_buf(), phase: Phase::Scanned, done: 0, total: 0 }
}
fn remote_diags(tag: u32, root: &Path, complete: bool, files: &[(&str, &[(u32, &str)])]) -> RemoteEvent {
    let files = files.iter().map(|(p, ds)| FileDiags { path: p.to_string(), diags: ds.iter().map(|&(l, m)| err(l, m)).collect() }).collect();
    RemoteEvent::Diagnostics { req: None, tag: JobTag(tag), root: root.to_path_buf(), complete, files }
}
fn done(req: Option<u32>, tag: u32, status: DoneStatus) -> RemoteEvent {
    RemoteEvent::Done { req: req.map(RemoteReq), tag: JobTag(tag), status, exit: 0, backend: Backend::Pc, elapsed_ms: 0, message: String::new() }
}
fn lines_of(ed: &EditorScreen) -> Vec<u32> {
    ed.diagnostics().iter().map(|d| d.line).collect()
}

/// 作業フォルダの下に Rust のプロジェクト（`proj/src/lib.rs`・`wrap.rs`、どちらも 10 行）を作り、
/// `lib.rs` を開いたエディタ。ビルド係は偽物。戻り値は（作業フォルダ, プロジェクトの根, …）。
fn rust_project(backend: DocBackend, tag: &str) -> (PathBuf, PathBuf, EditorScreen, Loop, FakeRemote) {
    let dir = tmp_dir(tag);
    let proj = dir.join("proj");
    fs::create_dir_all(proj.join("src")).unwrap();
    let body: String = (0..10).map(|i| format!("fn f{i}() {{}}\n")).collect();
    fs::write(proj.join("src/lib.rs"), &body).unwrap();
    fs::write(proj.join("src/wrap.rs"), &body).unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend, root: dir.clone(), files: vec![proj.join("src/lib.rs")] });
    let fake = FakeRemote::default();
    ed.set_remote(Box::new(fake.clone()));
    lp.with(|cx| ed.start(cx));
    lp.pump(&mut ed);
    (dir, proj, ed, lp, fake)
}

both!(syntax_and_remote_diagnostics_show_together, syntax_and_remote_diagnostics_show_together_simple, syntax_and_remote_diagnostics_show_together_buffer);
fn syntax_and_remote_diagnostics_show_together(backend: DocBackend) {
    let (dir, proj, mut ed, mut lp, _fake) = rust_project(backend, "both");
    let lib = proj.join("src/lib.rs");
    // 実機の構文チェック（絶対パス）と、ビルド係（根 + 相対パス）が同じファイルに付く
    lp.syntax(&mut ed, &lib, &[(1, "expected `;`")]);
    lp.remote(&mut ed, remote_diags(1, &proj, true, &[("src/lib.rs", &[(4, "E0308: mismatched types")])]));
    assert_eq!(lines_of(&ed), [1, 4]);
    assert_eq!(ed.active_view().unwrap().marks().len(), 2);
    // 右に出すのは、このファイルの最初の診断（出どころによらず行の順）
    assert_eq!(ed.message(), Some("2 行: expected `;`"));
    // 同じ行・同じメッセージが両方から出たら、1 つに数える
    lp.syntax(&mut ed, &lib, &[(1, "expected `;`"), (4, "E0308: mismatched types")]);
    assert_eq!(lines_of(&ed), [1, 4]);
    // 構文チェックが空で返っても、ビルド係の診断は残る
    lp.syntax(&mut ed, &lib, &[]);
    assert_eq!(lines_of(&ed), [4]);
    // ビルド係の完全な結果が空なら消える。構文チェックの分は消さない
    lp.syntax(&mut ed, &lib, &[(2, "x")]);
    lp.remote(&mut ed, remote_diags(2, &proj, true, &[]));
    assert_eq!(lines_of(&ed), [2]);
    // 保存時の check をビルド係がした（成功）なら、前の構文チェックの結果は古いので消える
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    lp.remote(&mut ed, done(Some(1), 3, DoneStatus::Ok));
    assert!(ed.diagnostics().is_empty());
    assert!(ed.active_view().unwrap().marks().is_empty());
    let _ = fs::remove_dir_all(&dir);
}

both!(late_remote_diagnostics_follow_edits, late_remote_diagnostics_follow_edits_simple, late_remote_diagnostics_follow_edits_buffer);
fn late_remote_diagnostics_follow_edits(backend: DocBackend) {
    let (dir, proj, mut ed, mut lp, fake) = rust_project(backend, "late");
    // ジョブの走査が済んだ（このときの保存済みの版が基準）。そのあと、先頭に 2 行足す
    lp.remote(&mut ed, scanned(7, &proj));
    lp.keys(&mut ed, &[KeyEvent::new(Key::Home, Mods::CTRL)]);
    lp.type_str(&mut ed, "// a\n// b\n");
    // 数秒遅れて届いた診断（保存した中身の 5 行目）は、今の 7 行目に付く
    lp.remote(&mut ed, remote_diags(7, &proj, true, &[("src/lib.rs", &[(4, "late")])]));
    assert_eq!(lines_of(&ed), [6]);
    assert_eq!(ed.active_view().unwrap().marks()[0].line, 6);
    lp.remote(&mut ed, done(None, 7, DoneStatus::Failed));
    // 付いたあとの編集でも動く
    lp.type_str(&mut ed, "// c\n");
    assert_eq!(lines_of(&ed), [7]);
    // 保存すると、基準はその版になる（保存時の check を頼む）
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    let jobs = fake.jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!((jobs[0].kind, jobs[0].lang), (JobKind::Check, RemoteLang::Rust));
    assert_eq!(jobs[0].file, proj.join("src/lib.rs"));
    lp.remote(&mut ed, RemoteEvent::Phase { req: Some(RemoteReq(1)), tag: JobTag(8), root: proj.clone(), phase: Phase::Scanned, done: 0, total: 0 });
    assert_eq!(ed.remote_progress().as_deref(), Some("ビルド係: 準備中"));
    lp.type_str(&mut ed, "// d\n");
    // 保存した中身（13 行）の 8 行目 → 1 行足したので 9 行目
    lp.remote(&mut ed, remote_diags(8, &proj, true, &[("src/lib.rs", &[(7, "late")])]));
    assert_eq!(lines_of(&ed), [8]);
    lp.remote(&mut ed, done(Some(1), 8, DoneStatus::Failed));
    assert_eq!(ed.remote_progress(), None);
    // 行を消して、診断の行がファイルの外になっても、波線は文書の中に収まる
    lp.keys(&mut ed, &[ctrl('a'), key(Key::Backspace)]);
    assert_eq!(lines_of(&ed), [0]);
    let _ = fs::remove_dir_all(&dir);
}

both!(incomplete_results_keep_squiggles, incomplete_results_keep_squiggles_simple, incomplete_results_keep_squiggles_buffer);
fn incomplete_results_keep_squiggles(backend: DocBackend) {
    let (dir, proj, mut ed, mut lp, _fake) = rust_project(backend, "partial");
    let wrap = proj.join("src/wrap.rs");
    lp.with(|cx| ed.open_path(&wrap, cx));
    lp.pump(&mut ed);
    assert_eq!(ed.active_path(), Some(wrap.as_path()));
    // 別のプロジェクト（開いていないファイル）の診断も置き場にある
    let other = dir.join("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("x.rs"), "fn x() {}\nfn y() {}\n").unwrap();
    lp.remote(&mut ed, remote_diags(1, &other, true, &[("x.rs", &[(1, "other")])]));
    // 完全な結果: 2 つのファイルに付く
    lp.remote(&mut ed, remote_diags(2, &proj, true, &[("src/lib.rs", &[(3, "a")]), ("src/wrap.rs", &[(5, "b")])]));
    assert_eq!(lines_of(&ed), [5]);
    // 取り消し・途中で切れた結果（complete でない）: 出てこなかったファイルの波線は消さない
    lp.remote(&mut ed, remote_diags(3, &proj, false, &[("src/lib.rs", &[])]));
    assert_eq!(lines_of(&ed), [5]);
    lp.keys(&mut ed, &[KeyEvent::new(Key::Tab, Mods::CTRL)]);
    assert!(ed.diagnostics().is_empty(), "出てきたファイルは置き換わる");
    // 完全な結果: 出てこなかったファイルの波線は消える
    lp.remote(&mut ed, remote_diags(4, &proj, true, &[("src/lib.rs", &[(3, "a")])]));
    assert_eq!(lines_of(&ed), [3]);
    lp.keys(&mut ed, &[KeyEvent::new(Key::Tab, Mods::CTRL)]);
    assert!(ed.diagnostics().is_empty());
    // ほかのプロジェクトの分は消えていない。開いたときに付く
    lp.with(|cx| ed.open_path(&other.join("x.rs"), cx));
    lp.pump(&mut ed);
    assert_eq!(lines_of(&ed), [1]);
    assert_eq!(ed.active_view().unwrap().marks().len(), 1);
    let _ = fs::remove_dir_all(&dir);
}

both!(superseded_remote_results_are_dropped, superseded_remote_results_are_dropped_simple, superseded_remote_results_are_dropped_buffer);
fn superseded_remote_results_are_dropped(backend: DocBackend) {
    let (dir, proj, mut ed, mut lp, _fake) = rust_project(backend, "stale");
    // F5 のビルド（1）の走査のあとで、保存時の check（2）が走査した。結果は 2 が先に届く
    lp.remote(&mut ed, scanned(1, &proj));
    lp.remote(&mut ed, scanned(2, &proj));
    lp.remote(&mut ed, remote_diags(2, &proj, true, &[("src/lib.rs", &[(3, "new")])]));
    lp.remote(&mut ed, remote_diags(1, &proj, true, &[("src/lib.rs", &[(5, "old")])]));
    assert_eq!(lines_of(&ed), [3], "古い木の結果で上書きしない");
    lp.remote(&mut ed, done(None, 2, DoneStatus::Failed));
    lp.remote(&mut ed, done(None, 1, DoneStatus::Failed));
    // 取り消された check（3）の途中までの結果は、次の check（4）が走査済みなら捨てる
    lp.remote(&mut ed, scanned(3, &proj));
    lp.remote(&mut ed, scanned(4, &proj));
    lp.remote(&mut ed, remote_diags(3, &proj, false, &[("src/lib.rs", &[(8, "cancelled")])]));
    assert_eq!(lines_of(&ed), [3]);
    lp.remote(&mut ed, done(None, 3, DoneStatus::Cancelled));
    lp.remote(&mut ed, remote_diags(4, &proj, true, &[]));
    assert!(ed.diagnostics().is_empty());
    assert_eq!(ed.message(), Some("エラーは無くなった"));
    // serve が再起動して番号が 1 に戻っても、新しい結果として取り込む
    lp.remote(&mut ed, scanned(1, &proj));
    lp.remote(&mut ed, remote_diags(1, &proj, true, &[("src/lib.rs", &[(2, "again")])]));
    assert_eq!(lines_of(&ed), [2]);
    // 位置なしの診断（パスが空）は、ステータスバーにだけ出す
    lp.remote(&mut ed, remote_diags(1, &proj, true, &[("", &[(0, "linking failed")]), ("src/lib.rs", &[(2, "again")])]));
    assert_eq!(lines_of(&ed), [2]);
    assert_eq!(ed.message(), Some("linking failed"));
    let _ = fs::remove_dir_all(&dir);
}

both!(not_applicable_falls_back_to_local_check, not_applicable_falls_back_to_local_check_simple, not_applicable_falls_back_to_local_check_buffer);
fn not_applicable_falls_back_to_local_check(backend: DocBackend) {
    if std::process::Command::new("gcc").arg("--version").output().map_or(true, |o| !o.status.success()) {
        eprintln!("gcc が無いので飛ばす");
        return;
    }
    let dir = tmp_dir("fallback");
    let p = dir.join("bad.c");
    fs::write(&p, "int main(void) {\n    return 0\n}\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend, root: dir.clone(), files: vec![p.clone()] });
    let fake = FakeRemote::default();
    ed.set_remote(Box::new(fake.clone()));
    lp.with(|cx| ed.start(cx));
    lp.pump(&mut ed);
    // 保存 → まずビルド係の check を頼む（実機の check はまだ走らせない）
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    assert_eq!(fake.jobs().iter().map(|j| (j.kind, j.lang)).collect::<Vec<_>>(), [(JobKind::Check, RemoteLang::C)]);
    lp.pump(&mut ed);
    assert!(ed.diagnostics().is_empty());
    // zc-remote が入っていない: 1 回だけ静かに知らせて、依頼は「対象ではない」で返る → 実機の gcc
    lp.remote(&mut ed, RemoteEvent::Unavailable("zc-remote が無い".into()));
    assert_eq!(ed.message(), Some("ビルド係を使えない: zc-remote が無い"));
    lp.remote(&mut ed, done(Some(1), 0, DoneStatus::NotApplicable));
    lp.pump(&mut ed);
    assert!(!ed.diagnostics().is_empty(), "実機の構文チェックに回る");
    assert!(ed.message().is_some_and(|m| m.contains("行:")), "{:?}", ed.message());
    // オフラインでも同じ。何も知らせず、実機の check に回す
    lp.keys(&mut ed, &[key(Key::Down), key(Key::End)]);
    lp.type_str(&mut ed, ";");
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    assert_eq!(fake.jobs().len(), 2);
    lp.remote(&mut ed, done(Some(2), 0, DoneStatus::Offline));
    lp.pump(&mut ed);
    assert!(ed.diagnostics().is_empty(), "{:?}", ed.diagnostics());
    assert_eq!(ed.message(), Some("構文エラーは無くなった"));
    let _ = fs::remove_dir_all(&dir);
}

both!(missing_check_tool_is_reported_once, missing_check_tool_is_reported_once_simple, missing_check_tool_is_reported_once_buffer);
fn missing_check_tool_is_reported_once(backend: DocBackend) {
    let has = |cmd: &str| std::process::Command::new("bash").arg("-c").arg(format!("command -v {cmd}")).output().is_ok_and(|o| o.status.success());
    if !has("bash") || has("tsc") {
        eprintln!("bash が無い、または tsc があるので飛ばす");
        return;
    }
    let dir = tmp_dir("notsc");
    let p = dir.join("a.ts");
    fs::write(&p, "const x: number = 1;\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend, root: dir.clone(), files: vec![p.clone()] });
    ed.set_remote(Box::new(FakeRemote::default()));
    lp.with(|cx| ed.start(cx));
    lp.pump(&mut ed);
    // 前の保存のときの波線が残っているとする
    lp.syntax(&mut ed, &p, &[(0, "TS1005: old")]);
    for (n, expect_notice) in [(1, true), (2, false)] {
        lp.keys(&mut ed, &[ctrl('s')]);
        lp.pump(&mut ed);
        // npm の依存が無いので、ビルド係の対象ではない → 実機の tsc（bash -c で包んである）
        lp.remote(&mut ed, done(Some(n), 0, DoneStatus::NotApplicable));
        lp.pump(&mut ed);
        // tsc が無いと bash が 127 で終わる。「エラーなし」にせず、1 回だけ知らせる
        let noticed = ed.message().is_some_and(|m| m.starts_with("構文チェックできない"));
        assert_eq!(noticed, expect_notice, "{n} 回目: {:?}", ed.message());
        assert_eq!(lines_of(&ed), [0], "波線はそのまま");
    }
    let _ = fs::remove_dir_all(&dir);
}

fn test_report() -> TestReport {
    let case = |name: &str, passed: bool, loc: Option<(&str, u32)>, message: &str| TestCase { name: name.into(), passed, location: loc.map(|(p, l)| (p.to_string(), l)), message: message.into() };
    TestReport {
        passed: 1,
        failed: 2,
        ignored: 0,
        cases: vec![
            case("tests::ok", true, None, ""),
            case("tests::parse_empty", false, Some(("src/lib.rs", 4)), "thread 'tests::parse_empty' panicked at src/lib.rs:5:9:\nassertion `left == right` failed"),
            case("wrap::tests::wide", false, Some(("src/wrap.rs", 7)), "boom"),
        ],
    }
}

both!(run_tests_shows_results_and_jumps, run_tests_shows_results_and_jumps_simple, run_tests_shows_results_and_jumps_buffer);
fn run_tests_shows_results_and_jumps(backend: DocBackend) {
    let (dir, proj, mut ed, mut lp, fake) = rust_project(backend, "tests");
    let mut g = Gfx::new();
    // F6: 保存済みなので、そのまま試験を頼む
    lp.keys(&mut ed, &[key(Key::F(6))]);
    assert_eq!(fake.jobs().iter().map(|j| (j.kind, j.lang, j.flags)).collect::<Vec<_>>(), [(JobKind::Test, RemoteLang::Rust, SubmitFlags::NONE)]);
    assert_eq!(ed.remote_progress().as_deref(), Some("ビルド係: 接続中"));
    // もう 1 回押しても、重ねて頼まない
    lp.keys(&mut ed, &[key(Key::F(6))]);
    assert_eq!(fake.jobs().len(), 1);
    assert_eq!(ed.message(), Some("試験は実行中"));
    lp.remote(&mut ed, RemoteEvent::Phase { req: Some(RemoteReq(1)), tag: JobTag(5), root: proj.clone(), phase: Phase::Running, done: 0, total: 0 });
    assert_eq!(ed.remote_progress().as_deref(), Some("ビルド係: 試験中 0 秒"));
    // 結果が届いた: 落ちた試験が先、通った試験が後
    lp.remote(&mut ed, RemoteEvent::Tests { req: RemoteReq(1), root: proj.clone(), report: test_report() });
    lp.remote(&mut ed, done(Some(1), 5, DoneStatus::Failed));
    assert_eq!(ed.remote_progress(), None);
    let (rows, sel) = ed.test_list().expect("一覧が開く");
    assert_eq!(rows, [("tests::parse_empty".to_string(), false), ("wrap::tests::wide".to_string(), false), ("tests::ok".to_string(), true)]);
    assert_eq!(sel, 0);
    if let Some(g) = g.as_mut() {
        assert!(!g.render(&mut ed).is_empty());
    }
    // 絞り込んで、Enter で落ちた行へ（開いていないファイルは開く）
    lp.type_str(&mut ed, "wide");
    assert_eq!(ed.test_list().unwrap().0.len(), 1);
    lp.keys(&mut ed, &[key(Key::Enter)]);
    assert!(ed.test_list().is_none());
    lp.pump(&mut ed);
    assert_eq!(ed.active_path(), Some(proj.join("src/wrap.rs").as_path()));
    assert_eq!(cursor(&ed), Pos::new(7, 0));
    if let Some(g) = g.as_mut() {
        // 閉じたあとの画面に一覧が残らない
        assert_screen_is_clean(g, &mut ed);
    }
    // 通った試験の Enter は何もしない。Esc で閉じる
    lp.keys(&mut ed, &[key(Key::F(6))]);
    lp.remote(&mut ed, RemoteEvent::Tests { req: RemoteReq(2), root: proj.clone(), report: test_report() });
    lp.remote(&mut ed, done(Some(2), 6, DoneStatus::Failed));
    lp.keys(&mut ed, &[key(Key::Up), key(Key::Enter)]);
    assert_eq!(ed.test_list().unwrap().1, 2);
    // もう開いているファイルへは、すぐ移る
    lp.keys(&mut ed, &[key(Key::Down), key(Key::Enter)]);
    assert_eq!(ed.active_path(), Some(proj.join("src/lib.rs").as_path()));
    assert_eq!(cursor(&ed), Pos::new(4, 0));
    let _ = fs::remove_dir_all(&dir);
}

both!(test_results_do_not_steal_keys, test_results_do_not_steal_keys_simple, test_results_do_not_steal_keys_buffer);
fn test_results_do_not_steal_keys(backend: DocBackend) {
    let (dir, proj, mut ed, mut lp, fake) = rust_project(backend, "tests-typing");
    // 未保存なら、保存が終わってから頼む（同じ木の check は重ねて頼まない）
    lp.type_str(&mut ed, "// x\n");
    lp.keys(&mut ed, &[key(Key::F(6))]);
    assert!(fake.jobs().is_empty());
    lp.pump(&mut ed);
    assert_eq!(fake.jobs().iter().map(|j| j.kind).collect::<Vec<_>>(), [JobKind::Test]);
    // 待っている間に書き続けた: 結果が届いても一覧は出さず、知らせるだけ
    lp.type_str(&mut ed, "// y");
    lp.remote(&mut ed, RemoteEvent::Tests { req: RemoteReq(1), root: proj.clone(), report: test_report() });
    lp.remote(&mut ed, done(Some(1), 5, DoneStatus::Failed));
    assert!(ed.test_list().is_none());
    assert_eq!(ed.run_state().1, Some("✕ 試験 2 失敗 / 1 成功  F6 で一覧"));
    lp.type_str(&mut ed, "z");
    assert!(text(&ed).starts_with("// x\n// yz"));
    // F6 で、その結果の一覧を開く（もう一度は走らせない）
    lp.keys(&mut ed, &[key(Key::F(6))]);
    assert_eq!(ed.test_list().map(|(r, _)| r.len()), Some(3));
    assert_eq!(fake.jobs().len(), 1);
    // 一覧の中の F6 は、もう一度実行（未保存なので、先に保存）
    lp.keys(&mut ed, &[key(Key::F(6))]);
    assert!(ed.test_list().is_none());
    lp.pump(&mut ed);
    assert_eq!(fake.jobs().len(), 2);
    // 全部通ったら、一覧は出さずステータスバーに出す
    let all_ok = TestReport { passed: 5, failed: 0, ignored: 1, cases: Vec::new() };
    lp.remote(&mut ed, RemoteEvent::Tests { req: RemoteReq(2), root: proj.clone(), report: all_ok });
    lp.remote(&mut ed, done(Some(2), 6, DoneStatus::Ok));
    assert!(ed.test_list().is_none());
    assert_eq!(ed.run_state().1, Some("✓ 試験 5 成功 / 1 無視"));
    // コンパイルできなかった（結果が無いまま失敗）・オフライン
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    lp.keys(&mut ed, &[key(Key::F(6))]);
    let n = fake.jobs().len() as u32;
    lp.remote(&mut ed, done(Some(n), 7, DoneStatus::Failed));
    assert_eq!(ed.run_state().1, Some("✕ 試験: ビルドできなかった"));
    lp.keys(&mut ed, &[key(Key::F(6))]);
    lp.remote(&mut ed, done(Some(n + 1), 0, DoneStatus::Offline));
    assert_eq!(ed.message(), Some("ビルド係: オフライン（ビルド係に届かない）"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn run_tests_needs_a_remote_language() {
    let mut lp = Loop::new();
    let mut ed = editor_with(DocBackend::Simple, "x = 1\n");
    let fake = FakeRemote::default();
    ed.set_remote(Box::new(fake.clone()));
    lp.keys(&mut ed, &[key(Key::F(6))]);
    assert!(ed.message().is_some_and(|m| m.contains("試験を実行できない")), "{:?}", ed.message());
    assert!(fake.jobs().is_empty());
}

// --- ショートカット一覧（段階 6、モック 5・6） ---

#[test]
fn f1_lists_shortcuts_filters_and_runs_the_selected_command() {
    let mut lp = Loop::new();
    let mut ed = editor_with(DocBackend::Simple, "abc\ndef\n");
    lp.keys(&mut ed, &[key(Key::F(1))]);
    assert_eq!(ed.shortcuts_query(), Some(""));
    check_golden(&mut ed, "shortcuts.png");
    // 日本語の操作名で絞り込み、Enter でその操作を実行する
    lp.type_str(&mut ed, "すべて選択");
    assert_eq!(ed.shortcuts_query(), Some("すべて選択"));
    lp.keys(&mut ed, &[key(Key::Enter)]);
    assert_eq!(ed.shortcuts_query(), None, "実行したら閉じる");
    assert_eq!(ed.active_doc().unwrap().selected_text(), "abc\ndef");
    // BackSpace で戻し、Esc で閉じる。F1 でも閉じる
    lp.keys(&mut ed, &[key(Key::F(1))]);
    lp.type_str(&mut ed, "選択");
    check_golden(&mut ed, "shortcuts-filter.png");
    lp.keys(&mut ed, &[key(Key::Backspace), key(Key::Backspace)]);
    assert_eq!(ed.shortcuts_query(), Some(""));
    lp.keys(&mut ed, &[key(Key::Esc)]);
    assert_eq!(ed.shortcuts_query(), None);
    lp.keys(&mut ed, &[key(Key::F(1)), key(Key::F(1))]);
    assert_eq!(ed.shortcuts_query(), None);
    // 一覧の打鍵は文書に入らない
    assert_eq!(doc_text(ed.active_doc().unwrap()), "abc\ndef");
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

// --- 打鍵が止まったときの check（check_on_save = idle） ---

impl Loop {
    /// 打鍵が止まったときの check のタイマーが鳴った（時間は待たずに鳴らす）。
    fn idle(&mut self, ed: &mut EditorScreen) {
        assert!(self.timers.deadline(TimerId::AutoCheck).is_some(), "タイマーが仕掛かっていない");
        self.timers.cancel(TimerId::AutoCheck);
        self.with(|cx| ed.on_timer(TimerId::AutoCheck, cx));
    }
}

fn auto_jobs(fake: &FakeRemote) -> Vec<RemoteJob> {
    fake.jobs().into_iter().filter(|j| j.flags.contains(SubmitFlags::AUTO)).collect()
}

fn overlay_text(j: &RemoteJob, i: usize) -> String {
    String::from_utf8(j.overlay[i].bytes(usize::MAX).unwrap()).unwrap()
}

both!(auto_check_fires_once_after_typing_stops, auto_check_fires_once_after_typing_stops_simple, auto_check_fires_once_after_typing_stops_buffer);
fn auto_check_fires_once_after_typing_stops(backend: DocBackend) {
    let (dir, proj, mut ed, mut lp, fake) = rust_project(backend, "auto-once");
    let (lib, wrap) = (proj.join("src/lib.rs"), proj.join("src/wrap.rs"));
    // 動くだけでは仕掛けない
    lp.keys(&mut ed, &[key(Key::Down)]);
    assert_eq!(lp.timers.deadline(TimerId::AutoCheck), None);
    // 打つたびに仕掛け直す（打っている間は鳴らない）
    lp.type_str(&mut ed, "x");
    let first = lp.timers.deadline(TimerId::AutoCheck).expect("仕掛かる");
    assert!(first >= now_us() + 1_000_000, "1.2 秒後");
    std::thread::sleep(std::time::Duration::from_millis(5));
    lp.type_str(&mut ed, "y");
    let second = lp.timers.deadline(TimerId::AutoCheck).unwrap();
    assert!(second > first, "打つたびに先へ延びる");
    assert!(fake.jobs().is_empty());
    // 鳴った: 1 回だけ頼む。未保存の中身（文書と同じ）を重ねる
    lp.idle(&mut ed);
    let jobs = auto_jobs(&fake);
    assert_eq!(jobs.len(), 1);
    assert_eq!((jobs[0].kind, jobs[0].lang, jobs[0].file.as_path()), (JobKind::Check, RemoteLang::Rust, lib.as_path()));
    assert_eq!(jobs[0].overlay.len(), 1);
    assert_eq!(jobs[0].overlay[0].path, lib);
    assert_eq!(overlay_text(&jobs[0], 0), text(&ed));
    assert!(text(&ed).contains("xyfn f1()"));
    // 大きすぎる中身は None（専用スレッドが送らずに断る）
    assert!(jobs[0].overlay[0].bytes(10).is_none());
    // 変わっていなければ、もう一度鳴っても頼まない
    lp.with(|cx| ed.on_timer(TimerId::AutoCheck, cx));
    assert_eq!(fake.jobs().len(), 1);
    // 進み具合も知らせも出さない
    lp.remote(&mut ed, RemoteEvent::Phase { req: Some(RemoteReq(1)), tag: JobTag(4), root: proj.clone(), phase: Phase::Running, done: 0, total: 0 });
    assert_eq!(ed.remote_progress(), None);
    lp.remote(&mut ed, done(Some(1), 4, DoneStatus::Ok));
    assert_eq!(ed.message(), None);

    // ほかの未保存の文書も重ねる。保存済みの文書は重ねない（ディスクのまま）
    lp.with(|cx| ed.open_path(&wrap, cx));
    lp.pump(&mut ed);
    lp.keys(&mut ed, &[key(Key::End)]);
    lp.type_str(&mut ed, " // w");
    lp.idle(&mut ed);
    let jobs = auto_jobs(&fake);
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[1].file, wrap);
    let mut paths: Vec<&Path> = jobs[1].overlay.iter().map(|o| o.path.as_path()).collect();
    paths.sort();
    assert_eq!(paths, [lib.as_path(), wrap.as_path()]);
    let w = jobs[1].overlay.iter().position(|o| o.path == wrap).unwrap();
    assert_eq!(overlay_text(&jobs[1], w), text(&ed));
    // wrap.rs を保存した: 保存時の check を頼み、同じ版では打鍵が止まったときの check を頼まない
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    assert_eq!(fake.jobs().len(), 3);
    assert!(!fake.jobs()[2].flags.contains(SubmitFlags::AUTO));
    lp.with(|cx| ed.on_timer(TimerId::AutoCheck, cx));
    assert_eq!(fake.jobs().len(), 3);
    // 保存した後に打てば、lib.rs（未保存）だけを重ねる
    lp.type_str(&mut ed, "!");
    lp.idle(&mut ed);
    let jobs = auto_jobs(&fake);
    assert_eq!(jobs.len(), 3);
    let mut paths: Vec<&Path> = jobs[2].overlay.iter().map(|o| o.path.as_path()).collect();
    paths.sort();
    assert_eq!(paths, [lib.as_path(), wrap.as_path()], "打った wrap.rs は未保存に戻った");
    let _ = fs::remove_dir_all(&dir);
}

both!(auto_check_results_follow_edits_and_drop_superseded, auto_check_results_follow_edits_and_drop_superseded_simple, auto_check_results_follow_edits_and_drop_superseded_buffer);
fn auto_check_results_follow_edits_and_drop_superseded(backend: DocBackend) {
    let (dir, proj, mut ed, mut lp, fake) = rust_project(backend, "auto-late");
    let diags = |req: u32, tag: u32, complete: bool, ds: &[(u32, &str)]| RemoteEvent::Diagnostics {
        req: Some(RemoteReq(req)),
        tag: JobTag(tag),
        root: proj.clone(),
        complete,
        files: vec![FileDiags { path: "src/lib.rs".into(), diags: ds.iter().map(|&(l, m)| err(l, m)).collect() }],
    };
    let scanned_mine = |req: u32, tag: u32| RemoteEvent::Phase { req: Some(RemoteReq(req)), tag: JobTag(tag), root: proj.clone(), phase: Phase::Scanned, done: 0, total: 0 };
    // 先頭に 2 行足して（未保存のまま）止まる → 頼む。診断の基準は、このときの文書の版
    lp.keys(&mut ed, &[KeyEvent::new(Key::Home, Mods::CTRL)]);
    lp.type_str(&mut ed, "// a\n// b\n");
    lp.idle(&mut ed);
    assert_eq!(auto_jobs(&fake).len(), 1);
    lp.remote(&mut ed, scanned_mine(1, 5));
    // 届く前に、もう 1 行足した
    lp.type_str(&mut ed, "// c\n");
    // 重ねた中身（12 行）の 5 行目は、今の 6 行目（保存済みの版を基準にすると 8 行目になる）
    lp.remote(&mut ed, diags(1, 5, true, &[(4, "E0308")]));
    assert_eq!(lines_of(&ed), [5]);
    assert_eq!(ed.active_view().unwrap().marks()[0].line, 5);
    assert_eq!(ed.message(), None, "知らせは出さない（波線と件数だけ）");
    lp.remote(&mut ed, done(Some(1), 5, DoneStatus::Failed));
    // 打ち続けて 2 回頼んだ。後から走査した方の結果を取り込んだら、古い方の結果は捨てる
    lp.type_str(&mut ed, "x");
    lp.idle(&mut ed);
    lp.type_str(&mut ed, "y");
    lp.idle(&mut ed);
    assert_eq!(auto_jobs(&fake).len(), 3);
    lp.remote(&mut ed, scanned_mine(2, 6));
    lp.remote(&mut ed, scanned_mine(3, 7));
    lp.remote(&mut ed, diags(3, 7, true, &[(1, "new")]));
    lp.remote(&mut ed, diags(2, 6, true, &[(8, "old")]));
    assert_eq!(lines_of(&ed), [1], "古い中身の結果で上書きしない");
    lp.remote(&mut ed, done(Some(2), 6, DoneStatus::Cancelled));
    lp.remote(&mut ed, done(Some(3), 7, DoneStatus::Failed));
    // 取り消された方（Cancelled）では休まない
    lp.type_str(&mut ed, "z");
    assert!(lp.timers.deadline(TimerId::AutoCheck).is_some());
    // 直った: 完全で空の結果で波線が消える。知らせは出さない
    lp.idle(&mut ed);
    lp.remote(&mut ed, scanned_mine(4, 8));
    lp.remote(&mut ed, diags(4, 8, true, &[]));
    lp.remote(&mut ed, done(Some(4), 8, DoneStatus::Ok));
    assert!(ed.diagnostics().is_empty());
    assert_eq!(ed.message(), None);
    let _ = fs::remove_dir_all(&dir);
}

both!(auto_check_failures_are_silent_and_pause_until_save, auto_check_failures_are_silent_and_pause_until_save_simple, auto_check_failures_are_silent_and_pause_until_save_buffer);
fn auto_check_failures_are_silent_and_pause_until_save(backend: DocBackend) {
    let (dir, _proj, mut ed, mut lp, fake) = rust_project(backend, "auto-quiet");
    // NotApplicable は、設定が always（既定）・warm・manual のときに serve が断る形。どれも
    // 何も知らせず、実機の構文チェックにも回さず、次の保存まで休む
    for status in [DoneStatus::NotApplicable, DoneStatus::Offline, DoneStatus::Limit, DoneStatus::Auth, DoneStatus::Clock, DoneStatus::Error] {
        lp.type_str(&mut ed, "x");
        lp.idle(&mut ed);
        let n = fake.jobs().len() as u32;
        assert!(fake.jobs()[n as usize - 1].flags.contains(SubmitFlags::AUTO));
        lp.remote(&mut ed, done(Some(n), 0, status));
        lp.pump(&mut ed);
        assert_eq!(ed.message(), None, "{status:?}");
        assert!(ed.diagnostics().is_empty());
        lp.type_str(&mut ed, "y");
        assert_eq!(lp.timers.deadline(TimerId::AutoCheck), None, "{status:?}: 休んでいる間は仕掛けない");
        lp.with(|cx| ed.on_timer(TimerId::AutoCheck, cx));
        assert_eq!(fake.jobs().len() as u32, n);
        // 保存すると、保存時の check を頼み、また試す
        lp.keys(&mut ed, &[ctrl('s')]);
        lp.pump(&mut ed);
        assert_eq!(fake.jobs().len() as u32, n + 1);
        assert!(!fake.jobs()[n as usize].flags.contains(SubmitFlags::AUTO));
        lp.remote(&mut ed, done(Some(n + 1), 0, DoneStatus::Ok));
    }
    let _ = fs::remove_dir_all(&dir);
}

both!(auto_check_waits_for_builds_and_tests, auto_check_waits_for_builds_and_tests_simple, auto_check_waits_for_builds_and_tests_buffer);
fn auto_check_waits_for_builds_and_tests(backend: DocBackend) {
    let (dir, proj, mut ed, mut lp, fake) = rust_project(backend, "auto-busy");
    // F5 のビルド（ほかの相手のジョブ）が同じプロジェクトで動いている間は頼まない。少し後にまた見る
    lp.remote(&mut ed, RemoteEvent::Phase { req: None, tag: JobTag(3), root: proj.clone(), phase: Phase::Running, done: 0, total: 0 });
    lp.type_str(&mut ed, "x");
    lp.idle(&mut ed);
    assert!(fake.jobs().is_empty());
    assert!(lp.timers.deadline(TimerId::AutoCheck).is_some(), "もう一度見る");
    // ほかのプロジェクトのビルドは関係ない
    lp.remote(&mut ed, done(None, 3, DoneStatus::Ok));
    lp.remote(&mut ed, RemoteEvent::Phase { req: None, tag: JobTag(9), root: dir.join("other"), phase: Phase::Running, done: 0, total: 0 });
    lp.idle(&mut ed);
    assert_eq!(auto_jobs(&fake).len(), 1);
    lp.remote(&mut ed, done(Some(1), 0, DoneStatus::Ok));
    // F6 の試験の間も頼まない（未保存なので、先に保存してから試験を頼む）
    lp.keys(&mut ed, &[key(Key::F(6))]);
    lp.pump(&mut ed);
    assert_eq!(fake.jobs().iter().map(|j| j.kind).collect::<Vec<_>>(), [JobKind::Check, JobKind::Test]);
    lp.type_str(&mut ed, "y");
    lp.idle(&mut ed);
    assert_eq!(fake.jobs().len(), 2);
    lp.remote(&mut ed, RemoteEvent::Tests { req: RemoteReq(2), root: proj.clone(), report: TestReport { passed: 1, failed: 0, ignored: 0, cases: Vec::new() } });
    lp.remote(&mut ed, done(Some(2), 5, DoneStatus::Ok));
    lp.idle(&mut ed);
    assert_eq!(auto_jobs(&fake).len(), 2);
    let _ = fs::remove_dir_all(&dir);
}

both!(auto_check_defers_the_unavailable_notice, auto_check_defers_the_unavailable_notice_simple, auto_check_defers_the_unavailable_notice_buffer);
fn auto_check_defers_the_unavailable_notice(backend: DocBackend) {
    let (dir, _proj, mut ed, mut lp, fake) = rust_project(backend, "auto-unavail");
    // zc-remote が無いと分かったのが打鍵が止まったときの check なら、その場では出さない
    lp.type_str(&mut ed, "x");
    lp.idle(&mut ed);
    lp.remote(&mut ed, RemoteEvent::Unavailable("zc-remote が無い".into()));
    lp.remote(&mut ed, done(Some(1), 0, DoneStatus::NotApplicable));
    assert_eq!(ed.message(), None);
    // 保存時の check が終わったときに 1 回だけ出す
    lp.keys(&mut ed, &[ctrl('s')]);
    lp.pump(&mut ed);
    assert_eq!(fake.jobs().len(), 2);
    lp.remote(&mut ed, done(Some(2), 0, DoneStatus::NotApplicable));
    assert_eq!(ed.message(), Some("ビルド係を使えない: zc-remote が無い"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pane_keys_move_toggle_and_maximize() {
    let mut lp = Loop::new();
    let mut ed = editor_with(DocBackend::Simple, "x\n");
    assert_eq!(ed.tree_state(), (false, false));
    // Ctrl+1: 閉じていれば開いてツリーへ。Ctrl+2: エディタへ（ツリーは開いたまま）
    lp.keys(&mut ed, &[ctrl('1')]);
    assert_eq!(ed.tree_state(), (true, true));
    lp.keys(&mut ed, &[ctrl('2')]);
    assert_eq!(ed.tree_state(), (true, false));
    // Ctrl+B はどこにいても開閉。開いていれば閉じる（エディタにいるならそのまま）
    lp.keys(&mut ed, &[ctrl('b')]);
    assert_eq!(ed.tree_state(), (false, false));
    lp.keys(&mut ed, &[ctrl('b')]);
    assert_eq!(ed.tree_state(), (true, true), "開いたらツリーへ移る");
    lp.keys(&mut ed, &[ctrl('b')]);
    assert_eq!(ed.tree_state(), (false, false), "ツリーにいて閉じたらエディタへ");
    // F11: エディタを最大化 / 戻す（ツリーの開閉の状態はそのまま）
    lp.keys(&mut ed, &[ctrl('1'), ctrl('2'), key(Key::F(11))]);
    assert_eq!((ed.maximized(), ed.tree_state()), ((true, false), (true, false)));
    // 最大化の中で Ctrl+1: 最大化をやめてツリーへ
    lp.keys(&mut ed, &[ctrl('1')]);
    assert_eq!((ed.maximized(), ed.tree_state()), ((false, false), (true, true)));
    // もう居る区画の番号は何もしない
    lp.keys(&mut ed, &[ctrl('1')]);
    assert_eq!(ed.tree_state(), (true, true));
}

#[test]
fn f11_on_the_tree_picks_a_new_root() {
    let dir = tmp_dir("folder");
    fs::create_dir_all(dir.join("proj/src")).unwrap();
    fs::create_dir_all(dir.join("other")).unwrap();
    fs::write(dir.join("proj/a.py"), "a = 1\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Simple, root: dir.join("proj"), files: vec![] });
    lp.with(|cx| ed.start(cx));
    lp.pump(&mut ed);
    lp.keys(&mut ed, &[ctrl('1'), key(Key::F(11))]);
    assert_eq!(ed.maximized(), (false, true));
    // BS で上へ（いた proj を選んだまま）→ ↑ で other → Space でそこを根にする
    lp.keys(&mut ed, &[key(Key::Backspace), key(Key::Up)]);
    lp.type_str(&mut ed, " ");
    assert_eq!(ed.maximized(), (false, false));
    assert_eq!(ed.root(), dir.join("other"));
    assert_eq!(ed.tree_state(), (true, true));
    // F11 → Esc は何もせず戻る
    lp.keys(&mut ed, &[key(Key::F(11)), key(Key::Esc)]);
    assert_eq!((ed.maximized(), ed.root()), ((false, false), dir.join("other").as_path()));
    // F11 → BS で上へ（other を選んだまま）→ ↑↑ で先頭の `.` → Enter でいまいるフォルダを根にする
    lp.keys(&mut ed, &[key(Key::F(11)), key(Key::Backspace), key(Key::Up), key(Key::Up), key(Key::Enter)]);
    assert_eq!((ed.maximized(), ed.root()), ((false, false), dir.as_path()));
    // Enter で other に入る → 下にフォルダが無いので `.` が選ばれていて、Enter だけで決まる
    lp.keys(&mut ed, &[key(Key::F(11)), key(Key::Enter)]);
    assert_eq!(ed.maximized(), (false, true));
    lp.keys(&mut ed, &[key(Key::Enter)]);
    assert_eq!(ed.root(), dir.join("other"));
    lp.keys(&mut ed, &[key(Key::F(11)), key(Key::Enter)]);
    assert_eq!((ed.maximized(), ed.root()), ((false, false), dir.join("other").as_path()));
    let _ = fs::remove_dir_all(&dir);
}

/// 正解画像: フォルダを開く画面と、移った瞬間の上の辺の光（決まったフォルダで描く）。
#[test]
fn folder_picker_and_focus_flash_pictures() {
    let dir = PathBuf::from("/tmp/zcode-golden-folder");
    let _ = fs::remove_dir_all(&dir);
    for p in ["blink/lib", "qr-test", "rust-hello"] {
        fs::create_dir_all(dir.join(p)).unwrap();
    }
    fs::write(dir.join("blink/main.py"), "import time\n").unwrap();
    fs::write(dir.join("todo.md"), "").unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Simple, root: dir.join("blink"), files: vec![] });
    lp.with(|cx| ed.start(cx));
    lp.pump(&mut ed);
    lp.with(|cx| ed.open_path(&dir.join("blink/main.py"), cx));
    lp.pump(&mut ed);
    // エディタから Ctrl+1 でツリーへ: ツリーの上の辺が光る（最初の段）
    lp.keys(&mut ed, &[ctrl('2'), ctrl('1')]);
    assert_eq!(ed.tree_state(), (true, true));
    check_golden(&mut ed, "focus-flash.png");
    // F11 → BS: ひとつ上のフォルダ（いた blink を選んだまま、いま開いている印）
    lp.keys(&mut ed, &[key(Key::F(11)), key(Key::Backspace)]);
    check_golden(&mut ed, "folder-picker.png");
    let _ = fs::remove_dir_all(&dir);
}
