//! 単調時計。evdev のタイムスタンプ（`zc-input` が CLOCK_MONOTONIC に切り替える）と
//! 同じ時計なので、そのまま引き算で遅延を出せる。

/// CLOCK_MONOTONIC の現在時刻（µs）。
pub fn now_us() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: ts は有効な書き込み先。
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000
}

#[cfg(test)]
mod tests {
    #[test]
    fn monotonic() {
        let a = super::now_us();
        let b = super::now_us();
        assert!(b >= a && a > 0);
    }
}
