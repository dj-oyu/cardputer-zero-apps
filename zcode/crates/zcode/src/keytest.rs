//! キー確認モード（`zcode --keytest`）。design.md §6.2 / §11.1 段階 1。
//!
//! evdev のイベントを 1 つずつ表示し、機械で読めるログ（JSON Lines）を
//! `/tmp/zcode-keytest.jsonl` に足していく。目的は 3 つ:
//!
//! 1. Sym レイヤーの「刻印 → キーコード」の対応表を作る（`--guide` で、刻印を 1 つずつ
//!    指定して押してもらう。結果は `zcode --keymap-from-log` で `keymap/` 用の TOML になる）
//! 2. ドライバが Shift / Sym / Fn の押下・ワンショット・ロックをどう送ってくるかを見る
//! 3. 遅延の内訳を測る: カーネルのタイムスタンプ → 読み取り → そのフレームの `fsync`
//!
//! 終了: **Esc を続けて 5 回**（ほかのキーを挟むと数え直し。モディファイアの追跡が
//! 間違っていても効くように、生のキーコードだけで判定する）。無操作が 120 秒続いても
//! 終わる。SIGINT / SIGTERM でも終わる。
//!
//! 画面の列: `#` 通し番号 / `ev` ↓押下 ↑離上 / `code` と名前 / `scan` 行列の位置
//! （MSC_SCAN）/ `Δms` 前のキーイベントからの間隔（カーネルの時刻）/ `rd` カーネルの
//! 時刻から読み取りまで（µs）/ `fs` カーネルの時刻から、そのイベントを描いたフレームの
//! `fsync` を呼ぶまで（ms）/ `map` こちらの解釈。

use crate::app::{AppEvent, LoopCx, Screen, TimerId};
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;
use zc_core::{palette, Color, Damage, Key, KeyEvent, Mods, Rect};
use zc_display::PresentInfo;
use zc_font::TextStyle;
use zc_input::keycodes::{key_name, EV_KEY, EV_MSC, EV_SYN, KEY_BACKSPACE, KEY_ENTER, KEY_ESC};
use zc_input::{Keyboard, Keymap, RawRecord};
use zc_ui::layout::{STATUS_H, TAB_H};
use zc_ui::{Chip, RenderCx, StatusBar, TitleBar, Widget};

/// Esc を何回続けたら終わるか。
pub const EXIT_ESC_COUNT: u8 = 5;
/// `--guide` で順に尋ねる刻印（ASCII の記号 32 個）。
pub const GUIDE_SYMBOLS: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

const LINE_H: u16 = 11;
const ROWS: usize = 11;
const KEEP: usize = 64;

/// 画面とログに出す 1 イベント。
#[derive(Clone, Debug)]
struct Row {
    seq: u32,
    rec: RawRecord,
    /// 前のキーイベントからの間隔（µs）。
    dt_us: Option<u64>,
    /// このイベントを描いたフレームの通し番号。
    frame: Option<u64>,
    /// そのフレームの `fsync` を呼んだ時刻。
    t_fsync_us: Option<u64>,
    /// こちらで作ったリピートの回数。
    repeats: u32,
    /// 処理後に押されていたキーコード。
    down: Vec<u16>,
    logged: bool,
}

/// キー確認モードの画面。
pub struct KeyTest {
    keymap: Keymap,
    rows: VecDeque<Row>,
    seq: u32,
    last_key_us: Option<u64>,
    esc_run: u8,
    mods: Mods,
    down: Vec<u16>,
    key_count: u32,
    repeat_count: u32,
    syn_dropped: u32,
    idle: Duration,
    idle_deadline_us: u64,
    idle_left_s: u64,
    log: Option<File>,
    guide: Option<usize>,
    guide_done: Vec<(char, Option<u16>)>,
    title: TitleBar,
    status: StatusBar,
    dirty: bool,
    finished: bool,
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn mods_json(m: Mods) -> String {
    let names = [(Mods::SHIFT, "shift"), (Mods::CTRL, "ctrl"), (Mods::ALT, "alt"), (Mods::SYM, "sym"), (Mods::FN, "fn")];
    let v: Vec<String> = names.iter().filter(|(b, _)| m.contains(*b)).map(|(_, n)| format!("\"{n}\"")).collect();
    format!("[{}]", v.join(","))
}

impl KeyTest {
    /// `log_path`: JSON Lines の書き出し先（追記）。`guide`: 刻印を順に尋ねる。
    pub fn new(keymap: Keymap, log_path: Option<&Path>, guide: bool, idle: Duration) -> KeyTest {
        let log = log_path.and_then(|p| match std::fs::OpenOptions::new().create(true).append(true).open(p) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("zcode: {}: {e}（ログなしで続ける）", p.display());
                None
            }
        });
        let mut kt = KeyTest {
            keymap,
            rows: VecDeque::new(),
            seq: 0,
            last_key_us: None,
            esc_run: 0,
            mods: Mods::NONE,
            down: vec![],
            key_count: 0,
            repeat_count: 0,
            syn_dropped: 0,
            idle,
            idle_deadline_us: 0,
            idle_left_s: idle.as_secs(),
            log,
            guide: guide.then_some(0),
            guide_done: vec![],
            title: TitleBar::new("keytest", "Esc×5 で終了 / 無操作で自動終了"),
            status: StatusBar::new(),
            dirty: true,
            finished: false,
        };
        kt.log_line(&format!(
            "{{\"type\":\"start\",\"version\":1,\"guide\":{},\"t_us\":{}}}",
            guide,
            zc_core::time::now_us()
        ));
        kt
    }

    fn log_line(&mut self, line: &str) {
        if let Some(f) = self.log.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }

    /// 物理キーの呼び名（行列の位置にある通常レイヤーのキー）。
    fn physical(&self, code: u16) -> Option<String> {
        let pos = self.keymap.get(code)?.pos?;
        Some(self.keymap.normal_at(pos).map_or_else(|| format!("r{}c{}", pos.0, pos.1), |d| d.key.to_string()))
    }

    /// こちらの解釈（画面の `map` 列）。
    fn mapping(&self, rec: &RawRecord) -> String {
        if let Some(m) = self.keymap.modifier(rec.ev.code) {
            return format!("[{m:?}]");
        }
        let def = self.keymap.get(rec.ev.code);
        let ev = self.keymap.translate(rec.ev.code, rec.mods);
        let mut s = match (ev.key, def) {
            (Key::Unknown(_), Some(d)) => format!("? {}+{}", d.layer.as_str(), self.physical(rec.ev.code).unwrap_or_default()),
            (Key::Unknown(_), None) => "?? 表に無い".to_string(),
            (k, _) => k.to_string(),
        };
        if let Some(t) = ev.text {
            if Key::Char(t) != ev.key {
                let _ = write!(s, " '{t}'");
            }
        }
        if def.is_some_and(|d| d.provisional) && !matches!(ev.key, Key::Unknown(_)) {
            s.push_str(" 仮");
        }
        if !ev.mods.chord().is_empty() {
            let _ = write!(s, " {:?}", ev.mods.chord());
        }
        s
    }

    /// 1 イベントのログ（JSON 1 行）。
    fn row_json(&self, r: &Row) -> String {
        let e = &r.rec.ev;
        let mut o = format!(
            "{{\"type\":\"ev\",\"seq\":{},\"ev_type\":{},\"code\":{},\"value\":{},\"name\":{},\"t_kernel_us\":{},\"t_read_us\":{}",
            r.seq,
            e.kind,
            e.code,
            e.value,
            json_str(&if e.kind == EV_KEY { key_name(e.code).map_or_else(|| format!("#{}", e.code), |n| format!("KEY_{n}")) } else { String::new() }),
            e.time_us,
            r.rec.t_read_us
        );
        match r.t_fsync_us {
            Some(t) => {
                let _ = write!(o, ",\"t_fsync_us\":{t}");
            }
            None => o.push_str(",\"t_fsync_us\":null"),
        }
        match r.rec.scan {
            Some(s) => {
                let _ = write!(o, ",\"scan\":{s}");
            }
            None => o.push_str(",\"scan\":null"),
        }
        let _ = write!(o, ",\"mods\":{},\"down\":{:?}", mods_json(r.rec.mods), r.down);
        if e.kind == EV_KEY {
            let def = self.keymap.get(e.code);
            let ev = self.keymap.translate(e.code, r.rec.mods);
            let _ = write!(
                o,
                ",\"map\":{{\"key\":{},\"text\":{},\"layer\":{},\"pos\":{},\"physical\":{},\"provisional\":{},\"modifier\":{}}}",
                json_str(&ev.key.to_string()),
                ev.text.map_or("null".into(), |t| json_str(&t.to_string())),
                def.map_or("null".into(), |d| json_str(d.layer.as_str())),
                def.and_then(|d| d.pos).map_or("null".into(), |p| format!("[{},{}]", p.0, p.1)),
                self.physical(e.code).map_or("null".into(), |p| json_str(&p)),
                def.is_some_and(|d| d.provisional),
                self.keymap.modifier(e.code).is_some()
            );
        }
        let _ = write!(o, ",\"repeats\":{}}}", r.repeats);
        o
    }

    /// `fsync` の時刻が決まった（または終了する）行をログに書く。
    fn write_ready_rows(&mut self, all: bool) {
        let lines: Vec<String> = self
            .rows
            .iter()
            .rev()
            .filter(|r| !r.logged && (all || r.t_fsync_us.is_some() || r.rec.ev.kind != EV_KEY))
            .map(|r| self.row_json(r))
            .collect();
        for r in self.rows.iter_mut() {
            if !r.logged && (all || r.t_fsync_us.is_some() || r.rec.ev.kind != EV_KEY) {
                r.logged = true;
            }
        }
        for l in lines {
            self.log_line(&l);
        }
    }

    /// `--guide`: いま尋ねている刻印。
    fn guide_symbol(&self) -> Option<char> {
        self.guide.and_then(|i| GUIDE_SYMBOLS.chars().nth(i))
    }

    /// `--guide`: 押下 1 つを答えとして処理する。
    fn guide_press(&mut self, rec: &RawRecord) {
        let Some(sym) = self.guide_symbol() else { return };
        let code = rec.ev.code;
        if self.keymap.modifier(code).is_some() {
            return;
        }
        let idx = self.guide.unwrap_or(0);
        match code {
            KEY_BACKSPACE => {
                // 1 つ戻る
                if idx > 0 {
                    self.guide = Some(idx - 1);
                    self.guide_done.pop();
                    let prev = GUIDE_SYMBOLS.chars().nth(idx - 1).unwrap_or('?');
                    self.log_line(&format!("{{\"type\":\"guide_undo\",\"symbol\":{}}}", json_str(&prev.to_string())));
                }
                return;
            }
            KEY_ENTER => {
                self.guide_done.push((sym, None));
                self.log_line(&format!("{{\"type\":\"guide\",\"symbol\":{},\"code\":null}}", json_str(&sym.to_string())));
            }
            _ => {
                self.guide_done.push((sym, Some(code)));
                let def = self.keymap.get(code);
                self.log_line(&format!(
                    "{{\"type\":\"guide\",\"symbol\":{},\"code\":{},\"scan\":{},\"mods\":{},\"layer\":{},\"pos\":{}}}",
                    json_str(&sym.to_string()),
                    code,
                    rec.scan.map_or("null".into(), |s| s.to_string()),
                    mods_json(rec.mods),
                    def.map_or("null".into(), |d| json_str(d.layer.as_str())),
                    def.and_then(|d| d.pos).map_or("null".into(), |p| format!("[{},{}]", p.0, p.1)),
                ));
            }
        }
        self.guide = Some(idx + 1);
        if idx + 1 >= GUIDE_SYMBOLS.chars().count() {
            self.guide = None;
            self.log_line("{\"type\":\"guide_end\"}");
        }
    }

    /// 生のイベントを 1 つ取り込む。終了の合図（Esc×5）なら `true`。
    pub fn push_raw(&mut self, rec: &RawRecord, down: &[u16]) -> bool {
        let e = rec.ev;
        if e.kind == EV_SYN && e.code == zc_input::keycodes::SYN_DROPPED {
            self.syn_dropped += 1;
        }
        // EV_SYN / EV_MSC は画面には出さない（ログには書く）
        let is_key = e.kind == EV_KEY;
        self.seq += 1;
        let dt_us = if is_key {
            let dt = self.last_key_us.map(|t| e.time_us.saturating_sub(t));
            self.last_key_us = Some(e.time_us);
            dt
        } else {
            None
        };
        let row = Row { seq: self.seq, rec: *rec, dt_us, frame: None, t_fsync_us: None, repeats: 0, down: down.to_vec(), logged: false };
        if !is_key {
            if e.kind != EV_SYN && e.kind != EV_MSC {
                let line = self.row_json(&row);
                self.log_line(&line);
            }
            return false;
        }
        self.mods = rec.mods;
        self.down = down.to_vec();
        self.rows.push_front(row);
        if self.rows.len() > KEEP {
            // 画面から消える行は、fsync の時刻が無くてもログに書いておく
            if let Some(old) = self.rows.pop_back() {
                if !old.logged {
                    let line = self.row_json(&old);
                    self.log_line(&line);
                }
            }
        }
        self.dirty = true;
        if e.value == 1 {
            self.key_count += 1;
            if e.code == KEY_ESC {
                self.esc_run += 1;
                if self.esc_run >= EXIT_ESC_COUNT {
                    return true;
                }
            } else {
                self.esc_run = 0;
                self.guide_press(rec);
            }
        }
        false
    }

    /// こちらで作ったリピートを数える（いちばん新しい押下の行に付ける）。
    pub fn push_repeat(&mut self) {
        self.repeat_count += 1;
        if let Some(r) = self.rows.iter_mut().find(|r| r.rec.ev.value == 1) {
            r.repeats += 1;
        }
        self.dirty = true;
    }

    fn draw_rows(&self, cx: &mut RenderCx<'_>, area: Rect) {
        let ts = TextStyle::mono(9.0);
        let small = TextStyle::mono(8.0);
        cx.canvas.fill_rect(area, palette::BG);
        // 見出しの行
        let head = Rect::new(area.x, area.y, area.w, LINE_H);
        for (x, s) in [(2, "#"), (22, "ev"), (36, "code"), (128, "scan"), (152, "Δms"), (186, "rd µs"), (220, "fs ms"), (252, "map")] {
            cx.text_in(Rect::new(x, head.y, 80, LINE_H), s, &small, palette::DIM, palette::BG);
        }
        cx.canvas.hline(0, head.bottom() - 1, area.w, palette::BORDER);
        for (i, r) in self.rows.iter().take(ROWS).enumerate() {
            let y = area.y + ((i + 1) as u16 * LINE_H) as i16;
            let bg = if i == 0 { palette::LINE } else { palette::BG };
            if i == 0 {
                cx.canvas.fill_rect(Rect::new(area.x, y, area.w, LINE_H), bg);
            }
            let e = r.rec.ev;
            let cell = |cx: &mut RenderCx<'_>, x: i16, w: u16, s: &str, fg: Color| {
                cx.text_in(Rect::new(x, y, w, LINE_H), s, &ts, fg, bg);
            };
            cell(cx, 2, 20, &format!("{:>3}", r.seq % 1000), palette::GUTTER);
            let (arrow, color) = match e.value {
                1 => ("↓", palette::ACCENT),
                0 => ("↑", palette::DIM),
                _ => ("R", palette::WARN),
            };
            cell(cx, 24, 8, arrow, color);
            let fg = if e.value == 1 { palette::FG } else { palette::DIM };
            cell(cx, 36, 16, &format!("{:>3}", e.code), palette::NUMBER);
            let mut name = key_name(e.code).map_or_else(|| "?".to_string(), str::to_string);
            if r.repeats > 0 {
                let _ = write!(name, " ×{}", r.repeats);
            }
            cell(cx, 54, 72, &name, fg);
            if let Some(s) = r.rec.scan {
                cell(cx, 128, 20, &format!("{s:02x}"), palette::DIM);
            }
            if let Some(dt) = r.dt_us {
                let s = if dt >= 100_000_000 { ">99s".to_string() } else { format!("{:.1}", dt as f64 / 1000.0) };
                cell(cx, 150, 34, &s, palette::DIM);
            }
            cell(cx, 186, 30, &format!("{}", r.rec.t_read_us.saturating_sub(e.time_us)), palette::VARIABLE);
            let fs = match r.t_fsync_us {
                Some(t) => format!("{:.1}", t.saturating_sub(e.time_us) as f64 / 1000.0),
                None => "…".to_string(),
            };
            cell(cx, 220, 28, &fs, palette::FUNCTION);
            cell(cx, 252, 68, &self.mapping(&r.rec), if e.value == 1 { Color::WHITE } else { palette::DIM });
        }
    }

    fn draw_state(&self, cx: &mut RenderCx<'_>, area: Rect) {
        let ts = TextStyle::mono(9.0);
        cx.canvas.fill_rect(area, palette::SIDE);
        cx.canvas.hline(0, area.y as i32, area.w, palette::BORDER);
        let inner = Rect::new(area.x + 3, area.y + 1, area.w - 6, area.h - 1);
        if let Some(sym) = self.guide_symbol() {
            let n = self.guide.unwrap_or(0) + 1;
            let total = GUIDE_SYMBOLS.chars().count();
            let x = cx.text_in(inner, "刻印 ", &ts, palette::FG, palette::SIDE);
            let chip = Rect::new(x as i16, area.y + 1, 13, area.h - 1);
            cx.canvas.fill_rect(chip, palette::WARN);
            cx.text_in(Rect::new(chip.x + 4, chip.y, 9, chip.h), &sym.to_string(), &ts, Color::BLACK, palette::WARN);
            let rest = Rect::new(chip.x + 16, inner.y, (inner.right() - chip.x as i32 - 16) as u16, inner.h);
            cx.text_in(rest, &format!("のキーを押す {n}/{total}  Enter:無い BS:戻る"), &ts, palette::FG, palette::SIDE);
        } else {
            let down: Vec<String> = self.down.iter().map(|c| c.to_string()).collect();
            let s = format!("mods {:?}  down [{}]  esc {}/{}", self.mods, down.join(" "), self.esc_run, EXIT_ESC_COUNT);
            cx.text_in(inner, &s, &ts, palette::FG, palette::SIDE);
        }
    }

    /// 見本の状態にする（render-test 用。実機の代わりに、決まったイベント列を流す）。
    pub fn demo(&mut self) {
        use zc_input::{RawEvent, RepeatConfig, Translator};
        let mut tr = Translator::new(self.keymap.clone(), RepeatConfig::default());
        // (キーコード, 値, 前のイベントからの間隔 ms)
        let script: &[(u16, i32, u64)] = &[
            (30, 1, 0),
            (30, 0, 85),
            (42, 1, 410),
            (30, 1, 122),
            (30, 0, 96),
            (42, 0, 31),
            (14, 1, 655),
            (14, 0, 812),
            (383, 1, 1203),
            (71, 1, 187),
            (71, 0, 74),
            (383, 0, 52),
            (29, 1, 930),
            (31, 1, 141),
        ];
        let mut t = 12_345_678u64;
        for (i, &(code, value, dt_ms)) in script.iter().enumerate() {
            t += dt_ms * 1000 + (i as u64 * 137) % 900;
            let ev = RawEvent { time_us: t, kind: EV_KEY, code, value };
            let key = tr.feed(&ev);
            let scan = self.keymap.get(code).and_then(|d| d.pos).map(|(r, c)| (r as i32) << 4 | c as i32);
            let rec = RawRecord { ev, t_read_us: t + 160 + (i as u64 * 53) % 120, scan, mods: tr.mods(), key };
            self.push_raw(&rec, &tr.down_codes());
            if code == 14 && value == 1 {
                for _ in 0..12 {
                    self.push_repeat();
                }
            }
            // いちばん新しい行以外は、fsync の時刻が分かっている
            if i + 1 < script.len() {
                self.rows[0].t_fsync_us = Some(t + 4_300 + (i as u64 * 311) % 2_000);
            }
        }
        self.idle_left_s = 118;
    }

    /// 終了時: 書いていない行をログに出す（2 回目以降は何もしない）。
    pub fn finish(&mut self) {
        if std::mem::replace(&mut self.finished, true) {
            return;
        }
        self.write_ready_rows(true);
        let line = format!(
            "{{\"type\":\"end\",\"keys\":{},\"repeats\":{},\"syn_dropped\":{},\"t_us\":{}}}",
            self.key_count,
            self.repeat_count,
            self.syn_dropped,
            zc_core::time::now_us()
        );
        self.log_line(&line);
        if let Some(f) = self.log.as_mut() {
            let _ = f.flush();
        }
    }
}

impl Screen for KeyTest {
    fn start(&mut self, cx: &mut LoopCx<'_>) {
        self.idle_deadline_us = cx.now_us + self.idle.as_micros() as u64;
        cx.timers.set(TimerId::Idle, self.idle_deadline_us);
        cx.timers.set_after(TimerId::Tick, Duration::from_secs(1));
    }

    fn on_keys(&mut self, keys: &[KeyEvent], raw: &[RawRecord], kb: &Keyboard, cx: &mut LoopCx<'_>) {
        self.idle_deadline_us = cx.now_us + self.idle.as_micros() as u64;
        self.idle_left_s = self.idle.as_secs();
        cx.timers.set(TimerId::Idle, self.idle_deadline_us);
        let down = kb.translator().down_codes();
        for r in raw {
            if self.push_raw(r, &down) {
                cx.quit = true;
            }
        }
        for _ in keys.iter().filter(|k| k.repeat) {
            self.push_repeat();
        }
    }

    fn on_event(&mut self, ev: AppEvent, cx: &mut LoopCx<'_>) {
        let AppEvent::Flushed(rec) = ev else { return };
        let mut filled = false;
        for r in self.rows.iter_mut() {
            if r.t_fsync_us.is_none() && r.frame.is_some_and(|f| f <= rec.seq) {
                r.t_fsync_us = Some(rec.t_fsync_us);
                filled = true;
            }
        }
        if filled {
            self.write_ready_rows(false);
            // fs の列を埋めるための描き直しは、少し待ってから（次の打鍵の計測を乱さない）
            cx.timers.set_after(TimerId::StatusDelay, Duration::from_millis(150));
        }
    }

    fn on_timer(&mut self, id: TimerId, cx: &mut LoopCx<'_>) {
        match id {
            TimerId::Idle => cx.quit = true,
            TimerId::Tick => {
                self.idle_left_s = self.idle_deadline_us.saturating_sub(cx.now_us) / 1_000_000;
                self.dirty = true;
                cx.timers.set_after(TimerId::Tick, Duration::from_secs(1));
            }
            TimerId::StatusDelay => self.dirty = true,
            _ => {}
        }
    }

    fn on_glyphs_ready(&mut self) {
        self.title.invalidate();
        self.status.invalidate();
        self.dirty = true;
    }

    fn on_presented(&mut self, info: PresentInfo) {
        for r in self.rows.iter_mut().filter(|r| r.frame.is_none()) {
            r.frame = Some(info.seq);
        }
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) -> Damage {
        if !self.dirty {
            return Damage::none();
        }
        self.dirty = false;
        let (rest, status_area) = Rect::SCREEN.split_bottom(STATUS_H);
        let (title_area, body) = rest.split_top(TAB_H);
        let (rows_area, state_area) = body.split_bottom(13);
        let mut dmg = self.title.render(cx, title_area);
        self.draw_rows(cx, rows_area);
        self.draw_state(cx, state_area);
        dmg |= Damage::rect(body);
        self.status.set_left(&format!("keys {}  rep {}  dropped {}", self.key_count, self.repeat_count, self.syn_dropped));
        self.status.set_right(&format!("idle {}s", self.idle_left_s));
        let mut chips = vec![];
        for (m, n) in [(Mods::CTRL, "CTRL"), (Mods::ALT, "ALT"), (Mods::SHIFT, "SHIFT"), (Mods::SYM, "SYM"), (Mods::FN, "FN")] {
            if self.mods.contains(m) {
                chips.push(Chip::modifier(n));
            }
        }
        self.status.set_chips(chips);
        dmg |= self.status.render(cx, status_area);
        dmg
    }

    fn stop(&mut self) {
        self.finish();
    }
    fn wants_raw(&self) -> bool {
        true
    }
    fn wants_flush_events(&self) -> bool {
        true
    }
}
