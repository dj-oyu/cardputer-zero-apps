//! 課題 D3 の受け入れ試験: 根の探し方・除外・ハッシュの覚え・2 秒の決まり・上限・read_blob。
//! 一時フォルダは試験ごとに別（並列に走っても消し合わない）。

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use zc_remote::scan::{apply_overlay, find_project, read_blob, resolve_overlay, HashCache, Project, ProjectError, ScanError, Shape};
use zc_wire::limits::{CHUNK_BYTES, MAX_FILE_BYTES, MAX_TREE_FILES};
use zc_wire::msg::Lang;

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("zc-scan-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        Tmp(fs::canonicalize(&d).unwrap())
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
    fn write(&self, rel: &str, data: impl AsRef<[u8]>) -> PathBuf {
        let p = self.p(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, data).unwrap();
        p
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
}

/// 「書いてから十分たった」時刻（2 秒の決まりに当たらない）。
fn later() -> u64 {
    now_ns() + 60_000_000_000
}

fn tree(root: &Path, lang: Lang) -> Project {
    Project { root: root.to_path_buf(), id: "0".repeat(16), lang, entry: None, shape: Shape::Tree }
}

fn paths(cache: &mut HashCache, p: &Project) -> Vec<String> {
    let (m, _) = cache.scan(p, later()).unwrap();
    m.files().iter().map(|f| f.path.clone()).collect()
}

fn sha_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

// ---- 根の探し方 ----

#[test]
fn rust_nearest_cargo_toml() {
    let t = Tmp::new("rust-near");
    t.write("foo/Cargo.toml", "[package]\nname = \"foo\"\n");
    let f = t.write("foo/src/main.rs", "fn main() {}\n");
    let p = find_project(&f, Lang::Rust).unwrap();
    assert_eq!(p.root, t.p("foo"));
    assert_eq!(p.shape, Shape::Tree);
    assert_eq!(p.entry, None);
    assert_eq!(p.lang, Lang::Rust);
    assert_eq!(p.id, zc_wire::tree::project_id(t.p("foo").to_str().unwrap().as_bytes()));
}

#[test]
fn rust_workspace_above_wins() {
    let t = Tmp::new("rust-ws");
    t.write("ws/Cargo.toml", "# 根\n[workspace] # コメント\nmembers = [\"crates/*\"]\n");
    t.write("ws/crates/a/Cargo.toml", "[package]\nname = \"a\"\n");
    let f = t.write("ws/crates/a/src/lib.rs", "");
    let p = find_project(&f, Lang::Rust).unwrap();
    assert_eq!(p.root, t.p("ws"));
    assert_eq!(p.shape, Shape::Tree);
    // `[workspace.dependencies]` だけの書き方も workspace とみなす。
    t.write("ws/Cargo.toml", "[workspace.dependencies]\nx = \"1\"\n");
    assert_eq!(find_project(&f, Lang::Rust).unwrap().root, t.p("ws"));
    // 上に workspace でない Cargo.toml があっても、いちばん近いもののまま。
    t.write("ws/Cargo.toml", "[package]\nname = \"outer\"\n# [workspace]\n");
    assert_eq!(find_project(&f, Lang::Rust).unwrap().root, t.p("ws/crates/a"));
}

#[test]
fn rust_single_file() {
    let t = Tmp::new("rust-single");
    let a = t.write("x/a.rs", "fn main() {}\n");
    let b = t.write("x/b.rs", "fn main() {}\n");
    let pa = find_project(&a, Lang::Rust).unwrap();
    let pb = find_project(&b, Lang::Rust).unwrap();
    assert_eq!(pa.shape, Shape::SingleFile);
    assert_eq!(pa.root, t.p("x"));
    assert_eq!(pa.entry.as_deref(), Some("a.rs"));
    // 同じフォルダでもファイルごとに別のプロジェクト。
    assert_ne!(pa.id, pb.id);
    t.write("x/other.rs", "");
    let (m, _) = HashCache::new().scan(&pa, later()).unwrap();
    assert_eq!(m.files().len(), 1);
    assert_eq!(m.files()[0].path, "a.rs");
}

#[test]
fn c_flat_folder() {
    let t = Tmp::new("c-flat");
    let f = t.write("c/main.c", "int main(void){return 0;}\n");
    t.write("c/util.h", "");
    t.write("c/util.c", "");
    t.write("c/README.md", "");
    t.write("c/sub/deep.c", "");
    t.write("c/.gitignore", "util.h\n");
    let p = find_project(&f, Lang::C).unwrap();
    assert_eq!(p.root, t.p("c"));
    assert_eq!(p.shape, Shape::Flat);
    assert_eq!(p.entry.as_deref(), Some("main.c"));
    assert_eq!(paths(&mut HashCache::new(), &p), ["main.c", "util.c"]);
}

#[test]
fn node_needs_dependencies() {
    let t = Tmp::new("node");
    t.write("app/package.json", r#"{"name":"app","dependencies":{}}"#);
    let f = t.write("app/src/main.ts", "");
    assert_eq!(find_project(&f, Lang::Node), Err(ProjectError::NotApplicable));
    // 依存が空のものは飛ばして上を探す（npm の workspaces の根）。
    t.write("package.json", r#"{"private":true,"devDependencies":{"typescript":"7"}}"#);
    let p = find_project(&f, Lang::Node).unwrap();
    assert_eq!(p.root, t.0);
    assert_eq!(p.entry.as_deref(), Some("app/src/main.ts"));
    // いちばん近いものに依存があればそちら。
    t.write("app/package.json", r#"{"dependencies":{"left-pad":"1"}}"#);
    let p = find_project(&f, Lang::Node).unwrap();
    assert_eq!(p.root, t.p("app"));
    assert_eq!(p.entry.as_deref(), Some("src/main.ts"));
    assert_eq!(p.shape, Shape::Tree);
    // 壊れた package.json は依存なしと同じ。
    t.write("app/package.json", "{");
    assert_eq!(find_project(&f, Lang::Node).unwrap().root, t.0);
}

#[test]
fn node_without_package_json() {
    let t = Tmp::new("node-none");
    let f = t.write("a.js", "");
    assert_eq!(find_project(&f, Lang::Node), Err(ProjectError::NotApplicable));
}

// ---- 除外 ----

#[test]
fn excludes_dirs_and_symlinks() {
    let t = Tmp::new("excl");
    t.write("Cargo.toml", "[package]\n");
    t.write("src/main.rs", "");
    t.write("src/build/mod.rs", ""); // 根の直下でない build/ は送る
    t.write("target/debug/x", "");
    t.write("node_modules/a/index.js", "");
    t.write("sub/node_modules/b.js", "");
    t.write(".git/HEAD", "");
    t.write(".idea/x", "");
    t.write("dist/a.js", "");
    t.write("build/out", "");
    t.write(".cargo/config.toml", "");
    t.write(".github/workflows/ci.yml", "");
    t.write(".env.example", ""); // `.` で始まるファイルは送る
    symlink(t.p("src/main.rs"), t.p("link.rs")).unwrap();
    symlink(t.p("src"), t.p("linkdir")).unwrap();
    symlink("/etc", t.p("outside")).unwrap();
    let got = paths(&mut HashCache::new(), &tree(&t.0, Lang::Rust));
    assert_eq!(
        got,
        [".cargo/config.toml", ".env.example", ".github/workflows/ci.yml", "Cargo.toml", "src/build/mod.rs", "src/main.rs"]
    );
}

#[test]
fn gitignore_forms() {
    let t = Tmp::new("gitignore");
    t.write(
        ".gitignore",
        "# コメント\n*.log\n!keep.log\n/only-root.txt\ngen/\ndocs/**/*.tmp\n**/cache\nsecret?.txt\n*.py[cod]\n",
    );
    for f in [
        "a.log",
        "x/b.log",
        "keep.log",
        "only-root.txt",
        "x/only-root.txt",
        "gen/a.rs",
        "x/gen/b.rs",
        "gen.rs",
        "docs/a/b/c.tmp",
        "docs/c.tmp",
        "docs/c.md",
        "a/cache/x",
        "cache",
        "secret1.txt",
        "secret12.txt",
        "m.pyc",
        "m.py",
    ] {
        t.write(f, "");
    }
    let got = paths(&mut HashCache::new(), &tree(&t.0, Lang::Rust));
    assert_eq!(
        got,
        [".gitignore", "docs/c.md", "gen.rs", "keep.log", "m.py", "secret12.txt", "x/only-root.txt"]
    );
}

#[test]
fn nested_gitignore_is_not_read() {
    let t = Tmp::new("gitignore-nested");
    t.write("sub/.gitignore", "*\n");
    t.write("sub/a.rs", "");
    assert_eq!(paths(&mut HashCache::new(), &tree(&t.0, Lang::Rust)), ["sub/.gitignore", "sub/a.rs"]);
}

// ---- ハッシュの覚え ----

#[test]
fn second_scan_reads_nothing_and_change_reads_one() {
    let t = Tmp::new("cache");
    for i in 0..10 {
        t.write(&format!("src/f{i}.rs"), format!("// {i}\n"));
    }
    let p = tree(&t.0, Lang::Rust);
    let mut c = HashCache::new();
    let (m1, s1) = c.scan(&p, later()).unwrap();
    assert_eq!(s1.files, 10);
    assert_eq!(s1.hashed_files, 10);
    assert_eq!(c.len(), 10);
    let (m2, s2) = c.scan(&p, later()).unwrap();
    assert_eq!(s2.hashed_files, 0);
    assert_eq!(s2.hashed_bytes, 0);
    assert_eq!(m1.tree_id(), m2.tree_id());

    t.write("src/f3.rs", "// changed\n");
    let (m3, s3) = c.scan(&p, later()).unwrap();
    assert_eq!(s3.hashed_files, 1);
    assert_eq!(s3.hashed_bytes, 11);
    assert_ne!(m3.tree_id(), m2.tree_id());
    let f3 = m3.files().iter().find(|f| f.path == "src/f3.rs").unwrap();
    assert_eq!(f3.sha256, sha_hex(b"// changed\n"));
    assert_eq!(f3.size, 11);

    // 消えたファイルは目録からも覚えからも消える。
    fs::remove_file(t.p("src/f0.rs")).unwrap();
    let (m4, s4) = c.scan(&p, later()).unwrap();
    assert_eq!(s4.hashed_files, 0);
    assert_eq!(m4.files().len(), 9);
    assert_eq!(c.len(), 9);
}

#[test]
fn same_size_same_mtime_rewrite_within_two_seconds() {
    let t = Tmp::new("two-sec");
    let f = t.write("a.rs", "aaaa");
    let mtime = fs::metadata(&f).unwrap().modified().unwrap();
    let p = tree(&t.0, Lang::Rust);
    let mut c = HashCache::new();
    let at = |d: Duration| (mtime + d).duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;

    // 書いた直後の走査。
    let (m1, _) = c.scan(&p, at(Duration::from_millis(100))).unwrap();
    assert_eq!(m1.files()[0].sha256, sha_hex(b"aaaa"));

    // 同じ大きさ・同じ mtime・同じ inode で中身だけ変える（同じ時刻の中の 2 回目の書き込み）。
    {
        use std::io::Write;
        let mut w = fs::OpenOptions::new().write(true).open(&f).unwrap();
        w.write_all(b"bbbb").unwrap();
        w.set_modified(mtime).unwrap();
    }
    assert_eq!(fs::metadata(&f).unwrap().modified().unwrap(), mtime);

    // 2 秒以内なら、覚えと stat が同じでも読み直す。
    let (m2, s2) = c.scan(&p, at(Duration::from_millis(1500))).unwrap();
    assert_eq!(s2.hashed_files, 1);
    assert_eq!(m2.files()[0].sha256, sha_hex(b"bbbb"));
    // 2 秒を過ぎたら読まない。
    let (_, s3) = c.scan(&p, at(Duration::from_secs(3))).unwrap();
    assert_eq!(s3.hashed_files, 0);
    // mtime が未来（時計が戻った）なら読み直す。
    let (_, s4) = c.scan(&p, at(Duration::ZERO).saturating_sub(5_000_000_000)).unwrap();
    assert_eq!(s4.hashed_files, 1);
}

#[test]
fn exec_bit_and_cache_follows_mode() {
    let t = Tmp::new("exec");
    let f = t.write("run.sh", "#!/bin/sh\n");
    let p = tree(&t.0, Lang::C);
    let mut c = HashCache::new();
    let (m, _) = c.scan(&p, later()).unwrap();
    assert!(!m.files()[0].exec);
    fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
    let (m, s) = c.scan(&p, later()).unwrap();
    assert!(m.files()[0].exec);
    assert_eq!(s.hashed_files, 0);
}

#[test]
fn japanese_and_empty_files() {
    let t = Tmp::new("ja");
    t.write("src/日本語.rs", "// こんにちは\n");
    t.write("empty.txt", "");
    let (m, _) = HashCache::new().scan(&tree(&t.0, Lang::Rust), later()).unwrap();
    let names: Vec<_> = m.files().iter().map(|f| (f.path.as_str(), f.size)).collect();
    assert_eq!(names, [("empty.txt", 0), ("src/日本語.rs", "// こんにちは\n".len() as u64)]);
    assert_eq!(m.files()[0].sha256, sha_hex(b""));
}

#[test]
fn unsendable_name_is_an_error() {
    let t = Tmp::new("badname");
    t.write("a\\b.rs", "");
    assert!(matches!(HashCache::new().scan(&tree(&t.0, Lang::Rust), later()), Err(ScanError::Io(_))));
}

// ---- 上限 ----

#[test]
fn file_too_large() {
    let t = Tmp::new("big");
    let f = fs::File::create(t.p("big.bin")).unwrap();
    f.set_len(MAX_FILE_BYTES + 1).unwrap();
    match HashCache::new().scan(&tree(&t.0, Lang::Rust), later()) {
        Err(ScanError::TooLarge(msg)) => assert!(msg.contains("big.bin"), "{msg}"),
        other => panic!("{other:?}"),
    }
    // ちょうど上限は通る。
    f.set_len(MAX_FILE_BYTES).unwrap();
    assert!(HashCache::new().scan(&tree(&t.0, Lang::Rust), later()).is_ok());
}

#[test]
fn too_many_files_and_total() {
    let t = Tmp::new("many");
    for i in 0..=MAX_TREE_FILES {
        t.write(&format!("d{}/f{i}", i % 20), "");
    }
    match HashCache::new().scan(&tree(&t.0, Lang::Rust), later()) {
        Err(ScanError::TooLarge(msg)) => assert!(msg.contains("2000"), "{msg}"),
        other => panic!("{other:?}"),
    }
    let t = Tmp::new("total");
    for i in 0..9 {
        fs::File::create(t.p(&format!("f{i}"))).unwrap().set_len(4 << 20).unwrap();
    }
    match HashCache::new().scan(&tree(&t.0, Lang::Rust), later()) {
        Err(ScanError::TooLarge(msg)) => assert!(msg.contains("32MiB"), "{msg}"),
        other => panic!("{other:?}"),
    }
}

// ---- read_blob ----

fn collect(root: &Path, rel: &str, size: u64, sha: &str) -> (std::io::Result<bool>, Vec<u8>, Vec<u64>) {
    let mut data = Vec::new();
    let mut offs = Vec::new();
    let r = read_blob(root, rel, size, sha, &mut |off, c| {
        offs.push(off);
        data.extend_from_slice(c);
    });
    (r, data, offs)
}

#[test]
fn read_blob_chunks_and_verifies() {
    let t = Tmp::new("blob");
    let body: Vec<u8> = (0..CHUNK_BYTES * 2 + 100).map(|i| (i * 7 % 251) as u8).collect();
    t.write("src/b.bin", &body);
    let (m, _) = HashCache::new().scan(&tree(&t.0, Lang::Rust), later()).unwrap();
    let e = &m.files()[0];
    let (r, data, offs) = collect(&t.0, &e.path, e.size, &e.sha256);
    assert!(r.unwrap());
    assert_eq!(data, body);
    assert_eq!(offs, [0, CHUNK_BYTES as u64, 2 * CHUNK_BYTES as u64]);

    // 走査のあとで書き換えた（同じ大きさ）→ false。
    let mut changed = body.clone();
    changed[5] ^= 1;
    t.write("src/b.bin", &changed);
    assert!(!collect(&t.0, &e.path, e.size, &e.sha256).0.unwrap());
    // 大きさが変わった・消えた・リンクに変わった → false。
    t.write("src/b.bin", b"short");
    assert!(!collect(&t.0, &e.path, e.size, &e.sha256).0.unwrap());
    fs::remove_file(t.p("src/b.bin")).unwrap();
    assert!(!collect(&t.0, &e.path, e.size, &e.sha256).0.unwrap());
    t.write("other", &body);
    symlink(t.p("other"), t.p("src/b.bin")).unwrap();
    assert!(!collect(&t.0, &e.path, e.size, &e.sha256).0.unwrap());
    // 目録のパスでないものは読まない。
    assert!(collect(&t.0, "../x", 0, "").0.is_err());
    assert!(collect(&t.0, "/etc/passwd", 0, "").0.is_err());
}

#[test]
fn read_blob_empty_file() {
    let t = Tmp::new("blob-empty");
    t.write("e", "");
    let (r, data, offs) = collect(&t.0, "e", 0, &sha_hex(b""));
    assert!(r.unwrap());
    assert!(data.is_empty());
    assert!(offs.is_empty());
}

// ---- 未保存の中身の重ね（打鍵が止まったときの check） ----

#[test]
fn overlay_replaces_the_disk_entry_without_touching_the_cache() {
    let t = Tmp::new("overlay");
    t.write("Cargo.toml", "[package]\n");
    t.write(".gitignore", "secret.rs\n");
    let main = t.write("src/main.rs", "fn main() {}\n");
    t.write("src/secret.rs", "x\n");
    let p = tree(&t.0, Lang::Rust);
    let unsaved = b"fn main() { let x: i32 = \"s\"; }\n".to_vec();
    let files = vec![
        (main.clone(), unsaved.clone()),
        // 除外されたもの・ディスクに無いもの・根の外のものは使わない
        (t.p("src/secret.rs"), b"y".to_vec()),
        (t.p("src/new.rs"), b"z".to_vec()),
        (std::env::temp_dir().join("outside.rs"), b"o".to_vec()),
    ];
    let ov = resolve_overlay(&p, files);
    assert_eq!(ov.iter().map(|o| o.rel.as_str()).collect::<Vec<_>>(), ["src/main.rs", "src/secret.rs", "src/new.rs"]);
    assert_eq!(ov[0].sha256, sha_hex(&unsaved));

    let mut cache = HashCache::new();
    let (m, _) = cache.scan(&p, later()).unwrap();
    let m2 = apply_overlay(m.clone(), &ov).unwrap();
    let entry = |m: &zc_wire::tree::Manifest, path: &str| m.files().iter().find(|f| f.path == path).cloned();
    let e = entry(&m2, "src/main.rs").unwrap();
    assert_eq!((e.size, e.sha256.as_str()), (unsaved.len() as u64, sha_hex(&unsaved).as_str()));
    assert_eq!(entry(&m2, "Cargo.toml"), entry(&m, "Cargo.toml"));
    assert!(entry(&m2, "src/secret.rs").is_none() && entry(&m2, "src/new.rs").is_none());
    assert_ne!(m2.tree_id(), m.tree_id());
    // 覚えはディスクの中身のまま: 次の走査（重ね無し）はディスクの SHA-256
    let (m3, st) = cache.scan(&p, later()).unwrap();
    assert_eq!(entry(&m3, "src/main.rs").unwrap().sha256, sha_hex(b"fn main() {}\n"));
    assert_eq!(st.hashed_files, 0);
    // 当たる重ねが無ければ、同じ目録をそのまま返す
    assert!(std::sync::Arc::ptr_eq(&apply_overlay(m.clone(), &ov[1..]).unwrap(), &m));
}
