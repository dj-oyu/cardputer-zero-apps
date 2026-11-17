//! 中身の置き場と作業の木の受け入れ試験（課題 B2。docs/stage3-design.md §4.2）。
//!
//! 目録は [`Manifest::new`]（課題 W2）で作る。

use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use zc_buildd::store::{Store, StoreError, SyncStats};
use zc_wire::msg::FileEntry;
use zc_wire::tree::Manifest;

const P: &str = "0123456789abcdef";

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!("zc-store-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// 目録を作り、足りない中身を置き場に入れる（実機が送る代わり）。
fn manifest(store: &Store, files: &[(&str, &[u8], bool)]) -> Manifest {
    let entries: Vec<FileEntry> = files.iter().map(|(p, b, x)| FileEntry { path: p.to_string(), size: b.len() as u64, sha256: sha(b), exec: *x }).collect();
    let m = Manifest::new(entries).unwrap();
    for h in store.missing(&m) {
        let (_, body, _) = files.iter().find(|(_, b, _)| sha(b) == h).unwrap();
        let mut w = store.begin_blob(&h, body.len() as u64).unwrap();
        // 刻んで送る
        let mut off = 0;
        for chunk in body.chunks(7) {
            w.write(off, chunk).unwrap();
            off += chunk.len() as u64;
        }
        w.finish().unwrap();
    }
    assert!(store.missing(&m).is_empty());
    m
}

fn mtimes(dir: &Path) -> Vec<(PathBuf, i64, i64, u64)> {
    let mut v = Vec::new();
    fn walk(d: &Path, v: &mut Vec<(PathBuf, i64, i64, u64)>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let md = std::fs::symlink_metadata(e.path()).unwrap();
            if md.is_dir() {
                walk(&e.path(), v);
            } else {
                v.push((e.path(), md.mtime(), md.mtime_nsec(), md.ino()));
            }
        }
    }
    walk(dir, &mut v);
    v.sort();
    v
}

#[test]
fn second_sync_writes_nothing_and_keeps_mtimes() {
    let t = Tmp::new("again");
    let s = Store::open(&t.0).unwrap();
    let m = manifest(&s, &[("Cargo.toml", b"[package]\n", false), ("src/main.rs", b"fn main() {}\n", false), ("src/日本語.rs", b"// x\n", false)]);
    assert_eq!(s.sync_tree(P, &m).unwrap(), SyncStats { written: 3, removed: 0, unchanged: 0 });
    let before = mtimes(&s.src_dir(P));
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(s.sync_tree(P, &m).unwrap(), SyncStats { written: 0, removed: 0, unchanged: 3 });
    assert_eq!(mtimes(&s.src_dir(P)), before);
    assert_eq!(std::fs::read(s.src_dir(P).join("src/main.rs")).unwrap(), b"fn main() {}\n");
}

#[test]
fn only_changed_file_is_written_and_removed_file_disappears() {
    let t = Tmp::new("change");
    let s = Store::open(&t.0).unwrap();
    let m1 = manifest(&s, &[("a.rs", b"a1", false), ("b.rs", b"b1", false), ("sub/c.rs", b"c1", false)]);
    s.sync_tree(P, &m1).unwrap();
    let src = s.src_dir(P);
    let b_before = std::fs::metadata(src.join("b.rs")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let m2 = manifest(&s, &[("a.rs", b"a2", false), ("b.rs", b"b1", false)]);
    assert_eq!(s.sync_tree(P, &m2).unwrap(), SyncStats { written: 1, removed: 1, unchanged: 1 });
    assert_eq!(std::fs::read(src.join("a.rs")).unwrap(), b"a2");
    let b_after = std::fs::metadata(src.join("b.rs")).unwrap();
    assert_eq!((b_before.mtime(), b_before.mtime_nsec()), (b_after.mtime(), b_after.mtime_nsec()));
    assert!(!src.join("sub").exists(), "空になったフォルダも消える");
}

#[test]
fn files_made_by_tools_are_removed_and_edits_are_undone() {
    let t = Tmp::new("tools");
    let s = Store::open(&t.0).unwrap();
    let m = manifest(&s, &[("Cargo.toml", b"x", false), ("src/main.rs", b"fn main() {}\n", false)]);
    s.sync_tree(P, &m).unwrap();
    let src = s.src_dir(P);
    // 道具が木の中に作ったもの・書き換えたもの（stat が変わる）
    std::fs::write(src.join("Cargo.lock"), "lock").unwrap();
    std::fs::create_dir_all(src.join("gen")).unwrap();
    std::fs::write(src.join("gen/out.rs"), "x").unwrap();
    std::fs::write(src.join("src/main.rs"), "fn main() { evil() }\n").unwrap();
    let st = s.sync_tree(P, &m).unwrap();
    assert_eq!(st.written, 1);
    assert_eq!(st.removed, 2);
    assert!(!src.join("Cargo.lock").exists() && !src.join("gen").exists());
    assert_eq!(std::fs::read(src.join("src/main.rs")).unwrap(), b"fn main() {}\n");
}

#[test]
fn bad_blob_is_not_stored() {
    let t = Tmp::new("badblob");
    let s = Store::open(&t.0).unwrap();
    let h = sha(b"right");
    let mut w = s.begin_blob(&h, 5).unwrap();
    w.write(0, b"wrong").unwrap();
    assert!(matches!(w.finish(), Err(StoreError::Mismatch)));
    // 大きさが違う・順番が違う・大きすぎる・ハッシュの形でない
    let mut w = s.begin_blob(&h, 5).unwrap();
    w.write(0, b"rig").unwrap();
    assert!(matches!(w.finish(), Err(StoreError::Mismatch)));
    let mut w = s.begin_blob(&h, 5).unwrap();
    assert!(matches!(w.write(1, b"x"), Err(StoreError::Mismatch)));
    assert!(matches!(w.write(0, b"right!"), Err(StoreError::TooLarge)));
    drop(w);
    assert!(matches!(s.begin_blob(&h, zc_wire::limits::MAX_FILE_BYTES + 1), Err(StoreError::TooLarge)));
    assert!(matches!(s.begin_blob("../../etc/passwd", 1), Err(StoreError::Mismatch)));
    let m = Manifest::new(vec![FileEntry { path: "a".into(), size: 5, sha256: h.clone(), exec: false }]).unwrap();
    assert_eq!(s.missing(&m), vec![h]);
    // 中身がそろっていない木は写せない
    assert!(matches!(s.sync_tree(P, &m), Err(StoreError::Mismatch)));
    // 途中で捨てた一時ファイルが残らない
    assert_eq!(std::fs::read_dir(t.0.join("blobs/tmp")).unwrap().count(), 0);
}

#[test]
fn symlink_made_by_tool_does_not_lead_outside() {
    let t = Tmp::new("symlink");
    let outside = Tmp::new("outside");
    let s = Store::open(&t.0).unwrap();
    let m1 = manifest(&s, &[("keep.rs", b"k", false)]);
    s.sync_tree(P, &m1).unwrap();
    let src = s.src_dir(P);
    // 道具が、木の中に外を指すリンクを作った
    std::os::unix::fs::symlink(&outside.0, src.join("sub")).unwrap();
    std::os::unix::fs::symlink(outside.0.join("f.rs"), src.join("f.rs")).unwrap();
    let m2 = manifest(&s, &[("keep.rs", b"k", false), ("sub/x.rs", b"x", false), ("f.rs", b"f", false)]);
    s.sync_tree(P, &m2).unwrap();
    assert_eq!(std::fs::read_dir(&outside.0).unwrap().count(), 0, "外に書いた");
    assert!(std::fs::symlink_metadata(src.join("sub")).unwrap().is_dir());
    assert!(std::fs::symlink_metadata(src.join("f.rs")).unwrap().is_file());
    assert_eq!(std::fs::read(src.join("sub/x.rs")).unwrap(), b"x");
    // 作業の木そのものがリンクにされていても
    std::fs::remove_dir_all(&src).unwrap();
    std::os::unix::fs::symlink(&outside.0, &src).unwrap();
    s.sync_tree(P, &m2).unwrap();
    assert_eq!(std::fs::read_dir(&outside.0).unwrap().count(), 0, "外に書いた");
    // 変な project id でも置き場の外を指さない
    let pd = s.project_dir("../../evil");
    assert!(pd.starts_with(t.0.join("proj")) && pd.parent() == Some(t.0.join("proj").as_path()));
}

#[test]
fn lost_memo_still_gives_right_tree() {
    let t = Tmp::new("memo");
    let s = Store::open(&t.0).unwrap();
    let m = manifest(&s, &[("a.rs", b"a", false), ("b.rs", b"b", false)]);
    s.sync_tree(P, &m).unwrap();
    let before = mtimes(&s.src_dir(P));
    // 覚えが消えた → 読み直してハッシュ。同じなら書かない
    std::fs::remove_file(s.project_dir(P).join("tree.json")).unwrap();
    assert_eq!(s.sync_tree(P, &m).unwrap(), SyncStats { written: 0, removed: 0, unchanged: 2 });
    assert_eq!(mtimes(&s.src_dir(P)), before);
    // 壊れた覚えでも
    std::fs::write(s.project_dir(P).join("tree.json"), "{not json").unwrap();
    std::fs::write(s.src_dir(P).join("b.rs"), "B").unwrap();
    assert_eq!(s.sync_tree(P, &m).unwrap(), SyncStats { written: 1, removed: 0, unchanged: 1 });
    assert_eq!(std::fs::read(s.src_dir(P).join("b.rs")).unwrap(), b"b");
}

#[test]
fn same_content_twice_empty_file_and_exec_bit() {
    let t = Tmp::new("misc");
    let s = Store::open(&t.0).unwrap();
    let m = manifest(&s, &[("a.txt", b"same", false), ("b/a.txt", b"same", false), ("empty", b"", false), ("run.sh", b"#!/bin/sh\n", true)]);
    let st = s.sync_tree(P, &m).unwrap();
    assert_eq!(st.written, 4);
    let src = s.src_dir(P);
    assert_eq!(std::fs::read(src.join("b/a.txt")).unwrap(), b"same");
    assert_eq!(std::fs::metadata(src.join("empty")).unwrap().len(), 0);
    assert_eq!(std::fs::metadata(src.join("run.sh")).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(std::fs::metadata(src.join("a.txt")).unwrap().permissions().mode() & 0o777, 0o644);
    // 実行権限だけ変わった: 書き直さず（mtime そのまま）権限を合わせる
    let before = std::fs::metadata(src.join("run.sh")).unwrap();
    let m2 = manifest(&s, &[("a.txt", b"same", false), ("b/a.txt", b"same", false), ("empty", b"", false), ("run.sh", b"#!/bin/sh\n", false)]);
    assert_eq!(s.sync_tree(P, &m2).unwrap().written, 0);
    let after = std::fs::metadata(src.join("run.sh")).unwrap();
    assert_eq!(after.permissions().mode() & 0o777, 0o644);
    assert_eq!((before.mtime(), before.mtime_nsec()), (after.mtime(), after.mtime_nsec()));
    // 同じ中身は置き場に 1 つ
    let blobs = std::fs::read_dir(t.0.join("blobs")).unwrap().flatten().filter(|e| e.file_name() != "tmp").map(|e| std::fs::read_dir(e.path()).unwrap().count()).sum::<usize>();
    assert_eq!(blobs, 3);
}

#[test]
fn corrupted_blob_is_detected_while_copying() {
    let t = Tmp::new("corrupt");
    let s = Store::open(&t.0).unwrap();
    let m = manifest(&s, &[("a.rs", b"good", false)]);
    let h = sha(b"good");
    std::fs::write(t.0.join("blobs").join(&h[..2]).join(&h), "evil").unwrap();
    assert!(matches!(s.sync_tree(P, &m), Err(StoreError::Mismatch)));
    // 壊れた中身は消えて、もう一度頼まれる
    assert_eq!(s.missing(&m), vec![h]);
    assert!(!s.src_dir(P).join("a.rs").exists());
}

#[test]
fn garbage_collection_by_quota() {
    let t = Tmp::new("gc");
    let s = Store::open(&t.0).unwrap();
    let big_a = vec![b'a'; 200_000];
    let big_b = vec![b'b'; 200_000];
    let ma = manifest(&s, &[("a.bin", &big_a, false)]);
    s.sync_tree("aaaaaaaaaaaaaaaa", &ma).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mb = manifest(&s, &[("b.bin", &big_b, false)]);
    s.sync_tree("bbbbbbbbbbbbbbbb", &mb).unwrap();
    s.out_dir("0123456789abcdef0123456789abcdef").unwrap();
    // 上限に余裕があれば、どちらも残る（指されている中身も）
    assert_eq!(s.collect_garbage(10 << 20).unwrap(), 0);
    assert!(s.src_dir("aaaaaaaaaaaaaaaa").join("a.bin").exists());
    // 上限を下回るまで古い方から消す。新しい方は残る。消した木だけが指していた中身も消える
    let freed = s.collect_garbage(500_000).unwrap();
    assert!(freed >= 400_000, "{freed}");
    assert!(!s.project_dir("aaaaaaaaaaaaaaaa").exists());
    assert!(s.src_dir("bbbbbbbbbbbbbbbb").join("b.bin").exists());
    assert_eq!(s.missing(&ma), vec![sha(&big_a)]);
    assert!(s.missing(&mb).is_empty());
}
