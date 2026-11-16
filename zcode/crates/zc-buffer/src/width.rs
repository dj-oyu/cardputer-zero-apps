//! 文字の表示幅（桁）。全角 = 2、タブはタブ位置まで、それ以外は 1。
//!
//! `Pos.col` は文字数（Unicode スカラー値）。画面の桁とずれるのはタブと全角だけなので、
//! 上下移動で桁を保つときなどに、ここで桁へ変換する。字形の寸法そのものは
//! `zc-font` / `zc-ui` の仕事で、ここは「桁の数え方」だけを決める。

/// 全角（2 桁）として数える範囲（East Asian Wide / Fullwidth の主なところ）。
const WIDE: &[(u32, u32)] = &[
    (0x1100, 0x115F),
    (0x2E80, 0x303E),
    (0x3041, 0x33FF),
    (0x3400, 0x4DBF),
    (0x4E00, 0x9FFF),
    (0xA000, 0xA4CF),
    (0xAC00, 0xD7A3),
    (0xF900, 0xFAFF),
    (0xFE30, 0xFE6F),
    (0xFF00, 0xFF60),
    (0xFFE0, 0xFFE6),
    (0x1F300, 0x1F64F),
    (0x1F900, 0x1F9FF),
    (0x20000, 0x3FFFD),
];

/// 文字の幅（タブ以外）。結合文字・ゼロ幅は 0。
pub fn char_width(c: char) -> u32 {
    let u = c as u32;
    if u < 0x300 {
        return 1;
    }
    if (0x300..=0x36F).contains(&u) || (0x200B..=0x200F).contains(&u) || (0xFE00..=0xFE0F).contains(&u) {
        return 0;
    }
    if WIDE.iter().any(|&(a, b)| a <= u && u <= b) {
        2
    } else {
        1
    }
}

/// 桁 `col`（表示桁）から、タブを 1 つ進めた次の桁。
pub fn next_tab_stop(col: u32, tab_width: u32) -> u32 {
    let tw = tab_width.max(1);
    (col / tw + 1) * tw
}

/// 1 文字ぶん進めた表示桁。
pub fn advance(col: u32, c: char, tab_width: u32) -> u32 {
    if c == '\t' {
        next_tab_stop(col, tab_width)
    } else {
        col + char_width(c)
    }
}

/// 文字列の先頭から `n_chars` 文字ぶんの表示桁。
pub fn display_col_of(chars: impl Iterator<Item = char>, n_chars: u32, tab_width: u32) -> u32 {
    let mut col = 0;
    for c in chars.take(n_chars as usize) {
        col = advance(col, c, tab_width);
    }
    col
}

/// 表示桁 `target` に最も近い（それを超えない）文字位置。全角の途中なら手前。
pub fn char_col_at_display(chars: impl Iterator<Item = char>, target: u32, tab_width: u32) -> u32 {
    let mut col = 0;
    let mut n = 0;
    for c in chars {
        let next = advance(col, c, tab_width);
        if next > target {
            break;
        }
        col = next;
        n += 1;
    }
    n
}
