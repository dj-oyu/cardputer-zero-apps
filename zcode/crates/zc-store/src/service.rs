//! storage スレッドへの依頼と返事（design.md §4.2）。
//!
//! メインは [`submit`] で `Workers::storage` に依頼を出し、結果は `MainSender` 経由で
//! 受け取る（eventfd で起こされる）。同じファイルの保存・同じ段の控えは、実行待ちの
//! ものを最新に置き換える（`Worker::submit_keyed`）。
//!
//! ```no_run
//! use std::sync::Arc;
//! use zc_core::rt::{main_channel, Worker, WorkerConfig};
//! use zc_core::{DocId, Version};
//! use zc_store::{BackupDirs, Content, StoreRequest, StoreResponse, StoreService};
//! enum AppEvent { Store(StoreResponse) }
//! let (tx, rx) = main_channel::<AppEvent>().unwrap();
//! let storage = Worker::new(WorkerConfig::STORAGE);
//! let svc = StoreService::new(BackupDirs::from_env());
//! let req = StoreRequest::Save {
//!     doc: DocId(1), version: Version(7), path: "/home/u/code/a.py".into(),
//!     content: Content::Bytes(b"print(1)\n".to_vec()),
//! };
//! zc_store::submit(&storage, &svc, req, tx.clone(), AppEvent::Store);
//! ```

use crate::atomic::{write_atomic, WriteOptions};
use crate::backup::{BackupDirs, BackupStore, Restore, Snapshot, Tier};
use crate::codec::hash64;
use crate::skk::{DictChange, SkkOutcome, SkkUserDict};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};
use zc_core::rt::{MainSender, Worker};
use zc_core::{DocId, Version};

/// 保存する内容。`Lazy` なら storage スレッドで作る（rope の写しから文字列にする、など。
/// メインで大きなコピーをしない）。
pub enum Content {
    Bytes(Vec<u8>),
    Lazy(Box<dyn FnOnce() -> Vec<u8> + Send>),
}

impl Content {
    fn into_bytes(self) -> Vec<u8> {
        match self {
            Content::Bytes(b) => b,
            Content::Lazy(f) => f(),
        }
    }
}

impl core::fmt::Debug for Content {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Content::Bytes(b) => write!(f, "Content::Bytes({} bytes)", b.len()),
            Content::Lazy(_) => f.write_str("Content::Lazy"),
        }
    }
}

/// storage スレッドへの依頼。
#[derive(Debug)]
pub enum StoreRequest {
    /// 編集したファイルを保存する（原子的に。内容が前回と同じでディスクも変わっていなければ書かない）。
    Save { doc: DocId, version: Version, path: PathBuf, content: Content },
    /// 控えを書く（前回と同じなら書かない）。
    Backup { tier: Tier, snapshot: Snapshot },
    /// 控えを全部消す。
    DiscardBackups,
    /// SKK 個人辞書に変更を書き戻す（ロック → 読み直し → 重ねて丸ごと書き直す）。
    SkkSync { dict: SkkUserDict, changes: Vec<DictChange> },
}

impl StoreRequest {
    /// 置き換えの鍵（同じ鍵の実行待ちは最新だけ残す）。SKK の変更は積み重ねなので鍵なし。
    pub fn key(&self) -> Option<u64> {
        match self {
            StoreRequest::Save { path, .. } => Some(hash64(path.as_os_str().as_encoded_bytes()) | 1 << 63),
            StoreRequest::Backup { tier, .. } => Some(*tier as u64 + 1),
            StoreRequest::DiscardBackups => Some(0x10),
            StoreRequest::SkkSync { .. } => None,
        }
    }
}

/// 保存の結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// 書いた。
    Written {
        bytes: usize,
        /// 書くのにかかった時間（µs。fsync 込み）。
        took_us: u64,
        /// 開いた後（前回の保存の後）に、ほかのプログラムがファイルを変えていた（上書きした）。
        overwrote_external_change: bool,
    },
    /// 内容が前回と同じで、ディスクも変わっていなかったので書かなかった。
    Unchanged,
}

/// storage スレッドからの返事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreResponse {
    Saved { doc: DocId, version: Version, path: PathBuf, outcome: SaveOutcome },
    SaveFailed { doc: DocId, version: Version, path: PathBuf, error: String },
    BackupWritten { tier: Tier, written: bool },
    BackupFailed { tier: Tier, error: String },
    BackupsDiscarded,
    SkkSynced(SkkOutcome),
    /// ロックが取れなかったときも含む（後でもう一度出す）。
    SkkFailed { error: String, retry: bool },
}

/// ディスク上の内容の印（前回読んだ・書いたときのもの）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DiskStamp {
    hash: u64,
    len: u64,
    mtime: Option<SystemTime>,
}

fn disk_meta(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()))
}

struct State {
    stamps: HashMap<PathBuf, DiskStamp>,
    backups: BackupStore,
}

/// storage スレッドの持ち物（保存の印・控え）。`Arc` で共有し、依頼はどのスレッドからでも
/// 処理できる（中身は Mutex。実際には storage スレッドだけが使う）。
pub struct StoreService {
    state: Mutex<State>,
    opt: WriteOptions,
}

impl StoreService {
    pub fn new(dirs: BackupDirs) -> Arc<StoreService> {
        Arc::new(StoreService::with_options(dirs, WriteOptions::USER_FILE))
    }

    #[doc(hidden)]
    pub fn with_options(dirs: BackupDirs, opt: WriteOptions) -> StoreService {
        let mut backups = BackupStore::new(dirs);
        if opt.skip_fsync {
            backups = backups.without_fsync();
        }
        StoreService { state: Mutex::new(State { stamps: HashMap::new(), backups }), opt }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// ファイルを開いて読んだ内容を知らせる（batch から呼んでよい）。次の保存で
    /// 「変わっていなければ書かない」「ほかのプログラムが変えた」の判定に使う。
    pub fn note_loaded(&self, path: &Path, bytes: &[u8]) {
        if let Some((len, mtime)) = disk_meta(path) {
            self.lock().stamps.insert(path.to_path_buf(), DiskStamp { hash: hash64(bytes), len, mtime });
        }
    }

    /// 文書を閉じた（印を捨てる）。
    pub fn forget(&self, path: &Path) {
        self.lock().stamps.remove(path);
    }

    /// 起動時の復元の候補（[`BackupStore::restore`]）。
    pub fn restore_candidates(&self) -> Option<Restore> {
        self.lock().backups.restore()
    }

    /// 依頼を 1 つ処理する（storage スレッドで呼ぶ）。
    pub fn handle(&self, req: StoreRequest) -> StoreResponse {
        match req {
            StoreRequest::Save { doc, version, path, content } => {
                let bytes = content.into_bytes();
                match self.save(&path, &bytes) {
                    Ok(outcome) => StoreResponse::Saved { doc, version, path, outcome },
                    Err(e) => StoreResponse::SaveFailed { doc, version, path, error: e.to_string() },
                }
            }
            StoreRequest::Backup { tier, snapshot } => match self.lock().backups.write(tier, &snapshot) {
                Ok(written) => StoreResponse::BackupWritten { tier, written },
                Err(e) => StoreResponse::BackupFailed { tier, error: e.to_string() },
            },
            StoreRequest::DiscardBackups => match self.lock().backups.discard() {
                Ok(()) => StoreResponse::BackupsDiscarded,
                Err(e) => StoreResponse::BackupFailed { tier: Tier::Disk, error: e.to_string() },
            },
            StoreRequest::SkkSync { dict, changes } => match dict.apply(&changes) {
                Ok(o) => StoreResponse::SkkSynced(o),
                Err(e) => {
                    let retry = matches!(e, crate::skk::SkkError::LockTimeout(_));
                    StoreResponse::SkkFailed { error: e.to_string(), retry }
                }
            },
        }
    }

    fn save(&self, path: &Path, bytes: &[u8]) -> std::io::Result<SaveOutcome> {
        let hash = hash64(bytes);
        let now = disk_meta(path);
        let stamp = self.lock().stamps.get(path).copied();
        let disk_unchanged = match (stamp, now) {
            (Some(s), Some((len, mtime))) => s.len == len && s.mtime == mtime,
            _ => false,
        };
        if disk_unchanged && stamp.is_some_and(|s| s.hash == hash) {
            return Ok(SaveOutcome::Unchanged);
        }
        let overwrote_external_change = stamp.is_some() && now.is_some() && !disk_unchanged;
        let t = Instant::now();
        write_atomic(path, bytes, self.opt)?;
        let took_us = t.elapsed().as_micros() as u64;
        if let Some((len, mtime)) = disk_meta(path) {
            self.lock().stamps.insert(path.to_path_buf(), DiskStamp { hash, len, mtime });
        }
        Ok(SaveOutcome::Written { bytes: bytes.len(), took_us, overwrote_external_change })
    }
}

/// 依頼を storage ワーカーに出す。返事は `wrap` でアプリの結果の型に包んで `tx` に送る。
/// 同じファイルの保存・同じ段の控えは、実行待ちのものを最新に置き換える。
pub fn submit<T: Send + 'static>(
    worker: &Worker,
    svc: &Arc<StoreService>,
    req: StoreRequest,
    tx: MainSender<T>,
    wrap: fn(StoreResponse) -> T,
) {
    let svc = svc.clone();
    let key = req.key();
    let job = move || {
        let r = svc.handle(req);
        tx.send(wrap(r));
    };
    match key {
        Some(k) => worker.submit_keyed(k, job),
        None => worker.submit(job),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::DocBackup;
    use crate::testutil::TempDir;
    use zc_core::rt::{main_channel, WorkerConfig};

    fn svc(d: &TempDir) -> Arc<StoreService> {
        Arc::new(StoreService::with_options(
            BackupDirs { ram: Some(d.path().join("run")), disk: Some(d.path().join("cache")) },
            WriteOptions { new_mode: 0o666, skip_fsync: true },
        ))
    }

    fn save(s: &StoreService, p: &Path, b: &[u8]) -> StoreResponse {
        s.handle(StoreRequest::Save { doc: DocId(1), version: Version(1), path: p.into(), content: Content::Bytes(b.to_vec()) })
    }

    #[test]
    fn save_skips_unchanged_and_notices_external_change() {
        let d = TempDir::new("svc");
        let s = svc(&d);
        let p = d.path().join("a.py");
        std::fs::write(&p, b"v0").unwrap();
        s.note_loaded(&p, b"v0");
        // 開いたまま保存（変わっていない）→ 書かない
        assert!(matches!(save(&s, &p, b"v0"), StoreResponse::Saved { outcome: SaveOutcome::Unchanged, .. }));
        let r = save(&s, &p, b"v1");
        assert!(matches!(r, StoreResponse::Saved { outcome: SaveOutcome::Written { bytes: 2, overwrote_external_change: false, .. }, .. }), "{r:?}");
        assert!(matches!(save(&s, &p, b"v1"), StoreResponse::Saved { outcome: SaveOutcome::Unchanged, .. }));
        // ほかのプログラムが書き換えた → 同じ内容でも書き、知らせる
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&p, b"external!").unwrap();
        let r = save(&s, &p, b"v1");
        assert!(matches!(r, StoreResponse::Saved { outcome: SaveOutcome::Written { overwrote_external_change: true, .. }, .. }), "{r:?}");
        assert_eq!(std::fs::read(&p).unwrap(), b"v1");
        // 書けない場所
        let r = save(&s, &d.path().join("no/such/dir/x"), b"x");
        assert!(matches!(r, StoreResponse::SaveFailed { .. }));
    }

    #[test]
    fn through_the_worker_latest_wins() {
        let d = TempDir::new("svc-worker");
        let s = svc(&d);
        let (tx, rx) = main_channel::<StoreResponse>().unwrap();
        let w = Worker::new(WorkerConfig { lazy: true, ..WorkerConfig::STORAGE });
        let p = d.path().join("b.txt");
        // ワーカーを塞いでおき、同じファイルの保存を 3 回出す → 最後の 1 回だけ実行される
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let g = gate.clone();
        w.submit(move || {
            let mut open = g.0.lock().unwrap();
            while !*open {
                open = g.1.wait(open).unwrap();
            }
        });
        for i in 0..3u64 {
            let req = StoreRequest::Save { doc: DocId(2), version: Version(i), path: p.clone(), content: Content::Lazy(Box::new(move || format!("gen{i}").into_bytes())) };
            submit(&w, &s, req, tx.clone(), |r| r);
        }
        let snap = Snapshot { saved_at_ms: 0, active: None, docs: vec![DocBackup { title: "t".into(), unsaved: Some(b"u".to_vec()), ..Default::default() }] };
        submit(&w, &s, StoreRequest::Backup { tier: Tier::Ram, snapshot: snap.clone() }, tx.clone(), |r| r);
        submit(&w, &s, StoreRequest::Backup { tier: Tier::Ram, snapshot: snap }, tx.clone(), |r| r);
        // 塞いでいる依頼が取り出されていれば 2、まだなら 3（保存 1 つ・控え 1 つに置き換わっている）
        assert!(w.pending() <= 3);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        w.wait_idle();
        let got: Vec<StoreResponse> = rx.drain().collect();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(matches!(&got[0], StoreResponse::Saved { version: Version(2), .. }));
        assert_eq!(std::fs::read(&p).unwrap(), b"gen2");
        assert_eq!(got[1], StoreResponse::BackupWritten { tier: Tier::Ram, written: true });
        let r = s.restore_candidates().unwrap();
        assert_eq!(r.unsaved_docs().count(), 1);
        assert_eq!(s.handle(StoreRequest::DiscardBackups), StoreResponse::BackupsDiscarded);
        assert!(s.restore_candidates().is_none());
    }

    #[test]
    fn skk_through_service() {
        let d = TempDir::new("svc-skk");
        let s = svc(&d);
        let dict = SkkUserDict::uim_default(d.path());
        let r = s.handle(StoreRequest::SkkSync { dict: dict.clone(), changes: vec![DictChange::Learn { reading: "かな".into(), candidate: "仮名".into() }] });
        assert!(matches!(r, StoreResponse::SkkSynced(SkkOutcome { written: true, entries: (0, 1), .. })), "{r:?}");
        assert!(StoreRequest::SkkSync { dict, changes: vec![] }.key().is_none());
    }
}
