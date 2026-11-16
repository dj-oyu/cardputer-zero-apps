//! フォント比較画面（`zcode --fonttest`）。同じ見本を大きさ・書体を変えて並べ、
//! 実機の液晶で読みやすさを見比べる（design.md §5.4、§11.1 段階 1）。
//!
//! | ページ | 中身 |
//! |---|---|
//! | 1 | DejaVu Sans Mono 8〜12px を 2 行ずつ並べる（CJK は Noto Sans Mono CJK JP） |
//! | 2〜4 | エディタ風の画面を 9px / 行 11、10px / 行 12、11px / 行 13 で |
//! | 5 | プロポーショナル（Noto Sans CJK JP）と太字・斜体（Markdown 用） |
//! | 6 | ビットマップフォント（`--bdf <パス>` を渡したときだけ） |
//!
//! 操作: Space / Enter / → で次、← / BackSpace で前、数字キーでそのページ、
//! Esc か Ctrl+Q で終了。`--auto <ms>` で自動的に切り替わる。無操作が続くと終わる。

use crate::app::{AppEvent, LoopCx, Screen, TimerId};
use crate::sample::{self, CODE};
use std::time::Duration;
use zc_core::{palette, Color, Damage, Key, KeyEvent, Rect, TokenKind};
use zc_font::{Face, Size, TextStyle};
use zc_input::{Keyboard, RawRecord};
use zc_ui::layout::{STATUS_H, TAB_H};
use zc_ui::{RenderCx, StatusBar, TitleBar, Widget};

/// エディタ風のページの (文字の大きさ, 行の高さ)。
const EDITOR_PAGES: [(f32, u16); 3] = [(9.0, 11), (10.0, 12), (11.0, 13)];

/// フォント比較画面。
pub struct FontTest {
    page: usize,
    pages: usize,
    has_bitmap: bool,
    auto: Option<Duration>,
    idle: Duration,
    title: TitleBar,
    status: StatusBar,
    dirty: bool,
    info: String,
}

impl FontTest {
    /// `auto`: ページを自動で切り替える間隔。`idle`: 無操作で終了するまで。
    /// `has_bitmap`: BDF を読み込んであるか。
    pub fn new(auto: Option<Duration>, idle: Duration, has_bitmap: bool) -> FontTest {
        FontTest {
            page: 0,
            pages: 5 + has_bitmap as usize,
            has_bitmap,
            auto,
            idle,
            title: TitleBar::new("fonttest", "Space 次  ← 前  Esc 終了"),
            status: StatusBar::new(),
            dirty: true,
            info: String::new(),
        }
    }

    pub fn page_count(&self) -> usize {
        self.pages
    }

    /// ページを選ぶ（0 始まり。範囲外は折り返す）。
    pub fn set_page(&mut self, page: usize) {
        self.page = page % self.pages;
        self.dirty = true;
    }

    fn body(&mut self, cx: &mut RenderCx<'_>, area: Rect) {
        cx.canvas.fill_rect(area, palette::BG);
        match self.page {
            0 => self.page_sizes(cx, area),
            p @ 1..=3 => {
                let (px, line_h) = EDITOR_PAGES[p - 1];
                let ts = TextStyle::mono(px);
                let (rows, cols) = sample::draw_editor(cx, area, &ts, line_h, 7);
                let cell = cx.metrics(&ts).cell_w;
                self.info = format!("DejaVu Sans Mono {px}px 行{line_h} 字幅{cell} {cols}桁x{rows}行");
            }
            4 => self.page_sans(cx, area),
            _ => self.page_bitmap(cx, area),
        }
    }

    /// ページ 1: 大きさを変えて 2 行ずつ。
    fn page_sizes(&mut self, cx: &mut RenderCx<'_>, area: Rect) {
        let mut y = area.y + 1;
        for px in [8.0f32, 9.0, 10.0, 11.0, 12.0] {
            let ts = TextStyle::mono(px);
            let line_h = px as u16 + 2;
            let label = format!("{px:>2}");
            for (i, line) in [CODE[4], CODE[12]].into_iter().enumerate() {
                let row = Rect::new(area.x, y, area.w, line_h);
                if i == 0 {
                    cx.text_in(Rect::new(area.x + 1, y, 12, line_h), &label, &TextStyle::mono(8.0), palette::DIM, palette::BG);
                }
                // 1 行目は字下げを詰めて、見本を長く見せる
                let text_area = Rect::new(area.x + 14, y, area.w - 14, line_h);
                if i == 0 {
                    let code: sample::Line = &[
                        (TokenKind::Keyword, "def"),
                        (TokenKind::Text, " "),
                        (TokenKind::Function, "main"),
                        (TokenKind::Text, "(): "),
                        (TokenKind::Comment, "# 0.5 秒ごとに点滅。10 回"),
                    ];
                    sample::draw_line(cx, text_area, code, &ts, palette::BG);
                } else {
                    sample::draw_line(cx, text_area, line, &ts, palette::BG);
                }
                let _ = row;
                y += line_h as i16;
            }
        }
        self.info = "DejaVu Sans Mono 8-12px + Noto Sans Mono CJK".into();
    }

    /// ページ 5: プロポーショナルと装飾（Markdown のライブプレビュー用）。
    fn page_sans(&mut self, cx: &mut RenderCx<'_>, area: Rect) {
        let mut y = area.y + 2;
        let x = area.x + 8;
        let w = area.w - 16;
        let mut line = |cx: &mut RenderCx<'_>, s: &str, ts: TextStyle, h: u16, fg: Color| {
            cx.text_in(Rect::new(x, y, w, h), s, &ts, fg, palette::BG);
            y += h as i16;
        };
        line(cx, "買い物と作業メモ Heading 13px", TextStyle::sans(13.0).bold(), 17, Color::WHITE);
        line(cx, "見出し 2 / Heading 10.5px 太字", TextStyle::sans(10.5).bold(), 15, Color::WHITE);
        line(cx, "本文 8.5px: CMA は 64MB しかない。カメラと同時に使うときは注意", TextStyle::sans(8.5), 12, palette::FG);
        line(cx, "本文 9px: The quick brown fox jumps over the lazy dog. 0123456789", TextStyle::sans(9.0), 12, palette::FG);
        line(cx, "本文 10px: 基板を発注する。ケースを 3D 印刷する", TextStyle::sans(10.0), 13, palette::FG);
        line(cx, "太字 10px: 基板を発注する Bold 0123", TextStyle::sans(10.0).bold(), 13, Color::WHITE);
        line(cx, "斜体 10px: 字形を傾けて合成する Italic 0123", TextStyle::sans(10.0).italic(), 13, palette::FG);
        line(cx, "等幅の斜体 9px: for i in range(10): # 傾ける", TextStyle::mono(9.0).italic(), 12, palette::COMMENT);
        line(cx, "等幅の太字 9px: def main(): 太字 0O1lI", TextStyle::mono(9.0).bold(), 12, palette::KEYWORD);
        line(cx, "記号 7.5px: - [ ] ** ` > # (カーソル行で薄く見せる)", TextStyle::mono(7.5), 11, palette::DIM);
        self.info = "Noto Sans CJK JP (プロポーショナル) / 太字 / 斜体".into();
    }

    /// ページ 6: ビットマップフォント。
    fn page_bitmap(&mut self, cx: &mut RenderCx<'_>, area: Rect) {
        let ts = TextStyle { face: Face::Bitmap, size: Size::px(12.0), bold: false, italic: false };
        let m = cx.metrics(&ts);
        let line_h = m.height() + 1;
        let (rows, cols) = sample::draw_editor(cx, area, &ts, line_h, 7);
        self.info = format!("BDF 高さ{} 字幅{} {cols}桁x{rows}行", m.height(), m.cell_w);
    }
}

impl Screen for FontTest {
    fn start(&mut self, cx: &mut LoopCx<'_>) {
        cx.timers.set_after(TimerId::Idle, self.idle);
        if let Some(d) = self.auto {
            cx.timers.set_after(TimerId::Step, d);
        }
    }

    fn on_keys(&mut self, keys: &[KeyEvent], _raw: &[RawRecord], _kb: &Keyboard, cx: &mut LoopCx<'_>) {
        cx.timers.set_after(TimerId::Idle, self.idle);
        for k in keys {
            match k.key {
                Key::Esc => cx.quit = true,
                Key::Char('q') if k.mods.ctrl() => cx.quit = true,
                Key::Left | Key::Backspace | Key::Up | Key::PageUp => self.set_page(self.page + self.pages - 1),
                Key::Char(c @ '1'..='9') => self.set_page(c as usize - '1' as usize),
                // 画面を上書きされたときの描き直し
                Key::Char('r') => cx.force_present = true,
                _ => self.set_page(self.page + 1),
            }
        }
    }

    fn on_event(&mut self, _ev: AppEvent, _cx: &mut LoopCx<'_>) {}

    fn on_timer(&mut self, id: TimerId, cx: &mut LoopCx<'_>) {
        match id {
            TimerId::Idle => cx.quit = true,
            TimerId::Step => {
                self.set_page(self.page + 1);
                if let Some(d) = self.auto {
                    cx.timers.set_after(TimerId::Step, d);
                }
            }
            _ => {}
        }
    }

    fn on_glyphs_ready(&mut self) {
        self.dirty = true;
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) -> Damage {
        if !self.dirty {
            return Damage::none();
        }
        self.dirty = false;
        let (rest, status_area) = Rect::SCREEN.split_bottom(STATUS_H);
        let (title_area, body_area) = rest.split_top(TAB_H);
        let mut dmg = self.title.render(cx, title_area);
        self.body(cx, body_area);
        dmg |= Damage::rect(body_area);
        self.status.set_left(&self.info);
        let page = format!("{}/{}{}", self.page + 1, self.pages, if self.has_bitmap { " BDF" } else { "" });
        self.status.set_right(&page);
        dmg |= self.status.render(cx, status_area);
        dmg
    }
}
