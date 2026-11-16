//! 目録と ID（docs/stage3-design.md §5.1）。
//!
//! - **tree id** = 目録の正規形の SHA-256（16 進 64 桁）。正規形は、パスのバイト列の順に
//!   並べた各行 `<パス>\0<大きさ（10 進）>\0<sha256>\0<x か ->\n` をつないだもの
//! - **job id** = `SHA-256("zc-job-v1\0" + tree id + "\0" + kind + "\0" + lang + "\0" +
//!   オプションの JSON)` の先頭 16 バイト（16 進 32 桁）。オプションの JSON は
//!   [`crate::msg::JobOpts`] を serde_json でそのまま出したもの（項目は定義の順。既定値は
//!   出ない）。同じ木・同じ種類・同じオプションなら同じ id になり、出し直しが同じジョブに当たる
//! - **project id** = プロジェクトの根の絶対パス（バイト列）の SHA-256 の先頭 8 バイト
//!   （16 進 16 桁）。受付は作業の木とビルドのキャッシュをこの名前で分ける
//!
//! mtime は目録に入れない（実機には時計の電池が無く、cargo は mtime で変更を判定する。
//! 受付は「中身のハッシュが変わったファイルだけ書く」）。

use crate::msg::{FileEntry, JobOpts, Kind, Lang};

/// 目録（パスの順に並べ、重複なし）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Manifest {
    files: Vec<FileEntry>,
}

/// 目録として受け取れない理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// パスが正規形でない（絶対パス・`..`・空の要素・`\`・NUL・長すぎる）。
    BadPath(String),
    /// 同じパスが 2 回ある。
    Duplicate(String),
    /// SHA-256 の形でない。
    BadHash(String),
    /// 上限（[`crate::limits`]）を超えた。
    TooLarge(&'static str),
}

impl core::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ManifestError {}

impl Manifest {
    /// 並べ直し、パス・ハッシュ・上限を確かめて目録にする（実機側は送る前、受付は受け取った
    /// 直後に通す。**受付は実機を信用しない**: ここを通らないパスには書かない）。
    pub fn new(files: Vec<FileEntry>) -> Result<Manifest, ManifestError> {
        let _ = files;
        todo!("W2")
    }

    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    /// 合計の大きさ。
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// tree id（16 進 64 桁）。
    pub fn tree_id(&self) -> String {
        todo!("W2")
    }
}

/// パスが正規形か（相対・`/` 区切り・`.` `..` 空の要素なし・`\` と制御文字なし・
/// 1 要素 255 バイト以下・全体 1024 バイト以下）。
pub fn is_canonical_path(path: &str) -> bool {
    let _ = path;
    todo!("W2")
}

/// job id（16 進 32 桁）。
pub fn job_id(tree_id: &str, kind: Kind, lang: Lang, opts: &JobOpts) -> String {
    let _ = (tree_id, kind, lang, opts);
    todo!("W2")
}

/// project id（16 進 16 桁）。`root` はプロジェクトの根の絶対パスのバイト列。
pub fn project_id(root: &[u8]) -> String {
    let _ = root;
    todo!("W2")
}

/// SHA-256 を 16 進 64 桁（小文字）にする。
pub fn hex(digest: &[u8]) -> String {
    let _ = digest;
    todo!("W2")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(p: &str, s: u64) -> FileEntry {
        FileEntry { path: p.into(), size: s, sha256: "ab".repeat(32), exec: false }
    }

    // 受け入れ試験（課題 W2）。正解の値は docs/stage3-design.md §5.1 の例と同じにする。
    #[test]
    #[ignore = "W2"]
    fn ids_are_stable_and_order_independent() {
        let a = Manifest::new(vec![e("src/main.rs", 10), e("Cargo.toml", 5)]).unwrap();
        let b = Manifest::new(vec![e("Cargo.toml", 5), e("src/main.rs", 10)]).unwrap();
        assert_eq!(a.tree_id(), b.tree_id());
        assert_eq!(a.tree_id().len(), 64);
        let j1 = job_id(&a.tree_id(), Kind::Build, Lang::Rust, &JobOpts::default());
        let j2 = job_id(&a.tree_id(), Kind::Check, Lang::Rust, &JobOpts::default());
        assert_eq!(j1.len(), 32);
        assert_ne!(j1, j2);
        assert_eq!(project_id(b"/home/u/code/foo").len(), 16);
    }

    #[test]
    #[ignore = "W2"]
    fn hostile_paths_are_rejected() {
        for p in ["/etc/passwd", "../x", "a/../b", "a//b", "a\\b", "", "a/./b", "a\0b"] {
            assert!(!is_canonical_path(p), "{p:?}");
            assert!(Manifest::new(vec![e(p, 1)]).is_err(), "{p:?}");
        }
        assert!(is_canonical_path("src/日本語.rs"));
        assert!(matches!(Manifest::new(vec![e("a", 1), e("a", 1)]), Err(ManifestError::Duplicate(_))));
        assert!(matches!(Manifest::new(vec![e("a", crate::limits::MAX_FILE_BYTES + 1)]), Err(ManifestError::TooLarge(_))));
    }
}
