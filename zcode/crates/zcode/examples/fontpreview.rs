//! コードのフォントの比較用に、実機と同じ描き方（`zc-font` の swash・ヒンティング・16 段の濃度、
//! BDF はそのまま）でエディタの画面を PNG に描く。
//!
//! ```text
//! cargo run -p zcode --example fontpreview -- <出力.png> (--ttf <フォント> | --bdf <BDF>) [--px 10] [--line 12]
//!     [--cjk <日本語のフォント> --cjk-px 10 --crisp 1]
//! ```
//!
//! 見本は、読み違えやすい記号（`+ - = 0 O l 1 I | { } ; : , . ' " ` ~ ^`）と日本語のコメントを含む Python。

use std::path::PathBuf;
use zc_font::FontConfig;
use zcode::doc::{DocBackend, SimpleDoc, TextDoc};
use zcode::editor::{EditorConfig, EditorScreen};

const SAMPLE: &str = r#"import time  # 0.5 秒ごとに点滅（O0 l1I|）
from utils import led, read_temp

def blink(n: int = 10, dt=0.5) -> None:
    total = 0
    for i in range(n):
        led(i % 2 == 0)
        total += i * 2 - 1
        if total != 0 and dt >= 0.1:
            time.sleep(dt / 2)
    print(f"done {total:+d}", {'a': [1, 2]}, ~x ^ y)

t = read_temp()  # 温度 → ℃、"°C" と `x`
"#;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().expect("出力の PNG"));
    let mut cfg = FontConfig::default();
    let (mut px, mut line) = (zc_ui::layout::CODE_PX, zc_ui::layout::CODE_LINE_H);
    while let Some(a) = args.next() {
        let v = args.next().expect("値");
        match a.as_str() {
            "--ttf" => cfg.mono = v.into(),
            "--bdf" => cfg.mono_bdf = Some(v.into()),
            "--cjk" => cfg.mono_cjk = Some(zc_font::CjkFont { path: v.into(), px: 12.0, crisp: false }),
            "--cjk-px" => cfg.mono_cjk.as_mut().expect("--cjk を先に").px = v.parse().expect("--cjk-px"),
            "--crisp" => cfg.mono_cjk.as_mut().expect("--cjk を先に").crisp = v == "1",
            "--px" => px = v.parse().expect("--px"),
            "--line" => line = v.parse().expect("--line"),
            _ => panic!("知らない引数 {a}"),
        }
    }
    cfg.code_px = px;
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Simple, root: PathBuf::from("/nonexistent"), files: vec![PathBuf::from("main.py")] });
    let mut doc = SimpleDoc::from_text(SAMPLE);
    let at = zc_core::Pos::new(7, 13);
    doc.set_selection(at, at);
    ed.open_doc(Some(PathBuf::from("/nonexistent/main.py")), "main.py", Box::new(doc), None);
    ed.set_code_font(px, line);
    let canvas = zcode::render_screen(&mut ed, &cfg).expect("描画");
    zc_display::png::write_png(&canvas, &out).expect("PNG を書く");
    // 字の幅と、エディタの 1 行に入る字数（行番号の欄を除いた幅）
    let mut fonts = zc_font::Fonts::load(&cfg, zc_font::RasterMode::Inline).expect("フォント");
    let m = fonts.metrics(&zc_font::TextStyle::mono(px));
    let gutter = zc_ui::editor::gutter_width(14, m.cell_w) as i32;
    println!("{} cell={} ascent={} descent={} cols={}", out.display(), m.cell_w, m.ascent, m.descent, (320 - gutter) / m.cell_w as i32);
}
