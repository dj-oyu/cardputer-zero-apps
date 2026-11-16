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

use sha2::{Digest, Sha256};

use crate::limits;
use crate::msg::{FileEntry, JobOpts, Kind, Lang};

/// パス全体の上限（バイト）。
const MAX_PATH: usize = 1024;
/// パスの 1 要素の上限（バイト）。
const MAX_COMPONENT: usize = 255;

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
    ///
    /// 確かめる順: ファイル数 → 各行（パス・ハッシュ・1 ファイルの大きさ）→ 重複 → 合計。
    pub fn new(mut files: Vec<FileEntry>) -> Result<Manifest, ManifestError> {
        if files.len() > limits::MAX_TREE_FILES {
            return Err(ManifestError::TooLarge("ファイル数"));
        }
        for f in &files {
            if !is_canonical_path(&f.path) {
                return Err(ManifestError::BadPath(f.path.clone()));
            }
            if !is_sha256_hex(&f.sha256) {
                return Err(ManifestError::BadHash(f.sha256.clone()));
            }
            if f.size > limits::MAX_FILE_BYTES {
                return Err(ManifestError::TooLarge("1 ファイルの大きさ"));
            }
        }
        // String の順はバイト列の順
        files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        if let Some(w) = files.windows(2).find(|w| w[0].path == w[1].path) {
            return Err(ManifestError::Duplicate(w[0].path.clone()));
        }
        // 1 ファイルが 4MiB 以下・2000 個以下なので、足しても u64 はあふれない
        let total: u64 = files.iter().map(|f| f.size).sum();
        if total > limits::MAX_TREE_BYTES {
            return Err(ManifestError::TooLarge("木の合計の大きさ"));
        }
        Ok(Manifest { files })
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
        let mut h = Sha256::new();
        for f in &self.files {
            h.update(f.path.as_bytes());
            h.update(b"\0");
            h.update(f.size.to_string().as_bytes());
            h.update(b"\0");
            h.update(f.sha256.as_bytes());
            h.update(if f.exec { b"\0x\n" } else { b"\0-\n" });
        }
        hex(&h.finalize())
    }
}

/// パスが正規形か（相対・`/` 区切り・`.` `..` 空の要素なし・`\` と制御文字なし・
/// 1 要素 255 バイト以下・全体 1024 バイト以下）。
pub fn is_canonical_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH
        && !path.chars().any(|c| c == '\\' || c.is_control())
        && path.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && c.len() <= MAX_COMPONENT)
}

/// SHA-256 の 16 進 64 桁（小文字）か。
fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// job id（16 進 32 桁）。
pub fn job_id(tree_id: &str, kind: Kind, lang: Lang, opts: &JobOpts) -> String {
    let kind = match kind {
        Kind::Build => "build",
        Kind::Check => "check",
        Kind::Test => "test",
    };
    let lang = match lang {
        Lang::Rust => "rust",
        Lang::C => "c",
        Lang::Node => "node",
    };
    let mut h = Sha256::new();
    h.update(b"zc-job-v1\0");
    h.update(tree_id.as_bytes());
    h.update(b"\0");
    h.update(kind.as_bytes());
    h.update(b"\0");
    h.update(lang.as_bytes());
    h.update(b"\0");
    h.update(crate::msg::encode(opts).as_bytes());
    hex(&h.finalize()[..16])
}

/// project id（16 進 16 桁）。`root` はプロジェクトの根の絶対パスのバイト列。
pub fn project_id(root: &[u8]) -> String {
    hex(&Sha256::digest(root)[..8])
}

/// SHA-256 を 16 進 64 桁（小文字）にする。任意の長さのバイト列に使える（2 倍の桁になる）。
pub fn hex(digest: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(p: &str, s: u64) -> FileEntry {
        FileEntry { path: p.into(), size: s, sha256: "ab".repeat(32), exec: false }
    }

    // 受け入れ試験（課題 W2）。正解の値は docs/stage3-design.md §5.1 の例と同じにする。
    #[test]
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
    fn hostile_paths_are_rejected() {
        for p in ["/etc/passwd", "../x", "a/../b", "a//b", "a\\b", "", "a/./b", "a\0b"] {
            assert!(!is_canonical_path(p), "{p:?}");
            assert!(Manifest::new(vec![e(p, 1)]).is_err(), "{p:?}");
        }
        assert!(is_canonical_path("src/日本語.rs"));
        assert!(matches!(Manifest::new(vec![e("a", 1), e("a", 1)]), Err(ManifestError::Duplicate(_))));
        assert!(matches!(Manifest::new(vec![e("a", crate::limits::MAX_FILE_BYTES + 1)]), Err(ManifestError::TooLarge(_))));
    }

    /// 正解の値は、正規形の文字列を Python の hashlib で別に計算したもの。
    /// ここが変わるなら約束が変わった（既存の job id が全部変わる）ということ。
    #[test]
    fn golden_ids() {
        let m = Manifest::new(vec![
            FileEntry { path: "src/main.rs".into(), size: 10, sha256: "cd".repeat(32), exec: true },
            FileEntry { path: "Cargo.toml".into(), size: 5, sha256: "ab".repeat(32), exec: false },
        ])
        .unwrap();
        let t = m.tree_id();
        assert_eq!(t, "a06f6bd1841e1034b0daf6ceaf2de174f39ba19d681600bbdb2f924903a32858");
        assert_eq!(job_id(&t, Kind::Build, Lang::Rust, &JobOpts::default()), "9500808e056df402f19ba0675777dbbc");
        assert_eq!(job_id(&t, Kind::Check, Lang::Rust, &JobOpts::default()), "ce540e11525fafe577aca7c396c50388");
        let rel = JobOpts { release: true, ..JobOpts::default() };
        assert_eq!(job_id(&t, Kind::Build, Lang::Rust, &rel), "4d2de443f73294c4b7d8d6b22a050886");
        let o = JobOpts { release: false, entry: Some("src/a.ts".into()), filter: Some("x".into()) };
        assert_eq!(job_id(&t, Kind::Test, Lang::Node, &o), "885a8ccd99963d54bfa5cbf877b70e89");
        assert_eq!(project_id(b"/home/u/code/foo"), "6c994ab425f895e4");
        // 空の目録は空の文字列の SHA-256
        assert_eq!(Manifest::new(vec![]).unwrap().tree_id(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn tree_id_sees_every_field() {
        let base = || vec![e("a", 1), e("b/c", 2)];
        let t0 = Manifest::new(base()).unwrap().tree_id();
        let mut v = base();
        v[0].size = 3;
        assert_ne!(Manifest::new(v).unwrap().tree_id(), t0);
        let mut v = base();
        v[1].sha256 = "cd".repeat(32);
        assert_ne!(Manifest::new(v).unwrap().tree_id(), t0);
        let mut v = base();
        v[1].exec = true;
        assert_ne!(Manifest::new(v).unwrap().tree_id(), t0);
        let mut v = base();
        v[1].path = "b/d".into();
        assert_ne!(Manifest::new(v).unwrap().tree_id(), t0);
        // 区切りがあるので、パスと大きさの境目をずらしても同じにならない
        assert_ne!(Manifest::new(vec![e("a1", 1)]).unwrap().tree_id(), Manifest::new(vec![e("a", 11)]).unwrap().tree_id());
    }

    #[test]
    fn sorted_by_path_bytes() {
        // バイト列の順（大文字が先、'.'(0x2e) < '/'(0x2f)、UTF-8 の多バイト文字は最後）
        let m = Manifest::new(vec![e("a/b", 1), e("é", 1), e("a.b", 1), e("B", 1), e("a", 1)]).unwrap();
        let p: Vec<&str> = m.files().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(p, ["B", "a", "a.b", "a/b", "é"]);
        assert_eq!(m.total_bytes(), 5);
    }

    #[test]
    fn job_id_ignores_nothing_but_order() {
        let t = "0".repeat(64);
        let d = JobOpts::default();
        let mut ids = std::collections::HashSet::new();
        for k in [Kind::Build, Kind::Check, Kind::Test] {
            for l in [Lang::Rust, Lang::C, Lang::Node] {
                assert!(ids.insert(job_id(&t, k, l, &d)));
            }
        }
        assert!(ids.insert(job_id(&"1".repeat(64), Kind::Build, Lang::Rust, &d)));
        assert!(ids.insert(job_id(&t, Kind::Build, Lang::Rust, &JobOpts { filter: Some(String::new()), ..d.clone() })));
        // 同じ入力は何度でも同じ
        assert_eq!(job_id(&t, Kind::Build, Lang::Rust, &d), job_id(&t, Kind::Build, Lang::Rust, &JobOpts::default()));
    }

    #[test]
    fn more_hostile_paths() {
        for p in [
            "a/",
            "/",
            ".",
            "..",
            "./a",
            "a/.",
            "a/..",
            "C:\\x",
            "a\nb",
            "a\rb",
            "a\tb",
            "a\u{7f}b",
            "a\u{85}b", // C1 制御文字
        ] {
            assert!(!is_canonical_path(p), "{p:?}");
        }
        for p in ["a", ".cargo/config.toml", "...", "..a", "a..", "a b", "C:x", "a/.b/c"] {
            assert!(is_canonical_path(p), "{p:?}");
        }
        // 長さの上限（1 要素 255、全体 1024）
        assert!(is_canonical_path(&"x".repeat(255)));
        assert!(!is_canonical_path(&"x".repeat(256)));
        let long = vec!["y".repeat(200); 5].join("/");
        assert_eq!(long.len(), 1004);
        assert!(is_canonical_path(&long));
        let too_long = vec!["y".repeat(205); 5].join("/");
        assert!(too_long.len() > 1024);
        assert!(!is_canonical_path(&too_long));
        // 1 要素 255 バイトは文字数ではなくバイト数（「あ」は 3 バイト）
        assert!(is_canonical_path(&"あ".repeat(85)));
        assert!(!is_canonical_path(&"あ".repeat(86)));
        assert!(matches!(Manifest::new(vec![e("../x", 1)]), Err(ManifestError::BadPath(p)) if p == "../x"));
    }

    #[test]
    fn bad_hashes() {
        for h in ["", "ab", &"AB".repeat(32), &"g0".repeat(32), &"ab".repeat(33), &format!("{} ", "a".repeat(63))] {
            let f = FileEntry { path: "a".into(), size: 1, sha256: h.to_string(), exec: false };
            assert!(matches!(Manifest::new(vec![f]), Err(ManifestError::BadHash(_))), "{h:?}");
        }
    }

    #[test]
    fn limits() {
        use crate::limits::{MAX_FILE_BYTES, MAX_TREE_BYTES, MAX_TREE_FILES};
        // ちょうどは通る
        assert!(Manifest::new(vec![e("a", MAX_FILE_BYTES)]).is_ok());
        let n = (MAX_TREE_BYTES / MAX_FILE_BYTES) as usize;
        let full: Vec<FileEntry> = (0..n).map(|i| e(&format!("f{i}"), MAX_FILE_BYTES)).collect();
        assert_eq!(Manifest::new(full.clone()).unwrap().total_bytes(), MAX_TREE_BYTES);
        let mut over = full;
        over.push(e("one-more", 1));
        assert!(matches!(Manifest::new(over), Err(ManifestError::TooLarge(_))));
        let many: Vec<FileEntry> = (0..MAX_TREE_FILES).map(|i| e(&format!("f{i}"), 0)).collect();
        assert!(Manifest::new(many.clone()).is_ok());
        let mut too_many = many;
        too_many.push(e("x", 0));
        assert!(matches!(Manifest::new(too_many), Err(ManifestError::TooLarge(_))));
        // 大きさの巨大な値でもあふれない（1 ファイルの上限で先に断る）
        assert!(matches!(Manifest::new(vec![e("a", u64::MAX), e("b", u64::MAX)]), Err(ManifestError::TooLarge(_))));
    }

    #[test]
    fn hex_of_any_length() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }
}
