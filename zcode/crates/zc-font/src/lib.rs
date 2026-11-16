//! フォント（design.md §5.4）。
//!
//! ```text
//! メイン: Fonts（グリフキャッシュを持つ）──draw_text()──▶ Canvas（色の表を引いて合成）
//!            │ キャッシュに無い字
//!            ▼
//! assist: FontFiles（mmap）+ ScaleContext ──ラスタライズ──▶ 字形を返す（最大 2ms だけ待つ）
//! ```
//!
//! - フォントは mmap して、必要な字形だけ取り出す（`swash`）。DejaVu Sans Mono は
//!   ヒンティングあり、CJK はなし
//! - フォールバック: 等幅は DejaVu Sans Mono → Noto Sans Mono CJK JP。全角は半角
//!   2 つぶんの幅になる大きさで描く
//! - 字形は 8bit の濃度でキャッシュ（[`GlyphCache`]、上限 512KB）。メインだけが持つ
//! - mmap に触る処理（[`FontFiles`]）は、実機では assist スレッドで走らせる
//!   （[`RasterMode::Assist`]）。キャッシュに無い字は最大 2ms だけ待ち、間に合わな
//!   ければその字を空白のまま描いて、あとで [`Fonts::take_ready`] が `true` を返す
//!   （呼ぶ側はその行を描き直す）。render-test は [`RasterMode::Inline`]（その場で
//!   作る。結果が時間に依存しない）
//!
//! 段階 2 で詰めること: 本物の太字のフォント（いまは字形を太らせている）、
//! 記号用のフォールバック、行の再描画の範囲を返す仕組み。

pub mod bdf;
pub mod cache;
pub mod files;
pub mod types;

pub use cache::{GlyphCache, DEFAULT_CACHE_BYTES};
pub use files::FontFiles;
pub use types::{is_wide, Face, FontConfig, Glyph, GlyphKey, GlyphSource, Metrics, Size, TextStyle};

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use swash::scale::ScaleContext;
use zc_core::rt::{Notifier, Worker};
use zc_core::{BlendLut, Canvas, Rect};

thread_local! {
    /// ラスタライズ用の作業領域。スレッドごとに 1 つ（assist スレッド、または
    /// Inline のときのメイン）。
    static SCALE_CTX: RefCell<ScaleContext> = RefCell::new(ScaleContext::new());
}

/// キャッシュに無い字形をどこで作るか。
#[derive(Clone)]
pub enum RasterMode {
    /// 呼んだスレッドでその場で作る（render-test・テスト用）。
    Inline,
    /// assist スレッドに頼み、最大 `wait` だけ待つ。
    Assist {
        worker: Arc<Worker>,
        /// 1 字あたりの待ち時間の上限（設計値は 2ms）。1 フレームの合計にも上限が
        /// ある（[`FRAME_WAIT_FACTOR`]）。
        wait: Duration,
        /// 待ち時間を過ぎてから字形ができたときに、メインを起こす通知口。
        notifier: Option<Notifier>,
    },
}

/// 統計（計測・デバッグ用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FontStats {
    /// キャッシュに無くて作った字形の数。
    pub rasterized: u32,
    /// 待ち時間を過ぎて、空白で描いた回数。
    pub late: u32,
    /// キャッシュの字形の数。
    pub cached: u32,
    /// キャッシュのバイト数の目安。
    pub cache_bytes: u32,
}

/// 1 フレームで字形を待つ時間の合計の上限（1 字あたりの待ち時間の何倍か）。
/// 初めての字が多い画面（日本語のファイルを開いた直後など）で、1 字ごとに 2ms ずつ
/// 待って 1 フレームが 100ms を超えるのを防ぐ。上限を使い切ったら、残りの字は待たずに
/// 頼むだけにして空白で描き、届いてから描き直す。
pub const FRAME_WAIT_FACTOR: u32 = 3;

type Reply = (GlyphKey, Glyph);

/// フォント一式とグリフキャッシュ。メインスレッドが持つ。
pub struct Fonts {
    files: Arc<FontFiles>,
    mode: RasterMode,
    cache: GlyphCache,
    metrics: HashMap<(Face, Size), Metrics>,
    pending: HashSet<GlyphKey>,
    tx: mpsc::Sender<Reply>,
    rx: mpsc::Receiver<Reply>,
    warnings: Vec<String>,
    stats: FontStats,
    /// このフレームで、あとどれだけ待ってよいか。
    frame_budget: Duration,
}

impl Fonts {
    /// フォントを開く。`Assist` のときは、開く処理も assist スレッドで行って待つ
    /// （起動時に 1 回だけ。初回は SD からの読み込みで十数 ms かかる）。
    pub fn load(cfg: &FontConfig, mode: RasterMode) -> io::Result<Fonts> {
        let (files, warnings) = match &mode {
            RasterMode::Inline => FontFiles::load(cfg)?,
            RasterMode::Assist { worker, .. } => {
                let (tx, rx) = mpsc::channel();
                let cfg = cfg.clone();
                worker.submit(move || {
                    let _ = tx.send(FontFiles::load(&cfg));
                });
                rx.recv().map_err(|_| io::Error::other("assist スレッドが応答しない"))??
            }
        };
        let (tx, rx) = mpsc::channel();
        Ok(Fonts {
            files: Arc::new(files),
            mode,
            cache: GlyphCache::new(DEFAULT_CACHE_BYTES),
            metrics: HashMap::new(),
            pending: HashSet::new(),
            tx,
            rx,
            warnings,
            stats: FontStats::default(),
            frame_budget: Duration::MAX,
        })
    }

    /// フレームの描き始めに呼ぶ（そのフレームで字形を待つ時間の合計の上限を戻す）。
    /// 呼ばなければ、合計の上限は無い（1 字あたりの上限だけ）。
    pub fn begin_frame(&mut self) {
        if let RasterMode::Assist { wait, .. } = &self.mode {
            self.frame_budget = *wait * FRAME_WAIT_FACTOR;
        }
    }

    /// 読み込みで出た警告（CJK フォントが無い、など）。
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
    /// ビットマップフォント（[`Face::Bitmap`]）が使えるか。
    pub fn has_bitmap(&self) -> bool {
        self.files.has_bitmap()
    }
    pub fn stats(&self) -> FontStats {
        FontStats { cached: self.cache.len() as u32, cache_bytes: self.cache.bytes() as u32, ..self.stats }
    }

    /// 行の寸法。初めての書体・大きさのときだけフォントを読む（`Assist` なら
    /// assist スレッドで計算して、終わるまで待つ）。
    pub fn metrics(&mut self, style: &TextStyle) -> Metrics {
        let k = (style.face, style.size);
        if let Some(m) = self.metrics.get(&k) {
            return *m;
        }
        let m = match &self.mode {
            RasterMode::Inline => self.files.metrics(k.0, k.1),
            RasterMode::Assist { worker, .. } => {
                let (tx, rx) = mpsc::channel();
                let files = self.files.clone();
                worker.submit(move || {
                    let _ = tx.send(files.metrics(k.0, k.1));
                });
                rx.recv().expect("assist スレッドが応答しない")
            }
        };
        self.metrics.insert(k, m);
        m
    }

    /// 字形を用意する（キャッシュに無ければ作る）。`true` ならキャッシュにある。
    /// `false` は「assist に頼んだが待ち時間内に来なかった」。
    fn ensure(&mut self, key: GlyphKey) -> bool {
        if self.cache.contains(&key) {
            return true;
        }
        match &self.mode {
            RasterMode::Inline => {
                let g = SCALE_CTX.with(|c| self.files.rasterize(&mut c.borrow_mut(), key));
                self.stats.rasterized += 1;
                self.cache.insert(key, g);
                true
            }
            RasterMode::Assist { worker, wait, notifier } => {
                let wait = (*wait).min(self.frame_budget);
                if self.pending.insert(key) {
                    let (files, tx, notifier) = (self.files.clone(), self.tx.clone(), notifier.clone());
                    let deadline = Instant::now() + wait;
                    worker.submit(move || {
                        let g = SCALE_CTX.with(|c| files.rasterize(&mut c.borrow_mut(), key));
                        let _ = tx.send((key, g));
                        // 待ち時間を過ぎていたら、メインはもう待っていない。起こして描き直させる
                        if Instant::now() > deadline {
                            if let Some(n) = notifier {
                                n.notify();
                            }
                        }
                    });
                    self.stats.rasterized += 1;
                    let started = Instant::now();
                    let deadline = started + wait;
                    let budget = &mut self.frame_budget;
                    let mut spend = || {
                        if *budget != Duration::MAX {
                            *budget = budget.saturating_sub(started.elapsed());
                        }
                    };
                    loop {
                        let left = deadline.saturating_duration_since(Instant::now());
                        match self.rx.recv_timeout(left) {
                            Ok((k, g)) => {
                                self.pending.remove(&k);
                                self.cache.insert(k, g);
                                if k == key {
                                    spend();
                                    return true;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    spend();
                }
                self.stats.late += 1;
                false
            }
        }
    }

    /// 待ち時間を過ぎてから届いた字形をキャッシュに取り込む。1 つでも取り込んだら
    /// `true`（呼ぶ側は、空白で描いた所を描き直す）。イベントループが eventfd で
    /// 起きたときに呼ぶ。
    pub fn take_ready(&mut self) -> bool {
        let mut any = false;
        while let Ok((k, g)) = self.rx.try_recv() {
            self.pending.remove(&k);
            self.cache.insert(k, g);
            any = true;
        }
        any
    }

    /// まだ届いていない字形があるか。
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// 字形を取り出す。`None` は「まだ無い」（[`Fonts::take_ready`] を待つ）。
    pub fn glyph(&mut self, ch: char, style: &TextStyle) -> Option<&Glyph> {
        let key = GlyphKey { ch, style: *style };
        if self.ensure(key) {
            self.cache.get(&key)
        } else {
            None
        }
    }

    /// 字形がまだ無いときの送り幅の見積もり。
    fn guess_advance(&mut self, ch: char, style: &TextStyle) -> u16 {
        let m = self.metrics(style);
        if is_wide(ch) {
            m.cell_w * 2
        } else {
            m.cell_w
        }
    }

    /// 1 字の送り幅（画素）。
    pub fn advance(&mut self, ch: char, style: &TextStyle) -> u16 {
        match self.glyph(ch, style) {
            Some(g) => g.advance,
            None => self.guess_advance(ch, style),
        }
    }

    /// 文字列の幅（画素）。
    pub fn measure(&mut self, text: &str, style: &TextStyle) -> i32 {
        text.chars().map(|c| self.advance(c, style) as i32).sum()
    }

    /// 1 字を描く。ペン位置 `(x, baseline)` に置き、`clip` で切り取る。送り幅を返す。
    /// 背景は塗らない（呼ぶ側が先に `lut.bg()` で塗っておく）。
    pub fn draw_char(&mut self, canvas: &mut Canvas, x: i32, baseline: i32, ch: char, style: &TextStyle, lut: &BlendLut, clip: Rect) -> u16 {
        let key = GlyphKey { ch, style: *style };
        if !self.ensure(key) {
            return self.guess_advance(ch, style);
        }
        let g = self.cache.get(&key).expect("ensure した字形がキャッシュに無い");
        if g.width > 0 {
            canvas.blit_mask(x + g.left as i32, baseline - g.top as i32, g.width, g.height, &g.data, lut, clip);
        }
        g.advance
    }

    /// 文字列を 1 行描く。描き終わったあとの x を返す。制御文字は描かない。
    pub fn draw_text(&mut self, canvas: &mut Canvas, x: i32, baseline: i32, text: &str, style: &TextStyle, lut: &BlendLut, clip: Rect) -> i32 {
        let mut pen = x;
        let right = clip.right();
        for ch in text.chars() {
            if ch.is_control() {
                continue;
            }
            if pen >= right {
                break;
            }
            pen += self.draw_char(canvas, pen, baseline, ch, style, lut, clip) as i32;
        }
        pen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::rt::WorkerConfig;
    use zc_core::Color;

    fn fonts(mode: RasterMode) -> Option<Fonts> {
        let cfg = FontConfig::default();
        if !cfg.available() {
            eprintln!("フォントが無いので飛ばす");
            return None;
        }
        Some(Fonts::load(&cfg, mode).unwrap())
    }

    #[test]
    fn mono_grid_and_cjk_fallback() {
        let Some(mut f) = fonts(RasterMode::Inline) else { return };
        assert!(f.warnings().is_empty(), "{:?}", f.warnings());
        for (px, cell) in [(9.0, 5), (10.0, 6), (11.0, 7), (12.0, 7)] {
            let st = TextStyle::mono(px);
            assert_eq!(f.metrics(&st).cell_w, cell, "{px}px");
            assert_eq!(f.advance('a', &st), cell);
            assert_eq!(f.advance('漢', &st), cell * 2);
            assert_eq!(f.advance('あ', &st), cell * 2);
            assert_eq!(f.measure("ab漢字", &st), cell as i32 * 6);
        }
        let st = TextStyle::mono(9.0);
        assert_eq!(f.glyph('a', &st).unwrap().source, GlyphSource::Primary);
        assert_eq!(f.glyph('漢', &st).unwrap().source, GlyphSource::Fallback);
        assert_eq!(f.glyph('\u{10FFFD}', &st).unwrap().source, GlyphSource::Missing);
        let s = f.stats();
        assert!(s.cached >= 4 && s.cache_bytes > 0);
    }

    #[test]
    fn draw_text_marks_pixels_and_respects_clip() {
        let Some(mut f) = fonts(RasterMode::Inline) else { return };
        let st = TextStyle::mono(11.0);
        let m = f.metrics(&st);
        let mut c = Canvas::new();
        let lut = BlendLut::new(Color::WHITE, Color::BLACK);
        let clip = Rect::new(0, 0, 60, 20);
        let end = f.draw_text(&mut c, 2, 2 + m.ascent as i32, "Hello, 世界!", &st, &lut, clip);
        assert!(end > 60); // clip の右端で打ち切る
        let lit = |c: &Canvas, r: Rect| {
            let mut n = 0;
            for y in r.y as i32..r.bottom() {
                for x in r.x as i32..r.right() {
                    n += (c.get(x, y) != Some(Color::BLACK)) as u32;
                }
            }
            n
        };
        assert!(lit(&c, clip) > 60);
        assert_eq!(lit(&c, Rect::new(60, 0, 260, 170)), 0);
        assert_eq!(lit(&c, Rect::new(0, 20, 320, 150)), 0);
    }

    #[test]
    fn sans_is_proportional() {
        let Some(mut f) = fonts(RasterMode::Inline) else { return };
        let st = TextStyle::sans(12.0);
        assert!(f.advance('i', &st) < f.advance('W', &st));
        assert_eq!(f.advance('漢', &st), 12);
        // 斜体・太字は別の字形としてキャッシュされる
        let a = f.glyph('a', &st).unwrap().clone();
        let b = f.glyph('a', &st.bold()).unwrap().clone();
        let i = f.glyph('a', &st.italic()).unwrap().clone();
        assert_ne!(a.data, b.data);
        assert_ne!(a.data, i.data);
    }

    #[test]
    fn assist_mode_gives_same_glyphs() {
        let worker = Arc::new(Worker::new(WorkerConfig::ASSIST));
        let mode = RasterMode::Assist { worker: worker.clone(), wait: Duration::from_secs(5), notifier: None };
        let (Some(mut a), Some(mut i)) = (fonts(mode), fonts(RasterMode::Inline)) else { return };
        let st = TextStyle::mono(10.0);
        assert_eq!(a.metrics(&st), i.metrics(&st));
        for ch in "fn main() { 日本語 }".chars() {
            assert_eq!(a.glyph(ch, &st).cloned(), i.glyph(ch, &st).cloned(), "{ch:?}");
        }
        assert!(!a.has_pending());
        assert_eq!(a.stats().late, 0);
    }

    #[test]
    fn frame_budget_caps_total_wait() {
        let worker = Arc::new(Worker::new(WorkerConfig::ASSIST));
        let mode = RasterMode::Assist { worker: worker.clone(), wait: Duration::from_millis(40), notifier: None };
        let Some(mut f) = fonts(mode) else { return };
        let st = TextStyle::mono(10.0);
        f.metrics(&st);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        worker.submit(move || {
            let _ = gate_rx.recv();
        });
        // assist が塞がっている間に 10 字: 1 字 40ms ずつ待つと 400ms。上限は 3 字ぶん (120ms)
        f.begin_frame();
        let t = Instant::now();
        for ch in "あいうえおかきくけこ".chars() {
            assert!(f.glyph(ch, &st).is_none());
        }
        let dt = t.elapsed();
        assert!(dt >= Duration::from_millis(115) && dt < Duration::from_millis(250), "{dt:?}");
        gate_tx.send(()).unwrap();
        worker.wait_idle();
        assert!(f.take_ready());
        // 次のフレームでは上限が戻り、全部キャッシュから出る
        f.begin_frame();
        assert!("あいうえおかきくけこ".chars().all(|ch| f.glyph(ch, &st).is_some()));
    }

    #[test]
    fn assist_timeout_draws_blank_then_ready() {
        let worker = Arc::new(Worker::new(WorkerConfig::ASSIST));
        let notifier = Notifier::new().unwrap();
        let mode = RasterMode::Assist { worker: worker.clone(), wait: Duration::ZERO, notifier: Some(notifier.clone()) };
        let Some(mut f) = fonts(mode) else { return };
        let st = TextStyle::mono(10.0);
        let cell = f.metrics(&st).cell_w;
        // assist を塞いでおく → 待ち時間 0 では間に合わない
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        worker.submit(move || {
            let _ = gate_rx.recv();
        });
        assert!(f.glyph('字', &st).is_none());
        assert_eq!(f.advance('字', &st), cell * 2); // 見積もりの幅
        assert!(f.has_pending());
        assert_eq!(f.stats().rasterized, 1); // 2 回目は頼み直さない
        gate_tx.send(()).unwrap();
        worker.wait_idle();
        assert!(notifier.drain()); // メインが起こされる
        assert!(f.take_ready());
        assert!(!f.has_pending());
        assert_eq!(f.glyph('字', &st).unwrap().advance, cell * 2);
    }
}
