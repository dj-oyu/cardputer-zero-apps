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
//!
//! 根の外を指す `path = "../x"` の依存は送らない（受付でビルドが失敗し、その診断が出る）。
//!
//! # 送らないもの
//!
//! `target/`・`node_modules/`・`.git/`・`dist/`・`build/`・名前が `.` で始まるフォルダ
//! （`.cargo/` と `.github/` は送る）、根の `.gitignore` に書かれたもの（`*`・`?`・`**`・
//! 先頭と末尾の `/`・`!` の打ち消しに対応。入れ子の `.gitignore` は見ない）、
//! シンボリックリンク（たどらない・送らない）、ふつうのファイルでないもの。
//! 上限（[`zc_wire::limits`]）を超えたら [`ScanError::TooLarge`]（何が超えたかを添える）。
//!
//! # 変わったファイルの見つけ方
//!
//! 依頼のたびに木を `stat` で歩き、覚えと（大きさ, mtime_ns, inode）が同じなら読まない。
//! **走査の直前 2 秒以内の mtime のものは無条件に読み直す**（同じ時刻の中で 2 回書かれた
//! ファイルを取りこぼさない）。エディタが保存したファイルだけに頼らない（ターミナルの
//! `git pull` や `scp` でも変わる）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use zc_wire::msg::Lang;
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
    /// 根のフォルダの直下だけ（C）。
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

/// `file`（絶対パス）のプロジェクトを探す。
pub fn find_project(file: &Path, lang: Lang) -> Result<Project, ProjectError> {
    let _ = (file, lang);
    todo!("D3")
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

/// ハッシュの覚え。serve に 1 つ（全プロジェクト共用。上限 4000 件、古い順に捨てる）。
#[derive(Debug, Default)]
pub struct HashCache {
    _private: (),
}

impl HashCache {
    pub fn new() -> HashCache {
        HashCache::default()
    }

    /// 木を歩いて目録を作る。`now_ns` は実時間（`CLOCK_REALTIME`。mtime と比べる）。
    pub fn scan(&mut self, project: &Project, now_ns: u64) -> Result<(Arc<Manifest>, ScanStats), ScanError> {
        let _ = (project, now_ns);
        todo!("D3")
    }

    /// 覚えている件数。
    pub fn len(&self) -> usize {
        todo!("D3")
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 送る直前に、ファイルが目録のとおりか確かめながら読む口。`chunk` に
/// [`zc_wire::limits::CHUNK_BYTES`] ずつ渡し、最後に SHA-256 が合えば `Ok(true)`。
/// 走査のあとで書き換わっていたら `Ok(false)`（serve は目録を作り直す）。
pub fn read_blob(root: &Path, rel: &str, size: u64, sha256: &str, chunk: &mut dyn FnMut(u64, &[u8])) -> std::io::Result<bool> {
    let _ = (root, rel, size, sha256, chunk);
    todo!("D3")
}

// 受け入れ試験（課題 D3）は tests/scan.rs に（一時フォルダは試験ごとに別）:
// 根の探し方（workspace・1 ファイル・C・package.json の依存の有無）、除外（target・.git・
// .gitignore の各書き方・シンボリックリンク）、2 回目は 1 つも読まない、中身を変えたら
// そのファイルだけ読む、同じ大きさ・同じ mtime で中身だけ変えた直後（2 秒の決まり）、
// 上限、日本語のファイル名、空のファイル、走査のあとで書き換えた read_blob。
