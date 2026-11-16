//! 実機のキーボード（evdev）。
//!
//! - evdev を直接読む。起動中は `EVIOCGRAB` で独占し、APPLaunch などほかの
//!   プロセスに打鍵が漏れないようにする。`Drop` で必ず放す。panic やシグナルで
//!   終わるときは [`emergency_ungrab`]（プロセスが終われば fd が閉じて grab も
//!   外れるが、念のため明示的に放す）
//! - タイムスタンプは `EVIOCSCLOCKID` で CLOCK_MONOTONIC に切り替える
//!   （`zc_core::time::now_us` と同じ時計なので、遅延をそのまま引き算できる）
//! - 1 回起きたら読めるイベントを全部読む。`SYN_DROPPED` が来たら、次の
//!   `SYN_REPORT` まで捨てて、押下状態を `EVIOCGKEY` で取り直す
//! - キーリピートは timerfd で作る。リピート中は、ときどき `EVIOCGKEY` で
//!   「本当にまだ押されているか」を確かめる（離上を取りこぼしても、リピートが
//!   止まらなくなることがないように）

use crate::keycodes::{EV_KEY, EV_MSC, EV_SYN, KEY_CNT, MSC_SCAN, SYN_DROPPED, SYN_REPORT};
use crate::keymap::Keymap;
use crate::repeat::RepeatConfig;
use crate::translate::{RawEvent, Translator};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use zc_core::rt::TimerFd;
use zc_core::time::now_us;
use zc_core::{KeyEvent, Mods};

const fn ioc(dir: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((dir << 30) | (size << 16) | ((b'E' as u32) << 8) | nr) as libc::c_ulong
}
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const EVIOCGRAB: libc::c_ulong = ioc(IOC_WRITE, 0x90, 4);
const EVIOCSCLOCKID: libc::c_ulong = ioc(IOC_WRITE, 0xa0, 4);
const fn eviocgname(len: u32) -> libc::c_ulong {
    ioc(IOC_READ, 0x06, len)
}
const fn eviocgkey(len: u32) -> libc::c_ulong {
    ioc(IOC_READ, 0x18, len)
}

/// リピート何回ごとに `EVIOCGKEY` で押下を確かめるか（30 回/秒なら約 0.27 秒ごと）。
const VERIFY_EVERY: u32 = 8;

static GRABBED_FD: AtomicI32 = AtomicI32::new(-1);

/// キーボードの独占を放す。異常終了の直前に呼ぶ。シグナルハンドラや panic フック
/// から呼べる（ioctl だけ）。独占していなければ何もしない。
pub fn emergency_ungrab() {
    let fd = GRABBED_FD.swap(-1, Ordering::SeqCst);
    if fd >= 0 {
        // SAFETY: fd は Keyboard が開いているもの。引数 0 で grab を放す。
        unsafe { libc::ioctl(fd, EVIOCGRAB as _, 0 as libc::c_int) };
    }
}

/// evdev の 1 イベントと、それをどう解釈したか（キー確認モード・計測用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawRecord {
    pub ev: RawEvent,
    /// このイベントを `read()` で受け取った時刻（µs、CLOCK_MONOTONIC）。
    pub t_read_us: u64,
    /// 直前の `MSC_SCAN` の値（キーボード行列の位置。キーのイベントにだけ付く）。
    pub scan: Option<i32>,
    /// このイベントを処理したあとのモディファイア。
    pub mods: Mods,
    /// このイベントから作った打鍵（押下のときだけ）。
    pub key: Option<KeyEvent>,
}

/// 実機のキーボード。メインスレッドが持つ。
pub struct Keyboard {
    file: File,
    path: PathBuf,
    timer: TimerFd,
    tr: Translator,
    grabbed: bool,
    /// `SYN_DROPPED` を受けて、次の `SYN_REPORT` まで捨てている最中。
    dropping: bool,
    last_scan: Option<i32>,
    last_key_time_us: u64,
    syn_dropped_count: u32,
    verify_count: u32,
}

fn device_name(fd: RawFd) -> Option<String> {
    let mut buf = [0u8; 128];
    // SAFETY: buf の長さを ioctl 番号に入れている。
    let n = unsafe { libc::ioctl(fd, eviocgname(buf.len() as u32) as _, buf.as_mut_ptr()) };
    if n <= 0 {
        return None;
    }
    let s = &buf[..n as usize];
    let s = s.split(|&b| b == 0).next().unwrap_or(s);
    Some(String::from_utf8_lossy(s).into_owned())
}

/// 名前が `name` の evdev デバイスを探す。
pub fn find_device(name: &str) -> Option<PathBuf> {
    (0..32).map(|i| PathBuf::from(format!("/dev/input/event{i}"))).find(|p| {
        File::open(p).ok().and_then(|f| device_name(f.as_raw_fd())).is_some_and(|n| n == name)
    })
}

impl Keyboard {
    /// キーボードを開く。`path` が `None` なら、キー配列の `device` の名前で探す
    /// （見つからなければ `/dev/input/event3`）。まだ独占はしない。
    pub fn open(path: Option<&Path>, keymap: Keymap, repeat: RepeatConfig) -> io::Result<Keyboard> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => keymap
                .device()
                .and_then(find_device)
                .unwrap_or_else(|| PathBuf::from("/dev/input/event3")),
        };
        let file = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC).open(&path)?;
        let clk: libc::c_int = libc::CLOCK_MONOTONIC;
        // SAFETY: int へのポインタを渡す ioctl。
        if unsafe { libc::ioctl(file.as_raw_fd(), EVIOCSCLOCKID as _, &clk as *const libc::c_int) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut kb = Keyboard {
            file,
            path,
            timer: TimerFd::new()?,
            tr: Translator::new(keymap, repeat),
            grabbed: false,
            dropping: false,
            last_scan: None,
            last_key_time_us: 0,
            syn_dropped_count: 0,
            verify_count: 0,
        };
        kb.resync()?;
        Ok(kb)
    }

    /// 開いているデバイスのパス。
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// デバイスの名前。
    pub fn name(&self) -> Option<String> {
        device_name(self.file.as_raw_fd())
    }

    /// キーボードを独占する（ほかのプロセスに打鍵が届かなくなる）。
    pub fn grab(&mut self) -> io::Result<()> {
        // SAFETY: 引数 1 で grab。
        if unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB as _, 1 as libc::c_int) } < 0 {
            return Err(io::Error::last_os_error());
        }
        self.grabbed = true;
        GRABBED_FD.store(self.file.as_raw_fd(), Ordering::SeqCst);
        Ok(())
    }

    /// 独占を放す。
    pub fn ungrab(&mut self) {
        if self.grabbed {
            GRABBED_FD.store(-1, Ordering::SeqCst);
            // SAFETY: 引数 0 で grab を放す。
            unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB as _, 0 as libc::c_int) };
            self.grabbed = false;
        }
    }

    pub fn is_grabbed(&self) -> bool {
        self.grabbed
    }

    /// `poll()` に渡す fd: `[evdev, リピート用 timerfd]`。evdev が読めたら
    /// [`Keyboard::read`]、timerfd が読めたら [`Keyboard::on_timer`] を呼ぶ。
    pub fn fds(&self) -> [RawFd; 2] {
        [self.file.as_raw_fd(), self.timer.as_raw_fd()]
    }

    /// 状態（押下・モディファイア・キー配列）を覗く。
    pub fn translator(&self) -> &Translator {
        &self.tr
    }
    pub fn translator_mut(&mut self) -> &mut Translator {
        &mut self.tr
    }
    /// いちばん新しいキーのイベントの、カーネルのタイムスタンプ（µs）。
    /// 打鍵から描画までの遅延の計測に使う。
    pub fn last_key_time_us(&self) -> u64 {
        self.last_key_time_us
    }
    /// `SYN_DROPPED` を受けた回数。
    pub fn syn_dropped_count(&self) -> u32 {
        self.syn_dropped_count
    }

    fn key_bits(&self) -> io::Result<[u8; KEY_CNT / 8]> {
        let mut bits = [0u8; KEY_CNT / 8];
        // SAFETY: bits の長さを ioctl 番号に入れている。
        if unsafe { libc::ioctl(self.file.as_raw_fd(), eviocgkey(bits.len() as u32) as _, bits.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(bits)
    }

    /// 押下状態を `EVIOCGKEY` で取り直す。
    pub fn resync(&mut self) -> io::Result<()> {
        let bits = self.key_bits()?;
        self.tr.resync(&bits);
        self.arm();
        Ok(())
    }

    fn arm(&self) {
        self.timer.set(self.tr.next_deadline());
    }

    /// 読めるイベントを全部読む。打鍵は `keys` に足す。`raw` を渡すと、受け取った
    /// 全イベント（`EV_SYN` / `EV_MSC` も）の記録を足す。
    pub fn read(&mut self, keys: &mut Vec<KeyEvent>, mut raw: Option<&mut Vec<RawRecord>>) -> io::Result<()> {
        const EV_SIZE: usize = core::mem::size_of::<libc::input_event>();
        let mut buf = [0u8; EV_SIZE * 64];
        loop {
            // SAFETY: buf の長さぶんの読み込み。
            let n = unsafe { libc::read(self.file.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::WouldBlock => break,
                    io::ErrorKind::Interrupted => continue,
                    _ => return Err(e),
                }
            }
            if n == 0 {
                break;
            }
            let t_read_us = now_us();
            for chunk in buf[..n as usize].chunks_exact(EV_SIZE) {
                // SAFETY: chunk は input_event 1 個ぶんのバイト列。整列は保証されないので
                // read_unaligned で読む。
                let ie: libc::input_event = unsafe { core::ptr::read_unaligned(chunk.as_ptr() as *const _) };
                let ev = RawEvent {
                    time_us: ie.time.tv_sec as u64 * 1_000_000 + ie.time.tv_usec as u64,
                    kind: ie.type_,
                    code: ie.code,
                    value: ie.value,
                };
                let mut rec = RawRecord { ev, t_read_us, scan: None, mods: self.tr.mods(), key: None };
                match (ev.kind, ev.code) {
                    (EV_SYN, SYN_DROPPED) => {
                        self.dropping = true;
                        self.syn_dropped_count += 1;
                    }
                    (EV_SYN, SYN_REPORT) if self.dropping => {
                        self.dropping = false;
                        self.resync()?;
                        rec.mods = self.tr.mods();
                    }
                    _ if self.dropping => {}
                    (EV_MSC, MSC_SCAN) => self.last_scan = Some(ev.value),
                    (EV_KEY, _) => {
                        self.last_key_time_us = ev.time_us;
                        rec.scan = self.last_scan.take();
                        rec.key = self.tr.feed(&ev);
                        rec.mods = self.tr.mods();
                        if let Some(k) = rec.key {
                            keys.push(k);
                        }
                    }
                    _ => {}
                }
                if let Some(raw) = raw.as_deref_mut() {
                    raw.push(rec);
                }
            }
        }
        self.arm();
        Ok(())
    }

    /// リピート用の timerfd が鳴った。リピートの打鍵があれば `keys` に足す。
    pub fn on_timer(&mut self, keys: &mut Vec<KeyEvent>) {
        self.timer.drain();
        if let Some(code) = self.tr.repeater().active_code() {
            // ときどき、本当にまだ押されているか確かめる
            let due = self.tr.repeater().next_deadline().is_some_and(|t| t <= now_us());
            if due {
                if let Some(k) = self.tr.tick(now_us()) {
                    keys.push(k);
                }
                self.verify_count = self.verify_count.wrapping_add(1);
                if self.verify_count % VERIFY_EVERY == 0 {
                    let still_down =
                        self.key_bits().map(|b| b[code as usize / 8] >> (code % 8) & 1 == 1).unwrap_or(false);
                    if !still_down {
                        let _ = self.resync();
                    }
                }
            }
        }
        self.arm();
    }
}

impl AsRawFd for Keyboard {
    fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        self.ungrab();
    }
}
