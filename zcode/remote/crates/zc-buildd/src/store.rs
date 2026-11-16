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

use std::path::{Path, PathBuf};
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

/// 写しの結果（ログと試験用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub written: u32,
    pub removed: u32,
    pub unchanged: u32,
}

/// 置き場。受付に 1 つ（接続のスレッドから同時に使う）。
pub struct Store {
    _root: PathBuf,
}

impl Store {
    /// `data_dir` の下に置き場を開く（フォルダは無ければ作る）。
    pub fn open(data_dir: &Path) -> std::io::Result<Store> {
        let _ = data_dir;
        todo!("B2")
    }

    /// 目録のうち、まだ持っていない中身の SHA-256（重複なし、目録の順）。
    pub fn missing(&self, manifest: &Manifest) -> Vec<String> {
        let _ = manifest;
        todo!("B2")
    }

    /// 中身 1 つの受け取りを始める。
    pub fn begin_blob(&self, sha256: &str, size: u64) -> Result<BlobWriter, StoreError> {
        let _ = (sha256, size);
        todo!("B2")
    }

    /// 目録のとおりに作業の木を作る（全部の中身がそろっていること）。同じプロジェクトへの
    /// 写しは同時に 1 つだけ（中で待たせる）。走っているジョブがある間は呼ばない
    /// （台帳が順番を守る）。
    pub fn sync_tree(&self, project: &str, manifest: &Manifest) -> Result<SyncStats, StoreError> {
        let _ = (project, manifest);
        todo!("B2")
    }

    /// 作業の木（`proj/<project id>/src`）。
    pub fn src_dir(&self, project: &str) -> PathBuf {
        let _ = project;
        todo!("B2")
    }

    /// プロジェクトのフォルダ（`proj/<project id>`）。
    pub fn project_dir(&self, project: &str) -> PathBuf {
        let _ = project;
        todo!("B2")
    }

    /// ジョブの成果物のフォルダ（`out/<job id>`。無ければ作る）。
    pub fn out_dir(&self, job: &str) -> std::io::Result<PathBuf> {
        let _ = job;
        todo!("B2")
    }

    /// 上限を超えていたら古いものを消す。消したバイト数を返す。
    pub fn collect_garbage(&self, quota: u64) -> std::io::Result<u64> {
        let _ = quota;
        todo!("B2")
    }
}

/// 中身 1 つの受け取り。
pub struct BlobWriter {
    _private: (),
}

impl BlobWriter {
    /// 続きを書く。`offset` は順番どおりでなければ [`StoreError::Mismatch`]。
    pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), StoreError> {
        let _ = (offset, data);
        todo!("B2")
    }

    /// 終わり。大きさとハッシュが合えば置き場に置く。
    pub fn finish(self) -> Result<(), StoreError> {
        todo!("B2")
    }
}

// 受け入れ試験（課題 B2）は tests/store.rs に:
// 2 回目の写しは 1 つも書かない（mtime が変わらない）、1 ファイル変えるとそれだけ書く、
// 目録から消えたファイルが消える、ハッシュの違う中身は置かれない、木の中に道具が作った
// シンボリックリンク（外を指す）があっても外に書かない、覚え（tree.json）を消しても正しい、
// 同じ中身の 2 ファイル、空のファイル、実行権限、上限での掃除。
