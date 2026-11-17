//! Markdown のライブプレビューをエディタに組み込んだ動き（段階 5、モック 10・11）。

use std::path::PathBuf;
use zc_core::rt::{main_channel, MainReceiver, MainSender};
use zc_core::time::now_us;
use zc_core::{Key, KeyEvent, Mods, Pos};
use zcode::app::{AppEvent, LoopCx, Screen, Timers, Workers};
use zcode::doc::{DocBackend, SimpleDoc, TextDoc};
use zcode::editor::{EditorConfig, EditorScreen};

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
        for k in keys {
            self.with(|cx| ed.keys(std::slice::from_ref(k), Mods::NONE, cx));
            self.pump(ed);
        }
    }
    fn pump(&mut self, ed: &mut EditorScreen) {
        for _ in 0..3 {
            self.workers.batch.wait_idle();
            self.workers.assist.wait_idle();
            let evs: Vec<AppEvent> = self.rx.drain().collect();
            for ev in evs {
                self.with(|cx| ed.on_event(ev, cx));
            }
        }
    }
}

fn chord(c: char, mods: Mods) -> KeyEvent {
    KeyEvent::new(Key::Char(c), mods)
}

/// `name` のファイルを開いたことにする。カーソルは `at`。
fn editor(name: &str, text: &str, at: (Pos, Pos)) -> EditorScreen {
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Simple, root: PathBuf::from("/nonexistent"), files: vec![PathBuf::from(name)] });
    let mut doc = SimpleDoc::from_text(text);
    doc.set_selection(at.0, at.1);
    ed.open_doc(Some(PathBuf::from("/nonexistent").join(name)), name, Box::new(doc), None);
    ed
}

/// 文書の中身（`doc_text` は最後の改行を落とすので、見本と同じく足して比べる）。
fn text(ed: &EditorScreen) -> String {
    zcode::edbench::doc_text(ed.active_doc().unwrap()) + "\n"
}

fn at(line: u32, col: u32) -> (Pos, Pos) {
    (Pos::new(line, col), Pos::new(line, col))
}

#[test]
fn md_opens_in_preview_and_ctrl_shift_v_toggles_source() {
    let mut lp = Loop::new();
    let mut ed = editor("memo.md", "# 見出し\n本文\n", at(1, 2));
    assert_eq!(ed.markdown_preview(), Some(true));
    lp.keys(&mut ed, &[chord('v', Mods::CTRL | Mods::SHIFT)]);
    assert_eq!(ed.markdown_preview(), Some(false));
    // 同じバッファ・同じカーソル位置
    assert_eq!(ed.active_doc().unwrap().selection().1, Pos::new(1, 2));
    lp.keys(&mut ed, &[chord('v', Mods::CTRL | Mods::SHIFT)]);
    assert_eq!(ed.markdown_preview(), Some(true));
    // ソース表示で編集しても、プレビューに戻れば反映されている
    lp.keys(&mut ed, &[chord('v', Mods::CTRL | Mods::SHIFT), KeyEvent::char('!')]);
    lp.keys(&mut ed, &[chord('v', Mods::CTRL | Mods::SHIFT)]);
    assert_eq!(text(&ed), "# 見出し\n本文!\n");
}

#[test]
fn preview_toggle_is_markdown_only() {
    let mut lp = Loop::new();
    let mut ed = editor("a.py", "x = 1\n", at(0, 0));
    assert_eq!(ed.markdown_preview(), None);
    lp.keys(&mut ed, &[chord('v', Mods::CTRL | Mods::SHIFT)]);
    assert_eq!(ed.markdown_preview(), None);
    assert_eq!(ed.message(), Some("プレビューは Markdown だけ"));
}

#[test]
fn ctrl_enter_toggles_the_checkbox_and_undo_restores() {
    let mut lp = Loop::new();
    let mut ed = editor("todo.md", "- [ ] ケース\n- [x] 基板\n", at(0, 8));
    lp.keys(&mut ed, &[KeyEvent::new(Key::Enter, Mods::CTRL)]);
    assert_eq!(text(&ed), "- [x] ケース\n- [x] 基板\n");
    assert_eq!(ed.active_doc().unwrap().selection().1, Pos::new(0, 8), "カーソルは動かない");
    lp.keys(&mut ed, &[KeyEvent::new(Key::Down, Mods::NONE), KeyEvent::new(Key::Enter, Mods::CTRL)]);
    assert_eq!(text(&ed), "- [x] ケース\n- [ ] 基板\n");
    lp.keys(&mut ed, &[chord('z', Mods::CTRL)]);
    assert_eq!(text(&ed), "- [x] ケース\n- [x] 基板\n", "1 回の取り消しで戻る");
}

#[test]
fn alt_b_and_alt_i_wrap_the_selection() {
    let mut lp = Loop::new();
    let mut ed = editor("a.md", "印刷する\n", (Pos::new(0, 0), Pos::new(0, 2)));
    lp.keys(&mut ed, &[chord('b', Mods::ALT)]);
    assert_eq!(text(&ed), "**印刷**する\n");
    // 囲んだ中身を選び直す
    assert_eq!(ed.active_doc().unwrap().selection(), (Pos::new(0, 2), Pos::new(0, 4)));
    // 選択が無ければ記号の組を入れて、間にカーソル
    let mut ed = editor("a.md", "ab\n", at(0, 1));
    lp.keys(&mut ed, &[chord('i', Mods::ALT), KeyEvent::char('x')]);
    assert_eq!(text(&ed), "a*x*b\n");
}

// --- 画面（モック 10・11）: 正解画像と比べる ---

/// モック 10・11 の見本（memo.md の 3 行目の行末にカーソル。隣のタブに main.py）。
const MEMO: &str = "# 買い物と作業メモ
- [x] 基板を発注する
- [ ] ケースを **3D 印刷** する
- [ ] `zcode` を実機で試す
## メモ
> CMA は **64MB** しかない。*カメラ*と同時に使うときは注意
詳細は [development.md](../development.md) を参照
```python
import zero_ui as ui
ui.font(12)
```
";

fn mock_memo() -> EditorScreen {
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Simple, root: PathBuf::from("/nonexistent"), files: vec![PathBuf::from("memo.md")] });
    ed.open_doc(Some(PathBuf::from("/nonexistent/main.py")), "main.py", Box::new(SimpleDoc::from_text("import time\n")), None);
    let mut doc = SimpleDoc::from_text(MEMO);
    let end = Pos::new(2, doc.line_len(2));
    doc.set_selection(end, end);
    ed.open_doc(Some(PathBuf::from("/nonexistent/memo.md")), "memo.md", Box::new(doc), None);
    ed
}

fn check(ed: &mut EditorScreen, name: &str) {
    let fonts = zc_font::FontConfig::default();
    if !fonts.available() {
        return;
    }
    let canvas = zcode::render_screen(ed, &fonts).unwrap();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden").join(name);
    if let Err(e) = zc_display::png::check_golden(&canvas, path) {
        panic!("{e}");
    }
}

#[test]
fn mock10_preview_screen() {
    let mut ed = mock_memo();
    assert_eq!(ed.markdown_preview(), Some(true));
    check(&mut ed, "md-preview.png");
}

#[test]
fn mock11_source_screen() {
    let mut lp = Loop::new();
    let mut ed = mock_memo();
    lp.keys(&mut ed, &[chord('v', Mods::CTRL | Mods::SHIFT)]);
    check(&mut ed, "md-source.png");
}
