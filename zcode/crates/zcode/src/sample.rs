//! 段階 1 の画面で使う見本のコード（モックと同じ Python）と、その描き方。
//! ハイライトは手で付けてある（`zc-highlight` は段階 2）。

use zc_core::{palette, Color, Damage, Rect, TokenKind as T};
use zc_font::TextStyle;
use zc_ui::RenderCx;

/// 色つきの 1 行（トークンの種類と文字列の並び）。
pub type Line = &'static [(T, &'static str)];

/// 見本のコード。
pub const CODE: &[Line] = &[
    &[(T::Control, "import"), (T::Text, " time")],
    &[(T::Control, "from"), (T::Text, " utils "), (T::Control, "import"), (T::Text, " led")],
    &[],
    &[(T::Keyword, "def"), (T::Text, " "), (T::Function, "main"), (T::Text, "():")],
    &[(T::Text, "    "), (T::Comment, "# 0.5 秒ごとに点滅。10 回で終わる")],
    &[
        (T::Text, "    "),
        (T::Control, "for"),
        (T::Text, " i "),
        (T::Control, "in"),
        (T::Text, " "),
        (T::Function, "range"),
        (T::Text, "("),
        (T::Number, "10"),
        (T::Text, "):"),
    ],
    &[(T::Text, "        "), (T::Function, "led"), (T::Text, "(i % "), (T::Number, "2"), (T::Text, ")")],
    &[
        (T::Text, "        time."),
        (T::Function, "sleep"),
        (T::Text, "("),
        (T::Number, "0.5"),
        (T::Text, ")"),
    ],
    &[(T::Text, "    "), (T::Function, "print"), (T::Text, "("), (T::String, "\"done: 完了\""), (T::Text, ")")],
    &[],
    &[
        (T::Control, "if"),
        (T::Text, " "),
        (T::Variable, "__name__"),
        (T::Text, " == "),
        (T::String, "\"__main__\""),
        (T::Text, ":"),
    ],
    &[(T::Text, "    "), (T::Function, "main"), (T::Text, "()")],
    &[(T::Comment, "# 0O 1lI| {}[]() <>=!~ ｶﾅ カナ かな 漢字")],
    &[(T::Text, "x = {"), (T::String, "'a'"), (T::Text, ": [1, 2], "), (T::String, "'日本'"), (T::Text, ": None}")],
];

/// 色つきの 1 行を描く。ベースラインは行の上下の中央。描き終わりの x を返す。
pub fn draw_line(cx: &mut RenderCx<'_>, area: Rect, line: Line, ts: &TextStyle, bg: Color) -> i32 {
    let base = area.y as i32 + cx.baseline_in(ts, area.h);
    let mut x = area.x as i32;
    for &(kind, text) in line {
        let fg = cx.theme.token(kind).fg;
        x = cx.text(x, base, text, ts, fg, bg, area);
    }
    x
}

/// エディタ風の画面（行番号つき）を `area` に描く。`cur` はカーソル行。
/// 入る行数と桁数を返す。
pub fn draw_editor(cx: &mut RenderCx<'_>, area: Rect, ts: &TextStyle, line_h: u16, cur: usize) -> (u16, u16) {
    let cell = cx.metrics(ts).cell_w;
    let gutter = cell * 3 + 5;
    cx.canvas.fill_rect(area, palette::BG);
    let rows = area.h / line_h;
    for i in 0..rows as usize {
        let y = area.y + (i as u16 * line_h) as i16;
        let row = Rect::new(area.x, y, area.w, line_h);
        let bg = if i == cur { palette::LINE } else { palette::BG };
        if i == cur {
            cx.canvas.fill_rect(row, bg);
        }
        let num = format!("{:>3}", i + 1);
        let fg = if i == cur { palette::GUTTER_CUR } else { palette::GUTTER };
        cx.text_in(Rect::new(area.x, y, gutter, line_h), &num, ts, fg, bg);
        let text_area = Rect::new(area.x + gutter as i16, y, area.w - gutter, line_h);
        let end = draw_line(cx, text_area, CODE[i % CODE.len()], ts, bg);
        if i == cur {
            cx.canvas.vline(end, y as i32 + 1, line_h - 2, palette::CURSOR);
        }
    }
    (rows, (area.w - gutter) / cell.max(1))
}

/// `area` 全体を描き変えたことを表す `Damage`。
pub fn damage_of(area: Rect) -> Damage {
    Damage::rect(area)
}
