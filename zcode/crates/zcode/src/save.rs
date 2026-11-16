//! 保存の出口。エディタ画面は [`Saver`] に依頼を渡すだけで、メインスレッドでは
//! ファイルに書かない（design.md §3・§8.3）。
//!
//! [`AtomicFileSaver`] は storage スレッドで「一時ファイル → fsync → rename →
//! ディレクトリの fsync」を行う仮の実装。§8 の書き込み層（`zc-store`）ができたら、
//! 統合側で [`Saver`] の実装を差し替える（[`crate::editor::EditorScreen::set_saver`]）。

use crate::app::{AppEvent, LoopCx};
use crate::doc::SaveWriter;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use zc_core::{DocId, Version};

/// 保存の依頼。
pub struct SaveRequest {
    pub doc: DocId,
    pub path: PathBuf,
    /// 書き出す中身の版。結果（[`AppEvent::Saved`]）にそのまま付けて返す。
    pub version: Version,
    /// 中身を書き出す関数（文書の写しを抱えている。storage スレッドで呼ぶ）。
    pub write: SaveWriter,
}

/// 保存を引き受けるもの。`save` はすぐ戻り、結果は `cx.results` に
/// [`AppEvent::Saved`] で届ける（同じファイルへの依頼が重なったら最新だけでよい）。
pub trait Saver {
    fn save(&mut self, req: SaveRequest, cx: &mut LoopCx<'_>);
}

/// storage スレッドで原子的に書く。
#[derive(Default)]
pub struct AtomicFileSaver;

impl Saver for AtomicFileSaver {
    fn save(&mut self, req: SaveRequest, cx: &mut LoopCx<'_>) {
        let tx = cx.results.clone();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        req.path.hash(&mut h);
        // 同じファイルへの実行待ちの依頼は、新しい方で置き換わる
        cx.workers.storage.submit_keyed(h.finish(), move || {
            let SaveRequest { doc, path, version, write } = req;
            let result = write_atomic(&path, write).map_err(|e| e.to_string());
            tx.send(AppEvent::Saved { doc, version, result });
        });
    }
}

/// 一時ファイルに書いて fsync し、rename で置き換える。途中で失敗しても元のファイルは
/// 残る。**メインスレッドで呼ばない**（fsync は数十 ms 以上かかることがある）。
pub fn write_atomic(path: &Path, write: SaveWriter) -> io::Result<()> {
    // リンクなら、指している先を置き換える
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let name = target.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "ファイル名が無い"))?;
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let tmp = dir.join(format!(".{}.zcode-tmp", name.to_string_lossy()));
    let run = || -> io::Result<()> {
        let file = File::create(&tmp)?;
        let mut out = io::BufWriter::with_capacity(64 * 1024, file);
        write(&mut out)?;
        out.flush()?;
        let file = out.into_inner().map_err(|e| e.into_error())?;
        // 実行権限などは元のファイルに合わせる
        if let Ok(meta) = fs::metadata(&target) {
            let _ = file.set_permissions(meta.permissions());
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &target)?;
        // rename を電源断のあとにも残す
        if let Ok(d) = File::open(&dir) {
            let _ = d.sync_all();
        }
        Ok(())
    };
    let r = run();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer(s: &'static str) -> SaveWriter {
        Box::new(move |out| out.write_all(s.as_bytes()))
    }

    #[test]
    fn atomic_write_replaces_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("zcode-save-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.py");
        write_atomic(&p, writer("one\n")).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "one\n");
        write_atomic(&p, writer("two\n")).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "two\n");
        // 書き出しが失敗したら、元の中身のまま。一時ファイルも残さない
        let fail: SaveWriter = Box::new(|out| {
            out.write_all(b"partial")?;
            Err(io::Error::other("途中で失敗"))
        });
        assert!(write_atomic(&p, fail).is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "two\n");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        // 無いフォルダ
        assert!(write_atomic(&dir.join("nope/x.py"), writer("x")).is_err());
        // 実行権限を保つ
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            write_atomic(&p, writer("three\n")).unwrap();
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o755);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
