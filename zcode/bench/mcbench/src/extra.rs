use super::*;

pub fn sec_ramp() {
    println!("## ramp: wait for 600MHz, then continuous load; unit = blend one full screen");
    let font = mk_font(); let text = mk_text(); let mut c = vec![0u16; W * H];
    let mut unit = || { let t = Instant::now(); for r in 0..ROWS { render_line(&mut c[r * CH * W..][..CH * W], &text[r * COLS..(r + 1) * COLS], &font); } us(t.elapsed()) };
    for trial in 0..3 {
        let t0 = Instant::now(); let mut ok = 0;
        while ok < 3 && t0.elapsed().as_secs() < 20 { sleep_ms(300); if cur_freq() == "600000" { ok += 1 } else { ok = 0 } }
        let waited = t0.elapsed().as_secs_f64();
        let t0 = Instant::now(); let mut series = vec![];
        while t0.elapsed() < Duration::from_millis(400) { let at = t0.elapsed().as_secs_f64() * 1e3; series.push((at, unit())); }
        let mut l: Vec<f64> = series.iter().rev().take(20).map(|x| x.1).collect(); l.sort_by(|a, b| a.partial_cmp(b).unwrap()); let fin = l[10];
        let mut ramp = -1.0; for w in series.windows(5) { if w.iter().all(|x| x.1 < fin * 1.15) { ramp = w[0].0; break; } }
        let mut buckets = String::new(); let mut i = 0;
        for b in 0..16 { let lim = (b + 1) as f64 * 25.0; let mut xs = vec![]; while i < series.len() && series[i].0 < lim { xs.push(series[i].1); i += 1; } if !xs.is_empty() { xs.sort_by(|a, b| a.partial_cmp(b).unwrap()); buckets += &format!("{}:{:.0} ", lim, xs[xs.len() / 2]); } }
        println!("trial {} (waited {:.1}s, ok={}) steady at t={:.0}ms final={:.0}us | median us per 25ms bucket: {}", trial, waited, ok, ramp, fin, buckets);
    }
}

pub fn sec_fb2() {
    println!("## fb2: SPI timeline after fsync, and fsync B issued d ms after fsync A");
    const LEN: usize = W * H * 2; const ROW: usize = W * 2;
    unsafe {
        let fd = libc::open(b"/dev/fb0\0".as_ptr() as *const _, libc::O_RDWR);
        if fd < 0 { println!("fb0 open failed"); return; }
        let p = libc::mmap(std::ptr::null_mut(), LEN, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0) as *mut u8;
        let saved = std::slice::from_raw_parts(p, LEN).to_vec();
        let s0 = spi_tx(); sleep_ms(1000); println!("background SPI rate {} bytes/s", spi_tx() - s0);
        let wr = |off: usize, len: usize| { std::ptr::copy_nonoverlapping(saved.as_ptr().add(off), p.add(off), len); };
        for (name, off, len) in [("1 row", 80 * ROW, ROW), ("13 rows", 78 * ROW, 13 * ROW), ("full", 0, LEN)] {
            let (mut first, mut last, mut bytes) = (vec![], vec![], vec![]);
            for trial in 0..12 {
                sleep_ms(150);
                let s0 = spi_tx(); wr(off, len); libc::fsync(fd); let t2 = Instant::now();
                let mut ev: Vec<(f64, u64)> = vec![]; let mut prev = s0;
                while t2.elapsed() < Duration::from_millis(30) { let s = spi_tx(); if s != prev { ev.push((us(t2.elapsed()), s - s0)); prev = s; } }
                if ev.is_empty() { continue; }
                first.push(ev[0].0); last.push(ev[ev.len() - 1].0); bytes.push(ev[ev.len() - 1].1 as f64);
                if trial < 2 { let e: Vec<String> = ev.iter().map(|x| format!("{:.0}us:{}", x.0, x.1)).collect(); println!("  timeline {} : {}", name, e.join(" ")); }
            }
            println!("fb2 {:8} first counter change: {} | last change: {} | bytes: {}", name, st(&first), st(&last), st(&bytes).replace(" us", ""));
        }
        for (aname, aoff, alen) in [("A=1 row", 20 * ROW, ROW), ("A=13 rows", 20 * ROW, 13 * ROW), ("A=full", 0, LEN)] {
            for d in [0u64, 1000, 3000, 6000, 10000, 16000] {
                let (mut w, mut f) = (vec![], vec![]);
                for _ in 0..8 {
                    sleep_ms(120);
                    wr(aoff, alen); libc::fsync(fd);
                    if d > 0 { std::thread::sleep(Duration::from_micros(d)); }
                    let t0 = Instant::now(); wr(160 * ROW, ROW); let t1 = Instant::now(); libc::fsync(fd); let t2 = Instant::now();
                    w.push(us(t1 - t0)); f.push(us(t2 - t1));
                }
                println!("fb2 {:10} then B=1 row after {:5}us: B write {} | B fsync {}", aname, d, st(&w), st(&f));
            }
        }
        wr(0, LEN); libc::fsync(fd);
        libc::munmap(p as *mut _, LEN); libc::close(fd);
    }
}

pub fn sec_pf(path: &str) {
    unsafe {
        let cp = std::ffi::CString::new(path).unwrap();
        let fd = libc::open(cp.as_ptr(), libc::O_RDONLY); if fd < 0 { println!("open failed {}", path); return; }
        let len = libc::lseek(fd, 0, libc::SEEK_END) as usize; let pages = (len + 4095) / 4096;
        let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, fd, 0) as *mut u8;
        let mut vec = vec![0u8; pages];
        libc::mincore(p as *mut _, len, vec.as_mut_ptr());
        let res0 = vec.iter().filter(|&&b| b & 1 == 1).count();
        let mut x = 0x2545F491u32; let mut v = vec![]; let mut tries = 0;
        while v.len() < 40 && tries < 20000 { tries += 1; x ^= x << 13; x ^= x >> 17; x ^= x << 5; let pg = x as usize % pages;
            let mut one = [0u8; 1]; libc::mincore(p.add(pg * 4096) as *mut _, 4096, one.as_mut_ptr());
            if one[0] & 1 == 1 { continue; }
            let t = Instant::now(); let b = std::ptr::read_volatile(p.add(pg * 4096)); v.push(us(t.elapsed())); std::hint::black_box(b); }
        libc::mincore(p as *mut _, len, vec.as_mut_ptr());
        let res1 = vec.iter().filter(|&&b| b & 1 == 1).count();
        println!("pf {} : {} pages, resident before {} ({:.0}%), after {} | cold page fault (random page): {}", path, pages, res0, res0 as f64 * 100.0 / pages as f64, res1, if v.is_empty() { "none cold".to_string() } else { st(&v) });
        libc::munmap(p as *mut _, len); libc::close(fd);
    }
}
