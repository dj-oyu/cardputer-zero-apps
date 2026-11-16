//! SD カード前提の書き込み層（design.md §8.3・§8.4）。**storage スレッドで使う。**
//!
//! | モジュール | 中身 |
//! |---|---|
//! | [`atomic`] | 原子的な保存（一時ファイル → fsync → rename → フォルダを fsync）。権限とシンボリックリンクを保つ |
//! | [`frame`] | 独自形式の枠（マジック・版・長さ・CRC32）と 1 世代前（`.bak`）。壊れていても panic せず、`.bak` か既定値で続ける |
//! | [`backup`] | 未保存の内容とセッションの控え（RAM の tmpfs と SD の 2 段）、書く時機（[`BackupPolicy`]）、起動時の復元の候補 |
//! | [`skk`] | uim-skk と共有する SKK 個人辞書の書き戻し（ロック → 読み直し → 丸ごと書き直し） |
//! | [`service`] | storage ワーカーへの依頼（[`StoreRequest`]）と返事（[`StoreResponse`]） |
//! | [`codec`] | 中身の読み書きの道具と、内容の比較に使うハッシュ |
//!
//! 書く回数を減らす: 内容が前回と同じなら書かない（ハッシュで比べる）。控えは 1 つの
//! ファイルに全部まとめて丸ごと書き直す。fsync はいつもワーカーで（メインで待たない）。

pub mod atomic;
pub mod backup;
pub mod codec;
pub mod frame;
pub mod service;
pub mod skk;

pub use atomic::{write_atomic, WriteOptions};
pub use backup::{BackupDirs, BackupPolicy, BackupStore, DocBackup, Restore, Snapshot, Tier};
pub use frame::{read_framed, write_framed, FrameError, Loaded};
pub use service::{submit, Content, SaveOutcome, StoreRequest, StoreResponse, StoreService};
pub use skk::{DictChange, SkkError, SkkOutcome, SkkUserDict};

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// テスト用の一時フォルダ（終わったら消す）。
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> TempDir {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!("zc-store-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
