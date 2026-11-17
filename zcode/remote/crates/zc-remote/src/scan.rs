//! プロジェクトの根・木の走査・ハッシュの覚え（docs/stage3-design.md §7.2）。課題 D3。
//!
//! SD には何も書かない。覚え（パス → 大きさ・mtime・inode・SHA-256）は serve の RAM に
//! 持ち、serve が終われば消える（再起動後の初回は全部ハッシュし直す）。
//!
//! # プロジェクトの根
//!
//! 保存したファイルから上へたどる。たどるのは `$HOME`（無ければ `/`）の手前まで。
//!
//! | 言語 | 根 | 無いとき |
//! |---|---|---|
//! | Rust | いちばん近い `Cargo.toml`。そこから上に `[workspace]` のある `Cargo.toml` があればそちら | **1 ファイルだけ**（そのファイルだけを送り、受付が `rustc` で作る。`entry` = ファイル名） |
//! | C | ファイルのあるフォルダ（そのフォルダの `*.c` `*.h` だけ。下のフォルダは見ない）。`entry` = ファイル名 | — |
//! | Node | いちばん近い `package.json` で、`dependencies` か `devDependencies` が空でないもの | [`ProjectError::NotApplicable`]（実機の `tsc` に回す） |
//! | Python | いちばん近い `pyproject.toml` か `setup.cfg` | 1 ファイルだけ |
//! | Shell | — | いつも 1 ファイルだけ（`source` 先は送らない。shellcheck は追わない） |
//! | C++ | C と同じ（そのフォルダの `*.cpp` `*.cc` `*.cxx` `*.hpp` `*.hh` `*.hxx` `*.h` など） | — |
//! | Go | いちばん近い `go.mod` | 1 ファイルだけ |
//! | Ruby | いちばん近い `Gemfile` | 1 ファイルだけ |
//! | Zig | いちばん近い `build.zig` | 1 ファイルだけ |
//! | MoonBit | いちばん近い `moon.mod.json`（新しい形の `moon.mod` も） | 1 ファイルだけ（受付が仮のモジュールを作る） |
//!
//! 2026-11-17 に Python 以下を足した。根の印のファイルを `stat` で探すだけなので、Rust・C・Node の
//! 走査の手間は変わらない。どの言語でも `entry` は根からの相対パス。
//!
//! 根の外を指す `path = "../x"` の依存は送らない（受付でビルドが失敗し、その診断が出る）。
//!
//! 細かい決まり（実装で決めたこと）:
//!
//! - 根のフォルダは `canonicalize` したもの（シンボリックリンクを経た開き方でも同じ project id
//!   になるように）。`$HOME` も同じく正規化して比べる
//! - Node は「依存が空の `package.json`」を飛ばして、さらに上を探す（npm の workspaces の根に
//!   当たるように）
//! - Node の `entry` は根からの相対パス（TS の入口の指定）
//! - **1 ファイルだけ**の Rust は、project id を「そのファイルの絶対パス」から作る（根の
//!   フォルダは同じでもファイルごとに別のプロジェクト。同じフォルダの `a.rs` と `b.rs` が受付の
//!   作業の木と成果物の控えを取り合わない）
//!
//! # 送らないもの
//!
//! `target/`・`node_modules/`・`__pycache__/`・`zig-cache/`・`.git/`・`dist/`・`build/`・`zig-out/`・
//! `_build/`（MoonBit）・名前が `.` で始まるフォルダ
//! （`.cargo/` と `.github/` は送る）、根の `.gitignore` に書かれたもの（`*`・`?`・`**`・
//! `[…]`・先頭と末尾の `/`・`!` の打ち消しに対応。入れ子の `.gitignore` は見ない）、
//! シンボリックリンク（たどらない・送らない）、ふつうのファイルでないもの。
//! 上限（[`zc_wire::limits`]）を超えたら [`ScanError::TooLarge`]（何が超えたかを添える）。
//!
//! - `target`・`node_modules`・`__pycache__`・`zig-cache`・`.` で始まるフォルダはどの深さでも除く。
//!   `dist`・`build`・`zig-out`・`_build` は **根の直下だけ**（Rust の `src/build/` のようなモジュールのフォルダを落とさない。
//!   下の階層の生成物は、ふつう `.gitignore` に書いてある）
//! - 名前が UTF-8 でない・`\` や制御文字を含むファイルは送れないので、黙って落とさず
//!   [`ScanError::Io`] で断る（落とすと、受付でのビルドの失敗の理由が分からなくなる）
//!
//! # 変わったファイルの見つけ方
//!
//! 依頼のたびに木を `stat` で歩き、覚えと（大きさ, mtime_ns, inode）が同じなら読まない。
//! **走査の直前 2 秒以内の mtime のものは無条件に読み直す**（同じ時刻の中で 2 回書かれた
//! ファイルを取りこぼさない）。mtime が未来（時計が戻った）のものも読み直す。エディタが
//! 保存したファイルだけに頼らない（ターミナルの `git pull` や `scp` でも変わる）。
//!
//! 読んでいる間に書き換わったら（読む前と後の `stat` が違う）、3 回まで読み直す。

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use sha2::{Digest, Sha256};
use zc_wire::limits::{CHUNK_BYTES, MAX_FILE_BYTES, MAX_TREE_BYTES, MAX_TREE_FILES};
use zc_wire::msg::{FileEntry, Lang};
use zc_wire::tree::Manifest;

/// プロジェクト。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// 根の絶対パス。
    pub root: PathBuf,
    /// project id（[`zc_wire::tree::project_id`]）。
    pub id: String,
    pub lang: Lang,
    /// 起点のファイル（根からの相対）。Cargo のプロジェクトでは `None`。
    pub entry: Option<String>,
    /// 根だけを見るか（C: 下のフォルダは見ない）、1 ファイルだけか。
    pub shape: Shape,
}

/// 木の形。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// 根から下を全部（除外を除く）。
    Tree,
    /// 根のフォルダの直下だけ（C・C++。拡張子は [`flat_exts`]）。
    Flat,
    /// `entry` の 1 ファイルだけ。
    SingleFile,
}

/// 根を決められない理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectError {
    /// ビルド係の対象ではない（npm の依存が無い TS / JS）。
    NotApplicable,
    Io(String),
}

/// `Cargo.toml` と `package.json` を読む上限（これより大きいものは中身を見ない）。
const MANIFEST_READ_MAX: u64 = 1 << 20;
/// `.gitignore` を読む上限。
const GITIGNORE_READ_MAX: u64 = 256 << 10;
/// 走査で潜る深さの上限（シンボリックリンクはたどらないので輪にはならないが、念のため）。
const MAX_DEPTH: usize = 64;
/// 「直前に書かれた」とみなす幅。
const RECENT_NS: u64 = 2_000_000_000;
/// 覚えの上限。
const CACHE_MAX: usize = 4000;

/// `file`（絶対パス）のプロジェクトを探す。
pub fn find_project(file: &Path, lang: Lang) -> Result<Project, ProjectError> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    let stop = home.map(|h| fs::canonicalize(&h).unwrap_or(h)).unwrap_or_else(|| PathBuf::from("/"));
    find_project_below(file, lang, &stop)
}

/// [`find_project`] の中身。`stop` のフォルダ（とその上）は見ない。`file` が `stop` の下に
/// 無ければ `/` の手前までたどる。
fn find_project_below(file: &Path, lang: Lang, stop: &Path) -> Result<Project, ProjectError> {
    if !file.is_absolute() {
        return Err(ProjectError::Io(format!("絶対パスでない: {}", file.display())));
    }
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| ProjectError::Io(format!("ファイル名が無い: {}", file.display())))?
        .to_owned();
    let parent = file.parent().ok_or_else(|| ProjectError::Io(format!("フォルダが無い: {}", file.display())))?;
    let dir = fs::canonicalize(parent).map_err(|e| ProjectError::Io(format!("{}: {e}", parent.display())))?;
    let stop: &Path = if dir.starts_with(stop) && dir != stop { stop } else { Path::new("/") };
    // 上へたどるフォルダ（`stop` は含めない。`/` は、`stop` の外のときも含めない）。
    let ancestors = || dir.ancestors().take_while(move |d| *d != stop && d.parent().is_some());

    let project = |root: PathBuf, id_src: &Path, entry: Option<String>, shape: Shape| Project {
        id: zc_wire::tree::project_id(id_src.as_os_str().as_bytes()),
        root,
        lang,
        entry,
        shape,
    };

    match lang {
        Lang::Rust => {
            let Some(nearest) = ancestors().find(|d| d.join("Cargo.toml").is_file()) else {
                let path = dir.join(&name);
                return Ok(project(dir.clone(), &path, Some(name), Shape::SingleFile));
            };
            let root = ancestors()
                .skip_while(|d| *d != nearest)
                .find(|d| has_workspace(&d.join("Cargo.toml")))
                .unwrap_or(nearest)
                .to_path_buf();
            Ok(project(root.clone(), &root, None, Shape::Tree))
        }
        Lang::C | Lang::Cpp => Ok(project(dir.clone(), &dir, Some(name), Shape::Flat)),
        Lang::Node => {
            let root = ancestors()
                .find(|d| has_npm_deps(&d.join("package.json")))
                .ok_or(ProjectError::NotApplicable)?
                .to_path_buf();
            let rel = dir.join(&name);
            let rel = rel.strip_prefix(&root).unwrap_or(Path::new(&name));
            let rel = rel.to_str().map(str::to_owned);
            Ok(project(root.clone(), &root, rel, Shape::Tree))
        }
        // 根の印のファイルがあれば木、無ければ 1 ファイルだけ（Rust の 1 ファイルと同じく、
        // project id はファイルの絶対パスから作る）。
        Lang::Python | Lang::Shell | Lang::Go | Lang::Ruby | Lang::Zig | Lang::Moonbit => {
            let marks: &[&str] = match lang {
                Lang::Python => &["pyproject.toml", "setup.cfg"],
                Lang::Go => &["go.mod"],
                Lang::Ruby => &["Gemfile"],
                Lang::Zig => &["build.zig"],
                Lang::Moonbit => &["moon.mod.json", "moon.mod"],
                _ => &[],
            };
            match ancestors().find(|d| marks.iter().any(|m| d.join(m).is_file())) {
                Some(root) => {
                    let root = root.to_path_buf();
                    let rel = dir.join(&name);
                    let rel = rel.strip_prefix(&root).unwrap_or(Path::new(&name)).to_str().map(str::to_owned);
                    Ok(project(root.clone(), &root, rel, Shape::Tree))
                }
                None => {
                    let path = dir.join(&name);
                    Ok(project(dir.clone(), &path, Some(name), Shape::SingleFile))
                }
            }
        }
        Lang::Unknown => Err(ProjectError::NotApplicable),
    }
}

/// [`Shape::Flat`] で送るファイルの拡張子。
fn flat_exts(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Cpp => &[".cpp", ".cc", ".cxx", ".c++", ".hpp", ".hh", ".hxx", ".h", ".inl", ".ipp", ".tpp"],
        _ => &[".c", ".h"],
    }
}

/// 小さなファイルを読む（大きすぎるもの・読めないものは `None`）。
fn read_small(path: &Path, max: u64) -> Option<Vec<u8>> {
    let mut f = fs::File::open(path).ok()?;
    if !f.metadata().ok()?.is_file() {
        return None;
    }
    let mut buf = Vec::new();
    f.by_ref().take(max + 1).read_to_end(&mut buf).ok()?;
    (buf.len() as u64 <= max).then_some(buf)
}

/// `Cargo.toml` に `[workspace]`（か `[workspace.…]`）の表があるか。
fn has_workspace(cargo_toml: &Path) -> bool {
    let Some(text) = read_small(cargo_toml, MANIFEST_READ_MAX) else { return false };
    String::from_utf8_lossy(&text).lines().any(|line| {
        let line = line.split('#').next().unwrap_or("").trim();
        line == "[workspace]" || line.starts_with("[workspace.")
    })
}

/// `package.json` の `dependencies` か `devDependencies` が空でないか。
fn has_npm_deps(package_json: &Path) -> bool {
    let Some(text) = read_small(package_json, MANIFEST_READ_MAX) else { return false };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&text) else { return false };
    ["dependencies", "devDependencies"]
        .iter()
        .any(|k| v.get(k).and_then(|d| d.as_object()).is_some_and(|o| !o.is_empty()))
}

/// 走査の失敗。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanError {
    /// 上限を超えた（人に見せる 1 行: 「src/big.bin が 4MB を超えている」など）。
    TooLarge(String),
    Io(String),
}

/// 走査の所要（`--stats`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanStats {
    pub files: u32,
    /// 読み直してハッシュしたファイルの数とバイト数。
    pub hashed_files: u32,
    pub hashed_bytes: u64,
    pub walk_us: u32,
    pub hash_us: u32,
}

/// 覚えの 1 件。
#[derive(Debug)]
struct Entry {
    size: u64,
    mtime_ns: u64,
    ino: u64,
    sha256: [u8; 32],
    /// 最後に使った走査の番号（古い順に捨てるため）。
    used: u64,
}

/// ハッシュの覚え。serve に 1 つ（全プロジェクト共用。上限 4000 件、古い順に捨てる）。
#[derive(Debug, Default)]
pub struct HashCache {
    /// 鍵はファイルの絶対パス。
    entries: HashMap<PathBuf, Entry>,
    /// 走査の通し番号。
    gen: u64,
    /// 読むときの入れ物（走査のたびに確保しない）。
    buf: Vec<u8>,
}

/// 走査で見つけたファイル（まだハッシュしていない）。
struct Found {
    abs: PathBuf,
    rel: String,
    size: u64,
    mtime_ns: u64,
    ino: u64,
    exec: bool,
}

impl HashCache {
    pub fn new() -> HashCache {
        HashCache::default()
    }

    /// 木を歩いて目録を作る。`now_ns` は実時間（`CLOCK_REALTIME`。mtime と比べる）。
    pub fn scan(&mut self, project: &Project, now_ns: u64) -> Result<(Arc<Manifest>, ScanStats), ScanError> {
        let t0 = Instant::now();
        let mut found = Vec::new();
        let mut total = 0u64;
        match project.shape {
            Shape::SingleFile => {
                let entry = project.entry.as_deref().ok_or_else(|| ScanError::Io("1 ファイルだけなのに entry が無い".into()))?;
                let abs = project.root.join(entry);
                let meta = fs::symlink_metadata(&abs).map_err(|e| io_err(&abs, e))?;
                if !meta.is_file() {
                    return Err(ScanError::Io(format!("ふつうのファイルでない: {}", abs.display())));
                }
                push_found(&mut found, &mut total, abs, entry.to_owned(), &meta)?;
            }
            Shape::Flat | Shape::Tree => {
                let ignore = Gitignore::load(&project.root.join(".gitignore"));
                let flat = (project.shape == Shape::Flat).then(|| flat_exts(project.lang));
                let mut walker = Walker { ignore: &ignore, found: &mut found, total: &mut total, flat };
                walker.walk(&project.root, "", 0)?;
            }
        }
        let walk_us = micros(t0);

        // ハッシュ（覚えと食い違うもの・直前に書かれたものだけ読む）。
        let t1 = Instant::now();
        self.gen += 1;
        let gen = self.gen;
        let mut stats = ScanStats { files: found.len() as u32, walk_us, ..ScanStats::default() };
        let mut files = Vec::with_capacity(found.len());
        if self.buf.len() < CHUNK_BYTES {
            self.buf = vec![0; CHUNK_BYTES];
        }
        for f in found {
            let recent = f.mtime_ns.saturating_add(RECENT_NS) > now_ns;
            let hit = match self.entries.get_mut(&f.abs) {
                Some(e) if !recent && e.size == f.size && e.mtime_ns == f.mtime_ns && e.ino == f.ino => {
                    e.used = gen;
                    Some(e.sha256)
                }
                _ => None,
            };
            let (size, sha) = match hit {
                Some(sha) => (f.size, sha),
                None => {
                    let (size, mtime_ns, ino, sha) = hash_stable(&f.abs, &mut self.buf)?;
                    if size > MAX_FILE_BYTES {
                        return Err(too_large_file(&f.rel));
                    }
                    stats.hashed_files += 1;
                    stats.hashed_bytes += size;
                    self.entries.insert(f.abs, Entry { size, mtime_ns, ino, sha256: sha, used: gen });
                    (size, sha)
                }
            };
            files.push(FileEntry { path: f.rel, size, sha256: hex(&sha), exec: f.exec });
        }
        stats.hash_us = micros(t1);

        // 木の形の走査なら、根の下で今回見なかった覚え（消えたファイル）を捨てる。
        if project.shape == Shape::Tree {
            self.entries.retain(|p, e| e.used == gen || !p.starts_with(&project.root));
        }
        self.trim();

        let manifest = Manifest::new(files).map_err(|e| match e {
            zc_wire::tree::ManifestError::TooLarge(what) => ScanError::TooLarge(format!("目録が上限を超えている: {what}")),
            e => ScanError::Io(format!("目録にできない: {e}")),
        })?;
        Ok((Arc::new(manifest), stats))
    }

    /// 覚えている件数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 上限を超えた分を、使ったのが古い順に捨てる。
    fn trim(&mut self) {
        if self.entries.len() <= CACHE_MAX {
            return;
        }
        let mut used: Vec<u64> = self.entries.values().map(|e| e.used).collect();
        let cut = self.entries.len() - CACHE_MAX;
        used.select_nth_unstable(cut - 1);
        let limit = used[cut - 1];
        // `limit` 以下を捨てる（同じ番号が多いと少し多めに捨てる。走査 1 回分は残す）。
        let gen = self.gen;
        self.entries.retain(|_, e| e.used > limit || e.used == gen);
    }
}

fn micros(t: Instant) -> u32 {
    t.elapsed().as_micros().min(u32::MAX as u128) as u32
}

fn io_err(path: &Path, e: io::Error) -> ScanError {
    ScanError::Io(format!("{}: {e}", path.display()))
}

fn too_large_file(rel: &str) -> ScanError {
    ScanError::TooLarge(format!("{rel} が {}MiB を超えている", MAX_FILE_BYTES >> 20))
}

fn mtime_ns(meta: &fs::Metadata) -> u64 {
    (meta.mtime().max(0) as u64).saturating_mul(1_000_000_000).saturating_add(meta.mtime_nsec().max(0) as u64)
}

/// 見つけたファイルを足す（上限もここで見る。超えたらハッシュする前に断る）。
fn push_found(found: &mut Vec<Found>, total: &mut u64, abs: PathBuf, rel: String, meta: &fs::Metadata) -> Result<(), ScanError> {
    let size = meta.len();
    if size > MAX_FILE_BYTES {
        return Err(too_large_file(&rel));
    }
    if found.len() >= MAX_TREE_FILES {
        return Err(ScanError::TooLarge(format!("ファイルが {MAX_TREE_FILES} 個を超えている")));
    }
    *total += size;
    if *total > MAX_TREE_BYTES {
        return Err(ScanError::TooLarge(format!("合計が {}MiB を超えている", MAX_TREE_BYTES >> 20)));
    }
    found.push(Found { abs, rel, size, mtime_ns: mtime_ns(meta), ino: meta.ino(), exec: meta.mode() & 0o100 != 0 });
    Ok(())
}

/// 送れる名前か（目録のパスの 1 要素になれるか）。
fn sendable_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && !name.chars().any(|c| c == '\\' || c.is_control())
}

/// 木を歩く。
struct Walker<'a> {
    ignore: &'a Gitignore,
    found: &'a mut Vec<Found>,
    total: &'a mut u64,
    /// 根の直下だけ・この拡張子のファイルだけ（C・C++）。
    flat: Option<&'static [&'static str]>,
}

impl Walker<'_> {
    fn walk(&mut self, dir: &Path, rel_dir: &str, depth: usize) -> Result<(), ScanError> {
        if depth > MAX_DEPTH {
            return Err(ScanError::TooLarge(format!("{rel_dir}: フォルダが深すぎる")));
        }
        let rd = fs::read_dir(dir).map_err(|e| io_err(dir, e))?;
        for ent in rd {
            let ent = ent.map_err(|e| io_err(dir, e))?;
            // DirEntry の種類はリンクをたどらない（lstat と同じ）。
            let ft = ent.file_type().map_err(|e| io_err(&ent.path(), e))?;
            if ft.is_symlink() || !(ft.is_file() || ft.is_dir()) {
                continue;
            }
            let os_name = ent.file_name();
            let Some(name) = os_name.to_str().filter(|n| sendable_name(n)) else {
                let shown = Path::new(rel_dir).join(&os_name);
                return Err(ScanError::Io(format!("送れないファイル名: {}", shown.display())));
            };
            let rel = if rel_dir.is_empty() { name.to_owned() } else { format!("{rel_dir}/{name}") };
            if ft.is_dir() {
                if self.flat.is_some() || skip_dir(name, depth == 0) || self.ignore.ignored(&rel, true) {
                    continue;
                }
                self.walk(&ent.path(), &rel, depth + 1)?;
            } else {
                if self.flat.is_some_and(|exts| !exts.iter().any(|e| name.ends_with(e))) {
                    continue;
                }
                if self.ignore.ignored(&rel, false) {
                    continue;
                }
                let abs = ent.path();
                let meta = fs::symlink_metadata(&abs).map_err(|e| io_err(&abs, e))?;
                if !meta.is_file() {
                    continue;
                }
                if rel.len() > 1024 {
                    return Err(ScanError::Io(format!("パスが長すぎる: {rel}")));
                }
                push_found(self.found, self.total, abs, rel, &meta)?;
            }
        }
        Ok(())
    }
}

/// 送らないフォルダか（`.gitignore` とは別の決まり）。
fn skip_dir(name: &str, at_root: bool) -> bool {
    match name {
        "target" | "node_modules" | "__pycache__" | "zig-cache" => true,
        ".cargo" | ".github" => false,
        "dist" | "build" | "zig-out" | "_build" => at_root,
        _ => name.starts_with('.'),
    }
}

/// ファイルを読んで SHA-256 を出す。読む前と後で `stat` が食い違ったら読み直す。
/// 返すのは（大きさ, mtime_ns, inode, SHA-256）。
fn hash_stable(path: &Path, buf: &mut [u8]) -> Result<(u64, u64, u64, [u8; 32]), ScanError> {
    for _ in 0..3 {
        let mut f = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| io_err(path, e))?;
        let before = f.metadata().map_err(|e| io_err(path, e))?;
        if !before.is_file() {
            return Err(ScanError::Io(format!("ふつうのファイルでない: {}", path.display())));
        }
        let mut h = Sha256::new();
        let mut n = 0u64;
        loop {
            let k = match f.read(buf) {
                Ok(0) => break,
                Ok(k) => k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io_err(path, e)),
            };
            h.update(&buf[..k]);
            n += k as u64;
            if n > MAX_FILE_BYTES {
                break;
            }
        }
        let after = f.metadata().map_err(|e| io_err(path, e))?;
        if n == after.len() && after.len() == before.len() && mtime_ns(&after) == mtime_ns(&before) {
            return Ok((n, mtime_ns(&after), after.ino(), h.finalize().into()));
        }
        if n > MAX_FILE_BYTES {
            return Ok((n, mtime_ns(&after), after.ino(), [0; 32]));
        }
    }
    Err(ScanError::Io(format!("書き換え中で読めない: {}", path.display())))
}

/// 16 進（小文字）。
fn hex(d: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(d.len() * 2);
    for &b in d {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

/// 未保存の中身（zcode の重ね。`SubmitFlags::AUTO` の check）。根からの相対パスと中身。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overlaid {
    pub rel: String,
    /// 中身の SHA-256（16 進）。
    pub sha256: String,
    pub bytes: Arc<[u8]>,
}

/// 重ね（絶対パスと中身）を根からの相対に直す。根の外のもの・フォルダが無いもの・送れない
/// 名前のものは捨てる（根はフォルダを `canonicalize` して比べる）。
pub fn resolve_overlay(project: &Project, files: Vec<(PathBuf, Vec<u8>)>) -> Vec<Overlaid> {
    let mut out = Vec::with_capacity(files.len());
    for (path, bytes) in files {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { continue };
        let Ok(dir) = fs::canonicalize(parent) else { continue };
        let abs = dir.join(name);
        let Some(rel) = abs.strip_prefix(&project.root).ok().and_then(|r| r.to_str()) else { continue };
        if !zc_wire::tree::is_canonical_path(rel) {
            continue;
        }
        let sha256 = hex(&Sha256::digest(&bytes));
        out.push(Overlaid { rel: rel.to_owned(), sha256, bytes: bytes.into() });
    }
    out
}

/// 目録のうち、重ねと同じパスのファイルを、重ねの大きさと SHA-256 に差し替える。目録に無い
/// ファイル（除外したもの・まだディスクに無いもの）は足さない。覚え（[`HashCache`]）には
/// 触らない（ディスクの中身の覚えのまま）。
pub fn apply_overlay(manifest: Arc<Manifest>, overlay: &[Overlaid]) -> Result<Arc<Manifest>, ScanError> {
    if !manifest.files().iter().any(|f| overlay.iter().any(|o| o.rel == f.path)) {
        return Ok(manifest);
    }
    let mut files = manifest.files().to_vec();
    for f in &mut files {
        if let Some(o) = overlay.iter().find(|o| o.rel == f.path) {
            if o.bytes.len() as u64 > MAX_FILE_BYTES {
                return Err(too_large_file(&o.rel));
            }
            f.size = o.bytes.len() as u64;
            f.sha256 = o.sha256.clone();
        }
    }
    let m = Manifest::new(files).map_err(|e| match e {
        zc_wire::tree::ManifestError::TooLarge(what) => ScanError::TooLarge(format!("目録が上限を超えている: {what}")),
        e => ScanError::Io(format!("目録にできない: {e}")),
    })?;
    Ok(Arc::new(m))
}

/// 送る直前に、ファイルが目録のとおりか確かめながら読む口。`chunk` に
/// [`zc_wire::limits::CHUNK_BYTES`] ずつ渡し、最後に SHA-256 が合えば `Ok(true)`。
/// 走査のあとで書き換わっていたら `Ok(false)`（serve は目録を作り直す）。
///
/// `chunk` には確かめる前の中身が渡る（`Ok(false)` なら、渡したものは捨てること）。
/// ファイルが消えた・リンクに変わった・大きさが違う、も `Ok(false)`。
pub fn read_blob(root: &Path, rel: &str, size: u64, sha256: &str, chunk: &mut dyn FnMut(u64, &[u8])) -> std::io::Result<bool> {
    if rel.is_empty() || rel.starts_with('/') || rel.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("目録のパスでない: {rel}")));
    }
    let path = root.join(rel);
    let mut f = match fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&path) {
        Ok(f) => f,
        // 消えた・シンボリックリンクに変わった（ELOOP）= 書き換わった。
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ELOOP) => return Ok(false),
        Err(e) => return Err(e),
    };
    let meta = f.metadata()?;
    if !meta.is_file() || meta.len() != size {
        return Ok(false);
    }
    thread_local! {
        static BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    }
    BUF.with(|b| {
        let mut buf = b.borrow_mut();
        if buf.len() < CHUNK_BYTES {
            buf.resize(CHUNK_BYTES, 0);
        }
        let mut h = Sha256::new();
        let mut off = 0u64;
        loop {
            // 1 回の chunk はちょうど CHUNK_BYTES（最後だけ短い）にそろえる。
            let mut fill = 0;
            while fill < CHUNK_BYTES {
                match f.read(&mut buf[fill..CHUNK_BYTES]) {
                    Ok(0) => break,
                    Ok(k) => fill += k,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            if fill == 0 {
                break;
            }
            if off + fill as u64 > size {
                return Ok(false);
            }
            h.update(&buf[..fill]);
            chunk(off, &buf[..fill]);
            off += fill as u64;
            if fill < CHUNK_BYTES {
                break;
            }
        }
        Ok(off == size && hex(&h.finalize()) == sha256)
    })
}

/// 根の `.gitignore`（入れ子のものは見ない）。
#[derive(Debug, Default)]
struct Gitignore {
    rules: Vec<Rule>,
}

#[derive(Debug)]
struct Rule {
    /// `/` の前置きと後置きを外した型。
    pat: Box<[u8]>,
    negate: bool,
    dir_only: bool,
    /// 根からのパス全体と比べる（型に `/` がある）。無ければ名前だけと比べる。
    anchored: bool,
}

impl Gitignore {
    fn load(path: &Path) -> Gitignore {
        // .gitignore がシンボリックリンクなら読まない（木の外を読まない）。
        match fs::symlink_metadata(path) {
            Ok(m) if m.is_file() => {}
            _ => return Gitignore::default(),
        }
        read_small(path, GITIGNORE_READ_MAX).map(|t| Gitignore::parse(&t)).unwrap_or_default()
    }

    fn parse(text: &[u8]) -> Gitignore {
        let mut rules = Vec::new();
        for line in text.split(|&b| b == b'\n') {
            let mut l = line.strip_suffix(b"\r").unwrap_or(line);
            // 末尾の空白は外す（`\ ` で守られたものは残す）。
            while let [rest @ .., b' ' | b'\t'] = l {
                if rest.last() == Some(&b'\\') {
                    break;
                }
                l = rest;
            }
            if l.is_empty() || l[0] == b'#' {
                continue;
            }
            let mut negate = false;
            if l[0] == b'!' {
                negate = true;
                l = &l[1..];
            } else if l.starts_with(b"\\!") || l.starts_with(b"\\#") {
                l = &l[1..];
            }
            let mut dir_only = false;
            if let Some(rest) = l.strip_suffix(b"/") {
                dir_only = true;
                l = rest;
            }
            let mut anchored = l.contains(&b'/');
            if let Some(rest) = l.strip_prefix(b"/") {
                anchored = true;
                l = rest;
            }
            if l.is_empty() {
                continue;
            }
            rules.push(Rule { pat: l.into(), negate, dir_only, anchored });
        }
        Gitignore { rules }
    }

    /// `rel`（根からの相対・`/` 区切り）を除くか。最後に当たった規則が決める。
    fn ignored(&self, rel: &str, is_dir: bool) -> bool {
        let path = rel.as_bytes();
        let name = rel.rsplit('/').next().unwrap_or(rel).as_bytes();
        let mut out = false;
        for r in &self.rules {
            if r.dir_only && !is_dir {
                continue;
            }
            let subject = if r.anchored { path } else { name };
            if glob(&r.pat, subject) {
                out = !r.negate;
            }
        }
        out
    }
}

/// gitignore の型の一致。`*` `?` `[…]` は `/` を越えない。`**/` は 0 個以上のフォルダ、
/// 末尾の `**` は何でも。`\` は次の 1 文字をそのまま。
fn glob(p: &[u8], s: &[u8]) -> bool {
    if let Some(rest) = p.strip_prefix(b"**") {
        if rest.is_empty() {
            return true;
        }
        if let Some(rest) = rest.strip_prefix(b"/") {
            if glob(rest, s) {
                return true;
            }
            return s.iter().enumerate().any(|(i, &c)| c == b'/' && glob(rest, &s[i + 1..]));
        }
        // ほかの位置の `**` は `*` と同じ。
        return glob(&p[1..], s);
    }
    match p.first() {
        None => s.is_empty(),
        Some(b'*') => {
            let rest = &p[1..];
            for i in 0..=s.len() {
                if glob(rest, &s[i..]) {
                    return true;
                }
                if i < s.len() && s[i] == b'/' {
                    break;
                }
            }
            false
        }
        Some(b'?') => !s.is_empty() && s[0] != b'/' && glob(&p[1..], &s[1..]),
        Some(b'[') => match class(&p[1..], s.first().copied()) {
            Some((true, used)) => glob(&p[1 + used..], &s[1..]),
            Some((false, _)) => false,
            // 閉じていない `[` はただの文字。
            None => s.first() == Some(&b'[') && glob(&p[1..], &s[1..]),
        },
        Some(b'\\') if p.len() >= 2 => s.first() == Some(&p[1]) && glob(&p[2..], &s[1..]),
        Some(&c) => s.first() == Some(&c) && glob(&p[1..], &s[1..]),
    }
}

/// `[` の後ろの文字の組。（当たったか, `]` までのバイト数）。閉じていなければ `None`。
fn class(p: &[u8], c: Option<u8>) -> Option<(bool, usize)> {
    let mut i = 0;
    let negate = matches!(p.first(), Some(b'!' | b'^'));
    if negate {
        i += 1;
    }
    let mut hit = false;
    let mut first = true;
    while i < p.len() {
        let a = p[i];
        if a == b']' && !first {
            let ok = c.is_some_and(|c| c != b'/') && (hit != negate);
            return Some((ok, i + 1));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1] == b'-' && p[i + 2] != b']' {
            if let Some(c) = c {
                if a <= c && c <= p[i + 2] {
                    hit = true;
                }
            }
            i += 3;
        } else {
            if c == Some(a) {
                hit = true;
            }
            i += 1;
        }
    }
    None
}

// 受け入れ試験（課題 D3）は tests/scan.rs に（一時フォルダは試験ごとに別）。ここには、
// 外から試しにくいもの（`$HOME` の手前で止まる・型の一致）だけを置く。
#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zc-scan-unit-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::canonicalize(&d).unwrap()
    }

    #[test]
    fn glob_rules() {
        assert!(glob(b"*.o", b"a.o"));
        assert!(!glob(b"*.o", b"a/b.o"));
        assert!(glob(b"a/*/c", b"a/b/c"));
        assert!(!glob(b"a/*/c", b"a/b/x/c"));
        assert!(glob(b"**/foo", b"foo"));
        assert!(glob(b"**/foo", b"a/b/foo"));
        assert!(glob(b"a/**/b", b"a/b"));
        assert!(glob(b"a/**/b", b"a/x/y/b"));
        assert!(glob(b"a/**", b"a/x/y"));
        assert!(!glob(b"a/**", b"a"));
        assert!(glob(b"?.rs", b"x.rs"));
        assert!(!glob(b"?.rs", b"xy.rs"));
        assert!(glob(b"*.py[cod]", b"m.pyc"));
        assert!(!glob(b"*.py[cod]", b"m.pyx"));
        assert!(glob(b"[!a]b", b"xb"));
        assert!(!glob(b"[!a]b", b"ab"));
        assert!(glob(b"[a-c]1", b"b1"));
        assert!(glob(b"\\*x", b"*x"));
        assert!(!glob(b"\\*x", b"ax"));
        assert!(glob(b"[x", b"[x"));
    }

    #[test]
    fn gitignore_parse_and_order() {
        let g = Gitignore::parse(b"# c\n\n*.log\n!keep.log\n/out\nsrc/gen/\ndocs/**/*.tmp\ntrail   \n");
        assert!(g.ignored("a.log", false));
        assert!(g.ignored("x/a.log", false));
        assert!(!g.ignored("keep.log", false));
        assert!(g.ignored("out", true));
        assert!(g.ignored("out", false));
        assert!(!g.ignored("x/out", true));
        assert!(g.ignored("src/gen", true));
        assert!(!g.ignored("src/gen", false));
        assert!(g.ignored("docs/a/b/c.tmp", false));
        assert!(g.ignored("docs/c.tmp", false));
        assert!(g.ignored("trail", false));
    }

    #[test]
    fn stops_before_home() {
        let base = tmp("home");
        // home/Cargo.toml は見ない（$HOME の手前で止まる）。
        fs::write(base.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::create_dir_all(base.join("code/x")).unwrap();
        fs::write(base.join("code/x/main.rs"), "fn main(){}").unwrap();
        let p = find_project_below(&base.join("code/x/main.rs"), Lang::Rust, &base).unwrap();
        assert_eq!(p.shape, Shape::SingleFile);
        assert_eq!(p.root, base.join("code/x"));
        // $HOME の外のファイルなら、その上の Cargo.toml も見る。
        let p = find_project_below(&base.join("code/x/main.rs"), Lang::Rust, Path::new("/nonexistent-home")).unwrap();
        assert_eq!(p.shape, Shape::Tree);
        assert_eq!(p.root, base);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn relative_path_is_refused() {
        assert!(matches!(find_project(Path::new("a/b.rs"), Lang::Rust), Err(ProjectError::Io(_))));
    }

    #[test]
    fn cache_trims_oldest() {
        let mut c = HashCache::new();
        for g in 0..(CACHE_MAX as u64 + 10) {
            c.gen = g;
            c.entries.insert(PathBuf::from(format!("/f{g}")), Entry { size: 0, mtime_ns: 0, ino: 0, sha256: [0; 32], used: g });
        }
        c.trim();
        assert_eq!(c.len(), CACHE_MAX);
        assert!(!c.entries.contains_key(Path::new("/f0")));
        assert!(c.entries.contains_key(&PathBuf::from(format!("/f{}", CACHE_MAX + 9))));
    }
}
