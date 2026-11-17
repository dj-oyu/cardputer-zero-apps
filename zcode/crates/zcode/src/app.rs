//! イベントループとスレッド構成（design.md §4）。
//!
//! ```text
//!   メイン: poll( evdev / リピート用 timerfd / アプリの timerfd / eventfd )
//!            └ 起きたら、溜まっている入力を全部処理してから 1 回だけ描く
//!   flush  : Display が持つ（fb0 への書き込みと fsync）
//!   assist : 字形のラスタライズ（最初の依頼で起動）
//!   batch / storage / check : 最初の依頼で起動（[`Workers`]）。check は保存時の実機の
//!            構文チェック専用（数秒かかる tsc で batch を塞がない）
//!   zc-remote : ビルド係の相手をする専用スレッド（[`crate::remote`]。画面が持つ）
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
use zc_core::{Canvas, Damage, DocId, KeyEvent, Theme, Version, Versioned};
use zc_display::{Backlight, Display, FlushRecord, PresentInfo};
use zc_font::{FontConfig, Fonts, RasterMode};
use zc_input::{Keyboard, Keymap, RawRecord, RepeatConfig};

/// evdev の EV_KEY。
const EV_KEY: u16 = 1;
use zc_ui::RenderCx;

/// ワーカーからメインへ届く結果。段階 2 以降、ここに項目を足していく
/// （版番号つきの結果は `Versioned<T>` で包み、受け取った側で古ければ捨てる）。
pub enum AppEvent {
    /// flush スレッドが 1 回 fsync した。
    Flushed(FlushRecord),
    /// batch がファイルを読み終えた（[`crate::editor::EditorScreen::open_path`]）。
    /// 単語補完の索引も batch で作っておく。
    Opened { doc: DocId, result: Result<(Box<dyn crate::doc::TextDoc + Send>, zc_intel::WordIndex), String> },
    /// assist が SKK の辞書を引き終えた（版つき。古ければ `Skk` が捨てる）。
    SkkCandidates(Versioned<Vec<zc_skk::Candidate>>),
    /// `check` ワーカーが実機の構文チェックを終えた（docs/stage3-design.md §8.3・§9）。
    /// 結果は出どころ `Syntax` の [`zc_core::DiagnosticSet`]: ファイルは 1 つ、パスは
    /// チェックしたファイルの絶対パス、`base` は保存した版。画面が置き場の鍵に直し、
    /// 届くまでの編集のぶんをずらしてから `zc_intel::DiagStore` に取り込む。
    Checked { doc: DocId, result: Result<zc_core::DiagnosticSet, crate::intel::CheckFail> },
    /// 保存が終わった（[`crate::save::Saver`]）。`version` は書き出した中身の版。
    Saved { doc: DocId, version: Version, result: Result<crate::save::SaveOk, String> },
    /// 書き込み層（`zc-store`）からの、保存以外の返事（控え・SKK 個人辞書）。
    Store(zc_store::StoreResponse),
    /// `zc-remote` の相手をする専用スレッドからの知らせ（[`crate::remote`]）。
    Remote(crate::remote::RemoteEvent),
    /// batch がハイライトの残り（行頭の文脈）を計算し終えた。
    HighlightRest { doc: DocId, rest: Versioned<zc_highlight::RestStates> },
    /// batch が Markdown 全体を解析し終えた（版つき。古ければ捨てる）。
    MarkdownParsed { doc: DocId, parsed: Versioned<zc_markdown::MdDocument> },
}

impl std::fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppEvent::Flushed(r) => f.debug_tuple("Flushed").field(r).finish(),
            AppEvent::Opened { doc, result } => write!(f, "Opened({doc:?}, {:?})", result.as_ref().map(|_| "..")),
            AppEvent::Saved { doc, version, result } => write!(f, "Saved({doc:?}, {version:?}, {result:?})"),
            AppEvent::SkkCandidates(v) => write!(f, "SkkCandidates({:?}, {} 件)", v.version, v.value.len()),
            AppEvent::Checked { doc, result } => write!(f, "Checked({doc:?}, {result:?})"),
            AppEvent::Store(r) => f.debug_tuple("Store").field(r).finish(),
            AppEvent::Remote(r) => f.debug_tuple("Remote").field(r).finish(),
            AppEvent::HighlightRest { doc, rest } => write!(f, "HighlightRest({doc:?}, {:?}, from {})", rest.version, rest.value.from_line),
            AppEvent::MarkdownParsed { doc, parsed } => write!(f, "MarkdownParsed({doc:?}, {:?})", parsed.version),
        }
    }
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
    /// SKK の個人辞書の書き戻し（学習した語を 5 分ためてまとめて書く。§8.3）。
    SkkFlush,
    /// 打鍵が止まったときのビルド係の check（最後の編集から少し待つ。打鍵のたびに仕掛け直す）。
    AutoCheck,
    /// 無操作での画面スリープ（最後の打鍵から `dark_time` 後に 1 回だけ鳴る。
    /// イベントループが受け持ち、画面には渡さない）。
    Sleep,
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
    /// ファイルを開く、ハイライトの残り、検索（nice +10）。
    pub batch: Arc<Worker>,
    /// 保存（nice +10）。終了時は join して書き切る。
    pub storage: Arc<Worker>,
    /// 保存時の構文チェックの子プロセス待ち（nice +10。docs/stage3-design.md §8.3）。
    pub check: Arc<Worker>,
}

impl Default for Workers {
    fn default() -> Self {
        Workers::new()
    }
}

impl Workers {
    pub fn new() -> Workers {
        Workers {
            assist: Arc::new(Worker::new(WorkerConfig::ASSIST)),
            batch: Arc::new(Worker::new(WorkerConfig::BATCH)),
            // 保存するまで要らないので、storage も最初の依頼まで作らない
            storage: Arc::new(Worker::new(WorkerConfig { lazy: true, ..WorkerConfig::STORAGE })),
            check: Arc::new(Worker::new(WorkerConfig::CHECK)),
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
    /// ほかに待つ fd（ターミナルの pty の master）。毎回の `poll()` の前に聞く。
    fn poll_fd(&self) -> Option<std::os::fd::RawFd> {
        None
    }
    /// [`Screen::poll_fd`] が読めるようになった（打鍵の処理の後に呼ぶ。§4.3）。
    /// 読む量の上限に達してまだ残っていれば `true`（次の `poll()` を待たずに続きを読む）。
    fn on_poll_fd(&mut self, _cx: &mut LoopCx<'_>) -> bool {
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
    /// キーボードを独占する（開発用。APPLaunch の横で SSH から動かすとき）。
    /// APPLaunch から起動されたときは独占しない: APPLaunch は自分のキー処理を止めて
    /// キーボードを共有し、Esc の約 3 秒長押しでアプリを強制終了させる。独占すると
    /// それが効かなくなる。
    pub grab: bool,
    /// fb0 の中身を定期的に読み戻して、ほかのプロセスに上書きされていないか調べる間隔。
    pub watch_fb: Option<Duration>,
    /// 終了時に統計を標準エラーに出す。
    pub print_stats: bool,
    /// バックライトを点けるときの明るさ（`None` なら自動。[`zc_display::backlight`]）。
    pub backlight: Option<u32>,
    /// バックライトに触らない（開発用）。
    pub no_backlight: bool,
    /// 無操作がこの時間続いたら画面を消す。`None` ならランチャーの設定
    /// `dark_time` に従う（[`crate::launcher_config`]）。`Some(None)` なら消さない。
    pub sleep_after: Option<Option<Duration>>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            fb: "/dev/fb0".into(),
            kbd: None,
            fonts: FontConfig::default(),
            inline_raster: false,
            grab: false,
            watch_fb: None,
            print_stats: false,
            backlight: None,
            no_backlight: false,
            sleep_after: None,
        }
    }
}

/// 実機で画面を動かす。戻るときには、キーボードの独占を放し、液晶を元に戻してある。
/// `stats` には、`main` の始まりの時刻と、ここに来るまでの区切り（引数・画面の用意）を入れて渡す。
pub fn run(opts: &Options, screen: &mut dyn Screen, mut stats: Stats) -> io::Result<Stats> {
    stats.mark("run entered");
    let (results_tx, results_rx): (MainSender<AppEvent>, MainReceiver<AppEvent>) = main_channel()?;
    crash::install(results_rx.fd());
    let workers = Workers::new();
    let mut timers = Timers::new()?;
    stats.mark("workers started");

    // 入力を先に開く（開けなければ、画面に触る前に終わる）
    // Sym レイヤーの記号はドライバの付属ファイルが正本（ランチャーと同じ）。あれば上書き
    let mut keymap = Keymap::builtin();
    keymap.load_driver_map();
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
    // 画面のスリープ（zc_display::backlight）。時間と明るさはランチャーの設定に従う
    let launcher = crate::launcher_config::load();
    let sleep_after = opts.sleep_after.unwrap_or(launcher.dark_time);
    let mut backlight = if opts.no_backlight {
        None
    } else {
        match Backlight::open_default(opts.backlight.or(launcher.brightness)) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("zcode: バックライトを開けない: {e}");
                None
            }
        }
    };
    if let Some(bl) = backlight.as_mut() {
        // ランチャーが眠らせた直後に起動された場合に備えて、点けてから始める
        bl.ensure_on();
        if let Some(d) = sleep_after {
            timers.set_after(TimerId::Sleep, d);
        }
    }
    if opts.grab {
        kb.grab()?;
        kb.resync()?;
    }

    let mut keys: Vec<KeyEvent> = Vec::new();
    let mut raw: Vec<RawRecord> = Vec::new();
    let mut flush_log: Vec<FlushRecord> = Vec::new();
    // 画面を起こすために捨てた打鍵のキーコード（離上のイベントも捨てるため）
    let mut swallowed: Vec<u16> = Vec::new();
    let mut next_watch = opts.watch_fb.map(|d| now_us() + d.as_micros() as u64);
    let mut cx_quit = false;
    let mut force_present = false;
    let mut first = true;
    let kfds = kb.fds();
    let mut ready = [false; 5];
    // 端末の出力が読み残しのまま（次は待たずに起きる）
    let mut fd_more = false;

    loop {
        let mut wake_us = now_us();
        if first {
            let mut cx = LoopCx { timers: &mut timers, workers: &workers, results: &results_tx, now_us: wake_us, quit: false, force_present: false };
            screen.start(&mut cx);
            cx_quit |= cx.quit;
            stats.mark("screen started");
        } else {
            timers.arm();
            let timeout = match next_watch {
                _ if fd_more => 0,
                Some(t) => (t.saturating_sub(now_us()) / 1000) as i32 + 1,
                None => -1,
            };
            let fds = [kfds[0], kfds[1], timers.fd.as_raw_fd(), results_rx.fd(), screen.poll_fd().unwrap_or(-1)];
            poll_fds(&fds, &mut ready, timeout)?;
            if fd_more && fds[4] >= 0 {
                ready[4] = true;
            }
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
            // 画面が消えている間の打鍵は、画面を起こすだけにしてアプリには渡さない
            // （ふつうの機器の「スリープからの復帰」と同じ）。そのキーのリピートも止め、
            // 後から届く離上のイベントも捨てる
            drop_swallowed_releases(&mut raw, &mut swallowed);
            let has_press = !keys.is_empty() || raw.iter().any(|r| r.ev.kind == EV_KEY && r.ev.value != 0);
            if has_press {
                // 眠らせる時刻を先へ延ばす（timerfd は次の poll の前に仕掛け直される）
                if let (Some(d), true) = (sleep_after, backlight.is_some()) {
                    timers.set(TimerId::Sleep, wake_us + d.as_micros() as u64);
                }
                if let Some(bl) = backlight.as_mut() {
                    if bl.ensure_on() {
                        for r in raw.iter().filter(|r| r.ev.kind == EV_KEY && r.ev.value == 1) {
                            swallowed.push(r.ev.code);
                        }
                        for code in kb.translator().down_codes() {
                            if !swallowed.contains(&code) {
                                swallowed.push(code);
                            }
                        }
                        keys.clear();
                        raw.clear();
                        kb.cancel_repeat();
                        stats.wakes_swallowed += 1;
                        // 眠っている間は描いていないので、起きたら全画面を送り直す
                        force_present = true;
                    }
                }
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
                    if id == TimerId::Sleep {
                        if let Some(bl) = backlight.as_mut() {
                            bl.sleep();
                        }
                        continue;
                    }
                    screen.on_timer(id, &mut cx);
                }
            }
            if !keys.is_empty() || !raw.is_empty() {
                stats.key_batches += 1;
                stats.keys += keys.len() as u32;
                screen.on_keys(&keys, &raw, &kb, &mut cx);
            }
            // 端末の出力は打鍵の後に読む（打鍵を待たせない。読む量には上限がある）
            fd_more = ready[4] && screen.on_poll_fd(&mut cx);
            cx_quit |= cx.quit;
            force_present |= cx.force_present;
        }
        if cx_quit || crash::quit_requested() {
            break;
        }

        // 画面を消して眠っている間は描かない（電池のため）。起きたら全画面を送り直す
        if backlight.as_ref().is_some_and(|b| b.is_asleep()) {
            continue;
        }

        // 溜まっていた入力を全部処理してから、1 回だけ描く
        let t0 = now_us();
        fonts.begin_frame();
        let damage = screen.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme));
        let t1 = now_us();
        if first {
            stats.mark("first frame rendered");
        }
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

        // ほかのプロセスに画面を上書きされていないか（計測用。--watch-fb のときだけ）
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
    // 眠っていたら点けて終わる（このあとは APPLaunch が画面を受け持つ）
    if let Some(bl) = backlight.as_mut() {
        bl.ensure_on();
    }
    // 後始末: 独占していれば放す → 送り残しを送り切る → 元の画面に戻す（Display の Drop）
    kb.ungrab();
    display.wait_idle(Duration::from_millis(500));
    display.take_flush_log(&mut flush_log);
    stats.flushes(&flush_log);
    stats.fonts = fonts.stats();
    stats.syn_dropped = kb.syn_dropped_count();
    stats.backlight = backlight.as_ref().map(|b| (b.initial(), b.level(), b.restores));
    stats.sleeps = backlight.as_ref().map_or(0, |b| b.sleeps);
    stats.sleep_after_s = sleep_after.map(|d| d.as_secs());
    stats.read_proc(); // スレッドを止める前の値
    drop(display);
    drop(kb);
    drop(fonts);
    drop(workers); // 溜まっている依頼を終わらせてから止まる
    stats.finish();
    Ok(stats)
}

/// 画面を起こすために捨てた打鍵（`swallowed`）の、離上のイベントを `raw` から除く。
/// 除いたキーコードは `swallowed` からも消す。
fn drop_swallowed_releases(raw: &mut Vec<RawRecord>, swallowed: &mut Vec<u16>) {
    if swallowed.is_empty() {
        return;
    }
    raw.retain(|r| {
        let release = r.ev.kind == EV_KEY && r.ev.value == 0;
        if release && swallowed.contains(&r.ev.code) {
            swallowed.retain(|&c| c != r.ev.code);
            false
        } else {
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::Mods;
    use zc_input::RawEvent;

    fn rec(kind: u16, code: u16, value: i32) -> RawRecord {
        RawRecord { ev: RawEvent { time_us: 0, kind, code, value }, t_read_us: 0, scan: None, mods: Mods::NONE, key: None }
    }

    #[test]
    fn swallowed_release_is_dropped_once() {
        // 30 (a) で画面を起こした。そのあと 30 の離上と、別のキー 31 の押下・離上が届く
        let mut swallowed = vec![30];
        let mut raw = vec![rec(EV_KEY, 30, 0), rec(0, 0, 0), rec(EV_KEY, 31, 1), rec(EV_KEY, 31, 0)];
        drop_swallowed_releases(&mut raw, &mut swallowed);
        let left: Vec<(u16, i32)> = raw.iter().map(|r| (r.ev.code, r.ev.value)).collect();
        assert_eq!(left, vec![(0, 0), (31, 1), (31, 0)]);
        assert!(swallowed.is_empty());
        // 2 回目の 30 の離上（ふつうの打鍵）は残す
        let mut raw = vec![rec(EV_KEY, 30, 1), rec(EV_KEY, 30, 0)];
        drop_swallowed_releases(&mut raw, &mut swallowed);
        assert_eq!(raw.len(), 2);
    }

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
