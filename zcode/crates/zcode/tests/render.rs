//! render-test: 段階 1 の画面を `Canvas` に描いて、正解画像（`zcode/tests/golden/`）と
//! 比べる。実機は要らない。フォント（DejaVu Sans Mono / Noto Sans CJK）が無い環境では
//! 飛ばす（開発コンテナには入っている）。
//!
//! 正解を更新するとき: `ZCODE_UPDATE_GOLDEN=1 cargo test -p zcode --test render`

use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use zc_core::{Mods, Pos, Span};
use zc_display::png::check_golden;
use zc_font::FontConfig;
use zc_input::Keymap;
use zcode::app::Screen;
use zcode::bench::{Bench, BenchKind};
use zcode::doc::{DocBackend, SimpleDoc, TextDoc};
use zcode::editor::{EditorConfig, EditorScreen};
use zcode::fonttest::FontTest;
use zcode::keytest::KeyTest;
use zcode::{render_screen, sample};

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden").join(name)
}

fn check(screen: &mut dyn Screen, name: &str) {
    let fonts = FontConfig::default();
    if !fonts.available() {
        eprintln!("フォントが無いので {name} を飛ばす");
        return;
    }
    let canvas = render_screen(screen, &fonts).unwrap();
    if let Err(e) = check_golden(&canvas, golden(name)) {
        panic!("{e}");
    }
}

#[test]
fn keytest_screen() {
    let mut kt = KeyTest::new(Keymap::builtin(), None, false, Duration::from_secs(120));
    kt.demo();
    check(&mut kt, "keytest.png");
}

#[test]
fn keytest_guide_screen() {
    let mut kt = KeyTest::new(Keymap::builtin(), None, true, Duration::from_secs(120));
    kt.demo();
    check(&mut kt, "keytest-guide.png");
}

#[test]
fn keytest_empty_screen() {
    let mut kt = KeyTest::new(Keymap::builtin(), None, false, Duration::from_secs(120));
    check(&mut kt, "keytest-empty.png");
}

#[test]
fn fonttest_pages() {
    for page in 0..5 {
        let mut ft = FontTest::new(None, Duration::from_secs(120), false);
        assert_eq!(ft.page_count(), 5);
        ft.set_page(page);
        check(&mut ft, &format!("fonttest-{}.png", page + 1));
    }
}

/// モック 1 と同じ並びの作業フォルダ（blink/ を開いた状態で描く）。
fn mock_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zcode-render-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("blink")).unwrap();
    fs::create_dir_all(dir.join("sensors")).unwrap();
    for f in ["blink/main.py", "blink/utils.py", "README.md", "notes.txt"] {
        fs::write(dir.join(f), "").unwrap();
    }
    dir
}

/// 見本の Python を 2 つのタブ（utils.py・main.py）で開いたエディタ。カーソルは
/// main.py の `line` 行目の行末。`spans` が真なら手書きのトークンで色を付ける。
fn mock_editor(root: &std::path::Path, modified: bool, line: u32, spans: bool) -> EditorScreen {
    let cfg = EditorConfig { backend: DocBackend::Simple, root: root.to_path_buf(), files: vec![root.join("blink/main.py")] };
    let mut ed = EditorScreen::new(cfg);
    ed.tree_mut().set_title("~/code");
    ed.open_doc(Some(root.join("blink/utils.py")), "utils.py", Box::new(SimpleDoc::from_text("def led(on):\n    pass\n")), None);
    let mut doc = SimpleDoc::from_text(&sample::text()).with_modified(modified);
    let end = Pos::new(line, doc.line_len(line));
    doc.set_selection(end, end);
    let hl: Option<Box<dyn zc_ui::SpanSource>> = if spans { Some(Box::new(|l: u32, t: &str, out: &mut Vec<Span>| sample::spans(l, t, out))) } else { None };
    ed.open_doc(Some(root.join("blink/main.py")), "main.py", Box::new(doc), hl);
    ed
}

/// モック 1: エクスプローラ + エディタ（未保存、SHIFT ロック中）。色なし。
#[test]
fn editor_with_tree_screen() {
    let root = mock_workspace("tree");
    let mut ed = mock_editor(&root, true, 6, false);
    ed.set_tree_open(true);
    ed.set_mods(Mods::SHIFT);
    check(&mut ed, "editor-tree.png");
    let _ = fs::remove_dir_all(&root);
}

/// モック 2: エディタ全幅。色なし。
#[test]
fn editor_full_width_screen() {
    let root = mock_workspace("full");
    let mut ed = mock_editor(&root, false, 7, false);
    check(&mut ed, "editor-full.png");
    let _ = fs::remove_dir_all(&root);
}

/// エディタ全幅、手書きのトークンで色つき。
#[test]
fn editor_highlighted_screen() {
    let root = mock_workspace("hl");
    let mut ed = mock_editor(&root, false, 7, true);
    check(&mut ed, "editor-highlight.png");
    let _ = fs::remove_dir_all(&root);
}

/// 色つき + 複数行の選択 + ツリーに入力がある状態。
#[test]
fn editor_selection_and_tree_focus_screen() {
    let root = mock_workspace("sel");
    let cfg = EditorConfig { backend: DocBackend::Simple, root: root.clone(), files: vec![] };
    let mut ed = EditorScreen::new(cfg);
    ed.tree_mut().set_title("~/code");
    let mut doc = SimpleDoc::from_text(&sample::text());
    doc.set_selection(Pos::new(4, 8), Pos::new(7, 13));
    ed.open_doc(Some(root.join("notes.py")), "notes.py", Box::new(doc), Some(Box::new(|l: u32, t: &str, out: &mut Vec<Span>| sample::spans(l, t, out))));
    check(&mut ed, "editor-selection.png");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn bench_base_screen() {
    let mut b = Bench::new(BenchKind::Line, 10);
    check(&mut b, "bench.png");
}

#[test]
fn second_render_without_changes_returns_no_damage() {
    let fonts = FontConfig::default();
    if !fonts.available() {
        return;
    }
    use zc_core::{Canvas, Theme};
    use zc_font::{Fonts, RasterMode};
    use zc_ui::RenderCx;
    let mut f = Fonts::load(&fonts, RasterMode::Inline).unwrap();
    let theme = Theme::dark_plus();
    let mut canvas = Canvas::new();
    let mut ft = FontTest::new(None, Duration::from_secs(120), false);
    let d1 = ft.render(&mut RenderCx::new(&mut canvas, &mut f, &theme));
    assert_eq!(d1.bounds(), zc_core::RowRange::new(0, 170));
    let d2 = ft.render(&mut RenderCx::new(&mut canvas, &mut f, &theme));
    assert!(d2.is_empty());
}
