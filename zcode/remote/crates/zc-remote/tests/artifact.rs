//! 課題 D4 の受け入れ試験: 成果物の受け取り（tmpfs）と SD の控え。置き場は試験ごとの一時フォルダ。
//!
//! 正解の zstd の流れは `tests/data/` に、`zstd` コマンド（1.5.7）で作って置いてある:
//!
//! | ファイル | 作り方 | 中身 |
//! |---|---|---|
//! | `lines.zst` | `zstd -19 --zstd=wlog=21` | [`lines`]（394280 バイト）。大きさとチェックサムつき |
//! | `lines-nocheck.zst` | `zstd -3 --no-check --no-content-size` | 同じ。窓の記述子つき・チェックサムなし |
//! | `zeros3m.zst` | `zstd -19` | 0 が 3MiB（窓 3MiB = 上限を超える） |
//! | `empty.zst` | `zstd -19` | 空 |

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use zc_remote::artifact::{find_placed, ArtifactError, Dirs, LastGood, Meta, Placed, Receiver, BIN_QUOTA, LASTGOOD_MAX};
use zc_wire::limits::{CHUNK_BYTES, MAX_ARTIFACT_BYTES};
use zc_wire::msg::ArtifactKind;

const P1: &str = "1111111111111111";
const P2: &str = "2222222222222222";
const TREE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("zc-artifact-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("run")).unwrap();
        Tmp(d)
    }
    fn dirs(&self) -> Dirs {
        Dirs { run: self.0.join("run"), cache: self.0.join("cache") }
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn data(name: &str) -> Vec<u8> {
    fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)).unwrap()
}

/// `lines.zst` の中身（tests/data を作ったときと同じ式）。
fn lines() -> Vec<u8> {
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let mut s = String::new();
    for i in 0..16000 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        s.push_str(&format!("line {i} value {}\n", x >> 40));
    }
    s.into_bytes()
}

fn sha_hex(d: &[u8]) -> String {
    Sha256::digest(d).iter().map(|b| format!("{b:02x}")).collect()
}

fn meta(project: &str, tree: &str, name: &str, content: &[u8], z: &[u8]) -> Meta {
    Meta {
        project: project.into(),
        tree: tree.into(),
        kind: ArtifactKind::Exe,
        name: name.into(),
        sha256: sha_hex(content),
        bytes: content.len() as u64,
        zbytes: z.len() as u64,
    }
}

/// `step` バイトずつ書いて置く。
fn receive(dirs: &Dirs, m: Meta, z: &[u8], step: usize) -> Result<Placed, ArtifactError> {
    let mut r = Receiver::begin(dirs, m, 1)?;
    for (i, c) in z.chunks(step).enumerate() {
        r.write((i * step) as u64, c)?;
    }
    r.finish()
}

/// 置き場の名前の一覧（並べたもの）。
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn fixture_matches_generator() {
    let l = lines();
    assert_eq!(l.len(), 394280);
    assert_eq!(sha_hex(&l), "dc98db6b3f29644612e79d37d6d826cdc4002f646683b4a3417d709423c4c777");
}

#[test]
fn any_chunking_gives_the_same_result() {
    let content = lines();
    for (file, steps) in [("lines.zst", &[1usize, 3, 4, 5, 4096, CHUNK_BYTES, 1 << 20][..]), ("lines-nocheck.zst", &[1, 7, CHUNK_BYTES][..])] {
        let z = data(file);
        for &step in steps {
            let t = Tmp::new(&format!("chunk-{file}-{step}"));
            let d = t.dirs();
            let p = receive(&d, meta(P1, TREE, "foo", &content, &z), &z, step).unwrap_or_else(|e| panic!("{file} {step}: {e:?}"));
            assert_eq!(p.path, d.run.join("foo-01234567"));
            assert_eq!(p.kind, ArtifactKind::Exe);
            assert_eq!(fs::read(&p.path).unwrap(), content, "{file} {step}");
            assert_eq!(mode(&p.path), 0o700);
            assert_eq!(find_placed(&d, P1), Some(p));
            // 残るのは、置いたもの・最新を指すリンク・圧縮された写しだけ。
            assert_eq!(names(&d.run), [format!(".cur-{P1}"), format!(".zst-{P1}"), "foo-01234567".into()]);
        }
    }
}

#[test]
fn shared_placed_name_is_not_removed_under_another_project() {
    let t = Tmp::new("shared");
    let d = t.dirs();
    let content = lines();
    let z = data("lines.zst");
    // 2 つのプロジェクトが同じ木（同じ名前・同じ tree id）から同じ成果物を受け取った。
    let p1 = receive(&d, meta(P1, TREE, "foo", &content, &z), &z, CHUNK_BYTES).unwrap();
    let p2 = receive(&d, meta(P2, TREE, "foo", &content, &z), &z, CHUNK_BYTES).unwrap();
    assert_eq!(p1.path, p2.path);
    // P1 だけ次の版になっても、P2 の指す先は消さない。
    let (c, zz) = compressible(3000, 2);
    receive(&d, meta(P1, &"ab".repeat(32), "foo", &c, &zz), &zz, CHUNK_BYTES).unwrap();
    assert_eq!(find_placed(&d, P2), Some(p2));
    assert_eq!(find_placed(&d, P1).unwrap().path, d.run.join("foo-abababab"));
    assert_eq!(find_placed(&d, "3333333333333333"), None);
}

#[test]
fn empty_artifact() {
    let t = Tmp::new("empty");
    let d = t.dirs();
    let z = data("empty.zst");
    let p = receive(&d, meta(P1, TREE, "e", b"", &z), &z, 1).unwrap();
    assert_eq!(fs::read(&p.path).unwrap(), b"");
}

#[test]
fn js_is_not_executable() {
    let t = Tmp::new("js");
    let d = t.dirs();
    let content = lines();
    let z = data("lines.zst");
    let mut m = meta(P1, TREE, "main.js", &content, &z);
    m.kind = ArtifactKind::Js;
    let p = receive(&d, m, &z, CHUNK_BYTES).unwrap();
    assert_eq!(p.kind, ArtifactKind::Js);
    assert_eq!(mode(&p.path), 0o600);
    assert_eq!(find_placed(&d, P1).unwrap().kind, ArtifactKind::Js);
}

#[test]
fn sha_mismatch_leaves_nothing() {
    let t = Tmp::new("mismatch");
    let d = t.dirs();
    let content = lines();
    let z = data("lines.zst");
    let mut m = meta(P1, TREE, "foo", &content, &z);
    m.sha256 = sha_hex(b"other");
    assert_eq!(receive(&d, m, &z, CHUNK_BYTES), Err(ArtifactError::Mismatch));
    assert!(names(&d.run).is_empty(), "{:?}", names(&d.run));
    assert_eq!(find_placed(&d, P1), None);

    // 大きさが違う（頭に大きさのある流れは、最初の書き込みで分かる）。
    let mut m = meta(P1, TREE, "foo", &content, &z);
    m.bytes -= 1;
    assert_eq!(receive(&d, m, &z, CHUNK_BYTES), Err(ArtifactError::Mismatch));
    assert!(names(&d.run).is_empty());
    // 頭に大きさの無い流れでも、超えた時点で止まる。
    let zn = data("lines-nocheck.zst");
    let mut m = meta(P1, TREE, "foo", &content, &zn);
    m.bytes -= 1000;
    m.sha256 = sha_hex(&content[..content.len() - 1000]);
    assert_eq!(receive(&d, m, &zn, CHUNK_BYTES), Err(ArtifactError::Mismatch));
    assert!(names(&d.run).is_empty());
}

#[test]
fn corrupt_and_truncated_streams() {
    let t = Tmp::new("corrupt");
    let d = t.dirs();
    let content = lines();
    let z = data("lines.zst");
    // 途中のバイトを壊す。
    let mut bad = z.clone();
    bad[z.len() / 2] ^= 0x55;
    let e = receive(&d, meta(P1, TREE, "foo", &content, &bad), &bad, CHUNK_BYTES).unwrap_err();
    assert!(matches!(e, ArtifactError::Decode(_) | ArtifactError::Mismatch), "{e:?}");
    assert!(names(&d.run).is_empty());
    // 最後の数バイト（チェックサム）が無い流れ。
    for cut in [1, 2, 4, 5] {
        let short = &z[..z.len() - cut];
        let e = receive(&d, meta(P1, TREE, "foo", &content, short), short, CHUNK_BYTES).unwrap_err();
        assert!(matches!(e, ArtifactError::Decode(_) | ArtifactError::Mismatch), "{cut}: {e:?}");
        assert!(names(&d.run).is_empty());
    }
    // フレームの後ろに余分なもの。
    let mut long = z.clone();
    long.extend_from_slice(b"junk");
    let e = receive(&d, meta(P1, TREE, "foo", &content, &long), &long, CHUNK_BYTES).unwrap_err();
    assert!(matches!(e, ArtifactError::Decode(_)), "{e:?}");
    assert!(names(&d.run).is_empty());
    // zstd でないもの。
    let junk = vec![0x42u8; 100];
    let e = receive(&d, meta(P1, TREE, "foo", &content, &junk), &junk, 10).unwrap_err();
    assert!(matches!(e, ArtifactError::Decode(_)), "{e:?}");
    assert!(names(&d.run).is_empty());
}

#[test]
fn window_over_2mib_is_refused() {
    let t = Tmp::new("window");
    let d = t.dirs();
    let content = vec![0u8; 3 << 20];
    let z = data("zeros3m.zst");
    let e = receive(&d, meta(P1, TREE, "z", &content, &z), &z, CHUNK_BYTES).unwrap_err();
    assert!(matches!(e, ArtifactError::Decode(ref s) if s.contains("窓")), "{e:?}");
    assert!(names(&d.run).is_empty());
}

#[test]
fn resume_after_drop_and_nothing_executable_meanwhile() {
    let t = Tmp::new("resume");
    let d = t.dirs();
    let content = lines();
    let z = data("lines.zst");
    let half = z.len() / 2 + 3;
    let mut r = Receiver::begin(&d, meta(P1, TREE, "foo", &content, &z), 1).unwrap();
    r.write(0, &z[..half]).unwrap();
    assert_eq!(r.received(), half as u64);
    // 途中: 実行できる名前・実行権限のあるファイルが無い。
    for n in names(&d.run) {
        assert!(n.starts_with('.'), "{n}");
        assert_eq!(mode(&d.run.join(&n)) & 0o111, 0, "{n}");
    }
    // 位置が飛んだ。
    assert!(matches!(r.write(half as u64 + 1, &z[half + 1..]), Err(ArtifactError::Invalid(_))));
    drop(r); // 切れた（serve が落ちた、など）

    // つなぎ直し（chan が変わってもよい）。
    let mut r = Receiver::begin(&d, meta(P1, TREE, "foo", &content, &z), 7).unwrap();
    assert_eq!(r.received(), half as u64);
    // 重なった書き込み（手前からもう一度）は、重なった分を捨てる。
    r.write(0, &z[..10]).unwrap();
    r.write(half as u64 - 100, &z[half - 100..half + 100]).unwrap();
    assert_eq!(r.received(), half as u64 + 100);
    // 全部届く前の finish は失敗し、途中のファイルは残る。
    let r2 = Receiver::begin(&d, meta(P1, TREE, "foo", &content, &z), 7).unwrap();
    assert_eq!(r2.received(), half as u64 + 100);
    assert!(matches!(r2.finish(), Err(ArtifactError::Invalid(_))));
    drop(r);
    let mut r = Receiver::begin(&d, meta(P1, TREE, "foo", &content, &z), 8).unwrap();
    let at = r.received() as usize;
    for (i, c) in z[at..].chunks(1000).enumerate() {
        r.write((at + i * 1000) as u64, c).unwrap();
    }
    let p = r.finish().unwrap();
    assert_eq!(fs::read(&p.path).unwrap(), content);
}

#[test]
fn abort_removes_partials_and_restarts_from_zero() {
    let t = Tmp::new("abort");
    let d = t.dirs();
    let content = lines();
    let z = data("lines.zst");
    let mut r = Receiver::begin(&d, meta(P1, TREE, "foo", &content, &z), 1).unwrap();
    r.write(0, &z[..1000]).unwrap();
    r.abort();
    assert!(names(&d.run).is_empty());
    let r = Receiver::begin(&d, meta(P1, TREE, "foo", &content, &z), 1).unwrap();
    assert_eq!(r.received(), 0);
    r.abort();
    // 別の成果物を始めると、同じプロジェクトの古い途中のものは消える。
    let mut r = Receiver::begin(&d, meta(P1, TREE, "foo", &content, &z), 1).unwrap();
    r.write(0, &z[..1000]).unwrap();
    drop(r);
    let other = data("empty.zst");
    let r = Receiver::begin(&d, meta(P1, TREE, "e", b"", &other), 1).unwrap();
    let s = &sha_hex(&content)[..16];
    assert!(!names(&d.run).iter().any(|n| n.contains(s)), "{:?}", names(&d.run));
    r.finish().unwrap_err();
}

#[test]
fn bad_meta_is_refused() {
    let t = Tmp::new("badmeta");
    let d = t.dirs();
    let z = data("empty.zst");
    let ok = meta(P1, TREE, "e", b"", &z);
    type Edit = Box<dyn Fn(&mut Meta)>;
    let cases: Vec<Edit> = vec![
        Box::new(|m| m.name = "a/b".into()),
        Box::new(|m| m.name = "..".into()),
        Box::new(|m| m.name = ".hidden".into()),
        Box::new(|m| m.name = String::new()),
        Box::new(|m| m.name = "a\nb".into()),
        Box::new(|m| m.project = "../x".into()),
        Box::new(|m| m.project = String::new()),
        Box::new(|m| m.tree = "abc".into()),
        Box::new(|m| m.tree = "../../../../etc".into()),
        Box::new(|m| m.sha256 = "zz".repeat(32)),
        Box::new(|m| m.sha256 = "AB".repeat(32)),
        Box::new(|m| m.bytes = MAX_ARTIFACT_BYTES + 1),
        Box::new(|m| m.zbytes = 1 << 20),
    ];
    for (i, f) in cases.iter().enumerate() {
        let mut m = ok.clone();
        f(&mut m);
        assert!(matches!(Receiver::begin(&d, m, 1), Err(ArtifactError::Invalid(_))), "case {i}");
    }
    assert!(names(&d.run).is_empty());
    // 圧縮後の大きさを超える書き込み。
    let mut r = Receiver::begin(&d, ok, 1).unwrap();
    assert!(matches!(r.write(0, &[0u8; 100]), Err(ArtifactError::Invalid(_))));
    assert!(names(&d.run).is_empty());
}

/// ruzstd で作った、`len` バイトのよく縮む中身とその流れ。
fn compressible(len: usize, seed: u8) -> (Vec<u8>, Vec<u8>) {
    let mut c = vec![0u8; len];
    for (i, b) in c.iter_mut().enumerate().step_by(4093) {
        *b = seed.wrapping_add(i as u8);
    }
    let z = ruzstd::encoding::compress_to_vec(&c[..], ruzstd::encoding::CompressionLevel::Fastest);
    (c, z)
}

fn age(p: &Path, secs_ago: u64) {
    let f = fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(secs_ago)).unwrap();
}

#[test]
fn quota_evicts_oldest_and_keeps_one_per_project() {
    let t = Tmp::new("quota");
    let d = t.dirs();
    let (c, z) = compressible(5 << 20, 1);
    let pa = receive(&d, meta("aa", TREE, "a", &c, &z), &z, CHUNK_BYTES).unwrap();
    let pb = receive(&d, meta("bb", TREE, "b", &c, &z), &z, CHUNK_BYTES).unwrap();
    let pc = receive(&d, meta("cc", TREE, "c", &c, &z), &z, CHUNK_BYTES).unwrap();
    age(&pa.path, 300);
    age(&pb.path, 200);
    age(&pc.path, 100);
    // 4 つ目: 5MiB × 3 + 5MiB は 16MiB に入らないので、いちばん古い a が消える。
    let pd = receive(&d, meta("dd", TREE, "d", &c, &z), &z, CHUNK_BYTES).unwrap();
    assert!(!pa.path.exists());
    assert_eq!(find_placed(&d, "aa"), None);
    assert!(!d.run.join(".cur-aa").exists());
    for p in [&pb, &pc, &pd] {
        assert!(p.path.exists(), "{p:?}");
    }
    let total: u64 = fs::read_dir(&d.run).unwrap().map(|e| e.unwrap().metadata().unwrap()).filter(|m| m.is_file()).map(|m| m.len()).sum();
    assert!(total <= BIN_QUOTA, "{total}");

    // 同じプロジェクトの次の版は前の版を置き換える（1 つだけ）。
    let tree2 = "fedcba9876543210".repeat(4);
    let (c2, z2) = compressible(1000, 9);
    let pb2 = receive(&d, meta("bb", &tree2, "b", &c2, &z2), &z2, CHUNK_BYTES).unwrap();
    assert_eq!(pb2.path, d.run.join("b-fedcba98"));
    assert!(!pb.path.exists());
    assert_eq!(find_placed(&d, "bb").unwrap().path, pb2.path);

    // 1 つで上限を超えるもの。
    let (c3, z3) = compressible(12 << 20, 3);
    let mut m = meta("ee", TREE, "e", &c3, &z3);
    m.zbytes = (5 << 20) as u64; // 12MiB + 5MiB > 16MiB
    let _ = z3;
    assert_eq!(Receiver::begin(&d, m, 1).err(), Some(ArtifactError::NoSpace));
}

#[test]
fn persist_restore_roundtrip() {
    let t = Tmp::new("persist");
    let d = t.dirs();
    let content = lines();
    let z = data("lines.zst");
    // 写しが無ければ何もしない。
    assert_eq!(LastGood::persist(&d, P1), Ok(false));
    assert_eq!(LastGood::restore(&d, P1), Ok(None));

    let p = receive(&d, meta(P1, TREE, "foo", &content, &z), &z, CHUNK_BYTES).unwrap();
    assert_eq!(LastGood::persist(&d, P1), Ok(true));
    let zcb = d.cache.join(format!("{P1}.zcb"));
    assert!(zcb.is_file());
    assert_eq!(mode(&zcb), 0o600);
    // SD の控えは、受け取った流れをそのまま持つ（圧縮し直さない）。
    let saved = fs::read(&zcb).unwrap();
    assert!(saved.starts_with(b"ZCB1"));
    assert!(saved.ends_with(&z));
    // tmpfs の写しは消える。
    assert!(!d.run.join(format!(".zst-{P1}")).exists());
    assert!(!names(&d.cache).iter().any(|n| n.starts_with(".tmp-")));

    // 再起動（tmpfs が消えた）→ 控えから戻す。
    fs::remove_dir_all(&d.run).unwrap();
    let r = LastGood::restore(&d, P1).unwrap().unwrap();
    assert_eq!(r, p);
    assert_eq!(fs::read(&r.path).unwrap(), content);
    assert_eq!(mode(&r.path), 0o700);
    assert_eq!(find_placed(&d, P1), Some(r));
    // 戻しただけでは写しを作らない（SD に書き直さない）。
    assert_eq!(LastGood::persist(&d, P1), Ok(false));
}

#[test]
fn same_content_is_not_written_again() {
    let t = Tmp::new("same");
    let d = t.dirs();
    let content = lines();
    let z = data("lines.zst");
    receive(&d, meta(P1, TREE, "foo", &content, &z), &z, CHUNK_BYTES).unwrap();
    assert_eq!(LastGood::persist(&d, P1), Ok(true));
    let zcb = d.cache.join(format!("{P1}.zcb"));
    let old = SystemTime::now() - Duration::from_secs(1000);
    fs::OpenOptions::new().write(true).open(&zcb).unwrap().set_modified(old).unwrap();
    // 別の tree（コメントだけ変えた、など）で同じ中身。
    let tree2 = "fedcba9876543210".repeat(4);
    receive(&d, meta(P1, &tree2, "foo", &content, &z), &z, 4096).unwrap();
    assert_eq!(LastGood::persist(&d, P1), Ok(false));
    assert_eq!(fs::metadata(&zcb).unwrap().modified().unwrap(), old);
    assert!(!d.run.join(format!(".zst-{P1}")).exists());
    // 中身が変われば書く。
    let (c2, z2) = compressible(5000, 4);
    receive(&d, meta(P1, &tree2, "foo", &c2, &z2), &z2, 4096).unwrap();
    assert_eq!(LastGood::persist(&d, P1), Ok(true));
    assert_ne!(fs::metadata(&zcb).unwrap().modified().unwrap(), old);
}

#[test]
fn broken_lastgood_is_removed() {
    let content = lines();
    let z = data("lines.zst");
    type Breaker = (&'static str, Box<dyn Fn(&mut Vec<u8>)>);
    let breakers: Vec<Breaker> = vec![
        ("magic", Box::new(|b| b[0] = b'X')),
        ("stream", Box::new(|b| {
            let n = b.len();
            b[n - 30000] ^= 0xff;
        })),
        ("truncated", Box::new(|b| b.truncate(b.len() - 10))),
        ("longer", Box::new(|b| b.extend_from_slice(b"xx"))),
        ("sha", Box::new(|b| {
            // 頭の SHA-256 の 1 バイト目（"ZCB1" + 種類 + 長さ + "foo" + u64 の後ろ）。
            b[4 + 1 + 1 + 3 + 8] ^= 1;
        })),
        ("empty", Box::new(|b| b.clear())),
    ];
    for (tag, f) in breakers {
        let t = Tmp::new(&format!("broken-{tag}"));
        let d = t.dirs();
        receive(&d, meta(P1, TREE, "foo", &content, &z), &z, CHUNK_BYTES).unwrap();
        assert_eq!(LastGood::persist(&d, P1), Ok(true));
        let zcb = d.cache.join(format!("{P1}.zcb"));
        let mut b = fs::read(&zcb).unwrap();
        f(&mut b);
        fs::write(&zcb, &b).unwrap();
        fs::remove_dir_all(&d.run).unwrap();
        assert_eq!(LastGood::restore(&d, P1), Ok(None), "{tag}");
        assert!(!zcb.exists(), "{tag}");
        assert_eq!(find_placed(&d, P1), None, "{tag}");
        // 途中のものも残さない。
        if d.run.exists() {
            assert!(names(&d.run).is_empty(), "{tag}: {:?}", names(&d.run));
        }
    }
}

#[test]
fn prune_keeps_newest() {
    let t = Tmp::new("prune");
    let cache = t.0.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let n = LASTGOOD_MAX + 3;
    for i in 0..n {
        let p = cache.join(format!("{:016x}.zcb", i));
        fs::write(&p, b"x").unwrap();
        age(&p, (1000 - i * 10) as u64);
    }
    fs::write(cache.join(".tmp-0000000000000001"), b"half").unwrap();
    fs::write(cache.join("other.txt"), b"keep").unwrap();
    LastGood::prune(&cache).unwrap();
    let left = names(&cache);
    assert_eq!(left.len(), LASTGOOD_MAX + 1, "{left:?}");
    assert!(left.contains(&"other.txt".to_string()));
    for i in 0..3 {
        assert!(!left.contains(&format!("{:016x}.zcb", i)), "{i}");
    }
    // 無いフォルダでもよい。
    LastGood::prune(&t.0.join("none")).unwrap();
}

#[test]
fn persist_prunes_to_limit() {
    let t = Tmp::new("persist-prune");
    let d = t.dirs();
    let (c, z) = compressible(2000, 5);
    for i in 0..LASTGOOD_MAX + 2 {
        let p = format!("{:016x}", i + 1);
        receive(&d, meta(&p, TREE, "x", &c, &z), &z, CHUNK_BYTES).unwrap();
        assert_eq!(LastGood::persist(&d, &p), Ok(true));
        age(&d.cache.join(format!("{p}.zcb")), (1000 - i * 10) as u64);
    }
    assert_eq!(names(&d.cache).len(), LASTGOOD_MAX);
}

#[test]
fn dirs_from_env() {
    let t = Tmp::new("env");
    // この試験だけが環境変数を読み書きする（ほかの試験は Dirs を直に作る）。
    std::env::set_var("XDG_RUNTIME_DIR", t.0.join("rt"));
    std::env::set_var("XDG_CACHE_HOME", t.0.join("ch"));
    let d = Dirs::from_env().unwrap();
    assert_eq!(d.run, t.0.join("rt/zcode/bin"));
    assert_eq!(d.cache, t.0.join("ch/zcode/bin"));
    assert!(d.run.is_dir() && d.cache.is_dir());
    assert_eq!(mode(&d.run), 0o700);
    std::env::remove_var("XDG_CACHE_HOME");
    std::env::set_var("HOME", t.0.join("home"));
    assert_eq!(Dirs::from_env().unwrap().cache, t.0.join("home/.cache/zcode/bin"));
}
