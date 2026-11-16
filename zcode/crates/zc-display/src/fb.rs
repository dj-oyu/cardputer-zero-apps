//! fb0（液晶）への出力。
//!
//! - mmap は長さ（320*170*2）を明示する。文字デバイスはファイルサイズを 0 と
//!   報告するので、長さを省くと 0 バイトのマップになる
//! - 開いた時点の内容を控え、`Drop` で書き戻す。panic やシグナルで終わるときは
//!   [`emergency_restore`] を呼ぶ（シグナルハンドラから呼べる作り）

use crate::backend::Backend;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering};

// 緊急時の書き戻し用。FbBackend を開いている間だけ有効。
static EMERG_MAP: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static EMERG_SAVED: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static EMERG_LEN: AtomicUsize = AtomicUsize::new(0);
static EMERG_FD: AtomicI32 = AtomicI32::new(-1);
/// 緊急の書き戻しが済んだら立つ。以後 flush は fb0 に書かない。
static DEAD: AtomicBool = AtomicBool::new(false);

/// 開いたときの fb0 の内容を書き戻して転送させる。異常終了の直前に呼ぶ。
///
/// シグナルハンドラや panic フックから呼べるように、メモリの確保もロックも
/// しない（メモリのコピーと `fsync` だけ）。何度呼んでもよい。fb0 を開いて
/// いなければ何もしない。呼んだあとは、flush スレッドは fb0 に書かなくなる。
pub fn emergency_restore() {
    DEAD.store(true, Ordering::SeqCst);
    let map = EMERG_MAP.load(Ordering::SeqCst);
    let saved = EMERG_SAVED.load(Ordering::SeqCst);
    let len = EMERG_LEN.load(Ordering::SeqCst);
    let fd = EMERG_FD.load(Ordering::SeqCst);
    if map.is_null() || saved.is_null() || len == 0 {
        return;
    }
    // SAFETY: map と saved は FbBackend が生きている間だけ登録されていて、
    // どちらも len バイトある。
    unsafe {
        core::ptr::copy_nonoverlapping(saved, map, len);
        if fd >= 0 {
            libc::fsync(fd);
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct FbBitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Default)]
struct FbVarScreeninfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: FbBitfield,
    green: FbBitfield,
    blue: FbBitfield,
    transp: FbBitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

#[repr(C)]
struct FbFixScreeninfo {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    type_: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;

/// fb0 の mmap と fd。flush スレッドが持つ。
pub struct FbBackend {
    file: File,
    map: *mut u8,
    len: usize,
    w: u16,
    h: u16,
    saved: Box<[u8]>,
}

// SAFETY: map はこの構造体だけが持つ共有マッピングで、1 スレッドからしか触らない。
unsafe impl Send for FbBackend {}

impl FbBackend {
    /// fb0 を開く。RGB565・行の詰め物なし（stride = 幅 × 2）でなければエラー。
    /// 開いた時点の内容を控える。
    pub fn open(path: impl AsRef<Path>) -> io::Result<FbBackend> {
        let file = OpenOptions::new().read(true).write(true).open(path.as_ref())?;
        let fd = file.as_raw_fd();
        let mut var = FbVarScreeninfo::default();
        // SAFETY: fix は全フィールドが整数で、ゼロ埋めでよい。
        let mut fix: FbFixScreeninfo = unsafe { core::mem::zeroed() };
        // SAFETY: どちらの ioctl も、渡した構造体に書き込むだけ。
        let ok = unsafe {
            libc::ioctl(fd, FBIOGET_VSCREENINFO as _, &mut var as *mut FbVarScreeninfo) == 0
                && libc::ioctl(fd, FBIOGET_FSCREENINFO as _, &mut fix as *mut FbFixScreeninfo) == 0
        };
        if !ok {
            return Err(io::Error::last_os_error());
        }
        if var.bits_per_pixel != 16 || fix.line_length != var.xres * 2 || var.xres > 4096 || var.yres > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "fb の形式が想定外: {}x{} {}bpp stride {}",
                    var.xres, var.yres, var.bits_per_pixel, fix.line_length
                ),
            ));
        }
        let (w, h) = (var.xres as u16, var.yres as u16);
        let len = w as usize * h as usize * 2;
        // SAFETY: 長さを明示した共有マッピング。失敗は MAP_FAILED で分かる。
        let map = unsafe {
            libc::mmap(core::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0)
        };
        if map == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let map = map as *mut u8;
        let mut saved = vec![0u8; len].into_boxed_slice();
        // SAFETY: map は len バイトの読み書きできる領域。
        unsafe { core::ptr::copy_nonoverlapping(map, saved.as_mut_ptr(), len) };
        let fb = FbBackend { file, map, len, w, h, saved };
        EMERG_LEN.store(len, Ordering::SeqCst);
        EMERG_FD.store(fd, Ordering::SeqCst);
        EMERG_SAVED.store(fb.saved.as_ptr() as *mut u8, Ordering::SeqCst);
        EMERG_MAP.store(map, Ordering::SeqCst);
        DEAD.store(false, Ordering::SeqCst);
        Ok(fb)
    }

    /// 開いた時点の内容（控え）。
    pub fn saved(&self) -> &[u8] {
        &self.saved
    }
}

impl Backend for FbBackend {
    fn size(&self) -> (u16, u16) {
        (self.w, self.h)
    }
    fn read_all(&mut self, out: &mut [u16]) {
        assert_eq!(out.len() * 2, self.len);
        // SAFETY: map は len バイト。out も同じ長さ。
        unsafe { core::ptr::copy_nonoverlapping(self.map, out.as_mut_ptr() as *mut u8, self.len) };
    }
    fn write_rows(&mut self, y0: u16, px: &[u16]) {
        let off = y0 as usize * self.w as usize * 2;
        let n = px.len() * 2;
        if off + n > self.len || DEAD.load(Ordering::SeqCst) {
            return;
        }
        // SAFETY: 範囲は上で確かめた。
        unsafe { core::ptr::copy_nonoverlapping(px.as_ptr() as *const u8, self.map.add(off), n) };
    }
    fn sync(&mut self) -> io::Result<()> {
        // SAFETY: 開いている fd への fsync。
        if unsafe { libc::fsync(self.file.as_raw_fd()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for FbBackend {
    fn drop(&mut self) {
        // 緊急の書き戻しを無効にしてから、控えを書き戻す
        EMERG_MAP.store(core::ptr::null_mut(), Ordering::SeqCst);
        EMERG_SAVED.store(core::ptr::null_mut(), Ordering::SeqCst);
        EMERG_FD.store(-1, Ordering::SeqCst);
        // SAFETY: map は len バイトで、まだ unmap していない。
        unsafe {
            core::ptr::copy_nonoverlapping(self.saved.as_ptr(), self.map, self.len);
            libc::fsync(self.file.as_raw_fd());
            libc::munmap(self.map as *mut libc::c_void, self.len);
        }
    }
}
