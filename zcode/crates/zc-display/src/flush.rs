//! `Display` と flush スレッド（design.md §4.2 / §5.1）。
//!
//! `fsync(fb0)` は続けて呼ぶと最大 7ms 止まるので、メインでは呼ばない。メインは
//! [`Display::present`] で行差分とコピーだけを行い、flush スレッドを起こして
//! すぐ返る。flush スレッドは変更行のまとまりを 1 つずつ書いて fsync する。

use crate::backend::Backend;
use crate::diff::diff_rows;
use crate::fb::FbBackend;
use crate::plan::{PendingRows, MERGE_GAP_ROWS};
use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use zc_core::time::now_us;
use zc_core::{Canvas, Damage, RowRange};

/// flush の設定。
#[derive(Clone, Copy, Debug)]
pub struct FlushConfig {
    /// これ未満の行数しか離れていない変更は 1 回の fsync にまとめる（§5.2）。
    pub merge_gap: u16,
    /// 覚えておく [`FlushRecord`] の数（古いものから捨てる）。
    pub log_len: usize,
}

impl Default for FlushConfig {
    fn default() -> Self {
        FlushConfig { merge_gap: MERGE_GAP_ROWS, log_len: 64 }
    }
}

/// [`Display::present`] の結果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentInfo {
    /// このフレームの通し番号。[`FlushRecord::seq`] と突き合わせる。
    /// 何も変わっていなければ、前のフレームの番号のまま。
    pub seq: u64,
    /// 提示用バッファと違っていた行の数（0 なら flush は起きない）。
    pub rows_changed: u16,
    /// flush スレッドに渡し終えた時刻（µs、CLOCK_MONOTONIC）。
    pub t_handoff_us: u64,
}

/// flush スレッドが 1 回 fsync するたびの記録。遅延の計測に使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlushRecord {
    /// このまとまりに含まれる、いちばん新しいフレームの通し番号。
    pub seq: u64,
    /// まとまりの行範囲。
    pub bounds: RowRange,
    /// 実際に書いた行数。
    pub rows: u16,
    /// 書き始めた時刻（µs）。
    pub t_start_us: u64,
    /// `fsync` を呼んだ時刻（µs）。ここから転送開始までの待ちは measurements.md §1.1。
    pub t_fsync_us: u64,
    /// `fsync` が返った時刻（µs）。
    pub t_done_us: u64,
    /// これを送り終えた時点で、送っていない行が残っていなければ `true`
    /// （フレーム `seq` までが全部液晶に向かった）。
    pub last: bool,
}

type Listener = Box<dyn FnMut(&FlushRecord) + Send>;

struct State {
    front: Vec<u16>,
    pending: PendingRows,
    seq: u64,
    closing: bool,
    busy: bool,
    log: VecDeque<FlushRecord>,
    listener: Option<Listener>,
    verify_req: bool,
    verify_result: Option<(u16, RowRange)>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    idle: Condvar,
}

/// 表示。メインスレッドが持つ。
///
/// 作ると flush スレッドが 1 本立つ。`Drop`（または [`Display::shutdown`]）は、
/// 送っていない行を送り切ってから flush スレッドを止める。fb0 の場合は、その後
/// 開いた時点の画面に戻る。
pub struct Display {
    w: u16,
    h: u16,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Display {
    /// 書き込み先を渡して作る。提示用バッファは、書き込み先の今の内容で始める。
    pub fn new(mut backend: Box<dyn Backend>, cfg: FlushConfig) -> Display {
        let (w, h) = backend.size();
        let mut front = vec![0u16; w as usize * h as usize];
        backend.read_all(&mut front);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                front,
                pending: PendingRows::new(h),
                seq: 0,
                closing: false,
                busy: false,
                log: VecDeque::new(),
                listener: None,
                verify_req: false,
                verify_result: None,
            }),
            wake: Condvar::new(),
            idle: Condvar::new(),
        });
        let s2 = shared.clone();
        let thread = std::thread::Builder::new()
            .name("zc-flush".into())
            .spawn(move || flush_loop(&s2, backend, cfg, w))
            .expect("flush スレッドを作れない");
        Display { w, h, shared, thread: Some(thread) }
    }

    /// fb0 を開いて作る。
    pub fn open_fb(path: impl AsRef<Path>) -> io::Result<Display> {
        Ok(Display::new(Box::new(FbBackend::open(path)?), FlushConfig::default()))
    }

    /// 幅と高さ。
    pub fn size(&self) -> (u16, u16) {
        (self.w, self.h)
    }

    /// 1 フレームを出す。`damage`（部品が返した行範囲の合計）の行だけを提示用
    /// バッファと比べ、変わった行を flush スレッドに渡してすぐ返る。
    pub fn present(&mut self, canvas: &Canvas, damage: &Damage) -> PresentInfo {
        assert_eq!((canvas.width(), canvas.height()), (self.w, self.h));
        let mut st = self.shared.state.lock().unwrap();
        let st = &mut *st;
        let rows_changed = diff_rows(canvas, &mut st.front, damage, &mut st.pending);
        if rows_changed > 0 {
            st.seq += 1;
            self.shared.wake.notify_one();
        }
        PresentInfo { seq: st.seq, rows_changed, t_handoff_us: now_us() }
    }

    /// 全部の行を比べて出す（部品の申告に頼らない）。
    pub fn present_all(&mut self, canvas: &Canvas) -> PresentInfo {
        self.present(canvas, &Damage::rows(0, self.h))
    }

    /// 中身が同じでも全部の行を送り直す（ほかのプロセスに画面を上書きされたとき用）。
    pub fn present_forced(&mut self, canvas: &Canvas) -> PresentInfo {
        assert_eq!((canvas.width(), canvas.height()), (self.w, self.h));
        let mut st = self.shared.state.lock().unwrap();
        st.front.copy_from_slice(canvas.pixels());
        st.pending.mark(RowRange::new(0, self.h), zc_core::Priority::Normal);
        st.seq += 1;
        self.shared.wake.notify_one();
        PresentInfo { seq: st.seq, rows_changed: self.h, t_handoff_us: now_us() }
    }

    /// fsync のたびに flush スレッドから呼ばれる関数を登録する。メインを起こす
    /// 通知（`MainSender::send`）を入れる。**flush スレッドで走るので、重い処理や
    /// `Display` の呼び出しはしないこと。**
    pub fn set_flush_listener(&mut self, f: impl FnMut(&FlushRecord) + Send + 'static) {
        self.shared.state.lock().unwrap().listener = Some(Box::new(f));
    }

    /// 溜まっている [`FlushRecord`] を取り出す（古い順）。
    pub fn take_flush_log(&mut self, out: &mut Vec<FlushRecord>) {
        out.extend(self.shared.state.lock().unwrap().log.drain(..));
    }

    /// 書き込み先の中身を読み戻して、提示用バッファと比べるよう flush スレッドに頼む
    /// （送るものが無くなったときに行われる）。ほかのプロセスが画面を上書きして
    /// いないかの確認用。結果は [`Display::take_verify`]。
    pub fn request_verify(&mut self) {
        self.shared.state.lock().unwrap().verify_req = true;
        self.shared.wake.notify_one();
    }

    /// [`Display::request_verify`] の結果: 提示用バッファと違っていた行の数と、
    /// それを含む行範囲。まだ終わっていなければ `None`。
    pub fn take_verify(&mut self) -> Option<(u16, RowRange)> {
        self.shared.state.lock().unwrap().verify_result.take()
    }

    /// 送っていない行が無くなるまで待つ。時間内に終われば `true`。
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let st = self.shared.state.lock().unwrap();
        let (st, res) = self
            .shared
            .idle
            .wait_timeout_while(st, timeout, |s| !s.pending.is_empty() || s.busy || s.verify_req)
            .unwrap();
        drop(st);
        !res.timed_out()
    }

    /// 送っていない行を送り切って flush スレッドを止める。fb0 なら元の画面に戻る。
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.shared.state.lock().unwrap().closing = true;
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        self.stop();
    }
}

fn flush_loop(shared: &Shared, mut backend: Box<dyn Backend>, cfg: FlushConfig, w: u16) {
    let w = w as usize;
    let mut scratch: Vec<u16> = Vec::new();
    let mut readback: Vec<u16> = Vec::new();
    loop {
        // 1. 次のまとまりを取り、その行を手元に写す（ロックはここだけ）
        let (group, seq) = {
            let mut st = shared.state.lock().unwrap();
            let g = loop {
                if let Some(g) = st.pending.take_next(cfg.merge_gap) {
                    st.busy = true;
                    break g;
                }
                if st.verify_req {
                    // 送るものが無いときに、書き込み先の中身を読み戻して比べる
                    st.verify_req = false;
                    readback.resize(st.front.len(), 0);
                    backend.read_all(&mut readback);
                    let mut n = 0u16;
                    let mut bounds = RowRange::default();
                    for (y, (a, b)) in st.front.chunks_exact(w).zip(readback.chunks_exact(w)).enumerate() {
                        if a != b {
                            n += 1;
                            bounds = bounds.union(RowRange::new(y as u16, y as u16 + 1));
                        }
                    }
                    st.verify_result = Some((n, bounds));
                }
                st.busy = false;
                shared.idle.notify_all();
                if st.closing {
                    return; // backend はここで drop される（fb0 は元の画面に戻る）
                }
                st = shared.wake.wait(st).unwrap();
            };
            scratch.clear();
            for r in &g.runs {
                scratch.extend_from_slice(&st.front[r.start as usize * w..r.end as usize * w]);
            }
            (g, st.seq)
        };
        // 2. 書いて fsync（止まるのは flush だけ）
        let t_start_us = now_us();
        let mut off = 0;
        for r in &group.runs {
            let n = r.len() as usize * w;
            backend.write_rows(r.start, &scratch[off..off + n]);
            off += n;
        }
        let t_fsync_us = now_us();
        let _ = backend.sync();
        let t_done_us = now_us();
        // 3. 記録
        let mut st = shared.state.lock().unwrap();
        let rec = FlushRecord {
            seq,
            bounds: group.bounds,
            rows: group.rows_written(),
            t_start_us,
            t_fsync_us,
            t_done_us,
            last: st.pending.is_empty(),
        };
        if st.log.len() >= cfg.log_len {
            st.log.pop_front();
        }
        st.log.push_back(rec);
        if let Some(f) = st.listener.as_mut() {
            f(&rec);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{MemBackend, MemOp};
    use zc_core::{Color, Priority, Rect};

    const W: u16 = 320;
    const H: u16 = 170;

    #[test]
    fn present_writes_only_changed_rows_and_syncs() {
        let (be, log) = MemBackend::new(W, H);
        let mut d = Display::new(Box::new(be), FlushConfig::default());
        let mut c = Canvas::new();
        c.fill_rect(Rect::new(0, 13, 320, 11), Color(0x1234));
        let info = d.present(&c, &Damage::rows(0, 170));
        assert_eq!(info.rows_changed, 11);
        assert_eq!(info.seq, 1);
        assert!(d.wait_idle(Duration::from_secs(2)));
        {
            let log = log.lock().unwrap();
            assert_eq!(log.ops, vec![MemOp::Write(RowRange::new(13, 24)), MemOp::Sync]);
            assert_eq!(&log.px[..], c.pixels());
        }
        // 同じ内容をもう一度出しても、何も書かない
        let info = d.present_all(&c);
        assert_eq!((info.rows_changed, info.seq), (0, 1));
        assert!(d.wait_idle(Duration::from_secs(2)));
        assert_eq!(log.lock().unwrap().ops.len(), 2);
        let mut recs = vec![];
        d.take_flush_log(&mut recs);
        assert_eq!(recs.len(), 1);
        assert_eq!((recs[0].seq, recs[0].bounds, recs[0].rows, recs[0].last), (1, RowRange::new(13, 24), 11, true));
        assert!(recs[0].t_start_us <= recs[0].t_fsync_us && recs[0].t_fsync_us <= recs[0].t_done_us);
    }

    #[test]
    fn distant_changes_get_separate_syncs_in_priority_order() {
        let (be, log) = MemBackend::new(W, H);
        let mut d = Display::new(Box::new(be), FlushConfig::default());
        let mut c = Canvas::new();
        c.fill_rect(Rect::new(0, 13, 320, 11), Color(1));
        c.fill_rect(Rect::new(0, 159, 320, 11), Color(2));
        let dmg = Damage::rows(159, 170).with_priority(Priority::Status)
            | Damage::rows(13, 24).with_priority(Priority::Cursor);
        d.present(&c, &dmg);
        assert!(d.wait_idle(Duration::from_secs(2)));
        let frames = log.lock().unwrap().frames();
        assert_eq!(frames, vec![vec![RowRange::new(13, 24)], vec![RowRange::new(159, 170)]]);
    }

    #[test]
    fn frames_presented_while_busy_are_merged() {
        // sync に 30ms かかる書き込み先。その間に 5 フレーム出す
        let (be, log) = MemBackend::new(W, H);
        let mut d = Display::new(Box::new(be.with_sync_delay(Duration::from_millis(30))), FlushConfig::default());
        let mut c = Canvas::new();
        for i in 0..6u16 {
            c.fill_rect(Rect::new(0, 40, 320, 11), Color(100 + i));
            d.present(&c, &Damage::rows(40, 51));
            if i == 0 {
                std::thread::sleep(Duration::from_millis(5)); // 1 枚目を flush に取らせる
            }
        }
        assert!(d.wait_idle(Duration::from_secs(2)));
        let log = log.lock().unwrap();
        // 6 回出したが、書き込みは 2 回（1 枚目と、残りをまとめた最新の内容）
        assert_eq!(log.frames().len(), 2);
        assert_eq!(&log.px[..], c.pixels());
        drop(log);
        let mut recs = vec![];
        d.take_flush_log(&mut recs);
        assert_eq!(recs.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1, 6]);
    }

    #[test]
    fn listener_is_called_and_shutdown_flushes_everything() {
        let (be, log) = MemBackend::new(W, H);
        let mut d = Display::new(Box::new(be.with_sync_delay(Duration::from_millis(10))), FlushConfig::default());
        let seen = Arc::new(Mutex::new(vec![]));
        let s2 = seen.clone();
        d.set_flush_listener(move |r| s2.lock().unwrap().push((r.seq, r.last)));
        let mut c = Canvas::new();
        c.clear(Color(7));
        d.present_all(&c);
        c.fill_rect(Rect::new(0, 0, 320, 1), Color(8));
        d.present_all(&c);
        d.shutdown(); // 待たずに閉じても、最後の内容まで書き切る
        assert_eq!(&log.lock().unwrap().px[..], c.pixels());
        let seen = seen.lock().unwrap();
        assert_eq!(seen.last(), Some(&(2, true)));
    }

    #[test]
    fn verify_detects_overwritten_rows() {
        let (be, log) = MemBackend::new(W, H);
        let mut d = Display::new(Box::new(be), FlushConfig::default());
        let mut c = Canvas::new();
        c.clear(Color(3));
        d.present_all(&c);
        d.request_verify();
        assert!(d.wait_idle(Duration::from_secs(2)));
        assert_eq!(d.take_verify(), Some((0, RowRange::default())));
        assert_eq!(d.take_verify(), None);
        // ほかのプロセスが 2 行ぶん上書きした
        log.lock().unwrap().px[320 * 10..320 * 12].fill(9);
        d.request_verify();
        assert!(d.wait_idle(Duration::from_secs(2)));
        assert_eq!(d.take_verify(), Some((2, RowRange::new(10, 12))));
    }

    #[test]
    fn forced_present_resends_identical_content() {
        let (be, log) = MemBackend::new(W, H);
        let mut d = Display::new(Box::new(be), FlushConfig::default());
        let c = Canvas::new();
        assert_eq!(d.present_all(&c).rows_changed, 0);
        d.present_forced(&c);
        assert!(d.wait_idle(Duration::from_secs(2)));
        assert_eq!(log.lock().unwrap().ops, vec![MemOp::Write(RowRange::new(0, 170)), MemOp::Sync]);
    }
}
