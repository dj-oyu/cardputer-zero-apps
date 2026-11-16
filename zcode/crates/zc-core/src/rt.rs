//! スレッドの受け渡し（design.md §4.2 / §4.3）。
//!
//! ```text
//!   メイン ──(依頼: クロージャ)──▶ Worker (flush 以外の assist / batch / storage)
//!   メイン ◀─(結果: MainSender<T> = mpsc + eventfd 1 本)── ワーカー
//! ```
//!
//! - [`Notifier`]: eventfd。ワーカーが書き、メインが `poll()` で待つ
//! - [`main_channel`]: 結果を運ぶ mpsc と、その到着を知らせる `Notifier` の組。
//!   送り口 [`MainSender`] は clone してどのワーカーにも渡せる。メインの
//!   イベントループが待つ eventfd は 1 本だけ
//! - [`Worker`]: 名前つきの常駐ワーカー 1 本。依頼はクロージャで渡す。最初の依頼で
//!   スレッドを作り（`lazy`）、優先度（nice）は下げる方向だけ指定できる
//! - [`TimerFd`] / [`poll_fds`]: イベントループの部品
//!
//! 古い結果を捨てる仕組みは [`crate::version`]。

use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// eventfd。どのスレッドからでも [`Notifier::notify`] でき、メインは fd を
/// `poll()` で待って [`Notifier::drain`] する。clone しても同じ eventfd を指す。
#[derive(Clone, Debug)]
pub struct Notifier(Arc<OwnedFd>);

impl Notifier {
    pub fn new() -> io::Result<Notifier> {
        // SAFETY: eventfd は新しい fd を返すだけ。
        let fd = cvt(unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) })?;
        // SAFETY: たった今作った、ほかに持ち主のいない fd。
        Ok(Notifier(Arc::new(unsafe { OwnedFd::from_raw_fd(fd) })))
    }
    /// 待っている側を起こす。シグナルハンドラからも呼べる（write だけ）。
    pub fn notify(&self) {
        let one: u64 = 1;
        // SAFETY: 8 バイトの書き込み。失敗（カウンタの飽和）は無視してよい。
        unsafe { libc::write(self.0.as_raw_fd(), &one as *const u64 as *const libc::c_void, 8) };
    }
    /// 溜まった通知を読み捨てる。1 回以上通知されていたら `true`。
    pub fn drain(&self) -> bool {
        let mut v: u64 = 0;
        // SAFETY: 8 バイトの読み込み。
        let n = unsafe { libc::read(self.0.as_raw_fd(), &mut v as *mut u64 as *mut libc::c_void, 8) };
        n == 8 && v > 0
    }
}

impl AsRawFd for Notifier {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// ワーカーからメインへ結果を送る口。送るたびに eventfd でメインを起こす。
pub struct MainSender<T> {
    tx: mpsc::Sender<T>,
    notifier: Notifier,
}

impl<T> Clone for MainSender<T> {
    fn clone(&self) -> Self {
        MainSender { tx: self.tx.clone(), notifier: self.notifier.clone() }
    }
}

impl<T> MainSender<T> {
    /// 結果を送ってメインを起こす。メインが終了していたら `false`。
    pub fn send(&self, v: T) -> bool {
        let ok = self.tx.send(v).is_ok();
        self.notifier.notify();
        ok
    }
    /// 同じ eventfd を使う通知口（値を送らずに起こすだけのとき）。
    pub fn notifier(&self) -> &Notifier {
        &self.notifier
    }
}

/// メイン側の受け口。
pub struct MainReceiver<T> {
    rx: mpsc::Receiver<T>,
    notifier: Notifier,
}

impl<T> MainReceiver<T> {
    /// `poll()` に渡す fd（読めるようになったら [`MainReceiver::drain`] を呼ぶ）。
    pub fn fd(&self) -> RawFd {
        self.notifier.as_raw_fd()
    }
    /// eventfd を読み捨ててから、届いている結果を全部取り出す。
    pub fn drain(&self) -> impl Iterator<Item = T> + '_ {
        self.notifier.drain();
        self.rx.try_iter()
    }
    pub fn notifier(&self) -> &Notifier {
        &self.notifier
    }
}

/// 結果の通り道（mpsc + eventfd 1 本）を作る。`T` はアプリ側で決める結果の列挙型。
pub fn main_channel<T>() -> io::Result<(MainSender<T>, MainReceiver<T>)> {
    let (tx, rx) = mpsc::channel();
    let notifier = Notifier::new()?;
    Ok((MainSender { tx, notifier: notifier.clone() }, MainReceiver { rx, notifier }))
}

/// ワーカーへの依頼。
pub type Job = Box<dyn FnOnce() + Send + 'static>;

/// ワーカーの設定。
#[derive(Clone, Copy, Debug)]
pub struct WorkerConfig {
    /// スレッド名（`/proc/<pid>/task/*/comm` に出る。15 バイトまで）。
    pub name: &'static str,
    /// nice 値。0 より大きい値だけ効く（実機は `RLIMIT_NICE=0` で上げられない）。
    pub nice: i32,
    /// `true` なら最初の依頼でスレッドを作る。
    pub lazy: bool,
}

impl WorkerConfig {
    /// assist: 字形のラスタライズ、SKK 辞書の探索（mmap に触る仕事）。
    pub const ASSIST: WorkerConfig = WorkerConfig { name: "zc-assist", nice: 0, lazy: true };
    /// batch: ファイルを開く、ハイライトの残り、検索、構文チェックなど。
    pub const BATCH: WorkerConfig = WorkerConfig { name: "zc-batch", nice: 10, lazy: true };
    /// storage: 保存（一時ファイル → fsync → rename）。終了時は join して書き切る。
    pub const STORAGE: WorkerConfig = WorkerConfig { name: "zc-storage", nice: 10, lazy: false };
}

struct Queue {
    jobs: VecDeque<(Option<u64>, Job)>,
    closing: bool,
    busy: bool,
}

struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
    idle: Condvar,
}

/// 常駐ワーカー 1 本。依頼は届いた順に 1 つずつ実行する。
///
/// 依頼はクロージャで渡す。結果は、クロージャに持たせた [`MainSender`] で返す。
/// 取り消しは [`crate::CancelToken`] をクロージャに持たせて、処理の途中で見る。
///
/// `Drop`（または [`Worker::shutdown`]）は、溜まっている依頼を**全部実行してから**
/// 終わる（storage が保存を書き切るため）。捨てたいときは先に
/// [`Worker::discard_pending`] を呼ぶ。
pub struct Worker {
    cfg: WorkerConfig,
    shared: Arc<Shared>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Worker {
    pub fn new(cfg: WorkerConfig) -> Worker {
        let w = Worker {
            cfg,
            shared: Arc::new(Shared {
                q: Mutex::new(Queue { jobs: VecDeque::new(), closing: false, busy: false }),
                cv: Condvar::new(),
                idle: Condvar::new(),
            }),
            handle: Mutex::new(None),
        };
        if !cfg.lazy {
            w.ensure_started();
        }
        w
    }

    pub fn name(&self) -> &'static str {
        self.cfg.name
    }

    /// スレッドがもう作られているか。
    pub fn is_started(&self) -> bool {
        self.handle.lock().unwrap().is_some()
    }

    fn ensure_started(&self) {
        let mut h = self.handle.lock().unwrap();
        if h.is_some() {
            return;
        }
        let shared = self.shared.clone();
        let nice = self.cfg.nice;
        let t = std::thread::Builder::new()
            .name(self.cfg.name.to_string())
            .spawn(move || {
                if nice > 0 {
                    // SAFETY: 自分のスレッド (tid) の nice を下げるだけ。Linux では
                    // PRIO_PROCESS に tid を渡すとスレッド単位で効く。
                    unsafe {
                        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
                        libc::setpriority(libc::PRIO_PROCESS as _, tid, nice);
                    }
                }
                run(&shared);
            })
            .expect("ワーカースレッドを作れない");
        *h = Some(t);
    }

    /// 依頼を足す。
    pub fn submit(&self, job: impl FnOnce() + Send + 'static) {
        self.push(None, Box::new(job));
    }

    /// 鍵つきの依頼を足す。同じ鍵の依頼がまだ実行待ちなら、それを**置き換える**
    /// （順番は元の位置のまま）。storage の「同じファイルへの依頼は最新だけ残す」用。
    pub fn submit_keyed(&self, key: u64, job: impl FnOnce() + Send + 'static) {
        self.push(Some(key), Box::new(job));
    }

    fn push(&self, key: Option<u64>, job: Job) {
        self.ensure_started();
        let mut q = self.shared.q.lock().unwrap();
        if key.is_some() {
            if let Some(slot) = q.jobs.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = job;
                return;
            }
        }
        q.jobs.push_back((key, job));
        drop(q);
        self.shared.cv.notify_one();
    }

    /// 実行待ちの依頼を捨てる（実行中のものは止めない）。捨てた数を返す。
    pub fn discard_pending(&self) -> usize {
        let mut q = self.shared.q.lock().unwrap();
        let n = q.jobs.len();
        q.jobs.clear();
        n
    }

    /// 実行待ちの依頼の数。
    pub fn pending(&self) -> usize {
        self.shared.q.lock().unwrap().jobs.len()
    }

    /// 依頼が全部終わるまで待つ（テスト用）。
    pub fn wait_idle(&self) {
        let mut q = self.shared.q.lock().unwrap();
        while !q.jobs.is_empty() || q.busy {
            q = self.shared.idle.wait(q).unwrap();
        }
    }

    /// 溜まっている依頼を全部実行させてから、スレッドを終わらせて待つ。
    pub fn shutdown(&self) {
        self.shared.q.lock().unwrap().closing = true;
        self.shared.cv.notify_all();
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(shared: &Shared) {
    loop {
        let job = {
            let mut q = shared.q.lock().unwrap();
            loop {
                if let Some((_, job)) = q.jobs.pop_front() {
                    q.busy = true;
                    break job;
                }
                q.busy = false;
                shared.idle.notify_all();
                if q.closing {
                    return;
                }
                q = shared.cv.wait(q).unwrap();
            }
        };
        // 依頼が panic してもワーカーは止めない（次の依頼を続ける）
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
    }
}

/// timerfd（CLOCK_MONOTONIC）。[`crate::time::now_us`] と同じ時計の絶対時刻で仕掛ける。
#[derive(Debug)]
pub struct TimerFd(OwnedFd);

impl TimerFd {
    pub fn new() -> io::Result<TimerFd> {
        // SAFETY: 新しい fd を返すだけ。
        let fd = cvt(unsafe { libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK) })?;
        // SAFETY: たった今作った fd。
        Ok(TimerFd(unsafe { OwnedFd::from_raw_fd(fd) }))
    }
    /// 絶対時刻 `at_us`（µs）に 1 回だけ鳴らす。`None` なら止める。
    pub fn set(&self, at_us: Option<u64>) {
        let mut spec: libc::itimerspec = unsafe { core::mem::zeroed() };
        if let Some(t) = at_us {
            let t = t.max(1); // 0 は「止める」の意味になる
            spec.it_value.tv_sec = (t / 1_000_000) as libc::time_t;
            spec.it_value.tv_nsec = ((t % 1_000_000) * 1000) as libc::c_long;
        }
        // SAFETY: spec は有効。古い値は要らない。
        unsafe { libc::timerfd_settime(self.0.as_raw_fd(), libc::TFD_TIMER_ABSTIME, &spec, core::ptr::null_mut()) };
    }
    /// 鳴った回数を読み捨てる。鳴っていたら `true`。
    pub fn drain(&self) -> bool {
        let mut v: u64 = 0;
        // SAFETY: 8 バイトの読み込み。
        let n = unsafe { libc::read(self.0.as_raw_fd(), &mut v as *mut u64 as *mut libc::c_void, 8) };
        n == 8 && v > 0
    }
}

impl AsRawFd for TimerFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// `poll()` で fd の集まりを待つ。`readable[i]` に、`fds[i]` が読める（または
/// エラー・切断）かを入れる。`timeout_ms` が負なら無期限。起きた fd の数を返す
/// （EINTR なら 0）。
pub fn poll_fds(fds: &[RawFd], readable: &mut [bool], timeout_ms: i32) -> io::Result<usize> {
    assert!(fds.len() <= 8 && fds.len() == readable.len());
    let mut p = [libc::pollfd { fd: -1, events: 0, revents: 0 }; 8];
    for (i, &fd) in fds.iter().enumerate() {
        p[i] = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    }
    // SAFETY: p の先頭 fds.len() 個は初期化済み。
    let n = unsafe { libc::poll(p.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
    if n < 0 {
        let e = io::Error::last_os_error();
        readable.fill(false);
        return if e.kind() == io::ErrorKind::Interrupted { Ok(0) } else { Err(e) };
    }
    for i in 0..fds.len() {
        readable[i] = p[i].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0;
    }
    Ok(n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::{VersionCounter, Versioned};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn result_wakes_poll_and_stale_is_dropped() {
        let (tx, rx) = main_channel::<Versioned<u32>>().unwrap();
        let w = Worker::new(WorkerConfig::BATCH);
        assert!(!w.is_started());
        let counter = VersionCounter::new();

        // 版 0 の依頼を出したあとで編集が入る → 結果は捨てられる
        let (t0, tx0) = (counter.token(), tx.clone());
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let g = gate.clone();
        w.submit(move || {
            let mut open = g.0.lock().unwrap();
            while !*open {
                open = g.1.wait(open).unwrap();
            }
            tx0.send(Versioned::new(t0.version(), 100));
        });
        counter.bump();
        let (t1, tx1) = (counter.token(), tx.clone());
        w.submit(move || {
            if t1.check().is_ok() {
                tx1.send(Versioned::new(t1.version(), 200));
            }
        });
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();

        let mut got = vec![];
        let mut ready = [false];
        while got.len() < 2 {
            assert_eq!(poll_fds(&[rx.fd()], &mut ready, 2000).unwrap(), 1);
            assert!(ready[0]);
            got.extend(rx.drain());
        }
        let kept: Vec<u32> = got.into_iter().filter_map(|r| r.take_if_current(&counter)).collect();
        assert_eq!(kept, vec![200]);
        // 読み捨て済みなので、もう起きない
        assert_eq!(poll_fds(&[rx.fd()], &mut ready, 0).unwrap(), 0);
    }

    #[test]
    fn keyed_jobs_keep_only_latest() {
        let w = Worker::new(WorkerConfig { name: "t-storage", nice: 10, lazy: false });
        let log = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let g = gate.clone();
        // 1 つ目で止めておき、その間に同じ鍵の依頼を重ねる
        let (entered_tx, entered_rx) = mpsc::channel();
        w.submit(move || {
            entered_tx.send(()).unwrap();
            let mut open = g.0.lock().unwrap();
            while !*open {
                open = g.1.wait(open).unwrap();
            }
        });
        entered_rx.recv().unwrap();
        for (key, v) in [(1u64, "a1"), (2, "b1"), (1, "a2"), (1, "a3"), (2, "b2")] {
            let log = log.clone();
            w.submit_keyed(key, move || log.lock().unwrap().push(v));
        }
        assert_eq!(w.pending(), 2);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        w.shutdown(); // 溜まった分を書き切ってから終わる
        assert_eq!(*log.lock().unwrap(), vec!["a3", "b2"]);
    }

    #[test]
    fn panicking_job_does_not_kill_worker() {
        let w = Worker::new(WorkerConfig::ASSIST);
        let n = Arc::new(AtomicUsize::new(0));
        w.submit(|| panic!("わざと"));
        let n2 = n.clone();
        w.submit(move || {
            n2.fetch_add(1, Ordering::SeqCst);
        });
        w.wait_idle();
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn timerfd_fires_at_absolute_time() {
        let t = TimerFd::new().unwrap();
        let start = crate::time::now_us();
        t.set(Some(start + 20_000));
        let mut ready = [false];
        assert_eq!(poll_fds(&[t.as_raw_fd()], &mut ready, 1000).unwrap(), 1);
        let dt = crate::time::now_us() - start;
        assert!((20_000..200_000).contains(&dt), "{dt}");
        assert!(t.drain());
        t.set(Some(crate::time::now_us() + 20_000));
        t.set(None);
        assert_eq!(poll_fds(&[t.as_raw_fd()], &mut ready, 60).unwrap(), 0);
    }
}
