//! 成果物の仕上げ（docs/stage3-design.md §4.5）。課題 B2。
//!
//! 1. 実行ファイルなら `aarch64-linux-gnu-strip --strip-all`（JS はそのまま）
//! 2. 大きさが [`zc_wire::limits::MAX_ARTIFACT_BYTES`] を超えたら失敗（実機の tmpfs に置けない）
//! 3. SHA-256 と大きさを取る（展開後のもの。実機が確かめる）
//! 4. `zstd -19 --zstd=wlog=21`（**窓は 2MiB まで**: 実機の展開メモリを抑える。圧縮は
//!    x86 側の仕事なので高い圧縮率でよい）で `<out_dir>/artifact.zst` に書く。`zstd` コマンドを
//!    使う（C のライブラリをリンクしない）
//!
//! 実行ファイルが aarch64 の ELF であること（`e_machine` = 183）も確かめる
//! （ホスト用を間違って返さない）。

use std::path::{Path, PathBuf};
use zc_wire::msg::ArtifactKind;

/// 仕上がった成果物。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packed {
    pub kind: ArtifactKind,
    /// 実機で置く名前（`foo`・`main.js`）。
    pub name: String,
    /// 展開後の SHA-256（16 進）と大きさ。
    pub sha256: String,
    pub bytes: u64,
    /// 圧縮したファイルと、その大きさ。
    pub zpath: PathBuf,
    pub zbytes: u64,
}

/// 仕上げの失敗（人に見せる 1 行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackError(pub String);

/// `file` を仕上げて `out_dir` に置く。
pub fn pack(file: &Path, kind: ArtifactKind, name: &str, out_dir: &Path) -> Result<Packed, PackError> {
    let _ = (file, kind, name, out_dir);
    todo!("B2")
}

// 受け入れ試験（課題 B2）: 開発コンテナでクロスビルドした小さな実行ファイルを仕上げて、
// `zstd -d` で戻すと SHA-256 と大きさが合う、窓が 2MiB 以下（`zstd -l -v` の Window Size）、
// x86_64 の実行ファイルは断る、上限超えは断る、JS は strip しない。
