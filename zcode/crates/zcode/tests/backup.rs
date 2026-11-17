//! 控え（design.md §8.4）をエディタに組み込んだ動き（段階 6）。控えの置き場は試験ごとの
//! 一時フォルダ（実際の `/run/user` や `~/.cache` には書かない）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use zc_core::rt::{main_channel, MainReceiver, MainSender};
use zc_core::time::now_us;
use zc_core::{Key, KeyEvent, Mods, Pos};
use zc_store::{BackupDirs, BackupStore, DocBackup, Snapshot, StoreService, Tier};
use zcode::app::{AppEvent, LoopCx, Screen, TimerId, Timers, Workers};
use zcode::doc::DocBackend;
use zcode::editor::{EditorConfig, EditorScreen};
use zcode::save::StoreSaver;

struct Loop {
    timers: Timers,
    workers: Workers,
    tx: MainSender<AppEvent>,
    rx: MainReceiver<AppEvent>,
}

impl Loop {
    fn new() -> Loop {
        let (tx, rx) = main_channel().unwrap();
        Loop { timers: Timers::new().unwrap(), workers: Workers::new(), tx, rx }
    }
    /// `later_us` だけ先の時刻として呼ぶ（控えの「操作が止まって n 秒」を待たずに試す）。
    fn with_at<R>(&mut self, later_us: u64, f: impl FnOnce(&mut LoopCx<'_>) -> R) -> R {
        let mut cx = LoopCx { timers: &mut self.timers, workers: &self.workers, results: &self.tx, now_us: now_us() + later_us, quit: false, force_present: false };
        f(&mut cx)
    }
    fn keys(&mut self, ed: &mut EditorScreen, keys: &[KeyEvent]) {
        for k in keys {
            self.with_at(0, |cx| ed.keys(std::slice::from_ref(k), Mods::NONE, cx));
            self.pump(ed);
        }
    }
    fn type_str(&mut self, ed: &mut EditorScreen, s: &str) {
        let keys: Vec<KeyEvent> = s.chars().map(KeyEvent::char).collect();
        self.keys(ed, &keys);
    }
    fn timer(&mut self, ed: &mut EditorScreen, later_us: u64) {
        self.with_at(later_us, |cx| ed.on_timer(TimerId::Backup, cx));
        self.pump(ed);
    }
    fn pump(&mut self, ed: &mut EditorScreen) {
        for _ in 0..4 {
            self.workers.storage.wait_idle();
            self.workers.batch.wait_idle();
            let evs: Vec<AppEvent> = self.rx.drain().collect();
            for ev in evs {
                self.with_at(0, |cx| ed.on_event(ev, cx));
            }
        }
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("zcode-backup-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn dirs(root: &Path) -> BackupDirs {
    BackupDirs { ram: Some(root.join("ram")), disk: Some(root.join("disk")) }
}

/// 一時フォルダの控えを使うエディタ。`files` を開いて起動する（控えの読み込みも走る）。
fn editor(root: &Path, files: Vec<PathBuf>, lp: &mut Loop) -> EditorScreen {
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Buffer, root: root.to_path_buf(), files });
    ed.set_saver(Box::new(StoreSaver::new(StoreService::new(dirs(root)))));
    lp.with_at(0, |cx| ed.start(cx));
    lp.pump(&mut ed);
    ed
}

fn text(ed: &EditorScreen) -> String {
    zcode::edbench::doc_text(ed.active_doc().unwrap())
}

fn read(root: &Path, tier: Tier) -> Option<Snapshot> {
    BackupStore::new(dirs(root)).read(tier).unwrap()
}

#[test]
fn idle_writes_ram_then_disk_and_save_clears() {
    let root = tmp_dir("idle");
    let file = root.join("a.txt");
    fs::write(&file, "orig\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = editor(&root, vec![file.clone()], &mut lp);
    assert_eq!(ed.restore_pending(), None, "控えが無ければ尋ねない");
    lp.type_str(&mut ed, "X");
    // 2 秒止まる → 段 1（RAM）だけ
    lp.timer(&mut ed, 2_100_000);
    let ram = read(&root, Tier::Ram).expect("段 1 が書かれる");
    assert_eq!(ram.docs[0].unsaved.as_deref(), Some(&b"Xorig\n"[..]));
    assert_eq!(ram.docs[0].path.as_deref(), Some(file.to_str().unwrap()));
    assert!(read(&root, Tier::Disk).is_none(), "段 2 は 60 秒止まってから");
    // 60 秒止まる → 段 2（SD）も
    lp.timer(&mut ed, 61_000_000);
    assert!(read(&root, Tier::Disk).is_some());
    // 保存して、未保存が無くなったら控えは消える
    lp.keys(&mut ed, &[KeyEvent::new(Key::Char('s'), Mods::CTRL)]);
    lp.timer(&mut ed, 61_000_000);
    assert!(read(&root, Tier::Ram).is_none() && read(&root, Tier::Disk).is_none());
    let _ = fs::remove_dir_all(&root);
}

/// 前回の控え（a.txt の未保存の内容と、無題の文書）を置いておく。
fn leave_backup(root: &Path, file: &Path) {
    let mut store = BackupStore::new(dirs(root)).without_fsync();
    let snap = Snapshot {
        saved_at_ms: 0,
        active: Some(0),
        docs: vec![
            DocBackup { path: Some(file.to_string_lossy().into_owned()), title: "a.txt".into(), cursor: Pos::new(0, 3), scroll_line: 0, unsaved: Some(b"changed\n".to_vec()) },
            DocBackup { path: None, title: "untitled".into(), cursor: Pos::new(0, 0), scroll_line: 0, unsaved: Some(b"memo\n".to_vec()) },
        ],
    };
    assert!(store.write(Tier::Ram, &snap).unwrap());
}

#[test]
fn startup_offers_restore_and_enter_restores_as_unsaved_edit() {
    let root = tmp_dir("restore");
    let file = root.join("a.txt");
    fs::write(&file, "orig\n").unwrap();
    leave_backup(&root, &file);
    let mut lp = Loop::new();
    let mut ed = editor(&root, vec![], &mut lp);
    assert_eq!(ed.restore_pending(), Some(2));
    // 問いの間はほかのキーで文書を変えない
    lp.type_str(&mut ed, "zz");
    assert_eq!(ed.restore_pending(), Some(2));
    lp.keys(&mut ed, &[KeyEvent::new(Key::Enter, Mods::NONE)]);
    assert_eq!(ed.restore_pending(), None);
    // 無題の文書が最後に開かれる。a.txt は戻した内容で、未保存
    assert_eq!(text(&ed), "memo\n");
    lp.keys(&mut ed, &[KeyEvent::new(Key::Tab, Mods::CTRL | Mods::SHIFT)]);
    assert_eq!(text(&ed), "changed\n");
    assert!(ed.active_doc().unwrap().is_modified());
    assert_eq!(ed.active_doc().unwrap().selection().1, Pos::new(0, 3));
    assert_eq!(fs::read_to_string(&file).unwrap(), "orig\n", "ディスクには書かない");
    // 1 回の取り消しでディスクの中身に戻る
    lp.keys(&mut ed, &[KeyEvent::new(Key::Char('z'), Mods::CTRL)]);
    assert_eq!(text(&ed), "orig\n");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn esc_discards_the_backup() {
    let root = tmp_dir("discard");
    let file = root.join("a.txt");
    fs::write(&file, "orig\n").unwrap();
    leave_backup(&root, &file);
    let mut lp = Loop::new();
    let mut ed = editor(&root, vec![], &mut lp);
    assert_eq!(ed.restore_pending(), Some(2));
    lp.keys(&mut ed, &[KeyEvent::new(Key::Esc, Mods::NONE)]);
    assert_eq!(ed.restore_pending(), None);
    assert!(read(&root, Tier::Ram).is_none());
    assert!(ed.active_doc().is_none(), "何も開かない");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn backup_identical_to_disk_is_not_offered() {
    let root = tmp_dir("same");
    let file = root.join("a.txt");
    fs::write(&file, "changed\n").unwrap();
    let mut store = BackupStore::new(dirs(&root)).without_fsync();
    let doc = DocBackup { path: Some(file.to_string_lossy().into_owned()), title: "a.txt".into(), cursor: Pos::new(0, 0), scroll_line: 0, unsaved: Some(b"changed\n".to_vec()) };
    store.write(Tier::Disk, &Snapshot { saved_at_ms: 0, active: Some(0), docs: vec![doc] }).unwrap();
    let mut lp = Loop::new();
    let ed = editor(&root, vec![], &mut lp);
    assert_eq!(ed.restore_pending(), None, "保存済みと同じ内容は尋ねない");
    assert!(read(&root, Tier::Disk).is_none(), "そのまま片づける");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn signal_stop_writes_backup_and_ctrl_q_discards() {
    let root = tmp_dir("stop");
    let file = root.join("a.txt");
    fs::write(&file, "orig\n").unwrap();
    // シグナルでの終了（stop だけ呼ばれる）: 未保存の内容を両方の段に書く
    let mut lp = Loop::new();
    let mut ed = editor(&root, vec![file.clone()], &mut lp);
    lp.type_str(&mut ed, "Y");
    ed.stop();
    for t in Tier::ALL {
        assert_eq!(read(&root, t).expect("書かれる").docs[0].unsaved.as_deref(), Some(&b"Yorig\n"[..]));
    }
    // Ctrl+Q（未保存なら 2 回）で終わると、控えを消す
    let mut ed = editor(&root, vec![], &mut lp);
    assert_eq!(ed.restore_pending(), Some(1));
    lp.keys(&mut ed, &[KeyEvent::new(Key::Enter, Mods::NONE)]);
    let q = KeyEvent::new(Key::Char('q'), Mods::CTRL);
    lp.keys(&mut ed, &[q, q]);
    ed.stop();
    assert!(read(&root, Tier::Ram).is_none() && read(&root, Tier::Disk).is_none());
    let _ = fs::remove_dir_all(&root);
}
