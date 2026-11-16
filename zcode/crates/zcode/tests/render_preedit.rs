//! render-test: 未確定文字列（SKK の ▽ / ▼）を挟んだエディタ本体（モック 7・8 の本文）。
//!
//! `EditorScreen` を通さず、`zc_ui::EditorView` だけを本文の領域に描く（タブバーと
//! ステータスバーは描かないので黒いまま）。画面全体の正解画像は、SKK を組み込む側で足す。
//!
//! 正解を更新するとき: `ZCODE_UPDATE_GOLDEN=1 cargo test -p zcode --test render_preedit`

use std::path::PathBuf;
use zc_core::{Canvas, LineSource, Pos, Rect, Span, Theme, TokenKind};
use zc_display::png::check_golden;
use zc_font::{FontConfig, Fonts, RasterMode};
use zc_ui::{EditorView, PreeditStyle, PreeditView, RenderCx};

/// モック 7・8 の本文（5 行目の「てんめつ」はまだ文書に入っていない）。
const LINES: [&str; 9] = [
    "import time",
    "from utils import led",
    "",
    "def main():",
    "    # 0.5 秒ごとに",
    "    for i in range(10):",
    "        led(i % 2)",
    "        time.sleep(0.5)",
    "    print(\"done\")",
];

struct Doc;

impl LineSource for Doc {
    fn len_lines(&self) -> u32 {
        LINES.len() as u32
    }
    fn line(&self, line: u32, out: &mut String) {
        out.clear();
        out.push_str(LINES.get(line as usize).copied().unwrap_or(""));
    }
}

/// モックと同じ色分け（手で書いたトークン。バイト位置）。
fn spans(line: u32, text: &str, out: &mut Vec<Span>) {
    use TokenKind::*;
    let toks: &[(u32, u32, TokenKind)] = match line {
        0 => &[(0, 6, Control)],
        1 => &[(0, 4, Control), (11, 17, Control), (18, 21, Function)],
        3 => &[(0, 3, Keyword), (4, 8, Function)],
        4 => &[(4, u32::MAX, Comment)],
        5 => &[(4, 7, Control), (8, 9, Variable), (10, 12, Control), (13, 18, Function), (19, 21, Number)],
        6 => &[(8, 11, Function), (12, 13, Variable), (16, 17, Number)],
        7 => &[(13, 18, Function), (19, 22, Number)],
        8 => &[(4, 9, Function), (10, 16, String)],
        _ => &[],
    };
    out.extend(toks.iter().map(|&(start, end, kind)| Span { start, end: end.min(text.len() as u32), kind }));
}

fn check(name: &str, cursor: Pos, preedit: PreeditView) {
    let cfg = FontConfig::default();
    if !cfg.available() {
        eprintln!("フォントが無いので {name} を飛ばす");
        return;
    }
    let mut fonts = Fonts::load(&cfg, RasterMode::Inline).unwrap();
    let theme = Theme::dark_plus();
    let mut canvas = Canvas::new();
    let mut view = EditorView::code();
    view.set_selection(cursor, cursor);
    view.set_preedit(Some(preedit));
    let area = Rect::new(0, 13, 320, 146);
    let mut hl = spans;
    view.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme), area, &Doc, &mut hl);
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden").join(name);
    if let Err(e) = check_golden(&canvas, golden) {
        panic!("{e}");
    }
}

/// モック 7: 行末に ▽ 読み（下線つき）。カーソルはその後ろ。
#[test]
fn preedit_reading() {
    check("preedit-reading.png", Pos::new(4, 14), PreeditView::reading("▽てんめつ"));
}

/// モック 8: 行末に ▼ 候補（青い背景 + 下線）。
#[test]
fn preedit_converting() {
    check("preedit-converting.png", Pos::new(4, 14), PreeditView::converting("▼点滅"));
}

/// 行の途中（文字列の中）に挟む: 後ろの `")` は右へずれ、色分けは挟む前のまま。
/// 打ちかけのローマ字は薄く出す。
#[test]
fn preedit_mid_line_with_pending_romaji() {
    check("preedit-midline.png", Pos::new(8, 15), PreeditView::reading("▽かんりょ").with(PreeditStyle::Pending, "ky"));
}
