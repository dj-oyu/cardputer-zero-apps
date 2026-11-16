//! 原子的な書き込み（design.md §8.3「電源断に耐える」）。
//!
//! 一時ファイル（同じフォルダ）に書く → fsync → rename → フォルダを fsync。途中で電源が
//! 落ちても、元のファイルか新しいファイルのどちらかが丸ごと残る。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// 書き込みの設定。
#[derive(Clone, Copy, Debug)]
pub struct WriteOptions {
    /// 新しく作るときの権限（既にあるファイルは元の権限を引き継ぐ）。
    pub new_mode: u32,
    /// `true` なら書く前に fsync しない（テスト用。実機では常に `false`）。
    pub skip_fsync: bool,
}

impl WriteOptions {
    /// 編集したファイルの保存（新しいファイルは 0644 相当。umask が効く）。
    pub const USER_FILE: WriteOptions = WriteOptions { new_mode: 0o666, skip_fsync: false };
    /// zcode 自身のデータ（控え・セッション。本人だけが読める 0600）。
    pub const PRIVATE: WriteOptions = WriteOptions { new_mode: 0o600, skip_fsync: false };
}

/// 一時ファイルの名前（同じフォルダの隠しファイル。名前は決まっているので、前回の
/// 書きかけが残っていても上書きされる）。
pub fn tmp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    path.with_file_name(format!(".{name}.zcode-tmp"))
}

/// フォルダを fsync する（rename を電源断に耐えさせる）。
pub fn fsync_dir(dir: &Path) -> io::Result<()> {
    let d = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    File::open(d)?.sync_all()
}

/// シンボリックリンクなら指す先に書く（リンクそのものを普通のファイルで置き換えない）。
fn resolve_target(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

/// `data` を `path` に原子的に書く。既にあるファイルの権限（実行権限など）は引き継ぐ。
/// ハードリンクは切れる（rename で別のファイルになる）。
pub fn write_atomic(path: &Path, data: &[u8], opt: WriteOptions) -> io::Result<()> {
    let target = resolve_target(path);
    let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
    let tmp = tmp_path(&target);
    let old_mode = fs::metadata(&target).ok().map(|m| m.permissions().mode() & 0o7777);
    let r = (|| {
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).mode(opt.new_mode).open(&tmp)?;
        if let Some(m) = old_mode {
            f.set_permissions(fs::Permissions::from_mode(m))?;
        }
        f.write_all(data)?;
        if !opt.skip_fsync {
            f.sync_all()?;
        }
        drop(f);
        fs::rename(&tmp, &target)?;
        if !opt.skip_fsync {
            fsync_dir(&dir)?;
        }
        Ok(())
    })();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn writes_and_keeps_mode_and_symlinks() {
        let d = TempDir::new("atomic");
        let p = d.path().join("a.sh");
        write_atomic(&p, b"one", WriteOptions::USER_FILE).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"one");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        write_atomic(&p, b"two", WriteOptions::USER_FILE).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o755);
        // 一時ファイルは残らない
        assert!(!tmp_path(&p).exists());
        // シンボリックリンクはリンクのまま、指す先が変わる
        let link = d.path().join("link.sh");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        write_atomic(&link, b"three", WriteOptions::USER_FILE).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&p).unwrap(), b"three");
        // 前回の書きかけ（一時ファイル）が残っていても上書きする
        fs::write(tmp_path(&p), b"garbage from a crash").unwrap();
        write_atomic(&p, b"four", WriteOptions::USER_FILE).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"four");
        assert!(!tmp_path(&p).exists());
        // 新しいファイルを本人だけの権限で
        let q = d.path().join("private.bin");
        write_atomic(&q, b"x", WriteOptions::PRIVATE).unwrap();
        assert_eq!(fs::metadata(&q).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn failure_leaves_original() {
        let d = TempDir::new("atomic-fail");
        let p = d.path().join("f.txt");
        write_atomic(&p, b"orig", WriteOptions::USER_FILE).unwrap();
        // 書き込み先のフォルダが無い → 失敗し、元のファイルは変わらない
        let bad = d.path().join("nope/f.txt");
        assert!(write_atomic(&bad, b"x", WriteOptions::USER_FILE).is_err());
        assert_eq!(fs::read(&p).unwrap(), b"orig");
    }
}
