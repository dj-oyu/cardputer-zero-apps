//! 中身の置き場と、作業の木への写し（docs/stage3-design.md §4.2）。課題 B2。
//!
//! - 中身は内容（SHA-256）で引く: `blobs/<先頭 2 桁>/<sha256>`。受け取りは一時ファイルに
//!   書きながらハッシュを計算し、**合ったときだけ** rename で置く
//! - 作業の木（`proj/<project id>/src`）への写しは、**中身のハッシュが変わったファイルだけ
//!   書く**。変わっていないファイルには触らない（mtime を変えない → cargo が作り直さない）。
//!   目録に無くなったファイルは消す。実機の mtime は持ち込まない（実機には時計の電池が無い）
//!   - 前回写した目録（パス → SHA-256）を `proj/<project id>/tree.json` に覚えておき、それと
//!     比べる。覚えが無い・壊れているときは、木のファイルを読んでハッシュし直す
//!   - 書くのは一時ファイル → rename（途中で止まっても半端なファイルを残さない）
//!   - 置き場から木へはコピーする（ハードリンクにしない: 道具が木のファイルを書き換えると
//!     置き場の中身が壊れる）
//! - **木の外に書かない**: パスは [`zc_wire::tree::Manifest`] の検査を通ったものだけ。写す先の
//!   途中にシンボリックリンクがあったら（道具が作りうる）消してから作る
//! - 大きさの上限: 置き場と全プロジェクトの合計が [`crate::Config::disk_quota`] を超えたら、
//!   最後に使った時刻の古いプロジェクトから消す。どの木からも指されていない中身も消す
//!
//! 実装の決まり（B2 で決めたこと）:
//!
//! - `tree.json` には、写したファイルごとに SHA-256 と、**受付の側で**書いた・確かめた直後の
//!   `stat`（大きさ・mtime・inode）を残す。次の写しでは、`stat` が覚えと全部同じものだけ
//!   覚えのハッシュを使い、1 つでも違えば（道具が書き換えた・`cargo` が `Cargo.lock` を
//!   作り直した）読み直してハッシュする。**書くか書かないかは、いつも中身のハッシュで決める**
//!   （実機の mtime は使わない。受付の mtime は「読み直すか」の目安だけ）
//! - 目録に無いもの（道具が木の中に作ったファイル・シンボリックリンク）は、写すたびに消す。
//!   作業の木はいつも目録とぴったり同じ（job id が中身を正しく表すように）。ビルドの出力は
//!   木の外（`target/`・`node_modules/` は `proj/<id>/` の直下）に置く
//! - project id と job id が決まった形（16 進）でなければ、ハッシュした名前のフォルダを使う
//!   （どんな文字列が来ても、置き場の外を指さない）

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use zc_wire::msg::FileEntry;
use zc_wire::tree::Manifest;

/// 置き場の失敗。
#[derive(Debug)]
pub enum StoreError {
    /// 大きさかハッシュが、言われたものと違う。
    Mismatch,
    /// 上限を超えた。
    TooLarge,
    Io(std::io::Error),
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> StoreError {
        StoreError::Io(e)
    }
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StoreError::Mismatch => write!(f, "大きさかハッシュが合わない"),
            StoreError::TooLarge => write!(f, "上限を超えた"),
            StoreError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// 写しの結果（ログと試験用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub written: u32,
    pub removed: u32,
    pub unchanged: u32,
}

/// 置き場。受付に 1 つ（接続のスレッドから同時に使う）。
pub struct Store {
    root: PathBuf,
    /// プロジェクトごとの錠（写しと掃除を同時にしない）。
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

/// 一時ファイルの通し番号（同じ受付の中で名前がぶつからない）。
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn tmp_name(prefix: &str) -> String {
    format!(".zc-{prefix}-{}-{}.tmp", std::process::id(), TMP_SEQ.fetch_add(1, Ordering::SeqCst))
}

/// `s` が `len` 桁の小文字の 16 進か。
fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// SHA-256 を 16 進 64 桁（小文字）にする。
pub(crate) fn hex(digest: &[u8]) -> String {
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// ID を、置き場の中の 1 要素の名前にする。決まった形（16 進）ならそのまま、そうでなければ
/// その SHA-256 の先頭 32 桁に `x` を付けたもの（`..` や `/` が来ても外を指さない）。
fn id_dir_name(id: &str, len: usize) -> String {
    if is_hex(id, len) {
        id.to_string()
    } else {
        format!("x{}", &hex(&Sha256::digest(id.as_bytes()))[..32])
    }
}

/// 木の中の相対パスとして安全か（[`Manifest::new`] と同じ考え。受付の中でもう一度確かめる）:
/// 空でない・`/` 区切り・先頭の `/` なし・`.` `..` 空の要素なし・`\` と制御文字なし・
/// 1 要素 255 バイト以下・全体 1024 バイト以下。
pub(crate) fn safe_rel_path(p: &str) -> bool {
    if p.is_empty() || p.len() > 1024 || p.starts_with('/') {
        return false;
    }
    if p.bytes().any(|b| b < 0x20 || b == 0x7f || b == b'\\') {
        return false;
    }
    p.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && c.len() <= 255)
}

/// `tree.json` の 1 行。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Rec {
    /// 中身の SHA-256。
    h: String,
    /// 受付の側で確かめた直後の stat（読み直すかの目安）。
    s: u64,
    m: i64,
    mn: i64,
    i: u64,
}

impl Rec {
    fn matches(&self, md: &fs::Metadata) -> bool {
        self.s == md.len() && self.m == md.mtime() && self.mn == md.mtime_nsec() && self.i == md.ino()
    }

    fn from_meta(h: String, md: &fs::Metadata) -> Rec {
        Rec { h, s: md.len(), m: md.mtime(), mn: md.mtime_nsec(), i: md.ino() }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct TreeMemo {
    files: HashMap<String, Rec>,
}

/// ファイルを読みながら SHA-256 を取る（大きさも返す）。
fn hash_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 << 10];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((hex(&h.finalize()), n))
}

/// `dir` が本物のフォルダであるようにする（シンボリックリンクやファイルなら消して作る）。
fn ensure_real_dir(dir: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(md) if md.is_dir() => return Ok(()),
        Ok(md) if md.file_type().is_symlink() || md.is_file() => fs::remove_file(dir)?,
        Ok(_) => fs::remove_file(dir)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    fs::create_dir(dir)
}

/// フォルダの中身の大きさの合計（シンボリックリンクはたどらない）。
fn du(path: &Path) -> u64 {
    let Ok(md) = fs::symlink_metadata(path) else { return 0 };
    if !md.is_dir() {
        return md.len();
    }
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(path) {
        for e in rd.flatten() {
            n += du(&e.path());
        }
    }
    n
}

fn mtime(path: &Path) -> SystemTime {
    fs::symlink_metadata(path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

/// 置いたばかりの中身・一時ファイルを掃除で消さない猶予（受け取り中・写す前のもの）。
const FRESH: Duration = Duration::from_secs(10 * 60);
/// 終わったジョブの成果物を残す長さ（台帳は 30 分で忘れる。余裕を見て 2 時間）。
const OUT_KEEP: Duration = Duration::from_secs(2 * 60 * 60);

impl Store {
    /// `data_dir` の下に置き場を開く（フォルダは無ければ作る）。
    pub fn open(data_dir: &Path) -> std::io::Result<Store> {
        fs::create_dir_all(data_dir)?;
        for sub in ["blobs", "blobs/tmp", "proj", "out"] {
            ensure_real_dir(&data_dir.join(sub))?;
        }
        Ok(Store { root: data_dir.to_path_buf(), locks: Mutex::new(HashMap::new()) })
    }

    fn blob_path(&self, sha256: &str) -> PathBuf {
        self.root.join("blobs").join(&sha256[..2]).join(sha256)
    }

    fn project_lock(&self, project: &str) -> Arc<Mutex<()>> {
        self.dir_lock(&id_dir_name(project, 16))
    }

    /// `proj/` の下のフォルダの名前で引く錠。
    fn dir_lock(&self, dir_name: &str) -> Arc<Mutex<()>> {
        let mut m = self.locks.lock().unwrap();
        m.entry(dir_name.to_string()).or_default().clone()
    }

    /// 目録のうち、まだ持っていない中身の SHA-256（重複なし、目録の順）。
    pub fn missing(&self, manifest: &Manifest) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for f in manifest.files() {
            if !is_hex(&f.sha256, 64) || !seen.insert(f.sha256.clone()) {
                continue;
            }
            let ok = fs::metadata(self.blob_path(&f.sha256)).map(|m| m.is_file() && m.len() == f.size).unwrap_or(false);
            if !ok {
                out.push(f.sha256.clone());
            }
        }
        out
    }

    /// 中身 1 つの受け取りを始める。
    pub fn begin_blob(&self, sha256: &str, size: u64) -> Result<BlobWriter, StoreError> {
        if !is_hex(sha256, 64) {
            return Err(StoreError::Mismatch);
        }
        if size > zc_wire::limits::MAX_FILE_BYTES {
            return Err(StoreError::TooLarge);
        }
        let tmp = self.root.join("blobs/tmp").join(tmp_name("blob"));
        let file = File::create(&tmp)?;
        Ok(BlobWriter { file: Some(file), tmp, dest: self.blob_path(sha256), sha256: sha256.to_string(), size, written: 0, hasher: Sha256::new() })
    }

    /// 目録のとおりに作業の木を作る（全部の中身がそろっていること）。同じプロジェクトへの
    /// 写しは同時に 1 つだけ（中で待たせる）。走っているジョブがある間は呼ばない
    /// （台帳が順番を守る）。
    pub fn sync_tree(&self, project: &str, manifest: &Manifest) -> Result<SyncStats, StoreError> {
        self.sync_entries(project, manifest.files())
    }

    fn sync_entries(&self, project: &str, files: &[FileEntry]) -> Result<SyncStats, StoreError> {
        let lock = self.project_lock(project);
        let _g = lock.lock().unwrap();
        // 受付の中でももう一度確かめる（目録の検査を通らないものは来ない前提だが、木の外に
        // 書かないことはここで守る）
        let mut want: HashMap<&str, &FileEntry> = HashMap::new();
        let mut want_dirs: HashSet<String> = HashSet::new();
        for f in files {
            if !safe_rel_path(&f.path) || !is_hex(&f.sha256, 64) || want.insert(f.path.as_str(), f).is_some() {
                return Err(StoreError::Mismatch);
            }
            let mut p = f.path.as_str();
            while let Some(i) = p.rfind('/') {
                p = &p[..i];
                want_dirs.insert(p.to_string());
            }
        }
        for d in &want_dirs {
            if want.contains_key(d.as_str()) {
                // 同じ名前がファイルとフォルダの両方
                return Err(StoreError::Mismatch);
            }
        }

        let pdir = self.project_dir(project);
        ensure_real_dir(&self.root.join("proj"))?;
        ensure_real_dir(&pdir)?;
        let src = pdir.join("src");
        ensure_real_dir(&src)?;
        let memo_path = pdir.join("tree.json");
        let old: TreeMemo = fs::read(&memo_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();

        let mut stats = SyncStats::default();
        // 1. 目録に無いもの（道具が作ったもの・消えたファイル・シンボリックリンク）を消す
        self.prune(&src, "", &want, &want_dirs, &mut stats)?;

        // 2. 目録のファイルを、中身が違うものだけ書く
        let mut memo = TreeMemo::default();
        let mut paths: Vec<&str> = want.keys().copied().collect();
        paths.sort_unstable();
        for rel in paths {
            let f = want[rel];
            let dst = src.join(rel);
            let mode = if f.exec { 0o755 } else { 0o644 };
            let current = match fs::symlink_metadata(&dst) {
                Ok(md) if md.is_file() => {
                    let h = match old.files.get(rel) {
                        Some(r) if r.matches(&md) => r.h.clone(),
                        _ => hash_file(&dst)?.0,
                    };
                    Some((h, md))
                }
                _ => None,
            };
            let rec = match current {
                Some((h, md)) if h == f.sha256 && md.len() == f.size => {
                    // 中身は同じ: 触らない（実行権限だけ合わせる。mtime は変わらない）
                    if md.permissions().mode() & 0o777 != mode {
                        fs::set_permissions(&dst, fs::Permissions::from_mode(mode))?;
                    }
                    stats.unchanged += 1;
                    Rec::from_meta(h, &fs::symlink_metadata(&dst)?)
                }
                _ => {
                    self.make_parents(&src, rel)?;
                    self.copy_blob(f, &dst, mode)?;
                    stats.written += 1;
                    Rec::from_meta(f.sha256.clone(), &fs::symlink_metadata(&dst)?)
                }
            };
            memo.files.insert(rel.to_string(), rec);
        }

        // 3. 覚えを書く（一時ファイル → rename。最後に使った時刻の印も兼ねる）
        let tmp = pdir.join(tmp_name("memo"));
        fs::write(&tmp, serde_json::to_vec(&memo).expect("JSON にできる"))?;
        fs::rename(&tmp, &memo_path)?;
        Ok(stats)
    }

    /// `dir`（木の中の `prefix`）の中で、目録に無いものを消す。シンボリックリンクはたどらない。
    fn prune(&self, dir: &Path, prefix: &str, want: &HashMap<&str, &FileEntry>, want_dirs: &HashSet<String>, stats: &mut SyncStats) -> std::io::Result<()> {
        for e in fs::read_dir(dir)? {
            let e = e?;
            let name = e.file_name();
            let Some(name) = name.to_str() else {
                // UTF-8 でない名前は目録に載りえない
                remove_any(&e.path(), stats)?;
                continue;
            };
            let rel = if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") };
            let ft = e.file_type()?;
            if ft.is_dir() {
                if want_dirs.contains(&rel) {
                    self.prune(&e.path(), &rel, want, want_dirs, stats)?;
                } else {
                    remove_any(&e.path(), stats)?;
                }
            } else if ft.is_file() && want.contains_key(rel.as_str()) {
                // 残す（中身は次の段で確かめる）
            } else {
                remove_any(&e.path(), stats)?;
            }
        }
        Ok(())
    }

    /// `rel` の親のフォルダを、1 段ずつ本物のフォルダとして作る（途中のシンボリックリンクは消す）。
    fn make_parents(&self, src: &Path, rel: &str) -> std::io::Result<()> {
        let mut cur = src.to_path_buf();
        let parts: Vec<&str> = rel.split('/').collect();
        for p in &parts[..parts.len() - 1] {
            cur.push(p);
            ensure_real_dir(&cur)?;
        }
        Ok(())
    }

    /// 置き場の中身を `dst` に写す（一時ファイル → rename）。写しながらハッシュを確かめ、
    /// 置き場の中身が壊れていたら、その中身を消して [`StoreError::Mismatch`]。
    fn copy_blob(&self, f: &FileEntry, dst: &Path, mode: u32) -> Result<(), StoreError> {
        let blob = self.blob_path(&f.sha256);
        let mut input = match File::open(&blob) {
            Ok(i) => i,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(StoreError::Mismatch),
            Err(e) => return Err(e.into()),
        };
        let tmp = dst.with_file_name(tmp_name("sync"));
        let res = (|| -> Result<(), StoreError> {
            let mut out = File::create(&tmp)?;
            let mut h = Sha256::new();
            let mut buf = vec![0u8; 64 << 10];
            let mut n = 0u64;
            loop {
                let k = input.read(&mut buf)?;
                if k == 0 {
                    break;
                }
                n += k as u64;
                if n > f.size {
                    break;
                }
                h.update(&buf[..k]);
                out.write_all(&buf[..k])?;
            }
            if n != f.size || hex(&h.finalize()) != f.sha256 {
                let _ = fs::remove_file(&blob);
                return Err(StoreError::Mismatch);
            }
            out.set_permissions(fs::Permissions::from_mode(mode))?;
            drop(out);
            fs::rename(&tmp, dst)?;
            Ok(())
        })();
        if res.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        res
    }

    /// 作業の木（`proj/<project id>/src`）。
    pub fn src_dir(&self, project: &str) -> PathBuf {
        self.project_dir(project).join("src")
    }

    /// プロジェクトのフォルダ（`proj/<project id>`）。
    pub fn project_dir(&self, project: &str) -> PathBuf {
        self.root.join("proj").join(id_dir_name(project, 16))
    }

    /// ジョブの成果物のフォルダ（`out/<job id>`。無ければ作る）。
    pub fn out_dir(&self, job: &str) -> std::io::Result<PathBuf> {
        ensure_real_dir(&self.root.join("out"))?;
        let d = self.root.join("out").join(id_dir_name(job, 32));
        ensure_real_dir(&d)?;
        Ok(d)
    }

    /// 上限を超えていたら古いものを消す。消したバイト数を返す。
    ///
    /// いつもすること: どの木の覚え（`tree.json`）からも指されていない中身と、置き去りの
    /// 一時ファイル（どちらも 10 分より古いもの）、2 時間より古い成果物を消す。
    /// 上限を超えていたら: 最後に写した時刻の古いプロジェクトから、上限を下回るまで消す
    /// （いちばん新しい 1 つは残す）。消したプロジェクトだけが指していた中身も消す。
    /// 走っているジョブのプロジェクトは消さないこと（台帳が、何も走っていないときに呼ぶ）。
    pub fn collect_garbage(&self, quota: u64) -> std::io::Result<u64> {
        let now = SystemTime::now();
        let old_enough = |p: &Path| now.duration_since(mtime(p)).map(|d| d >= FRESH).unwrap_or(false);
        let mut freed = 0u64;

        // 成果物
        if let Ok(rd) = fs::read_dir(self.root.join("out")) {
            for e in rd.flatten() {
                let p = e.path();
                if now.duration_since(mtime(&p)).map(|d| d >= OUT_KEEP).unwrap_or(false) {
                    freed += du(&p);
                    let _ = fs::remove_dir_all(&p);
                }
            }
        }

        // 置き去りの一時ファイル（受け取りの途中で切れたもの）
        if let Ok(rd) = fs::read_dir(self.root.join("blobs/tmp")) {
            for e in rd.flatten() {
                let p = e.path();
                if old_enough(&p) {
                    freed += du(&p);
                    let _ = fs::remove_file(&p);
                }
            }
        }

        // プロジェクトを、最後に写した時刻の順に
        let mut projects: Vec<(SystemTime, PathBuf, String)> = Vec::new();
        if let Ok(rd) = fs::read_dir(self.root.join("proj")) {
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                let memo = p.join("tree.json");
                let t = if memo.exists() { mtime(&memo) } else { mtime(&p) };
                projects.push((t, p, name));
            }
        }
        projects.sort();

        let refs_of = |p: &Path| -> HashSet<String> {
            fs::read(p.join("tree.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<TreeMemo>(&b).ok())
                .map(|m| m.files.into_values().map(|r| r.h).collect())
                .unwrap_or_default()
        };

        let mut total = du(&self.root);
        let mut evicted: HashSet<String> = HashSet::new();
        if total > quota {
            let keep_newest = projects.len().saturating_sub(1);
            for (_, p, name) in projects.iter().take(keep_newest) {
                if total <= quota {
                    break;
                }
                let lock = self.dir_lock(name);
                let _g = lock.lock().unwrap();
                evicted.extend(refs_of(p));
                let n = du(p);
                if fs::remove_dir_all(p).is_ok() {
                    freed += n;
                    total = total.saturating_sub(n);
                }
            }
        }

        // 残っている木が指している中身
        let mut live: HashSet<String> = HashSet::new();
        for (_, p, _) in &projects {
            if p.exists() {
                live.extend(refs_of(p));
            }
        }
        if let Ok(rd) = fs::read_dir(self.root.join("blobs")) {
            for d in rd.flatten() {
                let dn = d.file_name().to_string_lossy().into_owned();
                if dn == "tmp" || !d.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let Ok(rd2) = fs::read_dir(d.path()) else { continue };
                for b in rd2.flatten() {
                    let name = b.file_name().to_string_lossy().into_owned();
                    if live.contains(&name) {
                        continue;
                    }
                    let p = b.path();
                    // 消したプロジェクトだけが指していたものはすぐ、どこからも指されていない
                    // ものは 10 分たってから（受け取ったばかりで、まだ写していないものを残す）
                    if evicted.contains(&name) || old_enough(&p) {
                        freed += du(&p);
                        let _ = fs::remove_file(&p);
                    }
                }
            }
        }
        Ok(freed)
    }
}

fn remove_any(p: &Path, stats: &mut SyncStats) -> std::io::Result<()> {
    let md = fs::symlink_metadata(p)?;
    if md.is_dir() {
        stats.removed += count_files(p);
        fs::remove_dir_all(p)
    } else {
        stats.removed += 1;
        fs::remove_file(p)
    }
}

fn count_files(p: &Path) -> u32 {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => n += count_files(&e.path()),
                _ => n += 1,
            }
        }
    }
    n
}

/// 中身 1 つの受け取り。
pub struct BlobWriter {
    file: Option<File>,
    tmp: PathBuf,
    dest: PathBuf,
    sha256: String,
    size: u64,
    written: u64,
    hasher: Sha256,
}

impl BlobWriter {
    /// 続きを書く。`offset` は順番どおりでなければ [`StoreError::Mismatch`]。
    pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), StoreError> {
        if offset != self.written {
            return Err(StoreError::Mismatch);
        }
        if self.written + data.len() as u64 > self.size {
            return Err(StoreError::TooLarge);
        }
        let f = self.file.as_mut().ok_or(StoreError::Mismatch)?;
        f.write_all(data)?;
        self.hasher.update(data);
        self.written += data.len() as u64;
        Ok(())
    }

    /// 終わり。大きさとハッシュが合えば置き場に置く。
    pub fn finish(mut self) -> Result<(), StoreError> {
        let file = self.file.take().ok_or(StoreError::Mismatch)?;
        drop(file);
        let digest = hex(&std::mem::take(&mut self.hasher).finalize());
        if self.written != self.size || digest != self.sha256 {
            return Err(StoreError::Mismatch);
        }
        if let Some(parent) = self.dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&self.tmp, &self.dest)?;
        // rename できたら、Drop で消すものは無い
        self.tmp = PathBuf::new();
        Ok(())
    }
}

impl Drop for BlobWriter {
    fn drop(&mut self) {
        if !self.tmp.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_paths_are_checked() {
        for p in ["/etc/passwd", "../x", "a/../b", "a//b", "a\\b", "", "a/./b", "a\0b", "a/", "."] {
            assert!(!safe_rel_path(p), "{p:?}");
        }
        assert!(safe_rel_path("src/日本語.rs"));
        assert!(safe_rel_path(".cargo/config.toml"));
    }

    #[test]
    fn ids_never_escape() {
        assert_eq!(id_dir_name("0123456789abcdef", 16), "0123456789abcdef");
        for bad in ["..", "../../etc", "a/b", "", "0123456789ABCDEF"] {
            let n = id_dir_name(bad, 16);
            assert!(n.starts_with('x') && n.len() == 33 && !n.contains('/'), "{bad} → {n}");
        }
    }
}

// 受け入れ試験（課題 B2）は tests/store.rs に:
// 2 回目の写しは 1 つも書かない（mtime が変わらない）、1 ファイル変えるとそれだけ書く、
// 目録から消えたファイルが消える、ハッシュの違う中身は置かれない、木の中に道具が作った
// シンボリックリンク（外を指す）があっても外に書かない、覚え（tree.json）を消しても正しい、
// 同じ中身の 2 ファイル、空のファイル、実行権限、上限での掃除。
