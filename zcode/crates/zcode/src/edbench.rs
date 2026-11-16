//! エディタ画面の計測（`zcode --bench editor-type` / `editor-scroll`）。
//!
//! キーを押さずに、タイマーで打鍵を作ってエディタ画面（[`EditorScreen`]）に流す。
//! 打鍵 → 編集 → レイアウト → 差分描画 → 行差分 → flush への受け渡しまで、本物と同じ
//! 道を通る。結果は終了時の統計（`--stats`）の「起床 → flush に渡すまで」に出る
//! （design.md §6.1 の「poll から起きて処理・1 行描画・行差分」に当たる）。
//!
//! | 種類 | 1 フレームの中身 | 間隔 |
//! |---|---|---|
//! | `editor-type` | 1 字入力（30 字ごとに改行） | 30ms |
//! | `editor-scroll` | ↓ を 1 回（毎回 1 行スクロールする） | 50ms |
//!
//! 文書は [`SimpleDoc`]、色は見本の手書きのトークン（`zc-buffer` / `zc-highlight` の
//! 時間は含まない）。

use crate::app::{AppEvent, LoopCx, Screen, TimerId};
use crate::doc::{DocBackend, SimpleDoc, TextDoc};
use crate::editor::{EditorConfig, EditorScreen};
use crate::sample;
use std::path::PathBuf;
use std::time::Duration;
use zc_core::{Damage, Key, KeyEvent, Mods, Pos, Span};
use zc_input::{Keyboard, RawRecord};
use zc_ui::RenderCx;

/// 計測の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdBenchKind {
    Type,
    Scroll,
}

impl EdBenchKind {
    pub fn parse(s: &str) -> Option<EdBenchKind> {
        match s {
            "editor-type" => Some(EdBenchKind::Type),
            "editor-scroll" => Some(EdBenchKind::Scroll),
            _ => None,
        }
    }
}

/// 打つ文字（ASCII と日本語を混ぜる）。
const TYPED: &str = "result = compute(value, 42)  # 点滅の回数 ";

/// 計測用の画面。
pub struct EditorBench {
    inner: EditorScreen,
    kind: EdBenchKind,
    frames: u32,
    step: u32,
}

impl EditorBench {
    /// `frames` 回打鍵したら終わる。
    pub fn new(kind: EdBenchKind, frames: u32) -> EditorBench {
        let cfg = EditorConfig { backend: DocBackend::Simple, root: PathBuf::from("/nonexistent"), files: vec![PathBuf::from("bench.py")] };
        let mut inner = EditorScreen::new(cfg);
        // 見本のコードを 40 回繰り返した文書（560 行）
        let n = sample::CODE.len() as u32;
        let mut doc = SimpleDoc::from_text(&sample::text().repeat(40));
        let start = match kind {
            EdBenchKind::Type => Pos::new(7, 23),
            EdBenchKind::Scroll => Pos::new(11, 0),
        };
        doc.set_selection(start, start);
        let spans = move |line: u32, text: &str, out: &mut Vec<Span>| sample::spans(line % n, text, out);
        inner.open_doc(None, "bench.py", Box::new(doc), Some(Box::new(spans)));
        EditorBench { inner, kind, frames, step: 0 }
    }
    fn interval(&self) -> Duration {
        match self.kind {
            EdBenchKind::Type => Duration::from_millis(30),
            EdBenchKind::Scroll => Duration::from_millis(50),
        }
    }
    /// `step` 回目の打鍵。
    fn key(&self) -> KeyEvent {
        match self.kind {
            EdBenchKind::Type if self.step % 30 == 0 => KeyEvent::new(Key::Enter, Mods::NONE),
            EdBenchKind::Type => KeyEvent::char(TYPED.chars().cycle().nth(self.step as usize).unwrap_or('x')),
            EdBenchKind::Scroll => KeyEvent::new(Key::Down, Mods::NONE),
        }
    }
    /// 中のエディタ画面（テスト用）。
    pub fn editor(&self) -> &EditorScreen {
        &self.inner
    }
    /// 打鍵を 1 回流す（テスト用。実行中はタイマーが呼ぶ）。
    pub fn step(&mut self, cx: &mut LoopCx<'_>) {
        self.step += 1;
        let ev = self.key();
        self.inner.keys(&[ev], Mods::NONE, cx);
    }
}

impl Screen for EditorBench {
    fn start(&mut self, cx: &mut LoopCx<'_>) {
        // 最初の画面が出てから 300ms 待って始める
        cx.timers.set_after(TimerId::Step, Duration::from_millis(300));
    }
    fn on_keys(&mut self, keys: &[KeyEvent], _raw: &[RawRecord], _kb: &Keyboard, cx: &mut LoopCx<'_>) {
        if keys.iter().any(|k| k.key == Key::Esc) {
            cx.quit = true;
        }
    }
    fn on_event(&mut self, ev: AppEvent, cx: &mut LoopCx<'_>) {
        self.inner.on_event(ev, cx);
    }
    fn on_timer(&mut self, id: TimerId, cx: &mut LoopCx<'_>) {
        if id != TimerId::Step {
            return self.inner.on_timer(id, cx);
        }
        if self.step >= self.frames {
            cx.quit = true;
            return;
        }
        self.step(cx);
        cx.timers.set_after(TimerId::Step, self.interval());
    }
    fn on_glyphs_ready(&mut self) {
        self.inner.on_glyphs_ready();
    }
    fn render(&mut self, cx: &mut RenderCx<'_>) -> Damage {
        self.inner.render(cx)
    }
}

/// 文書の中身（テスト用）。
pub fn doc_text(doc: &dyn TextDoc) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for i in 0..doc.len_lines() {
        doc.line(i, &mut line);
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&line);
    }
    out
}
