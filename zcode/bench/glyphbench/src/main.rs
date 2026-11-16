use std::time::Instant;
use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::Format;
use swash::FontRef;

fn map(path: &str) -> &'static [u8] {
    unsafe {
        let cp = std::ffi::CString::new(path).unwrap();
        let fd = libc::open(cp.as_ptr(), libc::O_RDONLY);
        assert!(fd >= 0, "open {}", path);
        let len = libc::lseek(fd, 0, libc::SEEK_END) as usize;
        let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, fd, 0) as *const u8;
        std::slice::from_raw_parts(p, len)
    }
}
fn st(v: &[f64]) -> String {
    let mut s = v.to_vec(); s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| s[((s.len() - 1) as f64 * p).round() as usize];
    format!("n={} p50={:.0} p95={:.0} max={:.0} us, total={:.1}ms", s.len(), q(0.5), q(0.95), s[s.len() - 1], s.iter().sum::<f64>() / 1e3)
}
fn raster(font: FontRef, chars: &[char], size: f32, hint: bool) -> (Vec<f64>, usize) {
    let mut ctx = ScaleContext::new();
    let mut scaler = ctx.builder(font).size(size).hint(hint).build();
    let cm = font.charmap();
    let mut v = vec![]; let mut bytes = 0;
    for &ch in chars {
        let gid = cm.map(ch); if gid == 0 { continue; }
        let t = Instant::now();
        let img = Render::new(&[Source::Outline]).format(Format::Alpha).render(&mut scaler, gid);
        v.push(t.elapsed().as_secs_f64() * 1e6);
        if let Some(i) = img { bytes += i.data.len(); }
    }
    (v, bytes)
}
fn main() {
    let dj = map("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf");
    let cjk = map("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc");
    let t = Instant::now();
    let fdj = FontRef::from_index(dj, 0).unwrap();
    let fcjk = FontRef::from_index(cjk, 0).unwrap();
    println!("FontRef open both: {:.0}us", t.elapsed().as_secs_f64() * 1e6);
    let ascii: Vec<char> = (33u8..127).map(|c| c as char).collect();
    let kana: Vec<char> = (0x3041u32..0x3094).filter_map(char::from_u32).collect();
    let kanji: Vec<char> = (0..300u32).filter_map(|i| char::from_u32(0x4E00 + i * 37)).collect();
    for pass in 0..2 {
        let (v, b) = raster(fdj, &ascii, 11.0, true); println!("pass{} DejaVu ascii 11px hinted: {} cache={}B", pass, st(&v), b);
        let (v, b) = raster(fcjk, &kana, 12.0, false); println!("pass{} CJK kana 12px: {} cache={}B", pass, st(&v), b);
        let (v, b) = raster(fcjk, &kanji, 12.0, false); println!("pass{} CJK kanji 12px: {} cache={}B", pass, st(&v), b);
        let (v, b) = raster(fcjk, &kanji, 20.0, false); println!("pass{} CJK kanji 20px(heading): {} cache={}B", pass, st(&v), b);
    }
    let kanji2: Vec<char> = (0..300u32).filter_map(|i| char::from_u32(0x4E10 + i * 37)).collect();
    for n in [1usize, 2, 4] {
        let ks: Vec<char> = kanji2.iter().map(|&c| char::from_u32(c as u32 + n as u32).unwrap()).collect();
        let t = Instant::now();
        std::thread::scope(|s| { for part in ks.chunks((ks.len() + n - 1) / n) { s.spawn(move || { let f = FontRef::from_index(cjk, 0).unwrap(); raster(f, part, 12.0, false); }); } });
        println!("parallel raster 300 fresh kanji n={} wall={:.1}ms", n, t.elapsed().as_secs_f64() * 1e3);
    }
}
