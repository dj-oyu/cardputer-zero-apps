//! 外部ビルド（zc-remote）の実機側の計測（docs/stage3-design.md §15、課題 M1）。
//!
//! 区分:
//!   sha256 [MiB] [回数]          SHA-256 の速さ（sha2 と ring。大きな 1 塊と、4KiB ごとのファイル）
//!   zstd [ファイル.zst] [回数]   ruzstd の展開の速さと、展開中に増える RSS（ファイルが無ければ自分の
//!                                実行ファイルを ruzstd の Fastest で圧縮して使う）
//!   scan [フォルダ] [--files N] [回数]
//!                                木の stat 走査（lstat + RAM の表との照合）と、全部を読んでハッシュ。
//!                                フォルダを渡せば読むだけ（書かない）。渡さなければ $TMPDIR か /tmp に
//!                                N 個（既定 2000）のファイルを作って測り、消す
//!   tls [回数]                   TLS 1.3 のハンドシェイクの CPU（同じプロセスの中でクライアントとサーバを
//!                                つなぐ。ChaCha20-Poly1305・X25519・ECDSA P-256 の 3 段の鎖）。完全と再開
//!   crypto [MiB]                 AEAD の速さ（ChaCha20-Poly1305 と AES-GCM、16KiB のレコード）・
//!                                X25519・ECDSA P-256 の検証の 1 回の時間
//!   all                          全部（既定の大きさ）
//!
//! 出力は 1 行 1 項目。CPU のクロック（cpu0 の scaling_cur_freq）を始めと終わりに出す。
//! ondemand ガバナが上がりきる前の値が混ざるので、1 回目と最良・中央値を分けて出す。

use std::fs;
use std::hint::black_box;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    println!("# remotebench {} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::ARCH);
    print_freq("start");
    match cmd {
        "sha256" => sha256(rest),
        "zstd" => zstd(rest),
        "scan" => scan(rest),
        "tls" => tls(rest),
        "crypto" => crypto(rest),
        "all" => {
            sha256(&[]);
            zstd(&[]);
            scan(&[]);
            tls(&[]);
            crypto(&[]);
        }
        _ => {
            eprintln!("使い方: remotebench <sha256|zstd|scan|tls|crypto|all> [引数]（先頭のコメント参照）");
            std::process::exit(2);
        }
    }
    print_freq("end");
    print_mem("end");
}

// ---- 共通 ----

fn arg_num(args: &[String], i: usize, default: usize) -> usize {
    args.get(i).and_then(|s| s.parse().ok()).unwrap_or(default)
}

fn print_freq(label: &str) {
    let f = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    match f {
        Some(khz) => println!("cpu0_freq_{label}: {} MHz", khz / 1000),
        None => println!("cpu0_freq_{label}: unknown"),
    }
}

/// /proc/self/status の値（kB）。
fn status_kb(key: &str) -> Option<u64> {
    let s = fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

fn print_mem(label: &str) {
    println!(
        "mem_{label}: VmRSS {} kB, VmHWM {} kB",
        status_kb("VmRSS:").unwrap_or(0),
        status_kb("VmHWM:").unwrap_or(0)
    );
}

/// ヒープの確保を数える（今の量と最大）。
mod heap {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    static CUR: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);

    struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = System.alloc(l);
            if !p.is_null() {
                let c = CUR.fetch_add(l.size(), Relaxed) + l.size();
                PEAK.fetch_max(c, Relaxed);
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            System.dealloc(p, l);
            CUR.fetch_sub(l.size(), Relaxed);
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            let q = System.realloc(p, l, new);
            if !q.is_null() {
                if new >= l.size() {
                    let c = CUR.fetch_add(new - l.size(), Relaxed) + (new - l.size());
                    PEAK.fetch_max(c, Relaxed);
                } else {
                    CUR.fetch_sub(l.size() - new, Relaxed);
                }
            }
            q
        }
    }

    #[global_allocator]
    static A: Counting = Counting;

    /// 最大を今の量に戻し、今の量を返す。
    pub fn reset_peak() -> usize {
        let c = CUR.load(Relaxed);
        PEAK.store(c, Relaxed);
        c
    }

    pub fn peak() -> usize {
        PEAK.load(Relaxed)
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn mbps(bytes: usize, d: Duration) -> f64 {
    bytes as f64 / 1e6 / d.as_secs_f64().max(1e-9)
}

/// 1 回目・最良・中央値・最悪（時間）。
struct Runs(Vec<Duration>);

impl Runs {
    fn sorted(&self) -> Vec<Duration> {
        let mut v = self.0.clone();
        v.sort();
        v
    }
    fn first(&self) -> Duration {
        self.0[0]
    }
    fn best(&self) -> Duration {
        self.sorted()[0]
    }
    fn median(&self) -> Duration {
        let v = self.sorted();
        v[v.len() / 2]
    }
    fn pct(&self, p: f64) -> Duration {
        let v = self.sorted();
        v[((v.len() - 1) as f64 * p).round() as usize]
    }
    fn worst(&self) -> Duration {
        *self.sorted().last().unwrap()
    }
}

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed())
}

/// 再現できる擬似乱数のバイト列（xorshift）。
fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut v = Vec::with_capacity(len + 8);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

/// ソースらしい中身（ASCII の行）。
fn source_like(len: usize, seed: u64) -> Vec<u8> {
    const WORDS: &[&str] = &[
        "fn", "let", "mut", "self", "return", "match", "if", "else", "for", "in", "impl", "pub", "struct",
        "Vec<u8>", "Option", "Some(x)", "None", "Ok(())", "=>", "{", "}", "(", ")", ";", "0", "1", "len",
        "buf", "path", "error", "//", "TODO",
    ];
    let r = pseudo_random(len, seed);
    let mut out = Vec::with_capacity(len + 16);
    let mut i = 0;
    while out.len() < len {
        let w = WORDS[r[i % r.len()] as usize % WORDS.len()];
        out.extend_from_slice(w.as_bytes());
        out.push(if r[(i + 1) % r.len()] % 9 == 0 { b'\n' } else { b' ' });
        i += 2;
    }
    out.truncate(len);
    out
}

// ---- SHA-256 ----

fn sha256(args: &[String]) {
    use sha2::Digest;
    let mib = arg_num(args, 0, 8).max(1);
    let runs = arg_num(args, 1, 5).max(1);
    let data = pseudo_random(mib << 20, 0x5eed);
    println!("== sha256: {mib} MiB x {runs} 回");

    type HashFn = fn(&[u8]) -> [u8; 32];
    let impls: [(&str, HashFn); 2] = [
        ("sha2", |b| sha2::Sha256::digest(b).into()),
        ("ring", |b| ring::digest::digest(&ring::digest::SHA256, b).as_ref().try_into().unwrap()),
    ];
    // 2 つの実装の答えが同じか
    let a = impls[0].1(&data[..4096]);
    let b = impls[1].1(&data[..4096]);
    assert_eq!(a, b, "sha2 と ring の答えが違う");

    for (name, f) in impls {
        // 大きな 1 塊（実行ファイルの検証・大きなファイル）
        let r = Runs((0..runs).map(|_| time(|| black_box(f(black_box(&data)))).1).collect());
        println!(
            "sha256_{name}_bulk: first {:.1} MB/s, best {:.1} MB/s, median {:.1} MB/s",
            mbps(data.len(), r.first()),
            mbps(data.len(), r.best()),
            mbps(data.len(), r.median())
        );
        // 4KiB ごとに別のハッシュ（ソースの木: 小さなファイルが多い）
        let r = Runs(
            (0..runs)
                .map(|_| {
                    time(|| {
                        for c in data.chunks(4096) {
                            black_box(f(black_box(c)));
                        }
                    })
                    .1
                })
                .collect(),
        );
        let files = data.len().div_ceil(4096);
        println!(
            "sha256_{name}_4k: best {:.1} MB/s ({:.1} us/ファイル), median {:.1} MB/s",
            mbps(data.len(), r.best()),
            r.best().as_secs_f64() * 1e6 / files as f64,
            mbps(data.len(), r.median())
        );
    }
}

// ---- zstd の展開 ----

fn zstd(args: &[String]) {
    use sha2::Digest;
    let file = args.first().filter(|a| a.parse::<usize>().is_err());
    let runs = arg_num(args, if file.is_some() { 1 } else { 0 }, 5).max(1);

    let (compressed, original, label) = match file {
        Some(p) => {
            let c = fs::read(p).unwrap_or_else(|e| panic!("{p} を読めない: {e}"));
            (c, None, format!("file {p}"))
        }
        None => {
            let exe = fs::read("/proc/self/exe").or_else(|_| fs::read(std::env::current_exe().unwrap())).unwrap();
            let (c, d) =
                time(|| ruzstd::encoding::compress_to_vec(&exe[..], ruzstd::encoding::CompressionLevel::Fastest));
            println!("zstd_encode_self (ruzstd Fastest, 参考): {:.0} ms", ms(d));
            (c, Some(exe), "self exe, ruzstd Fastest (≒ zstd -1)".to_string())
        }
    };
    println!("== zstd: {label}, 圧縮後 {} バイト, {runs} 回", compressed.len());

    // 64KiB ずつ読む（zc-remote の artifact.rs と同じく、全体をヒープに持たない形）
    let decode = |hash: bool| -> (usize, [u8; 32]) {
        let mut dec = ruzstd::decoding::StreamingDecoder::new(&compressed[..]).expect("zstd の頭が読めない");
        let mut buf = vec![0u8; 64 << 10];
        let mut h = sha2::Sha256::new();
        let mut n = 0;
        loop {
            let k = dec.read(&mut buf).expect("zstd の展開に失敗");
            if k == 0 {
                break;
            }
            if hash {
                h.update(&buf[..k]);
            }
            black_box(&buf[..k]);
            n += k;
        }
        (n, h.finalize().into())
    };

    // 展開中のヒープの最大（窓の大きさ + 読みの塊。RSS は解放済みのページを使い回すので、
    // ヒープの確保を数える）
    let base = heap::reset_peak();
    let ((out_len, digest), _) = time(|| decode(true));
    println!("zstd_decode_heap_peak: {} kB（展開中に増えたヒープの最大）", (heap::peak() - base) / 1024);
    if let Some(orig) = &original {
        assert_eq!(out_len, orig.len(), "展開した長さが違う");
        let want: [u8; 32] = sha2::Sha256::digest(orig).into();
        assert_eq!(digest, want, "展開した中身が違う");
    }
    println!("zstd_output_bytes: {out_len} (比 {:.2})", out_len as f64 / compressed.len() as f64);

    for (name, hash) in [("decode", false), ("decode_sha256", true)] {
        let r = Runs((0..runs).map(|_| time(|| black_box(decode(hash))).1).collect());
        println!(
            "zstd_{name}: first {:.1} ms, best {:.1} ms ({:.1} MB/s 展開後), median {:.1} ms",
            ms(r.first()),
            ms(r.best()),
            mbps(out_len, r.best()),
            ms(r.median())
        );
    }
}

// ---- 木の stat 走査 ----

/// zc-remote の scan.rs が覚える項目（パス → 大きさ・mtime・inode）。
#[derive(Clone, PartialEq)]
struct Meta {
    len: u64,
    mtime: (i64, i64),
    ino: u64,
}

/// 送らないフォルダ（docs/stage3-design.md §7.2。`.cargo/`・`.github/` は送る）。
fn skip_dir(name: &str) -> bool {
    matches!(name, "target" | "node_modules" | ".git" | "dist" | "build")
        || (name.starts_with('.') && name != ".cargo" && name != ".github")
}

fn walk(root: &Path, out: &mut Vec<(PathBuf, Meta)>) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir)? {
            let e = e?;
            let p = e.path();
            let m = fs::symlink_metadata(&p)?;
            let ft = m.file_type();
            if ft.is_dir() {
                if !skip_dir(&e.file_name().to_string_lossy()) {
                    stack.push(p);
                }
            } else if ft.is_file() {
                let meta = Meta { len: m.len(), mtime: (m.mtime(), m.mtime_nsec()), ino: m.ino() };
                out.push((p, meta));
            }
            // シンボリックリンクは送らない
        }
    }
    Ok(())
}

fn scan(args: &[String]) {
    use sha2::Digest;
    use std::collections::HashMap;
    let mut dir: Option<PathBuf> = None;
    let mut nfiles = 2000usize;
    let mut runs = 5usize;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--files" {
            nfiles = it.next().and_then(|s| s.parse().ok()).unwrap_or(nfiles);
        } else if let Ok(n) = a.parse::<usize>() {
            runs = n.max(1);
        } else {
            dir = Some(PathBuf::from(a));
        }
    }

    // 渡されなければ RAM（/tmp は Debian 13 では tmpfs）に作る。SD には書かない。
    let generated = dir.is_none();
    let root = match dir {
        Some(d) => d,
        None => {
            let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
            let root = base.join(format!("remotebench-scan-{}", std::process::id()));
            let (_, d) = time(|| gen_tree(&root, nfiles));
            println!("scan_generate: {nfiles} ファイル, {:.0} ms（{}）", ms(d), root.display());
            root
        }
    };
    println!("== scan: {}, {runs} 回", root.display());

    // 1 回目（作った直後か、ほかで読んだあとなら dentry は温かい。冷えた状態は README 参照）
    let mut table: HashMap<PathBuf, Meta> = HashMap::new();
    let mut r = Vec::new();
    let mut count = 0;
    let mut bytes = 0u64;
    for i in 0..runs {
        let (changed, d) = time(|| {
            let mut v = Vec::with_capacity(table.len().max(64));
            walk(&root, &mut v).expect("走査に失敗");
            // RAM の表と照合する（食い違ったものが「読み直す」もの）
            let mut changed = 0usize;
            for (p, m) in &v {
                if table.get(p) != Some(m) {
                    changed += 1;
                }
            }
            if i == 0 {
                count = v.len();
                bytes = v.iter().map(|(_, m)| m.len).sum();
                table = v.into_iter().collect();
            }
            changed
        });
        if i == 0 {
            assert_eq!(changed, count);
        } else {
            assert_eq!(changed, 0, "走査の間に木が変わった");
        }
        r.push(d);
    }
    let r = Runs(r);
    let per = |d: Duration| d.as_secs_f64() * 1e6 / count.max(1) as f64;
    println!("scan_files: {count} ファイル, {bytes} バイト");
    println!(
        "scan_stat: first {:.1} ms ({:.1} us/ファイル), best {:.1} ms ({:.1} us/ファイル), median {:.1} ms",
        ms(r.first()),
        per(r.first()),
        ms(r.best()),
        per(r.best()),
        ms(r.median())
    );

    // 全部を読んでハッシュ（再起動後の初回 = 全部ハッシュし直す）
    let mut paths: Vec<&PathBuf> = table.keys().collect();
    paths.sort();
    let mut buf = vec![0u8; 64 << 10];
    let rh = Runs(
        (0..runs.min(3))
            .map(|_| {
                time(|| {
                    for p in &paths {
                        let mut f = fs::File::open(p).expect("読めない");
                        let mut h = sha2::Sha256::new();
                        loop {
                            let k = f.read(&mut buf).expect("読めない");
                            if k == 0 {
                                break;
                            }
                            h.update(&buf[..k]);
                        }
                        black_box(h.finalize());
                    }
                })
                .1
            })
            .collect(),
    );
    println!(
        "scan_read_hash: first {:.1} ms, best {:.1} ms ({:.1} MB/s)",
        ms(rh.first()),
        ms(rh.best()),
        mbps(bytes as usize, rh.best())
    );

    if generated {
        let _ = fs::remove_dir_all(&root);
    }
}

/// 試しの木を作る: 1 フォルダに最大 50 ファイル、2 段の入れ子。大きさは 200 バイト〜16KiB。
/// 除外されるフォルダ（target/・.git/）も少し作り、走査が中に入らないことを確かめる。
fn gen_tree(root: &Path, n: usize) {
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(root).expect("作れない");
    let sizes = pseudo_random(n * 2, 7);
    for i in 0..n {
        let d = root.join(format!("src{:02}", i / 500)).join(format!("m{:02}", (i / 50) % 10));
        if i % 50 == 0 {
            fs::create_dir_all(&d).expect("作れない");
        }
        let len = 200 + (u16::from_le_bytes([sizes[2 * i], sizes[2 * i + 1]]) as usize % (16 << 10));
        let mut f = fs::File::create(d.join(format!("f{i:05}.rs"))).expect("作れない");
        f.write_all(&source_like(len, i as u64 + 1)).expect("書けない");
    }
    for skip in ["target/debug", ".git/objects"] {
        let d = root.join(skip);
        fs::create_dir_all(&d).expect("作れない");
        for i in 0..100 {
            fs::write(d.join(format!("x{i}")), b"skip").expect("書けない");
        }
    }
}

// ---- TLS のハンドシェイク ----

struct Pki {
    roots: rustls::RootCertStore,
    chain: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
}

/// 根 → 中間 → 末端（どれも ECDSA P-256）。実物（Cloudflare の末端・中間）と同じく、
/// クライアントの署名の検証は 3 回（鎖 2 + CertificateVerify 1）。
fn make_pki() -> Pki {
    use rcgen::{BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair};
    let ca_params = |cn: &str| {
        let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
        p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        p.distinguished_name.push(DnType::CommonName, cn);
        p
    };
    let root_key = KeyPair::generate().unwrap();
    let root = ca_params("remotebench root").self_signed(&root_key).unwrap();
    let int_key = KeyPair::generate().unwrap();
    let int = ca_params("remotebench intermediate").signed_by(&int_key, &root, &root_key).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let mut lp = CertificateParams::new(vec!["bench.invalid".to_string()]).unwrap();
    lp.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = lp.signed_by(&leaf_key, &int, &int_key).unwrap();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(root.der().clone()).unwrap();
    Pki {
        roots,
        chain: vec![leaf.der().clone(), int.der().clone()],
        key: rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
    }
}

/// zc_remote::net::tls_config と同じ絞り方（TLS 1.3・X25519・ChaCha20-Poly1305 だけ）。
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    let mut p = rustls::crypto::ring::default_provider();
    p.cipher_suites.retain(|s| s.suite() == rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256);
    p.kx_groups.retain(|g| g.name() == rustls::NamedGroup::X25519);
    Arc::new(p)
}

/// 1 回のハンドシェイク。（クライアント側の時間, サーバ側の時間, 再開だったか）。
fn handshake(
    ccfg: &Arc<rustls::ClientConfig>,
    scfg: &Arc<rustls::ServerConfig>,
) -> (Duration, Duration, bool) {
    let name = rustls::pki_types::ServerName::try_from("bench.invalid").unwrap();
    let mut tc = Duration::ZERO;
    let mut ts = Duration::ZERO;
    let t = Instant::now();
    let mut client = rustls::ClientConnection::new(ccfg.clone(), name).unwrap(); // ClientHello と鍵共有を作る
    tc += t.elapsed();
    let t = Instant::now();
    let mut server = rustls::ServerConnection::new(scfg.clone()).unwrap();
    ts += t.elapsed();

    let mut buf = Vec::with_capacity(16 << 10);
    // 両方が黙るまで回す（ハンドシェイクのあとの NewSessionTicket も受け取る）
    for _ in 0..32 {
        let mut moved = false;
        buf.clear();
        let t = Instant::now();
        while client.wants_write() {
            client.write_tls(&mut buf).unwrap();
        }
        tc += t.elapsed();
        if !buf.is_empty() {
            moved = true;
            let t = Instant::now();
            let mut s = &buf[..];
            while !s.is_empty() {
                server.read_tls(&mut s).unwrap();
                server.process_new_packets().expect("サーバ側の失敗");
            }
            ts += t.elapsed();
        }
        buf.clear();
        let t = Instant::now();
        while server.wants_write() {
            server.write_tls(&mut buf).unwrap();
        }
        ts += t.elapsed();
        if !buf.is_empty() {
            moved = true;
            let t = Instant::now();
            let mut s = &buf[..];
            while !s.is_empty() {
                client.read_tls(&mut s).unwrap();
                client.process_new_packets().expect("クライアント側の失敗");
            }
            tc += t.elapsed();
        }
        if !moved && !client.is_handshaking() && !server.is_handshaking() {
            break;
        }
    }
    assert!(!client.is_handshaking() && !server.is_handshaking(), "ハンドシェイクが終わらない");
    let s = client.negotiated_cipher_suite().unwrap().suite();
    assert_eq!(s, rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256);
    let resumed = client.handshake_kind() == Some(rustls::HandshakeKind::Resumed);
    (tc, ts, resumed)
}

fn tls(args: &[String]) {
    let n = arg_num(args, 0, 50).max(2);
    let (pki, d) = time(make_pki);
    println!("== tls: TLS 1.3 / X25519 / ChaCha20-Poly1305 / ECDSA P-256（根→中間→末端）, {n} 回");
    println!("tls_make_certs (参考): {:.1} ms", ms(d));

    let p = provider();
    let scfg = Arc::new(
        rustls::ServerConfig::builder_with_provider(p.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(pki.chain.clone(), pki.key.clone_key())
            .unwrap(),
    );
    let client_cfg = |resume: bool| {
        let mut c = rustls::ClientConfig::builder_with_provider(p.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(pki.roots.clone())
            .with_no_client_auth();
        c.alpn_protocols = vec![b"http/1.1".to_vec()];
        if !resume {
            c.resumption = rustls::client::Resumption::disabled();
        }
        Arc::new(c)
    };

    for (label, resume) in [("full", false), ("resumed", true)] {
        let ccfg = client_cfg(resume);
        let mut tcs = Vec::new();
        let mut tss = Vec::new();
        let mut resumed = 0;
        // 再開の測定は、1 回目（完全）でチケットを受け取ってから数える
        let warm = if resume { 1 } else { 0 };
        for i in 0..n + warm {
            let (tc, ts, r) = handshake(&ccfg, &scfg);
            if i >= warm {
                tcs.push(tc);
                tss.push(ts);
                resumed += r as usize;
            }
        }
        let c = Runs(tcs);
        let s = Runs(tss);
        println!(
            "tls_{label}_client_ms: first {:.2}, min {:.2}, median {:.2}, p90 {:.2}, max {:.2}",
            ms(c.first()),
            ms(c.best()),
            ms(c.median()),
            ms(c.pct(0.9)),
            ms(c.worst())
        );
        println!(
            "tls_{label}_server_ms: first {:.2}, median {:.2}（参考: 実物のサーバは Cloudflare）",
            ms(s.first()),
            ms(s.median())
        );
        println!("tls_{label}_resumed: {resumed}/{n}");
    }

    // 1 回の完全なハンドシェイクの間に増えるヒープの最大（クライアントとサーバの合計。上限の目安）
    let ccfg = client_cfg(false);
    let base = heap::reset_peak();
    black_box(handshake(&ccfg, &scfg));
    println!("tls_full_heap_peak_client_plus_server: {} kB", (heap::peak() - base) / 1024);
}

// ---- 暗号の部品 ----

fn crypto(args: &[String]) {
    use ring::aead;
    let mib = arg_num(args, 0, 4).max(1);
    println!("== crypto: AEAD {mib} MiB（16KiB のレコード）, X25519, ECDSA P-256 の検証");
    let rng = ring::rand::SystemRandom::new();
    let data = pseudo_random(mib << 20, 3);
    let algs: [(&str, &'static aead::Algorithm); 3] = [
        ("chacha20_poly1305", &aead::CHACHA20_POLY1305),
        ("aes_128_gcm", &aead::AES_128_GCM),
        ("aes_256_gcm", &aead::AES_256_GCM),
    ];
    for (name, alg) in algs {
        let key = aead::LessSafeKey::new(aead::UnboundKey::new(alg, &vec![7u8; alg.key_len()]).unwrap());
        let mut rec = vec![0u8; 16 << 10];
        let r = Runs(
            (0..3)
                .map(|_| {
                    time(|| {
                        for (i, c) in data.chunks(16 << 10).enumerate() {
                            rec.clear();
                            rec.extend_from_slice(c);
                            let mut nonce = [0u8; 12];
                            nonce[4..].copy_from_slice(&(i as u64).to_be_bytes());
                            let tag = key
                                .seal_in_place_separate_tag(
                                    aead::Nonce::assume_unique_for_key(nonce),
                                    aead::Aad::empty(),
                                    &mut rec,
                                )
                                .unwrap();
                            let _ = black_box(tag);
                        }
                    })
                    .1
                })
                .collect(),
        );
        println!("aead_{name}_seal: best {:.1} MB/s, first {:.1} MB/s", mbps(data.len(), r.best()), mbps(data.len(), r.first()));
    }

    // X25519: 鍵を作って 1 回合意する（TLS のクライアントがやること）
    let iters = 50;
    let peer = ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::X25519, &rng).unwrap();
    let peer_pub = peer.compute_public_key().unwrap();
    let r = Runs(
        (0..iters)
            .map(|_| {
                time(|| {
                    let k = ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::X25519, &rng).unwrap();
                    black_box(k.compute_public_key().unwrap());
                    let pk = ring::agreement::UnparsedPublicKey::new(&ring::agreement::X25519, peer_pub.as_ref());
                    ring::agreement::agree_ephemeral(k, &pk, |s| black_box(s.len())).unwrap();
                })
                .1
            })
            .collect(),
    );
    println!("x25519_keygen_agree_ms: first {:.3}, median {:.3}, min {:.3}", ms(r.first()), ms(r.median()), ms(r.best()));

    // ECDSA P-256 の検証（証明書の鎖と CertificateVerify）
    use ring::signature::{EcdsaKeyPair, KeyPair as _, UnparsedPublicKey, ECDSA_P256_SHA256_ASN1, ECDSA_P256_SHA256_ASN1_SIGNING};
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
    let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
    let msg = b"remotebench certificate verify";
    let sig = kp.sign(&rng, msg).unwrap();
    let pk = UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, kp.public_key().as_ref().to_vec());
    let r = Runs((0..iters).map(|_| time(|| pk.verify(msg, sig.as_ref()).unwrap()).1).collect());
    println!("ecdsa_p256_verify_ms: first {:.3}, median {:.3}, min {:.3}", ms(r.first()), ms(r.median()), ms(r.best()));
}
