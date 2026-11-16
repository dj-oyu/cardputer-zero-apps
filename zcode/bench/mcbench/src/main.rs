use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

mod extra;
fn us(d: Duration) -> f64 { d.as_secs_f64() * 1e6 }
fn st(v: &[f64]) -> String {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| s[((s.len() - 1) as f64 * p).round() as usize];
    format!("n={} min={:.0} p50={:.0} p95={:.0} max={:.0} us", s.len(), s[0], q(0.5), q(0.95), s[s.len() - 1])
}
fn sleep_ms(ms: u64) { std::thread::sleep(Duration::from_millis(ms)); }
fn cur_freq() -> String {
    std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq").unwrap_or_default().trim().to_string()
}
fn pin(cpu: Option<usize>) {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        match cpu { Some(c) => libc::CPU_SET(c, &mut set), None => for c in 0..4 { libc::CPU_SET(c, &mut set) } }
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

// ---------- workloads ----------
const W: usize = 320; const H: usize = 170; const CW: usize = 6; const CH: usize = 13; const COLS: usize = 53; const ROWS: usize = 13;
struct Font { cov: Vec<u8>, luts: [[u16; 16]; 6] }
fn mk_font() -> Font {
    let mut x = 0x12345678u32;
    let mut cov = vec![0u8; 96 * CW * CH];
    for c in cov.iter_mut() { x ^= x << 13; x ^= x >> 17; x ^= x << 5; *c = if x % 10 < 6 { 0 } else { (x >> 8) as u8 }; }
    let mut luts = [[0u16; 16]; 6];
    for (i, l) in luts.iter_mut().enumerate() { for (j, e) in l.iter_mut().enumerate() { *e = (i as u16 * 4001).wrapping_add(j as u16 * 2113); } }
    Font { cov, luts }
}
#[inline(never)]
fn render_line(band: &mut [u16], text: &[u8], f: &Font) {
    for (col, &ch) in text.iter().enumerate().take(COLS) {
        let g = &f.cov[(ch as usize - 32) * CW * CH..][..CW * CH];
        let lut = &f.luts[(col / 6) % 6];
        for y in 0..CH {
            let dst = &mut band[y * W + col * CW..][..CW];
            let src = &g[y * CW..][..CW];
            for x in 0..CW { dst[x] = lut[(src[x] >> 4) as usize]; }
        }
    }
}
fn mk_text() -> Vec<u8> { (0..COLS * ROWS).map(|i| 32 + ((i * 7 + i / 5) % 95) as u8).collect() }
fn mk_dfa() -> Vec<u8> {
    let mut t = vec![0u8; 8 * 256];
    let mut x = 0x9e3779b9u32;
    for e in t.iter_mut() { x ^= x << 13; x ^= x >> 17; x ^= x << 5; *e = (x % 8) as u8; }
    t
}
#[inline(never)]
fn run_dfa(t: &[u8], data: &[u8]) -> u32 {
    let mut s = 0usize; let mut tok = 0u32;
    for &b in data { let n = t[s * 256 + b as usize] as usize; tok += (n != s) as u32; s = n; }
    tok
}

type Job = Arc<dyn Fn(usize) + Send + Sync>;
// ---------- persistent pool (barrier based) ----------
struct Pool { start: Arc<Barrier>, end: Arc<Barrier>, job: Arc<Mutex<Option<Job>>>, stop: Arc<AtomicBool>, hs: Vec<std::thread::JoinHandle<()>>, n: usize }
impl Pool {
    fn new(n: usize) -> Pool {
        let start = Arc::new(Barrier::new(n)); let end = Arc::new(Barrier::new(n));
        let job: Arc<Mutex<Option<Job>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let mut hs = vec![];
        for i in 1..n {
            let (s, e, j, st) = (start.clone(), end.clone(), job.clone(), stop.clone());
            hs.push(std::thread::spawn(move || loop {
                s.wait();
                if st.load(Ordering::Relaxed) { break; }
                let f = j.lock().unwrap().clone().unwrap();
                f(i);
                e.wait();
            }));
        }
        Pool { start, end, job, stop, hs, n }
    }
    fn run(&self, f: Job) {
        if self.n == 1 { f(0); return; }
        *self.job.lock().unwrap() = Some(f.clone());
        self.start.wait(); f(0); self.end.wait();
    }
    fn close(self) { self.stop.store(true, Ordering::Relaxed); if self.n > 1 { self.start.wait(); } for h in self.hs { h.join().unwrap(); } }
}
struct Ptr<T>(*mut T);
unsafe impl<T> Send for Ptr<T> {}
unsafe impl<T> Sync for Ptr<T> {}

fn sec_pool() {
    println!("## pool: full-screen job split in n bands (persistent workers), per-job wall time");
    let font = Arc::new(mk_font()); let text = Arc::new(mk_text());
    let mut canvas = vec![0u16; W * H]; let mut front = vec![0u16; W * H];
    let cp = Arc::new(Ptr(canvas.as_mut_ptr())); let fp = Arc::new(Ptr(front.as_mut_ptr()));
    let diffs = Arc::new(AtomicU64::new(0));
    for n in [1usize, 2, 4] {
        let pool = Pool::new(n);
        let blend: Job = { let (font, text, cp) = (font.clone(), text.clone(), cp.clone()); Arc::new(move |i| {
            let (a, b) = (ROWS * i / n, ROWS * (i + 1) / n);
            for r in a..b { let band = unsafe { std::slice::from_raw_parts_mut(cp.0.add(r * CH * W), CH * W) }; render_line(band, &text[r * COLS..(r + 1) * COLS], &font); }
        }) };
        let diff: Job = { let (cp, fp, diffs) = (cp.clone(), fp.clone(), diffs.clone()); Arc::new(move |i| {
            let (a, b) = (H * i / n, H * (i + 1) / n); let mut d = 0u64;
            for y in a..b { let (p, q) = unsafe { (std::slice::from_raw_parts(cp.0.add(y * W), W), std::slice::from_raw_parts(fp.0.add(y * W), W)) }; if p != q { d += 1; } }
            diffs.fetch_add(d, Ordering::Relaxed);
        }) };
        let copy: Job = { let (cp, fp) = (cp.clone(), fp.clone()); Arc::new(move |i| {
            let (a, b) = (H * i / n, H * (i + 1) / n);
            unsafe { std::ptr::copy_nonoverlapping(cp.0.add(a * W), fp.0.add(a * W), (b - a) * W) };
        }) };
        let noop: Job = Arc::new(|_| {});
        for (name, job) in [("noop(dispatch only)", &noop), ("blend 13x53 glyphs", &blend), ("copy 108KB", &copy), ("rowdiff equal 108KB", &diff)] {
            for (mode, iters, gap) in [("hot", 300, 0u64), ("cold(40ms gap)", 40, 40)] {
                let mut v = vec![];
                for _ in 0..iters { if gap > 0 { sleep_ms(gap); } let t = Instant::now(); pool.run(job.clone()); v.push(us(t.elapsed())); }
                println!("pool n={} {:22} {:15} {}  freq={}", n, name, mode, st(&v), cur_freq());
            }
        }
        pool.close();
    }
    let mut v = vec![];
    for _ in 0..200 { let t = Instant::now(); canvas.copy_within(CH * W.., 0); v.push(us(t.elapsed())); }
    println!("memmove scroll 1 line (inline) hot {}", st(&v));
    let mut v = vec![];
    for _ in 0..50 { let t = Instant::now(); std::thread::scope(|s| { for _ in 0..3 { s.spawn(|| {}); } }); v.push(us(t.elapsed())); }
    println!("scope spawn+join 3 threads {}", st(&v));
    std::hint::black_box(&front);
}

fn sec_scale() {
    println!("## raw scaling: n threads each doing the same private work, total throughput");
    let font = Arc::new(mk_font()); let text = Arc::new(mk_text()); let dfa = Arc::new(mk_dfa());
    for (name, kind) in [("dfa 2MB", 0), ("dfa 64KB(L2 fit)", 3), ("memcpy 4MB", 1), ("blend private canvas", 2)] {
        let mut base = 0.0;
        for n in [1usize, 2, 3, 4] {
            let bar = Arc::new(Barrier::new(n + 1)); let done = Arc::new(Barrier::new(n + 1));
            let mut hs = vec![];
            for _ in 0..n {
                let (bar, done, font, text, dfa) = (bar.clone(), done.clone(), font.clone(), text.clone(), dfa.clone());
                hs.push(std::thread::spawn(move || -> f64 {
                    let units: f64;
                    match kind {
                        0 | 3 => { let sz: usize = if kind == 0 { 2 << 20 } else { 64 << 10 }; let reps = if kind == 0 { 6 } else { 192 };
                            let data: Vec<u8> = (0..sz).map(|i: usize| (i.wrapping_mul(2654435761) >> 7) as u8).collect();
                            bar.wait(); let mut k = 0; for _ in 0..reps { k += run_dfa(&dfa, &data); } std::hint::black_box(k); units = (sz * reps) as f64 / 1e6; }
                        1 => { let a = vec![1u8; 4 << 20]; let mut b = vec![2u8; 4 << 20];
                            bar.wait(); for _ in 0..24 { b.copy_from_slice(&a); std::hint::black_box(&b); } units = 24.0 * 4.0; }
                        _ => { let mut c = vec![0u16; W * H];
                            bar.wait(); for _ in 0..400 { for r in 0..ROWS { render_line(&mut c[r * CH * W..][..CH * W], &text[r * COLS..(r + 1) * COLS], &font); } } std::hint::black_box(&c); units = 400.0; }
                    }
                    done.wait(); units
                }));
            }
            sleep_ms(50);
            bar.wait(); let t = Instant::now(); done.wait(); let el = t.elapsed().as_secs_f64();
            let tot: f64 = hs.into_iter().map(|h| h.join().unwrap()).sum();
            let rate = tot / el; if n == 1 { base = rate; }
            let unit = match kind { 0 | 1 | 3 => "MB/s", _ => "screens/s" };
            println!("scale {:22} n={} total={:.1} {} speedup={:.2}x wall={:.0}ms", name, n, rate, unit, rate / base, el * 1e3);
        }
    }
}

fn sec_freq() {
    println!("## freq: governor ramp (unit = blend one full screen)");
    let font = mk_font(); let text = mk_text(); let mut c = vec![0u16; W * H];
    let mut unit = || { let t = Instant::now(); for r in 0..ROWS { render_line(&mut c[r * CH * W..][..CH * W], &text[r * COLS..(r + 1) * COLS], &font); } us(t.elapsed()) };
    println!("freq at start {}", cur_freq());
    let t0 = Instant::now();
    loop { sleep_ms(500); let f = cur_freq(); if f == "600000" || t0.elapsed().as_secs() > 12 { println!("idle wait {:.1}s -> freq {}", t0.elapsed().as_secs_f64(), f); break; } }
    let mut v = vec![]; for _ in 0..30 { sleep_ms(100); v.push(unit()); }
    println!("typing cadence (100ms gap) after idle: {} freq={}", st(&v), cur_freq());
    let t0 = Instant::now(); let mut series = vec![];
    while t0.elapsed() < Duration::from_millis(500) { let at = t0.elapsed().as_secs_f64() * 1e3; series.push((at, unit())); }
    let first: Vec<f64> = series.iter().take(20).map(|x| x.1).collect();
    let last: Vec<f64> = series.iter().rev().take(20).map(|x| x.1).collect();
    println!("continuous first20: {}", st(&first)); println!("continuous last20: {} freq={}", st(&last), cur_freq());
    let lastmed = { let mut l = last.clone(); l.sort_by(|a, b| a.partial_cmp(b).unwrap()); l[10] };
    let mut ramp = -1.0; for w in series.windows(5) { if w.iter().all(|x| x.1 < lastmed * 1.1) { ramp = w[0].0; break; } }
    println!("ramp reached steady speed at t={:.0}ms (final unit {:.0}us)", ramp, lastmed);
    let mut buckets = String::new(); let mut i = 0;
    for b in 0..10 { let lim = (b + 1) as f64 * 50.0; let mut xs = vec![]; while i < series.len() && series[i].0 < lim { xs.push(series[i].1); i += 1; } if !xs.is_empty() { xs.sort_by(|a, b| a.partial_cmp(b).unwrap()); buckets += &format!("{}ms:{:.0} ", lim, xs[xs.len() / 2]); } }
    println!("median unit per 50ms bucket: {}", buckets);
    let mut v = vec![]; for _ in 0..20 { sleep_ms(100); v.push(unit()); }
    println!("typing cadence right after burst: {} freq={}", st(&v), cur_freq());
    let t0 = Instant::now(); let mut log = String::new();
    for _ in 0..16 { sleep_ms(500); let f = cur_freq(); log += &format!("{:.1}s:{} ", t0.elapsed().as_secs_f64(), &f[..f.len().saturating_sub(3)]); if f == "600000" { break; } }
    println!("hold after burst: {}", log);
}

fn efd() -> i32 { unsafe { libc::eventfd(0, 0) } }
fn sec_handoff() {
    println!("## handoff: main -> worker (eventfd) -> main (poll on eventfd)");
    let base = Instant::now();
    for (label, mp, wp) in [("unpinned", None, None), ("pinned main=cpu0 worker=cpu1", Some(0), Some(1)), ("pinned same cpu0", Some(0), Some(0))] {
        let (a, b) = (efd(), efd());
        let tw = Arc::new(AtomicU64::new(0)); let stop = Arc::new(AtomicBool::new(false));
        let h = { let (tw, stop) = (tw.clone(), stop.clone()); std::thread::spawn(move || { pin(wp); let mut buf = 0u64; loop {
            unsafe { libc::read(a, &mut buf as *mut u64 as *mut _, 8) }; tw.store(base.elapsed().as_nanos() as u64, Ordering::SeqCst);
            if stop.load(Ordering::SeqCst) { break; }
            let one = 1u64; unsafe { libc::write(b, &one as *const u64 as *const _, 8) }; } }) };
        pin(mp);
        for (mode, iters, gap) in [("hot", 500, 0u64), ("cold(20ms gap)", 150, 20)] {
            let (mut wake, mut rt) = (vec![], vec![]);
            for _ in 0..iters {
                if gap > 0 { sleep_ms(gap); }
                let t0 = base.elapsed().as_nanos() as u64; let one = 1u64;
                unsafe { libc::write(a, &one as *const u64 as *const _, 8) };
                let mut p = libc::pollfd { fd: b, events: libc::POLLIN, revents: 0 }; unsafe { libc::poll(&mut p, 1, -1) };
                let mut buf = 0u64; unsafe { libc::read(b, &mut buf as *mut u64 as *mut _, 8) };
                let t2 = base.elapsed().as_nanos() as u64;
                wake.push((tw.load(Ordering::SeqCst) - t0) as f64 / 1e3); rt.push((t2 - t0) as f64 / 1e3);
            }
            println!("eventfd {:30} {:15} wake: {} | roundtrip: {}", label, mode, st(&wake), st(&rt));
        }
        stop.store(true, Ordering::SeqCst); let one = 1u64; unsafe { libc::write(a, &one as *const u64 as *const _, 8) }; h.join().unwrap();
        unsafe { libc::close(a); libc::close(b); }
        pin(None);
    }
    let (tx, rx) = mpsc::channel::<u64>(); let (tx2, rx2) = mpsc::channel::<u64>();
    let h = std::thread::spawn(move || { while let Ok(v) = rx.recv() { if v == 0 { break; } tx2.send(base.elapsed().as_nanos() as u64).unwrap(); } });
    for (mode, iters, gap) in [("hot", 500, 0u64), ("cold(20ms gap)", 150, 20)] {
        let (mut wake, mut rt) = (vec![], vec![]);
        for _ in 0..iters { if gap > 0 { sleep_ms(gap); } let t0 = base.elapsed().as_nanos() as u64; tx.send(1).unwrap(); let w = rx2.recv().unwrap(); let t2 = base.elapsed().as_nanos() as u64; wake.push((w - t0) as f64 / 1e3); rt.push((t2 - t0) as f64 / 1e3); }
        println!("mpsc(futex) unpinned {:15} wake: {} | roundtrip: {}", mode, st(&wake), st(&rt));
    }
    tx.send(0).unwrap(); h.join().unwrap();
}

fn sec_jitter() {
    println!("## jitter: sleep(5ms) overshoot, and poll(timerfd)");
    let mut v = vec![];
    for _ in 0..400 { let t = Instant::now(); std::thread::sleep(Duration::from_millis(5)); v.push(us(t.elapsed()) - 5000.0); }
    println!("nanosleep 5ms overshoot {}", st(&v));
    unsafe {
        let tfd = libc::timerfd_create(libc::CLOCK_MONOTONIC, 0);
        let mut v = vec![];
        for _ in 0..200 {
            let its = libc::itimerspec { it_interval: libc::timespec { tv_sec: 0, tv_nsec: 0 }, it_value: libc::timespec { tv_sec: 0, tv_nsec: 33_000_000 } };
            let t = Instant::now(); libc::timerfd_settime(tfd, 0, &its, std::ptr::null_mut());
            let mut p = libc::pollfd { fd: tfd, events: libc::POLLIN, revents: 0 }; libc::poll(&mut p, 1, -1);
            let mut b = 0u64; libc::read(tfd, &mut b as *mut u64 as *mut _, 8);
            v.push(us(t.elapsed()) - 33000.0);
        }
        println!("timerfd 33ms + poll overshoot {}", st(&v));
        libc::close(tfd);
    }
}

fn rss_kb() -> u64 { let s = std::fs::read_to_string("/proc/self/statm").unwrap(); s.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap() * 4 }
fn sec_mem() {
    println!("## mem: thread cost");
    let r0 = rss_kb(); let stop = Arc::new(Barrier::new(5)); let mut hs = vec![];
    let t = Instant::now();
    for _ in 0..4 { let s = stop.clone(); hs.push(std::thread::spawn(move || { s.wait(); })); }
    let sp = us(t.elapsed()); sleep_ms(50); let r1 = rss_kb();
    println!("spawn 4 threads {:.0}us, RSS {}KB -> {}KB (+{}KB)", sp, r0, r1, r1 - r0);
    stop.wait(); for h in hs { h.join().unwrap(); }
    unsafe { let mut rl: libc::rlimit = std::mem::zeroed(); libc::getrlimit(libc::RLIMIT_RTPRIO, &mut rl); println!("RLIMIT_RTPRIO cur={} max={}", rl.rlim_cur, rl.rlim_max);
        libc::getrlimit(libc::RLIMIT_NICE, &mut rl); println!("RLIMIT_NICE cur={} max={}", rl.rlim_cur, rl.rlim_max); }
}

fn spi_tx() -> u64 { std::fs::read_to_string("/sys/class/spi_master/spi0/statistics/bytes_tx").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0) }
fn sec_fb() {
    println!("## fb0: does fsync block the caller? do writes block during transfer?");
    const LEN: usize = W * H * 2; const ROW: usize = W * 2;
    unsafe {
        let fd = libc::open(b"/dev/fb0\0".as_ptr() as *const _, libc::O_RDWR);
        if fd < 0 { println!("fb0 open failed"); return; }
        let p = libc::mmap(std::ptr::null_mut(), LEN, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0) as *mut u8;
        if p as isize == -1 { println!("mmap failed"); return; }
        let saved = std::slice::from_raw_parts(p, LEN).to_vec();
        let s0 = spi_tx(); sleep_ms(1000); let bg = (spi_tx() - s0) as f64; println!("background SPI rate {:.0} bytes/s (APPLaunch)", bg);
        let wr = |off: usize, len: usize| { std::ptr::copy_nonoverlapping(saved.as_ptr().add(off), p.add(off), len); };
        for (name, off, len) in [("1 row", 80 * ROW, ROW), ("13 rows(1 text line)", 78 * ROW, 13 * ROW), ("full", 0, LEN)] {
            let (mut w, mut f, mut done) = (vec![], vec![], vec![]);
            for _ in 0..25 {
                sleep_ms(120);
                let t0 = Instant::now(); wr(off, len); let t1 = Instant::now(); libc::fsync(fd); let t2 = Instant::now();
                w.push(us(t1 - t0)); f.push(us(t2 - t1));
                let mut last = spi_tx(); let mut lastchg = Instant::now(); let mut changed = false;
                loop { std::thread::sleep(Duration::from_micros(300)); let s = spi_tx(); if s != last { last = s; lastchg = Instant::now(); changed = true; }
                    if (changed && lastchg.elapsed() > Duration::from_millis(3)) || t2.elapsed() > Duration::from_millis(80) { break; } }
                done.push(if changed { us(lastchg - t2) } else { -1.0 });
            }
            println!("fb {:22} write: {} | fsync: {} | spi last activity after fsync return: {}", name, st(&w), st(&f), st(&done));
        }
        let mut v = vec![];
        for _ in 0..8 { sleep_ms(120); wr(0, LEN); libc::fsync(fd);
            for i in 0..30 { let t = Instant::now(); wr((i % 4) * 40 * ROW, ROW); v.push(us(t.elapsed())); std::thread::sleep(Duration::from_micros(500)); } }
        println!("fb row write while full transfer in flight: {}", st(&v));
        let mut v = vec![];
        for _ in 0..15 { sleep_ms(120); wr(0, LEN); libc::fsync(fd); let t = Instant::now(); wr(80 * ROW, ROW); libc::fsync(fd); v.push(us(t.elapsed())); }
        println!("fb 1-row write+fsync issued right after full fsync: {}", st(&v));
        let stop = Arc::new(AtomicBool::new(false)); let sp = Ptr(p); let sv = saved.clone();
        let h = { let stop = stop.clone(); std::thread::spawn(move || { let sp = sp; let mut n = 0; while !stop.load(Ordering::Relaxed) { std::ptr::copy_nonoverlapping(sv.as_ptr(), sp.0, LEN); libc::fsync(fd); n += 1; std::thread::sleep(Duration::from_millis(2)); } n }) };
        let mut v = vec![]; let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(400) { let t = Instant::now(); wr(100 * ROW, ROW); v.push(us(t.elapsed())); std::thread::sleep(Duration::from_micros(300)); }
        stop.store(true, Ordering::Relaxed); let n: i32 = h.join().unwrap();
        println!("fb main row writes while flusher thread did {} full write+fsync in 400ms: {}", n, st(&v));
        sleep_ms(200);
        let s0 = spi_tx(); let t0 = Instant::now(); let mut v = vec![];
        for _ in 0..12 { let t = Instant::now(); wr(0, LEN); libc::fsync(fd); v.push(us(t.elapsed())); }
        let burst = t0.elapsed().as_secs_f64(); sleep_ms(300); let el = t0.elapsed().as_secs_f64();
        let d = (spi_tx() - s0) as f64 - bg * el;
        println!("fb burst 12x full write+fsync: per-iter {} total {:.1}ms; SPI bytes above background {:.0} = {:.1} frames", st(&v), burst * 1e3, d, d / LEN as f64);
        wr(0, LEN); libc::fsync(fd);
        libc::munmap(p as *mut _, LEN); libc::close(fd);
    }
}

fn sec_sd(path: &str) {
    println!("## sd: 64KB write + fsync ({})", path);
    use std::io::Write;
    let data = vec![0x5au8; 64 << 10]; let (mut w, mut f) = (vec![], vec![]);
    for i in 0..10 {
        let t0 = Instant::now(); let mut fl = std::fs::File::create(path).unwrap(); fl.write_all(&data[..data.len() - i]).unwrap(); let t1 = Instant::now(); fl.sync_all().unwrap(); let t2 = Instant::now();
        w.push(us(t1 - t0)); f.push(us(t2 - t1)); sleep_ms(200);
    }
    let _ = std::fs::remove_file(path);
    println!("sd write: {} | fsync: {}", st(&w), st(&f));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |s: &str| args.iter().any(|a| a == s || a == "all");
    if has("mem") { sec_mem(); }
    if has("freq") { sec_freq(); }
    if has("handoff") { sec_handoff(); }
    if has("jitter") { sec_jitter(); }
    if has("pool") { sec_pool(); }
    if has("scale") { sec_scale(); }
    if has("fb") { sec_fb(); }
    if has("ramp") { extra::sec_ramp(); }
    if has("fb2") { extra::sec_fb2(); }
    for (i, a) in args.iter().enumerate() { if a == "pf" { extra::sec_pf(&args[i + 1]); } }
    if let Some(i) = args.iter().position(|a| a == "sd") { sec_sd(&args[i + 1]); }
}
