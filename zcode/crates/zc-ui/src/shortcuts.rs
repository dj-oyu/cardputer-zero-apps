//! ショートカット一覧（コマンドパレット兼用。モック 5・6）。
//!
//! 上に検索欄、その下にカテゴリの見出しと操作の行（名前が左、キーの札が右）。
//! 文字を打つと操作名の部分一致か、キー名（`ctrl s`）で絞り込む。
//!
//! - 入力は直接受け取らない。アプリが `push_char` / `backspace` / `move_sel` / `page` を呼び、
//!   Enter のときは [`ShortcutList::selected`]（元の行の番号）の操作を実行する
//! - タブバー（「⌨ ショートカット」）とステータスバー（[`ShortcutList::status_left`] /
//!   [`ShortcutList::status_right`]）はアプリが描く。この部品は間の領域だけ
//! - 描画は行ごとに前回の中身と比べ、変わった行だけ描き直す（打鍵 1 回・選択の移動で
//!   全面を描き直さない）

use crate::layout::UI_PX;
use crate::{RenderCx, Widget};
use zc_core::{palette, Color, Damage, Priority, Rect};
use zc_font::TextStyle;

/// 一覧に渡す行。`BINDINGS`（表の順、カテゴリごとにまとまっている）から作る。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShortcutRow {
    pub category: &'static str,
    pub title: &'static str,
    /// キー（`Ctrl+Shift+Tab`・`F5`・`Shift+Left` の書き方）。空ならキー無し（一覧から選んで実行する）。
    pub keys: &'static str,
}

/// Fn レイヤーのキーと、実際に押すキー（`keymap/cardputer-zero.toml` の pos から。
/// 同じ pos の通常レイヤーのキーがその物理キー）。F11 = Fn+O、F12 = Fn+P。
const FN_KEYS: &[(&str, &str)] = &[
    ("F1", "Fn+1"),
    ("F2", "Fn+2"),
    ("F3", "Fn+3"),
    ("F4", "Fn+4"),
    ("F5", "Fn+5"),
    ("F6", "Fn+6"),
    ("F7", "Fn+7"),
    ("F8", "Fn+8"),
    ("F9", "Fn+9"),
    ("F10", "Fn+0"),
    ("F11", "Fn+O"),
    ("F12", "Fn+P"),
    ("Delete", "Fn+BkSp"),
    ("Up", "Fn+F"),
    ("Left", "Fn+Z"),
    ("Down", "Fn+X"),
    ("Right", "Fn+C"),
    ("Home", "Fn+K"),
    ("PageUp", "Fn+L"),
    ("End", "Fn+N"),
    ("PageDown", "Fn+M"),
    ("Insert", "Fn+B"),
];

/// Fn レイヤーのキーなら、実際に押すキー（`Fn+5` など）。
pub fn fn_physical(key: &str) -> Option<&'static str> {
    FN_KEYS.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|&(_, p)| p)
}

/// 札に出す名前。
fn display_name(key: &str) -> &str {
    match key {
        "Left" => "←",
        "Right" => "→",
        "Up" => "↑",
        "Down" => "↓",
        "PageUp" => "PgUp",
        "PageDown" => "PgDn",
        "Delete" => "Del",
        "Insert" => "Ins",
        "Backspace" => "BkSp",
        k => k,
    }
}

/// 札の色（モックの `.chip` / `.chip.fn`）。
const CHIP_BG: Color = Color::hex(0x3a3d41);
const CHIP_BORDER: Color = Color::hex(0x555555);
const CHIP_BORDER_FN: Color = Color::hex(0xb8860b);
const CHIP_FG: Color = Color::hex(0xe8e8e8);
/// 一致した部分の色（モックの `.hit`）。
const HIT_YELLOW: Color = Color::hex(0xffd479);
/// 検索欄の地の色。
const SEARCH_BG: Color = Color::hex(0x3c3c3c);
/// スクロールバーの色。
const BAR: Color = Color::hex(0x4f4f4f);

/// 検索欄の高さ（上下の余白 1 を含む）。
const SEARCH_H: u16 = 15;
/// 行・見出しの高さ。
const ROW_H: u16 = 11;
/// 札の高さ。
const CHIP_H: u16 = 9;
/// 右端の余白（スクロールバーの分を含む）。
const PAD_R: i32 = 6;
/// 既定で見えている行数（まだ描いていないときの `page` の幅）。
const DEFAULT_VISIBLE: usize = 11;

/// 並べるもの 1 つ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Header(&'static str),
    /// `hit` は操作名の中の一致した区間（バイト位置）。
    Row { orig: usize, hit: Option<(usize, usize)> },
}

/// 画面の 1 行の中身（前回描いたものと比べる）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotSig {
    Blank,
    /// 一致する操作が無いときの文。
    Empty,
    Header(&'static str),
    Row { orig: usize, hit: Option<(usize, usize)>, sel: bool },
}

/// 前回描いたもの。
struct Drawn {
    area: Rect,
    query: String,
    slots: Vec<SlotSig>,
    bar: Option<(i32, u16)>,
}

/// ショートカット一覧。
pub struct ShortcutList {
    rows: Vec<ShortcutRow>,
    query: String,
    items: Vec<Item>,
    /// `items` の中の、選べる行（`Item::Row`）の位置。
    row_idx: Vec<usize>,
    /// 選んでいる行（`row_idx` の何番目か）。
    sel_pos: usize,
    /// 画面の先頭に出す `items` の位置。
    scroll: usize,
    /// 前回の描画で見えていた行数。
    visible: usize,
    dirty: bool,
    drawn: Option<Drawn>,
}

impl ShortcutList {
    pub fn new(rows: Vec<ShortcutRow>) -> ShortcutList {
        let mut l = ShortcutList {
            rows,
            query: String::new(),
            items: Vec::new(),
            row_idx: Vec::new(),
            sel_pos: 0,
            scroll: 0,
            visible: DEFAULT_VISIBLE,
            dirty: true,
            drawn: None,
        };
        l.refilter();
        l
    }

    /// 打った文字。
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.refilter();
    }

    /// 1 文字消す。もともと空なら `false`。
    pub fn backspace(&mut self) -> bool {
        if self.query.pop().is_none() {
            return false;
        }
        self.refilter();
        true
    }

    /// 選択を `delta` 行ずらす（見出しは飛ばす。端で止まる）。
    pub fn move_sel(&mut self, delta: i32) {
        if self.row_idx.is_empty() {
            return;
        }
        let max = self.row_idx.len() as i64 - 1;
        self.sel_pos = (self.sel_pos as i64 + delta as i64).clamp(0, max) as usize;
    }

    /// 1 画面ぶん動く。
    pub fn page(&mut self, down: bool) {
        let n = self.visible.saturating_sub(1).max(1) as i32;
        self.move_sel(if down { n } else { -n });
    }

    /// 選んでいる操作の、渡した `rows` の中の番号。一致が無ければ `None`。
    pub fn selected(&self) -> Option<usize> {
        match self.items.get(*self.row_idx.get(self.sel_pos)?)? {
            Item::Row { orig, .. } => Some(*orig),
            Item::Header(_) => None,
        }
    }

    /// ステータスバーの左（操作の案内）。
    pub fn status_left(&self) -> String {
        if self.query.trim().is_empty() {
            "↑↓ 選択  Enter 実行  Esc 閉じる".into()
        } else {
            format!("「{}」{} 件  BkSp 消す  Esc 閉じる", self.query.trim(), self.row_idx.len())
        }
    }

    /// ステータスバーの右（何番目 / 全部で）。
    pub fn status_right(&self) -> String {
        if self.row_idx.is_empty() {
            "0/0".into()
        } else {
            format!("{}/{}", self.sel_pos + 1, self.row_idx.len())
        }
    }

    /// 絞り込みを組み直す。選択は先頭に戻す。
    fn refilter(&mut self) {
        self.items.clear();
        self.row_idx.clear();
        self.sel_pos = 0;
        self.scroll = 0;
        let q = self.query.trim().to_ascii_lowercase();
        let toks: Vec<&str> = q.split([' ', '+']).filter(|t| !t.is_empty()).collect();
        let mut last_cat: Option<&'static str> = None;
        for (i, r) in self.rows.iter().enumerate() {
            let hit = if q.is_empty() {
                Some(None)
            } else if let Some(s) = r.title.to_ascii_lowercase().find(&q) {
                Some(Some((s, s + q.len())))
            } else if keys_match(r.keys, &toks) {
                Some(None)
            } else {
                None
            };
            let Some(hit) = hit else { continue };
            if last_cat != Some(r.category) {
                last_cat = Some(r.category);
                self.items.push(Item::Header(r.category));
            }
            self.row_idx.push(self.items.len());
            self.items.push(Item::Row { orig: i, hit });
        }
    }
}

/// キーの書き方を `+` で分ける。
fn key_parts(keys: &str) -> Vec<&str> {
    keys.split('+').filter(|p| !p.is_empty()).collect()
}

/// 打った語（小文字）がキーの部品に当たるか。1 文字なら完全一致、2 文字以上は前方一致。
fn tok_matches(part: &str, tok: &str) -> bool {
    let names = [part.to_ascii_lowercase(), display_name(part).to_ascii_lowercase()];
    names.iter().any(|n| n == tok || (tok.chars().count() >= 2 && n.starts_with(tok)))
}

/// 打った語がすべて、キーのどれかの部品に当たるか。
fn keys_match(keys: &str, toks: &[&str]) -> bool {
    let parts = key_parts(keys);
    !parts.is_empty() && !toks.is_empty() && toks.iter().all(|t| parts.iter().any(|p| tok_matches(p, t)))
}

/// 矢印の札か（7px の等幅では矢印の字形が潰れて読めないので、画素で描く）。
fn is_arrow(text: &str) -> bool {
    matches!(text, "←" | "→" | "↑" | "↓")
}

/// 札 1 つの幅（枠と余白を含む）。
fn chip_w(cx: &mut RenderCx<'_>, text: &str, ts: &TextStyle) -> i32 {
    match text {
        "←" | "→" => 12,
        "↑" | "↓" => 11,
        _ => cx.fonts.measure(text, ts) + 6,
    }
}

/// 矢印を札の中に描く（塗りつぶした三角と軸）。`x`・`y` は札の左上。
fn draw_arrow(cx: &mut RenderCx<'_>, x: i32, y: i32, dir: &str) {
    let c = CHIP_FG;
    let cy = y + 4;
    match dir {
        "←" | "→" => {
            let left = dir == "←";
            let ox = x + 3;
            for i in 0..3 {
                let hx = if left { ox + i } else { ox + 5 - i };
                cx.canvas.vline(hx, cy - i, 1 + 2 * i as u16, c);
            }
            cx.canvas.hline(if left { ox + 3 } else { ox }, cy, 3, c);
        }
        _ => {
            let up = dir == "↑";
            let (mx, oy) = (x + 5, y + 2);
            for i in 0..3 {
                let hy = if up { oy + i } else { oy + 5 - i };
                cx.canvas.hline(mx - i, hy, 1 + 2 * i as u16, c);
            }
            cx.canvas.vline(mx, if up { oy + 3 } else { oy }, 3, c);
        }
    }
}

/// 札を描く。
fn draw_chip(cx: &mut RenderCx<'_>, x: i32, y: i32, text: &str, ts: &TextStyle, fn_layer: bool) -> i32 {
    let w = chip_w(cx, text, ts);
    let r = Rect::new(x as i16, y as i16, w as u16, CHIP_H);
    cx.canvas.fill_rect(r, CHIP_BG);
    cx.canvas.stroke_rect(r, if fn_layer { CHIP_BORDER_FN } else { CHIP_BORDER });
    if is_arrow(text) {
        draw_arrow(cx, x, y, text);
    } else {
        let inner = Rect::new((x + 3) as i16, y as i16, (w - 6).max(0) as u16, CHIP_H);
        cx.text_in(inner, text, ts, CHIP_FG, CHIP_BG);
    }
    w
}

/// 検索欄を描く。
fn draw_search(cx: &mut RenderCx<'_>, area: Rect, query: &str) {
    let ts = TextStyle::mono(UI_PX);
    let sr = Rect::new(area.x, area.y, area.w, SEARCH_H.min(area.h)).inset(4, 1);
    cx.canvas.fill_rect(Rect::new(area.x, area.y, area.w, SEARCH_H.min(area.h)), palette::BG);
    cx.canvas.fill_rect(sr, SEARCH_BG);
    if query.is_empty() {
        cx.canvas.stroke_rect(sr, SEARCH_BG);
        cx.text_in(sr.inset(3, 0), "文字を打つと絞り込み…", &ts, palette::DIM, SEARCH_BG);
    } else {
        cx.canvas.stroke_rect(sr, palette::STATUS);
        let x = cx.text_in(sr.inset(3, 0), query, &ts, palette::FG, SEARCH_BG);
        cx.canvas.vline(x, sr.y as i32 + 2, sr.h.saturating_sub(4), palette::CURSOR);
    }
}

/// 画面の 1 行を描く。
fn draw_slot(cx: &mut RenderCx<'_>, rows: &[ShortcutRow], r: Rect, sig: SlotSig) {
    let sel = matches!(sig, SlotSig::Row { sel: true, .. });
    let bg = if sel { palette::LIST_FOCUS } else { palette::BG };
    cx.canvas.fill_rect(r, bg);
    let sans8 = TextStyle::sans(8.0);
    match sig {
        SlotSig::Blank => {}
        SlotSig::Empty => {
            cx.text_in(Rect::new(r.x + 8, r.y, r.w.saturating_sub(8), r.h), "一致する操作がありません", &sans8, palette::DIM, bg);
        }
        SlotSig::Header(name) => {
            let ts = TextStyle::sans(7.0);
            cx.text_in(Rect::new(r.x + 4, r.y, r.w.saturating_sub(4), r.h), name, &ts, palette::STATUS, bg);
        }
        SlotSig::Row { orig, hit, .. } => {
            let Some(row) = rows.get(orig) else { return };
            draw_row(cx, row, r, hit, bg, &sans8);
        }
    }
}

/// 操作の行: 左に名前、右にキーの札（Fn のキーは黄色い枠で、左に実際の押し方を薄く添える）。
fn draw_row(cx: &mut RenderCx<'_>, row: &ShortcutRow, r: Rect, hit: Option<(usize, usize)>, bg: Color, sans8: &TextStyle) {
    let small = TextStyle::mono(7.0);
    let parts = key_parts(row.keys);
    let chips: Vec<(&str, bool)> = parts.iter().map(|p| (display_name(p), fn_physical(p).is_some())).collect();
    let widths: Vec<i32> = chips.iter().map(|(t, _)| chip_w(cx, t, &small) + 2).collect();
    let chips_w: i32 = widths.iter().sum();
    let chips_x = r.right() - PAD_R - chips_w;
    // Fn の押し方（Fn+5 など）。名前が欠けるなら省く
    let mut hint: Vec<&str> = Vec::new();
    for p in &parts {
        if let Some(ph) = fn_physical(p) {
            hint.push(ph);
        }
    }
    let hint = hint.join(" ");
    let hint_w = if hint.is_empty() { 0 } else { cx.fonts.measure(&hint, &small) + 4 };
    let name_x = r.x as i32 + 8;
    let title_w = cx.fonts.measure(row.title, sans8);
    let show_hint = hint_w > 0 && name_x + title_w + 4 <= chips_x - hint_w;
    let name_end = if show_hint { chips_x - hint_w } else { chips_x } - 4;
    // 名前（一致した区間は黄色）
    let clip = Rect::new(name_x as i16, r.y, (name_end - name_x).max(0) as u16, r.h);
    let base = r.y as i32 + cx.baseline_in(sans8, r.h);
    let (s, e) = match hit {
        Some((s, e)) if s < e && e <= row.title.len() && row.title.is_char_boundary(s) && row.title.is_char_boundary(e) => (s, e),
        _ => (0, 0),
    };
    let mut x = cx.text(name_x, base, &row.title[..s], sans8, palette::FG, bg, clip);
    x = cx.text(x, base, &row.title[s..e], sans8, HIT_YELLOW, bg, clip);
    cx.text(x, base, &row.title[e..], sans8, palette::FG, bg, clip);
    if show_hint {
        let hr = Rect::new((chips_x - hint_w) as i16, r.y, hint_w as u16, r.h);
        cx.text_in(hr, &hint, &small, palette::DIM, bg);
    }
    let mut x = chips_x + 2;
    for ((text, fn_layer), w) in chips.iter().zip(&widths) {
        draw_chip(cx, x, r.y as i32 + 1, text, &small, *fn_layer);
        x += w;
    }
}

impl Widget for ShortcutList {
    fn render(&mut self, cx: &mut RenderCx<'_>, area: Rect) -> Damage {
        if area.is_empty() {
            return Damage::none();
        }
        let list = Rect::new(area.x, area.y + SEARCH_H.min(area.h) as i16, area.w, area.h.saturating_sub(SEARCH_H));
        let visible = (list.h / ROW_H).max(1) as usize;
        self.visible = visible;
        // 選んだ行（と、その見出し）が見えるようにずらす
        let sel_item = self.row_idx.get(self.sel_pos).copied().unwrap_or(0);
        if sel_item < self.scroll {
            self.scroll = sel_item;
        } else if sel_item >= self.scroll + visible {
            self.scroll = sel_item + 1 - visible;
        }
        if sel_item == self.scroll && sel_item > 0 && matches!(self.items[sel_item - 1], Item::Header(_)) {
            self.scroll = sel_item - 1;
        }
        self.scroll = self.scroll.min(self.items.len().saturating_sub(visible));
        // 画面の行ごとの中身
        let sigs: Vec<SlotSig> = (0..visible)
            .map(|k| match self.items.get(self.scroll + k) {
                Some(Item::Header(c)) => SlotSig::Header(c),
                Some(&Item::Row { orig, hit }) => SlotSig::Row { orig, hit, sel: self.scroll + k == sel_item },
                None if self.items.is_empty() && k == 0 => SlotSig::Empty,
                None => SlotSig::Blank,
            })
            .collect();
        // 下に続きがあれば、右端に細いつまみ
        let bar = (self.items.len() > visible).then(|| {
            let n = self.items.len();
            let h = (list.h as usize * visible / n).max(4).min(list.h as usize) as u16;
            let y = list.y as i32 + (list.h - h) as i32 * self.scroll as i32 / (n - visible) as i32;
            (y, h)
        });
        let draw_bar = |cx: &mut RenderCx<'_>| {
            if let Some((y, h)) = bar {
                cx.canvas.fill_rect(Rect::new((list.right() - 3) as i16, y as i16, 3, h), BAR);
            }
        };
        let slot_rect = |k: usize| Rect::new(list.x, list.y + (k as u16 * ROW_H) as i16, list.w, ROW_H);

        let full = self.dirty || self.drawn.as_ref().is_none_or(|d| d.area != area || d.slots.len() != visible);
        if full {
            self.dirty = false;
            cx.canvas.fill_rect(area, palette::BG);
            draw_search(cx, area, &self.query);
            for (k, &sig) in sigs.iter().enumerate() {
                draw_slot(cx, &self.rows, slot_rect(k), sig);
            }
            draw_bar(cx);
            self.drawn = Some(Drawn { area, query: self.query.clone(), slots: sigs, bar });
            return Damage::rect(area);
        }
        let Some(d) = self.drawn.as_mut() else { return Damage::none() };
        let mut dmg = Damage::none();
        if d.query != self.query {
            draw_search(cx, area, &self.query);
            dmg.add(Rect::new(area.x, area.y, area.w, SEARCH_H.min(area.h)).rows(), Priority::Cursor);
            d.query.clone_from(&self.query);
        }
        let force = d.bar != bar;
        let mut changed = false;
        for (k, &sig) in sigs.iter().enumerate() {
            if force || d.slots[k] != sig {
                let r = slot_rect(k);
                draw_slot(cx, &self.rows, r, sig);
                dmg.add(r.rows(), Priority::Normal);
                d.slots[k] = sig;
                changed = true;
            }
        }
        if changed {
            draw_bar(cx);
        }
        d.bar = bar;
        dmg
    }

    fn invalidate(&mut self) {
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::{Canvas, Theme};
    use zc_font::{FontConfig, Fonts, RasterMode};

    fn fonts() -> Option<Fonts> {
        Fonts::load(&FontConfig::default(), RasterMode::Inline).ok()
    }

    fn r(category: &'static str, title: &'static str, keys: &'static str) -> ShortcutRow {
        ShortcutRow { category, title, keys }
    }

    /// モック 5・6 に近い行（一部。実際の `BINDINGS` の形式）。
    fn mock_rows() -> Vec<ShortcutRow> {
        vec![
            r("ファイル", "保存", "Ctrl+S"),
            r("ファイル", "新規ファイル", "Ctrl+N"),
            r("ファイル", "タブを閉じる", "Ctrl+W"),
            r("ファイル", "次のタブ", "Ctrl+Tab"),
            r("ファイル", "前のタブ", "Ctrl+Shift+Tab"),
            r("ファイル", "zcode を終了", "Ctrl+Q"),
            r("編集", "元に戻す", "Ctrl+Z"),
            r("編集", "すべて選択", "Ctrl+A"),
            r("編集", "文字単位で選択", "Shift+Left"),
            r("編集", "行単位で選択", "Shift+Up"),
            r("編集", "選択範囲をコピー", "Ctrl+C"),
            r("編集", "選択範囲を切り取り", "Ctrl+X"),
            r("編集", "選択範囲をインデント", "Tab"),
            r("編集", "行へ移動", "Ctrl+G"),
            r("表示", "ツリー開閉", "Ctrl+B"),
            r("表示", "ツリーで選択中のファイルを開く", "Enter"),
            r("実行", "保存して実行", "F5"),
            r("実行", "実行を止める（ターミナル内）", "Ctrl+C"),
            r("実行", "ターミナル開閉", "F12"),
            r("実行", "ターミナル全画面", "F11"),
            r("実行", "キー無しの操作", ""),
        ]
    }

    fn titles(l: &ShortcutList) -> Vec<&'static str> {
        l.row_idx.iter().map(|&i| match l.items[i] {
            Item::Row { orig, .. } => l.rows[orig].title,
            Item::Header(_) => unreachable!(),
        }).collect()
    }

    fn headers(l: &ShortcutList) -> Vec<&'static str> {
        l.items.iter().filter_map(|i| if let Item::Header(c) = i { Some(*c) } else { None }).collect()
    }

    #[test]
    fn empty_query_shows_everything() {
        let l = ShortcutList::new(mock_rows());
        assert_eq!(l.row_idx.len(), 21);
        assert_eq!(headers(&l), ["ファイル", "編集", "表示", "実行"]);
        assert_eq!(l.selected(), Some(0));
        assert_eq!(l.status_left(), "↑↓ 選択  Enter 実行  Esc 閉じる");
        assert_eq!(l.status_right(), "1/21");
    }

    #[test]
    fn japanese_substring_filters_and_drops_empty_categories() {
        let mut l = ShortcutList::new(mock_rows());
        for c in "選択".chars() {
            l.push_char(c);
        }
        assert_eq!(titles(&l), ["すべて選択", "文字単位で選択", "行単位で選択", "選択範囲をコピー", "選択範囲を切り取り", "選択範囲をインデント", "ツリーで選択中のファイルを開く"]);
        assert_eq!(headers(&l), ["編集", "表示"]);
        assert_eq!(l.status_left(), "「選択」7 件  BkSp 消す  Esc 閉じる");
        assert_eq!(l.status_right(), "1/7");
        // 一致区間（バイト位置）
        if let Item::Row { hit, .. } = l.items[1] {
            assert_eq!(hit, Some((9, 15)));
        } else {
            panic!();
        }
    }

    #[test]
    fn key_names_filter() {
        let mut l = ShortcutList::new(mock_rows());
        for c in "ctrl s".chars() {
            l.push_char(c);
        }
        // Ctrl+S だけ（Ctrl+Shift+Tab の Shift の s は 1 文字なので当たらない）
        assert_eq!(titles(&l), ["保存"]);
        assert_eq!(l.selected(), Some(0));
        // 大文字小文字・「+」区切り
        let mut l = ShortcutList::new(mock_rows());
        for c in "CTRL+shift".chars() {
            l.push_char(c);
        }
        assert_eq!(titles(&l), ["前のタブ"]);
        // 表示名（←）や別名で引ける
        let mut l = ShortcutList::new(mock_rows());
        for c in "shift up".chars() {
            l.push_char(c);
        }
        assert_eq!(titles(&l), ["行単位で選択"]);
        let mut l = ShortcutList::new(mock_rows());
        l.push_char('f');
        l.push_char('1');
        l.push_char('2');
        assert_eq!(titles(&l), ["ターミナル開閉"]);
    }

    #[test]
    fn no_match_and_backspace_back_to_all() {
        let mut l = ShortcutList::new(mock_rows());
        for c in "zzzz".chars() {
            l.push_char(c);
        }
        assert!(l.row_idx.is_empty());
        assert!(l.items.is_empty());
        assert_eq!(l.selected(), None);
        assert_eq!(l.status_right(), "0/0");
        l.move_sel(3); // 何も起きない
        for _ in 0..4 {
            assert!(l.backspace());
        }
        assert!(!l.backspace(), "空のときは false");
        assert_eq!(l.row_idx.len(), 21);
        assert_eq!(l.query(), "");
    }

    #[test]
    fn selection_skips_headers_and_clamps() {
        let mut l = ShortcutList::new(mock_rows());
        l.move_sel(-1);
        assert_eq!(l.selected(), Some(0));
        l.move_sel(5); // 「zcode を終了」
        assert_eq!(l.selected(), Some(5));
        l.move_sel(1); // 見出し「編集」を飛ばして「元に戻す」
        assert_eq!(l.selected(), Some(6));
        l.move_sel(-1);
        assert_eq!(l.selected(), Some(5));
        l.move_sel(1000);
        assert_eq!(l.selected(), Some(20));
        assert_eq!(l.status_right(), "21/21");
        l.page(false);
        assert_eq!(l.selected(), Some(20 - (DEFAULT_VISIBLE - 1)));
    }

    #[test]
    fn selected_maps_to_original_index_after_filtering() {
        let mut l = ShortcutList::new(mock_rows());
        for c in "ターミナル".chars() {
            l.push_char(c);
        }
        assert_eq!(l.selected(), Some(17));
        l.move_sel(1);
        assert_eq!(l.selected(), Some(18));
        l.move_sel(5);
        assert_eq!(l.selected(), Some(19));
        // キー無しの行も一覧に出て、選べる
        let mut l = ShortcutList::new(mock_rows());
        for c in "キー無し".chars() {
            l.push_char(c);
        }
        assert_eq!(l.selected(), Some(20));
    }

    #[test]
    fn fn_keys_map_to_physical() {
        assert_eq!(fn_physical("F5"), Some("Fn+5"));
        assert_eq!(fn_physical("F11"), Some("Fn+O"));
        assert_eq!(fn_physical("F12"), Some("Fn+P"));
        assert_eq!(fn_physical("Left"), Some("Fn+Z"));
        assert_eq!(fn_physical("Ctrl"), None);
    }

    #[test]
    fn incremental_damage() {
        let Some(mut fonts) = fonts() else { return };
        let (mut canvas, theme) = (Canvas::new(), Theme::dark_plus());
        let mut cx = RenderCx::new(&mut canvas, &mut fonts, &theme);
        let area = Rect::new(0, 13, 320, 146);
        let mut l = ShortcutList::new(mock_rows());
        assert!(!l.render(&mut cx, area).is_empty());
        assert!(l.render(&mut cx, area).is_empty(), "変わらなければ描かない");
        // 選択を 1 つ動かすと、2 行（旧と新）だけ
        l.move_sel(1);
        let d = l.render(&mut cx, area);
        assert!(!d.is_empty());
        assert!(d.bounds().len() <= 2 * ROW_H, "{:?}", d.bounds());
        // 1 文字打つと全面ではなく、検索欄と変わった行
        l.push_char('保');
        assert!(!l.render(&mut cx, area).is_empty());
        assert!(l.render(&mut cx, area).is_empty());
        // invalidate で全部
        l.invalidate();
        assert_eq!(l.render(&mut cx, area), Damage::rect(area));
    }

    /// モック 5・6 と同じ状態を描いて PNG に出す（目で見て比べる用）。
    #[test]
    fn render_mock5_6_png() {
        let Some(mut fonts) = fonts() else { return };
        let theme = Theme::dark_plus();
        let dir = std::env::var("ZCODE_SHORTCUTS_PNG_DIR").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../out").to_string());
        let _ = std::fs::create_dir_all(&dir);
        let area = Rect::new(0, 13, 320, 146);
        let mut l = ShortcutList::new(mock_rows());
        for (name, query) in [("shortcuts-mock5.png", ""), ("shortcuts-mock6.png", "選択"), ("shortcuts-mock6-keys.png", "ctrl s")] {
            for c in query.chars() {
                l.push_char(c);
            }
            let mut canvas = Canvas::new();
            let mut cx = RenderCx::new(&mut canvas, &mut fonts, &theme);
            cx.canvas.fill_rect(Rect::SCREEN, palette::BG);
            // タブバーとステータスバー（アプリ側が描くものの代わり）
            let ts = TextStyle::mono(UI_PX);
            cx.canvas.fill_rect(Rect::new(0, 0, 320, 13), palette::TABBAR);
            cx.text_in(Rect::new(8, 0, 120, 13), "⌨ ショートカット", &ts, palette::FG, palette::TABBAR);
            let st = Rect::new(0, 159, 320, 11);
            cx.canvas.fill_rect(st, palette::STATUS);
            let small = TextStyle::mono(8.0);
            cx.text_in(Rect::new(4, 159, 250, 11), &l.status_left(), &small, Color::WHITE, palette::STATUS);
            cx.text_right(Rect::new(0, 159, 316, 11), &l.status_right(), &small, Color::WHITE, palette::STATUS);
            l.invalidate();
            l.render(&mut cx, area);
            zc_display::png::write_png(cx.canvas, format!("{dir}/{name}")).unwrap();
            while l.backspace() {}
        }
    }
}
