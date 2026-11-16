//! イベントループとスレッド構成（design.md §4）。
//!
//! ```text
//!   メイン: poll( evdev / リピート用 timerfd / アプリの timerfd / eventfd )
//!            └ 起きたら、溜まっている入力を全部処理してから 1 回だけ描く
//!   flush  : Display が持つ（fb0 への書き込みと fsync）
//!   assist : 字形のラスタライズ（最初の依頼で起動）
//!   batch / storage : 段階 2 以降の依頼用。枠だけ用意してある（[`Workers`]）
//!   結果   : どのワーカーも MainSender<AppEvent>（mpsc + eventfd 1 本）でメインへ
//! ```
//!
//! 画面ごとの処理は [`Screen`] に分ける。段階 1 の画面はキー確認・フォント比較・
//! 計測の 3 つ。段階 2 でエディタ本体の `Screen` が加わる。pty の fd を足す場所は
//! [`run`] の `fds`（段階 3）。

use crate::crash;
use crate::stats::Stats;
use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use zc_core::rt::{main_channel, poll_fds, MainReceiver, MainSender, TimerFd, Worker, WorkerConfig};
use zc_core::time::now_us;
use zc_core::{Canvas, Damage, KeyEvent, Theme};
use zc_display::{Backlight, Display, FlushRecord, PresentInfo};
use zc_font::{FontConfig, Fonts, RasterMode};
use zc_input::{Keyboard, Keymap, RawRecord, RepeatConfig};
use zc_ui::RenderCx;

/// ワーカーからメインへ届く結果。段階 2 以降、ここに項目を足していく
/// （版番号つきの結果は `Versioned<T>` で包み、受け取った側で古ければ捨てる）。
#[derive(Debug)]
pub enum AppEvent {
    /// flush スレッドが 1 回 fsync した。
    Flushed(FlushRecord),
}

/// アプリ側のタイマーの種類。1 本の timerfd で全部を扱う（[`Timers`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerId {
    /// 無操作での自動終了（段階 1 の画面の安全策）。
    Idle,
    /// 1 秒ごとの表示更新。
    Tick,
    /// 画面の自動切り替え・計測の次の一歩。
    Step,
    /// ステータスバーの遅延更新（50ms 静かになってから。§5.2）。
    StatusDelay,
    /// カーソルの点滅。
    CursorBlink,
    /// 控えの保存（§8.4）。
    Backup,
    /// バックライトの見張り（1 秒ごと。イベントループが受け持ち、画面には渡さない）。
    Backlight,
}

/// タイマーの集まり。種類ごとに 1 つの締め切りを持ち、いちばん早いものを
/// timerfd に仕掛ける。
pub struct Timers {
    fd: TimerFd,
    items: Vec<(TimerId, u64)>,
}

impl Timers {
    pub fn new() -> io::Result<Timers> {
        Ok(Timers { fd: TimerFd::new()?, items: Vec::new() })
    }
    /// `id` を絶対時刻 `at_us` に仕掛ける（仕掛け直しも同じ）。
    pub fn set(&mut self, id: TimerId, at_us: u64) {
        self.cancel(id);
        self.items.push((id, at_us));
    }
    /// いまから `after` 後に仕掛ける。
    pub fn set_after(&mut self, id: TimerId, after: Duration) {
        self.set(id, now_us() + after.as_micros() as u64);
    }
    pub fn cancel(&mut self, id: TimerId) {
        self.items.retain(|&(i, _)| i != id);
    }
    pub fn deadline(&self, id: TimerId) -> Option<u64> {
        self.items.iter().find(|&&(i, _)| i == id).map(|&(_, t)| t)
    }
    /// 締め切りを過ぎたタイマーを取り出す（早い順）。
    pub fn take_due(&mut self, now_us: u64) -> Vec<TimerId> {
        let mut due: Vec<(TimerId, u64)> = self.items.iter().copied().filter(|&(_, t)| t <= now_us).collect();
        due.sort_by_key(|&(_, t)| t);
        self.items.retain(|&(_, t)| t > now_us);
        due.into_iter().map(|(i, _)| i).collect()
    }
    /// いちばん早い締め切りを timerfd に仕掛ける。
    fn arm(&self) {
        self.fd.set(self.items.iter().map(|&(_, t)| t).min());
    }
}

/// 常駐ワーカー（design.md §4.2）。flush は `Display` が持つので、ここには無い。
pub struct Workers {
    /// 字形のラスタライズ、SKK 辞書の探索（mmap に触る仕事）。
    pub assist: Arc<Worker>,
    /// ファイルを開く、ハイライトの残り、検索、構文チェック（nice +10）。
    pub batch: Arc<Worker>,
    /// 保存（nice +10）。終了時は join して書き切る。
    pub storage: Arc<Worker>,
}

impl Workers {
    fn new() -> Workers {
        Workers {
            assist: Arc::new(Worker::new(WorkerConfig::ASSIST)),
            batch: Arc::new(Worker::new(WorkerConfig::BATCH)),
            // 段階 1 では保存しないので、storage も最初の依頼まで作らない
            storage: Arc::new(Worker::new(WorkerConfig { lazy: true, ..WorkerConfig::STORAGE })),
        }
    }
}

/// イベントループが画面に渡す文脈。
pub struct LoopCx<'a> {
    pub timers: &'a mut Timers,
    pub workers: &'a Workers,
    /// ワーカーに持たせる結果の送り口。
    pub results: &'a MainSender<AppEvent>,
    /// このフレームを起こした時刻（µs）。
    pub now_us: u64,
    /// `true` にするとループを抜ける。
    pub quit: bool,
    /// 次の描画で、中身が同じでも全画面を送り直す。
    pub force_present: bool,
}

/// 1 つの画面（段階 1: キー確認・フォント比較・計測。段階 2: エディタ）。
pub trait Screen {
    /// 最初の描画の前に 1 回呼ばれる（タイマーを仕掛ける）。
    fn start(&mut self, cx: &mut LoopCx<'_>);
    /// 打鍵（1 回の起床ぶんをまとめて）。`raw` は [`Screen::wants_raw`] のときだけ入る。
    fn on_keys(&mut self, keys: &[KeyEvent], raw: &[RawRecord], kb: &Keyboard, cx: &mut LoopCx<'_>);
    /// ワーカーからの結果。
    fn on_event(&mut self, ev: AppEvent, cx: &mut LoopCx<'_>);
    /// タイマー。
    fn on_timer(&mut self, id: TimerId, cx: &mut LoopCx<'_>);
    /// 待ち時間に間に合わなかった字形が届いた（空白で描いた所を描き直す）。
    fn on_glyphs_ready(&mut self);
    /// 描く。描き変えた行範囲を返す。
    fn render(&mut self, rcx: &mut RenderCx<'_>) -> Damage;
    /// フレームを flush スレッドに渡した（変わった行があったときだけ呼ばれる）。
    /// `info.seq` を [`AppEvent::Flushed`] の `seq` と突き合わせると、そのフレームの
    /// `fsync` の時刻が分かる。
    fn on_presented(&mut self, _info: PresentInfo) {}
    /// ループを抜けたあとに 1 回呼ばれる（ログを書き切る、など）。
    fn stop(&mut self) {}
    /// evdev の生のイベントも欲しいか。
    fn wants_raw(&self) -> bool {
        false
    }
    /// flush の記録（[`AppEvent::Flushed`]）を受け取りたいか。
    fn wants_flush_events(&self) -> bool {
        false
    }
}

/// 実行時の指定（コマンドラインから）。
#[derive(Clone, Debug)]
pub struct Options {
    pub fb: PathBuf,
    /// キーボードのデバイス（`None` なら名前で探す）。
    pub kbd: Option<PathBuf>,
    pub fonts: FontConfig,
    /// 字形をその場で作る（assist スレッドを使わない）。
    pub inline_raster: bool,
    /// キーボードを独占しない（開発用）。
    pub no_grab: bool,
    /// fb0 の中身を定期的に読み戻して、ほかのプロセスに上書きされていないか調べる間隔。
    pub watch_fb: Option<Duration>,
    /// 終了時に統計を標準エラーに出す。
    pub print_stats: bool,
    /// バックライトを点けるときの明るさ（`None` なら自動。[`zc_display::backlight`]）。
    pub backlight: Option<u32>,
    /// バックライトに触らない（開発用）。
    pub no_backlight: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            fb: "/dev/fb0".into(),
            kbd: None,
            fonts: FontConfig::default(),
            inline_raster: false,
            no_grab: false,
            watch_fb: None,
            print_stats: false,
            backlight: None,
            no_backlight: false,
        }
    }
}

/// 実機で画面を動かす。戻るときには、キーボードの独占を放し、液晶を元に戻してある。
pub fn run(opts: &Options, screen: &mut dyn Screen, t_start_us: u64) -> io::Result<Stats> {
    let mut stats = Stats::new(t_start_us);
    let (results_tx, results_rx): (MainSender<AppEvent>, MainReceiver<AppEvent>) = main_channel()?;
    crash::install(results_rx.fd());
    let workers = Workers::new();
    let mut timers = Timers::new()?;

    // 入力を先に開く（開けなければ、画面に触る前に終わる）
    let keymap = Keymap::builtin();
    stats.mark("keymap parsed");
    let mut kb = Keyboard::open(opts.kbd.as_deref(), keymap, RepeatConfig::default())?;
    stats.mark("keyboard opened");
    let mode = if opts.inline_raster {
        RasterMode::Inline
    } else {
        RasterMode::Assist {
            worker: workers.assist.clone(),
            wait: Duration::from_millis(2),
            notifier: Some(results_rx.notifier().clone()),
        }
    };
    let mut fonts = Fonts::load(&opts.fonts, mode)?;
    stats.mark("fonts loaded");
    for w in fonts.warnings() {
        eprintln!("zcode: {w}");
    }
    let theme = Theme::dark_plus();
    let mut canvas = Canvas::new();
    let mut display = Display::open_fb(&opts.fb)?;
    if screen.wants_flush_events() {
        let tx = results_tx.clone();
        display.set_flush_listener(move |r| {
            tx.send(AppEvent::Flushed(*r));
        });
    }
    stats.mark("fb0 opened");
    // 独占すると APPLaunch に打鍵が届かず、APPLaunch の時間切れで画面が消えたまま
    // 戻らなくなる。実行中はバックライトをこちらで受け持つ（zc_display::backlight）
    let mut backlight = if opts.no_backlight {
        None
    } else {
        match Backlight::open_default(opts.backlight) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("zcode: バックライトを開けない: {e}");
                None
            }
        }
    };
    if let Some(bl) = backlight.as_mut() {
        bl.ensure_on();
        timers.set_after(TimerId::Backlight, Duration::from_secs(1));
    }
    if !opts.no_grab {
        kb.grab()?;
        kb.resync()?;
    }

    let mut keys: Vec<KeyEvent> = Vec::new();
    let mut raw: Vec<RawRecord> = Vec::new();
    let mut flush_log: Vec<FlushRecord> = Vec::new();
    let mut next_watch = opts.watch_fb.map(|d| now_us() + d.as_micros() as u64);
    let mut cx_quit = false;
    let mut force_present = false;
    let mut first = true;
    let kfds = kb.fds();
    let fds = [kfds[0], kfds[1], timers.fd.as_raw_fd(), results_rx.fd()];
    let mut ready = [false; 4];

    loop {
        let mut wake_us = now_us();
        if first {
            let mut cx = LoopCx { timers: &mut timers, workers: &workers, results: &results_tx, now_us: wake_us, quit: false, force_present: false };
            screen.start(&mut cx);
            cx_quit |= cx.quit;
        } else {
            timers.arm();
            let timeout = match next_watch {
                Some(t) => (t.saturating_sub(now_us()) / 1000) as i32 + 1,
                None => -1,
            };
            poll_fds(&fds, &mut ready, timeout)?;
            wake_us = now_us();
            keys.clear();
            raw.clear();
            let want_raw = screen.wants_raw();
            if ready[0] {
                kb.read(&mut keys, if want_raw { Some(&mut raw) } else { None })?;
            }
            if ready[1] {
                kb.on_timer(&mut keys);
            }
            let mut cx = LoopCx { timers: &mut timers, workers: &workers, results: &results_tx, now_us: wake_us, quit: false, force_present: false };
            if ready[3] {
                for ev in results_rx.drain() {
                    screen.on_event(ev, &mut cx);
                }
                if fonts.take_ready() {
                    screen.on_glyphs_ready();
                }
            }
            if ready[2] {
                cx.timers.fd.drain();
                for id in cx.timers.take_due(wake_us) {
                    if id == TimerId::Backlight {
                        if let Some(bl) = backlight.as_mut() {
                            bl.ensure_on();
                        }
                        cx.timers.set_after(TimerId::Backlight, Duration::from_secs(1));
                        continue;
                    }
                    screen.on_timer(id, &mut cx);
                }
            }
            if !keys.is_empty() || !raw.is_empty() {
                if let Some(bl) = backlight.as_mut() {
                    bl.ensure_on();
                }
                stats.key_batches += 1;
                stats.keys += keys.len() as u32;
                screen.on_keys(&keys, &raw, &kb, &mut cx);
            }
            cx_quit |= cx.quit;
            force_present |= cx.force_present;
        }
        if cx_quit || crash::quit_requested() {
            break;
        }

        // 溜まっていた入力を全部処理してから、1 回だけ描く
        let t0 = now_us();
        fonts.begin_frame();
        let damage = screen.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme));
        let t1 = now_us();
        let info = if force_present {
            force_present = false;
            Some(display.present_forced(&canvas))
        } else if !damage.is_empty() {
            Some(display.present(&canvas, &damage)).filter(|i| i.rows_changed > 0)
        } else {
            None
        };
        if let Some(info) = info {
            screen.on_presented(info);
            stats.frame(info.seq, wake_us, t0, t1, info.t_handoff_us, info.rows_changed);
        }
        if first {
            first = false;
            stats.first_frame_us = now_us();
            stats.mark("first frame handed off");
        }

        // ほかのプロセスに画面を上書きされていないか（計測用）
        if let Some((n, bounds)) = display.take_verify() {
            stats.verify(n, bounds);
        }
        if let (Some(t), Some(every)) = (next_watch, opts.watch_fb) {
            if now_us() >= t {
                display.request_verify();
                next_watch = Some(now_us() + every.as_micros() as u64);
            }
        }
        display.take_flush_log(&mut flush_log);
        stats.flushes(&flush_log);
        flush_log.clear();
    }

    screen.stop();
    // 後始末: 独占を放す → 送り残しを送り切る → 元の画面に戻す（Display の Drop）
    kb.ungrab();
    display.wait_idle(Duration::from_millis(500));
    display.take_flush_log(&mut flush_log);
    stats.flushes(&flush_log);
    stats.fonts = fonts.stats();
    stats.syn_dropped = kb.syn_dropped_count();
    stats.backlight = backlight.as_ref().map(|b| (b.initial(), b.level(), b.restores));
    stats.read_proc(); // スレッドを止める前の値
    drop(display);
    drop(kb);
    drop(fonts);
    drop(workers); // 溜まっている依頼を終わらせてから止まる
    stats.finish();
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timers_due_in_order_and_replace() {
        let mut t = Timers::new().unwrap();
        t.set(TimerId::Idle, 3000);
        t.set(TimerId::Tick, 1000);
        t.set(TimerId::Step, 2000);
        t.set(TimerId::Tick, 1500); // 仕掛け直し
        assert_eq!(t.deadline(TimerId::Tick), Some(1500));
        assert_eq!(t.take_due(999), vec![]);
        assert_eq!(t.take_due(2000), vec![TimerId::Tick, TimerId::Step]);
        t.cancel(TimerId::Idle);
        assert_eq!(t.take_due(u64::MAX), vec![]);
    }
}
