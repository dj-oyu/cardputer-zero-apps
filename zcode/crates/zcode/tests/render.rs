//! render-test: 段階 1 の画面を `Canvas` に描いて、正解画像（`zcode/tests/golden/`）と
//! 比べる。実機は要らない。フォント（DejaVu Sans Mono / Noto Sans CJK）が無い環境では
//! 飛ばす（開発コンテナには入っている）。
//!
//! 正解を更新するとき: `ZCODE_UPDATE_GOLDEN=1 cargo test -p zcode --test render`

use std::path::PathBuf;
use std::time::Duration;
use zc_display::png::check_golden;
use zc_font::FontConfig;
use zc_input::Keymap;
use zcode::app::Screen;
use zcode::bench::{Bench, BenchKind};
use zcode::fonttest::FontTest;
use zcode::keytest::KeyTest;
use zcode::render_screen;

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
