//! 保存の出口。エディタ画面は [`Saver`] に依頼を渡すだけで、メインスレッドでは
//! ファイルに書かない（design.md §3・§8.3）。
//!
//! 既定の [`StoreSaver`] は §8 の書き込み層（`zc-store`）に依頼する: storage スレッドで
//! 「一時ファイル → fsync → rename → フォルダの fsync」、内容が前回と同じでディスクも
//! 変わっていなければ書かない、ほかのプログラムが変えたファイルを上書きしたら知らせる。

use crate::app::{AppEvent, LoopCx};
use crate::doc::SaveWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zc_core::{DocId, Version};
use zc_store::{BackupDirs, Content, Restore, SaveOutcome, Snapshot, StoreRequest, StoreResponse, StoreService, Tier};

/// 保存の依頼。
pub struct SaveRequest {
    pub doc: DocId,
    pub path: PathBuf,
    /// 書き出す中身の版。結果（[`AppEvent::Saved`]）にそのまま付けて返す。
    pub version: Version,
    /// 中身を書き出す関数（文書の写しを抱えている。storage スレッドで呼ぶ）。
    pub write: SaveWriter,
}

/// 保存できたときの中身。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveOk {
    /// 書いた。`overwrote_external_change` は、開いた後（前回の保存の後）にほかの
    /// プログラムがファイルを変えていて、それを上書きしたこと。
    Written { overwrote_external_change: bool },
    /// 内容が前回と同じで、ディスクも変わっていなかったので書かなかった。
    Unchanged,
}

/// 開いたファイルの中身を書き込み層に知らせる関数（batch スレッドで呼ぶ）。
pub type LoadNote = Arc<dyn Fn(&Path, &[u8]) + Send + Sync>;

/// 保存を引き受けるもの。`save` はすぐ戻り、結果は `cx.results` に
/// [`AppEvent::Saved`] で届ける（同じファイルへの依頼が重なったら最新だけでよい）。
pub trait Saver {
    fn save(&mut self, req: SaveRequest, cx: &mut LoopCx<'_>);
    /// ファイルを開いて読んだ中身を知らせる口（「変わっていなければ書かない」の判定用）。
    fn load_note(&self) -> Option<LoadNote> {
        None
    }
    /// 文書を閉じた。
    fn closed(&mut self, _path: &Path) {}
    /// SKK の個人辞書（`~/.skk-uim-jisyo`）に学習した語を書き戻す（storage スレッドで、
    /// uim-skk と同じロックを取って読み直し、重ねて丸ごと書く）。
    fn skk_sync(&mut self, _changes: Vec<zc_store::DictChange>, _cx: &mut LoopCx<'_>) {}
    /// 控え（未保存の内容とセッション）を書く（storage スレッドで。前回と同じなら書かない）。
    fn backup(&mut self, _tier: Tier, _snapshot: Snapshot, _cx: &mut LoopCx<'_>) {}
    /// 控えを全部消す（storage スレッドで）。
    fn discard_backups(&mut self, _cx: &mut LoopCx<'_>) {}
    /// 終了の直前: 控えを書く、または消す（その場で。storage スレッドを待たない）。
    fn finish_backups(&mut self, _snapshot: Option<Snapshot>) {}
    /// 起動時: 復元の候補を読む仕事（storage スレッドで走らせる）。
    fn restore_job(&self) -> Option<Box<dyn FnOnce() -> Option<Restore> + Send>> {
        None
    }
}

/// `zc-store` の storage サービスに依頼する。
pub struct StoreSaver {
    svc: Arc<StoreService>,
}

impl StoreSaver {
    pub fn new(svc: Arc<StoreService>) -> StoreSaver {
        StoreSaver { svc }
    }
    /// 控えの置き場を環境（`XDG_RUNTIME_DIR` / `XDG_CACHE_HOME`）から決める。作るだけでは
    /// ディスクに触らない。
    pub fn from_env() -> StoreSaver {
        StoreSaver::new(StoreService::new(BackupDirs::from_env()))
    }
    pub fn service(&self) -> &Arc<StoreService> {
        &self.svc
    }
}

fn wrap(r: StoreResponse) -> AppEvent {
    match r {
        StoreResponse::Saved { doc, version, outcome, .. } => {
            let ok = match outcome {
                SaveOutcome::Written { overwrote_external_change, .. } => SaveOk::Written { overwrote_external_change },
                SaveOutcome::Unchanged => SaveOk::Unchanged,
            };
            AppEvent::Saved { doc, version, result: Ok(ok) }
        }
        StoreResponse::SaveFailed { doc, version, error, .. } => AppEvent::Saved { doc, version, result: Err(error) },
        other => AppEvent::Store(other),
    }
}

impl Saver for StoreSaver {
    fn save(&mut self, req: SaveRequest, cx: &mut LoopCx<'_>) {
        let SaveRequest { doc, path, version, write } = req;
        // 文字列にするのは storage スレッドで（メインで大きなコピーをしない）
        let content = Content::Lazy(Box::new(move || {
            let mut v = Vec::new();
            // Vec への書き出しは失敗しない
            let _ = write(&mut v);
            v
        }));
        zc_store::submit(&cx.workers.storage, &self.svc, StoreRequest::Save { doc, version, path, content }, cx.results.clone(), wrap);
    }

    fn load_note(&self) -> Option<LoadNote> {
        let svc = self.svc.clone();
        Some(Arc::new(move |path: &Path, bytes: &[u8]| svc.note_loaded(path, bytes)))
    }

    fn closed(&mut self, path: &Path) {
        self.svc.forget(path);
    }

    fn skk_sync(&mut self, changes: Vec<zc_store::DictChange>, cx: &mut LoopCx<'_>) {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
        let dict = zc_store::SkkUserDict::uim_default(&home);
        zc_store::submit(&cx.workers.storage, &self.svc, StoreRequest::SkkSync { dict, changes }, cx.results.clone(), wrap);
    }

    fn backup(&mut self, tier: Tier, snapshot: Snapshot, cx: &mut LoopCx<'_>) {
        zc_store::submit(&cx.workers.storage, &self.svc, StoreRequest::Backup { tier, snapshot }, cx.results.clone(), wrap);
    }

    fn discard_backups(&mut self, cx: &mut LoopCx<'_>) {
        zc_store::submit(&cx.workers.storage, &self.svc, StoreRequest::DiscardBackups, cx.results.clone(), wrap);
    }

    fn finish_backups(&mut self, snapshot: Option<Snapshot>) {
        match snapshot {
            Some(snapshot) => {
                for tier in Tier::ALL {
                    self.svc.handle(StoreRequest::Backup { tier, snapshot: snapshot.clone() });
                }
            }
            None => {
                self.svc.handle(StoreRequest::DiscardBackups);
            }
        }
    }

    fn restore_job(&self) -> Option<Box<dyn FnOnce() -> Option<Restore> + Send>> {
        let svc = self.svc.clone();
        Some(Box::new(move || svc.restore_candidates()))
    }
}
