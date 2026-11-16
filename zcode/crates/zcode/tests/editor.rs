//! エディタ画面（`EditorScreen`）の動き: 打鍵 → 操作 → 文書、ツリー、保存、
//! ステータスバーの遅延、描き変える範囲。実機もイベントループも要らない
//! （ワーカーと、結果を受ける口だけ本物を使う）。文書は `SimpleDoc`。

use std::fs;
use std::path::PathBuf;
use zc_core::rt::{main_channel, MainReceiver, MainSender};
use zc_core::time::now_us;
use zc_core::{Canvas, Damage, Key, KeyEvent, Mods, Pos, Priority, RowRange, Theme};
use zc_font::{FontConfig, Fonts, RasterMode};
use zc_ui::layout::CODE_LINE_H;
use zc_ui::RenderCx;
use zcode::app::{AppEvent, LoopCx, Screen, TimerId, Timers, Workers};
use zcode::doc::{DocBackend, SimpleDoc};
use zcode::edbench::{doc_text, EdBenchKind, EditorBench};
use zcode::editor::{EditorConfig, EditorScreen};

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

fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zcode-editor-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// 文書を 1 つ開いたエディタ。
fn editor_with(text: &str) -> EditorScreen {
    let cfg = EditorConfig { backend: DocBackend::Simple, root: PathBuf::from("/nonexistent"), files: vec![PathBuf::from("a.py")] };
    let mut ed = EditorScreen::new(cfg);
    ed.open_doc(Some(PathBuf::from("/nonexistent/a.py")), "a.py", Box::new(SimpleDoc::from_text(text)), None);
    ed
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

#[test]
fn typing_newline_indent_and_undo() {
    let mut lp = Loop::new();
    let mut ed = editor_with("");
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

#[test]
fn quit_and_close_ask_when_modified() {
    let mut lp = Loop::new();
    let mut ed = editor_with("x");
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

#[test]
fn tabs_new_file_and_switching() {
    let mut lp = Loop::new();
    let mut ed = editor_with("first");
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

#[test]
fn goto_line_and_find_prompts() {
    let mut lp = Loop::new();
    let body: String = (1..=50).map(|i| format!("line {i} foo\n")).collect();
    let mut ed = editor_with(&body);
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

#[test]
fn open_from_tree_edit_and_save() {
    let dir = tmp_dir("save");
    fs::create_dir_all(dir.join("blink")).unwrap();
    fs::write(dir.join("blink/main.py"), "import time\n").unwrap();
    fs::write(dir.join("notes.txt"), "memo\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Simple, root: dir.clone(), files: vec![] });
    lp.with(|cx| ed.start(cx));
    // ファイルを渡さずに起動: ツリーに入力がある。blink を開いて main.py を選ぶ
    assert_eq!(ed.tab_count(), 0);
    lp.keys(&mut ed, &[key(Key::Enter), key(Key::Down), key(Key::Enter)]);
    assert_eq!(ed.tab_count(), 1);
    assert_eq!(ed.active_path(), Some(dir.join("blink/main.py").as_path()));
    // 読み込みは batch。終わるまでは空で、打鍵も入らない
    lp.type_str(&mut ed, "zzz");
    lp.pump(&mut ed);
    assert_eq!(text(&ed), "import time");
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
    assert_eq!(fs::read_to_string(dir.join("blink/new.py")).unwrap(), "x = 1\n");
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

#[test]
fn keystroke_damage_is_cursor_line_and_status_is_deferred() {
    let Some(mut g) = Gfx::new() else { return };
    let mut lp = Loop::new();
    let body: String = (1..=40).map(|i| format!("value_{i} = {i}\n")).collect();
    let mut ed = editor_with(&body);
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

#[test]
fn cursor_blink_stops_when_idle() {
    let Some(mut g) = Gfx::new() else { return };
    let mut lp = Loop::new();
    let mut ed = editor_with("abc\ndef\n");
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

#[test]
fn tree_toggle_and_focus() {
    let Some(mut g) = Gfx::new() else { return };
    let dir = tmp_dir("tree");
    fs::write(dir.join("a.py"), "a = 1\n").unwrap();
    fs::write(dir.join("b.py"), "b = 2\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Simple, root: dir.clone(), files: vec![dir.join("a.py")] });
    lp.with(|cx| ed.start(cx));
    lp.pump(&mut ed);
    g.render(&mut ed);
    // ファイルを渡して起動: ツリーは閉じていて、エディタは全幅
    assert_eq!(ed.active_view().unwrap().point_of(Pos::new(0, 0)).unwrap().x, 23);
    lp.keys(&mut ed, &[ctrl('b')]);
    let d = g.render(&mut ed);
    assert_eq!(d.bounds(), RowRange::new(0, 159));
    assert_eq!(ed.active_view().unwrap().point_of(Pos::new(0, 0)).unwrap().x, 84 + 23);
    // Ctrl+E でツリーへ。文字は文書に入らない。↓ Enter で b.py
    lp.keys(&mut ed, &[ctrl('e')]);
    lp.type_str(&mut ed, "zz");
    lp.keys(&mut ed, &[key(Key::Down), key(Key::Enter)]);
    lp.pump(&mut ed);
    assert_eq!((ed.tab_count(), text(&ed)), (2, "b = 2".to_string()));
    // 開いたらエディタに入力が戻る
    lp.type_str(&mut ed, "#");
    assert_eq!(text(&ed), "#b = 2");
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
