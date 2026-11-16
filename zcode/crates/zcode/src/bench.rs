//! 描画の計測（`zcode --bench <種類>`）。キーを押さずに、打鍵やスクロールに相当する
//! フレームをタイマーで作り、メインの処理時間と flush スレッドへの受け渡しを測る。
//! 結果は終了時の統計（`--stats`）に出る。measurements.md の数字と見比べるためのもの。
//!
//! | 種類 | 1 フレームの中身 | 間隔 |
//! |---|---|---|
//! | `line` | テキスト 1 行（11 画素行）だけを描き変える（1 打鍵に相当） | 30ms |
//! | `line-status` | 1 行 + ステータスバー（離れた 2 か所 → fsync が 2 回に分かれる） | 30ms |
//! | `scroll` | 本文を 1 行ぶんスクロール（ほぼ全画面） | 50ms（リピート 20 回/秒に相当） |
//! | `full` | 全画面を描き直す | 50ms |
//!
//! 異常終了の後始末（[`crate::crash`]）を実機で確かめるための種類もある:
//! `panic` は 10 フレーム目でメインが panic する。`hang` は 10 フレーム目でメインが
//! 固まる（シグナルを送ると 2 秒後に SIGALRM のハンドラが後始末して終わる）。

use crate::app::{AppEvent, LoopCx, Screen, TimerId};
use crate::sample::{self, CODE};
use std::time::Duration;
use zc_core::{palette, Damage, Key, KeyEvent, Priority, Rect};
use zc_font::TextStyle;
use zc_input::{Keyboard, RawRecord};
use zc_ui::layout::{CODE_LINE_H, CODE_PX, STATUS_H, TAB_H};
use zc_ui::{RenderCx, StatusBar, TitleBar, Widget};

/// 計測の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchKind {
    Line,
    LineStatus,
    Scroll,
    Full,
    /// 後始末の試験: メインが panic する。
    Panic,
    /// 後始末の試験: メインが固まる。
    Hang,
}

impl BenchKind {
    pub fn parse(s: &str) -> Option<BenchKind> {
        Some(match s {
            "line" => BenchKind::Line,
            "line-status" => BenchKind::LineStatus,
            "scroll" => BenchKind::Scroll,
            "full" => BenchKind::Full,
            "panic" => BenchKind::Panic,
            "hang" => BenchKind::Hang,
            _ => return None,
        })
    }
    fn interval(self) -> Duration {
        match self {
            BenchKind::Line | BenchKind::LineStatus | BenchKind::Panic | BenchKind::Hang => Duration::from_millis(30),
            BenchKind::Scroll | BenchKind::Full => Duration::from_millis(50),
        }
    }
}

/// 計測用の画面。
pub struct Bench {
    kind: BenchKind,
    frames: u32,
    step: u32,
    pending_step: bool,
    drawn_base: bool,
    title: TitleBar,
    status: StatusBar,
}

impl Bench {
    /// `frames` フレーム描いたら終わる。
    pub fn new(kind: BenchKind, frames: u32) -> Bench {
        Bench {
            kind,
            frames,
            step: 0,
            pending_step: false,
            drawn_base: false,
            title: TitleBar::new("bench", &format!("{kind:?}")),
            status: StatusBar::new(),
        }
    }
}

impl Screen for Bench {
    fn start(&mut self, cx: &mut LoopCx<'_>) {
        // 最初の画面が出てから 300ms 待って始める
        cx.timers.set_after(TimerId::Step, Duration::from_millis(300));
    }

    fn on_keys(&mut self, keys: &[KeyEvent], _raw: &[RawRecord], _kb: &Keyboard, cx: &mut LoopCx<'_>) {
        if keys.iter().any(|k| k.key == Key::Esc) {
            cx.quit = true;
        }
    }

    fn on_event(&mut self, _ev: AppEvent, _cx: &mut LoopCx<'_>) {}

    fn on_timer(&mut self, id: TimerId, cx: &mut LoopCx<'_>) {
        if id != TimerId::Step {
            return;
        }
        if self.step >= self.frames {
            cx.quit = true;
            return;
        }
        self.step += 1;
        if self.step == 10 {
            match self.kind {
                BenchKind::Panic => panic!("後始末の試験（--bench panic）"),
                BenchKind::Hang => loop {
                    std::thread::sleep(Duration::from_secs(3600));
                },
                _ => {}
            }
        }
        self.pending_step = true;
        cx.timers.set_after(TimerId::Step, self.kind.interval());
    }

    fn on_glyphs_ready(&mut self) {
        self.drawn_base = false;
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) -> Damage {
        let ts = TextStyle::mono(CODE_PX);
        let (rest, status_area) = Rect::SCREEN.split_bottom(STATUS_H);
        let (title_area, body) = rest.split_top(TAB_H);
        let mut dmg = Damage::none();
        if !self.drawn_base {
            self.drawn_base = true;
            dmg |= self.title.render(cx, title_area);
            sample::draw_editor(cx, body, &ts, CODE_LINE_H, 7);
            dmg |= Damage::rect(body);
            self.status.set_left("bench");
            self.status.invalidate();
            dmg |= self.status.render(cx, status_area);
        }
        if !std::mem::take(&mut self.pending_step) {
            return dmg;
        }
        let n = self.step as usize;
        match self.kind {
            BenchKind::Line | BenchKind::LineStatus | BenchKind::Panic | BenchKind::Hang => {
                // カーソル行（8 行目）の文字を増やしていく
                let cell = cx.metrics(&ts).cell_w;
                let gutter = cell * 3 + 5;
                let row = Rect::new(body.x, body.y + 7 * CODE_LINE_H as i16, body.w, CODE_LINE_H);
                cx.canvas.fill_rect(Rect::new(row.x + gutter as i16, row.y, row.w - gutter, row.h), palette::LINE);
                let text: String = "        time.sleep(0.5)  # ".chars().chain("あいうえおabcdefghij".chars().cycle().take(n % 20)).collect();
                let end = cx.text_in(Rect::new(row.x + gutter as i16, row.y, row.w - gutter, row.h), &text, &ts, palette::FG, palette::LINE);
                cx.canvas.vline(end, row.y as i32 + 1, row.h - 2, palette::CURSOR);
                dmg |= Damage::rect(row).with_priority(Priority::Cursor);
                if self.kind == BenchKind::LineStatus {
                    self.status.set_right(&format!("Ln 8, Col {}", 28 + n % 20));
                    dmg |= self.status.render(cx, status_area);
                }
            }
            BenchKind::Scroll => {
                // 本文を 1 行ぶん上へずらし、いちばん下に新しい行を描く
                let rows = body.h / CODE_LINE_H;
                let text_area = Rect::new(body.x, body.y, body.w, rows * CODE_LINE_H);
                cx.canvas.scroll_rows(text_area, -(CODE_LINE_H as i32), palette::BG);
                let y = text_area.y + ((rows - 1) * CODE_LINE_H) as i16;
                let cell = cx.metrics(&ts).cell_w;
                let gutter = cell * 3 + 5;
                let num = format!("{:>3}", rows as usize + n);
                cx.text_in(Rect::new(body.x, y, gutter, CODE_LINE_H), &num, &ts, palette::GUTTER, palette::BG);
                let line = CODE[(rows as usize + n - 1) % CODE.len()];
                sample::draw_line(cx, Rect::new(body.x + gutter as i16, y, body.w - gutter, CODE_LINE_H), line, &ts, palette::BG);
                dmg |= Damage::rect(text_area);
            }
            BenchKind::Full => {
                sample::draw_editor(cx, body, &ts, CODE_LINE_H, n % 13);
                dmg |= Damage::rect(body);
            }
        }
        dmg
    }
}
