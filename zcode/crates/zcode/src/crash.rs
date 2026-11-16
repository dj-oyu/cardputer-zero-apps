//! 異常終了への備え。どんな終わり方でも、キーボードの独占を放し、液晶を元の画面に
//! 戻してから終わる。
//!
//! | 終わり方 | 動き |
//! |---|---|
//! | ふつうの終了 | `Keyboard` と `Display` の `Drop` |
//! | SIGINT / SIGTERM / SIGHUP | フラグを立てて eventfd でメインを起こす → ふつうに終了。2 秒たっても終わらなければ（メインが固まっている）、SIGALRM のハンドラがその場で後始末して `_exit` |
//! | 2 回目のシグナル | その場で後始末して `_exit` |
//! | panic（メイン・flush） | フックで後始末 → メッセージ → `_exit(101)` |
//! | panic（assist / batch / storage の依頼の中） | メッセージだけ出して、ワーカーは次の依頼を続ける |
//! | SIGKILL | 何もできない。独占は fd が閉じるので外れる。画面は次に描くプロセスが上書きする |
//!
//! 後始末（[`emergency_cleanup`]）は、シグナルハンドラから呼べる処理だけでできている
//! （メモリのコピー・`fsync`・`ioctl`）。

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

static QUIT: AtomicBool = AtomicBool::new(false);
static WAKE_FD: AtomicI32 = AtomicI32::new(-1);

/// 画面を元に戻し、キーボードの独占を放す。何度呼んでもよい。
pub fn emergency_cleanup() {
    zc_input::emergency_ungrab();
    zc_display::emergency_restore();
}

/// 終了を求めるシグナルを受けたか。
pub fn quit_requested() -> bool {
    QUIT.load(Ordering::SeqCst)
}

extern "C" fn on_signal(sig: libc::c_int) {
    if sig == libc::SIGALRM || QUIT.swap(true, Ordering::SeqCst) {
        // 2 回目のシグナル、または時間切れ: メインに頼らず後始末して終わる
        emergency_cleanup();
        // SAFETY: _exit は async-signal-safe。
        unsafe { libc::_exit(128 + sig) };
    }
    let fd = WAKE_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        let one: u64 = 1;
        // SAFETY: write と alarm は async-signal-safe。
        unsafe { libc::write(fd, &one as *const u64 as *const libc::c_void, 8) };
    }
    // SAFETY: 同上。
    unsafe { libc::alarm(2) };
}

/// シグナルハンドラと panic フックを仕掛ける。`wake_fd` は、メインのイベントループが
/// 待っている eventfd（シグナルを受けたら書いて起こす）。
pub fn install(wake_fd: i32) {
    WAKE_FD.store(wake_fd, Ordering::SeqCst);
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGALRM] {
        // SAFETY: sigaction に、上の async-signal-safe なハンドラを渡す。
        unsafe {
            let mut sa: libc::sigaction = core::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = 0;
            libc::sigaction(sig, &sa, core::ptr::null_mut());
        }
    }
    // SIGPIPE で黙って死なない（ログの書き込み先が閉じた、など）
    // SAFETY: SIG_IGN を設定するだけ。
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // assist / batch / storage の依頼の panic は、ワーカーが受け止めて次の依頼を
        // 続ける（zc_core::rt::Worker）。編集中の内容を道連れにしない
        let name = std::thread::current().name().map(str::to_owned).unwrap_or_default();
        if name.starts_with("zc-") && name != "zc-flush" {
            default_hook(info);
            return;
        }
        // メインか flush の panic: 画面とキーボードを握ったまま残らないよう、全体を止める
        emergency_cleanup();
        default_hook(info);
        // SAFETY: _exit はどこからでも呼べる。
        unsafe { libc::_exit(101) };
    }));
}
