//! 未保存の内容とセッションの控え（design.md §8.4）。2 段:
//!
//! | 段 | 置き場所 | いつ書くか（[`BackupPolicy`]） | 守れるもの |
//! |---|---|---|---|
//! | 1 [`Tier::Ram`] | `$XDG_RUNTIME_DIR/zcode/`（`/run/user/<uid>/zcode/`、tmpfs） | 操作が 2 秒止まるたび | zcode の異常終了 |
//! | 2 [`Tier::Disk`] | `$XDG_CACHE_HOME/zcode/`（`~/.cache/zcode/`、SD） | 操作が 60 秒止まったとき・タブを切り替えたとき・電池が少ないとき | 電池切れ・電源断 |
//!
//! どちらも 1 つのファイル（`backup.zcb`。開いているタブ全部のセッションと未保存の内容）を
//! 丸ごと書き直す（小さなファイルをたくさん作らない）。中身が前回と同じなら書かない。
//! 起動時は [`BackupStore::restore`] が 2 段の新しい方を読み、ディスクの内容と同じ控えは
//! 除いて返す（保存に成功したファイルの控えは、次の控えから消える）。

use crate::atomic::WriteOptions;
use crate::codec::{hash64, Reader, Writer};
use crate::frame::{read_framed, remove_framed, write_framed, Loaded};
use std::io;
use std::path::{Path, PathBuf};
use zc_core::Pos;

/// 控えのファイルの種類。
pub const MAGIC: [u8; 4] = *b"ZCBK";
/// 中身の形の版。
pub const VERSION: u16 = 1;
/// 控えのファイル名。
pub const FILE_NAME: &str = "backup.zcb";

/// 控えの段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    /// 段 1: RAM（tmpfs）。
    Ram = 0,
    /// 段 2: SD。
    Disk = 1,
}

impl Tier {
    pub const ALL: [Tier; 2] = [Tier::Ram, Tier::Disk];
}

/// 開いている文書 1 つぶんの控え。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DocBackup {
    /// ファイルのパス（無題なら `None`）。
    pub path: Option<String>,
    /// タブの表示名（無題の文書の名前など）。
    pub title: String,
    /// カーソルの位置。
    pub cursor: Pos,
    /// 画面の一番上の行。
    pub scroll_line: u32,
    /// 未保存の内容（保存済みなら `None`）。
    pub unsaved: Option<Vec<u8>>,
}

/// 控え 1 回ぶん（セッション全体）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// 書いた時刻（UNIX 時刻のミリ秒）。0 なら書くときに入れる。
    pub saved_at_ms: u64,
    /// 開いていたタブの番号。
    pub active: Option<u32>,
    pub docs: Vec<DocBackup>,
}

impl Snapshot {
    /// 時刻を除いた中身（「前回と同じなら書かない」の比較用）。
    fn encode_body(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.active.unwrap_or(u32::MAX));
        w.u32(self.docs.len() as u32);
        for d in &self.docs {
            w.opt_bytes(d.path.as_deref().map(str::as_bytes));
            w.str(&d.title);
            w.u32(d.cursor.line);
            w.u32(d.cursor.col);
            w.u32(d.scroll_line);
            w.opt_bytes(d.unsaved.as_deref());
        }
        w.into_inner()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u64(self.saved_at_ms);
        w.0.extend_from_slice(&self.encode_body());
        w.into_inner()
    }

    /// 壊れていれば `None`（panic しない）。
    pub fn decode(b: &[u8]) -> Option<Snapshot> {
        let mut r = Reader::new(b);
        let saved_at_ms = r.u64()?;
        let active = match r.u32()? {
            u32::MAX => None,
            v => Some(v),
        };
        let n = r.u32()? as usize;
        let mut docs = Vec::with_capacity(n.min(1024));
        for _ in 0..n {
            let path = match r.opt_bytes()? {
                Some(p) => Some(String::from_utf8(p.to_vec()).ok()?),
                None => None,
            };
            let title = r.str()?.to_string();
            let cursor = Pos::new(r.u32()?, r.u32()?);
            let scroll_line = r.u32()?;
            let unsaved = r.opt_bytes()?.map(|b| b.to_vec());
            docs.push(DocBackup { path, title, cursor, scroll_line, unsaved });
        }
        if !r.is_done() {
            return None;
        }
        Some(Snapshot { saved_at_ms, active, docs })
    }

    /// 未保存の内容を持つ文書の数。
    pub fn unsaved_count(&self) -> usize {
        self.docs.iter().filter(|d| d.unsaved.is_some()).count()
    }
}

/// 控えの置き場所。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct BackupDirs {
    /// 段 1（tmpfs）。見つからなければ `None`（段 1 は使わない）。
    pub ram: Option<PathBuf>,
    /// 段 2（SD）。
    pub disk: Option<PathBuf>,
}

impl BackupDirs {
    /// 環境から決める: 段 1 は `$XDG_RUNTIME_DIR/zcode`（無ければ `/run/user/<uid>/zcode`、
    /// その親が無ければ使わない）、段 2 は `$XDG_CACHE_HOME/zcode`（無ければ `~/.cache/zcode`）。
    pub fn from_env() -> BackupDirs {
        let env_dir = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        // SAFETY: getuid は失敗しない。
        let uid = unsafe { libc::getuid() };
        let run = env_dir("XDG_RUNTIME_DIR").unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
        let ram = run.is_dir().then(|| run.join("zcode"));
        let cache = env_dir("XDG_CACHE_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".cache")));
        BackupDirs { ram, disk: cache.map(|c| c.join("zcode")) }
    }

    pub fn dir(&self, tier: Tier) -> Option<&Path> {
        match tier {
            Tier::Ram => self.ram.as_deref(),
            Tier::Disk => self.disk.as_deref(),
        }
    }
}

/// 起動時の復元の候補。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restore {
    /// どちらの段から読んだか。
    pub tier: Tier,
    /// セッション（開いていたタブとカーソル）。未保存の内容は、ディスクと同じものを除いてある。
    pub snapshot: Snapshot,
    /// 読めなかった控えなど、ステータスバーで知らせること。
    pub notes: Vec<String>,
}

impl Restore {
    /// 復元を尋ねる対象（未保存の内容がある文書）。
    pub fn unsaved_docs(&self) -> impl Iterator<Item = &DocBackup> {
        self.snapshot.docs.iter().filter(|d| d.unsaved.is_some())
    }
}

/// 2 段の控え。storage スレッドが持つ（[`crate::StoreService`] の中）。
pub struct BackupStore {
    dirs: BackupDirs,
    /// 段ごとに、前回書いた中身（時刻を除く）のハッシュ。
    last: [Option<u64>; 2],
    opt: WriteOptions,
}

impl BackupStore {
    pub fn new(dirs: BackupDirs) -> BackupStore {
        BackupStore { dirs, last: [None; 2], opt: WriteOptions::PRIVATE }
    }
    /// テスト用: fsync を省く。
    #[doc(hidden)]
    pub fn without_fsync(mut self) -> BackupStore {
        self.opt.skip_fsync = true;
        self
    }
    pub fn dirs(&self) -> &BackupDirs {
        &self.dirs
    }
    /// 段のファイルのパス。
    pub fn path(&self, tier: Tier) -> Option<PathBuf> {
        self.dirs.dir(tier).map(|d| d.join(FILE_NAME))
    }

    /// 控えを書く。前回この段に書いた中身と同じなら書かない（`Ok(false)`）。
    /// 段の置き場所が無ければ何もしない（`Ok(false)`）。
    pub fn write(&mut self, tier: Tier, snap: &Snapshot) -> io::Result<bool> {
        let Some(path) = self.path(tier) else { return Ok(false) };
        let body_hash = hash64(&snap.encode_body());
        if self.last[tier as usize] == Some(body_hash) {
            return Ok(false);
        }
        let dir = path.parent().unwrap();
        if !dir.is_dir() {
            std::fs::create_dir_all(dir)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut s = snap.clone();
        if s.saved_at_ms == 0 {
            s.saved_at_ms = now_ms();
        }
        let written = write_framed(&path, MAGIC, VERSION, &s.encode(), self.opt)?;
        self.last[tier as usize] = Some(body_hash);
        Ok(written)
    }

    /// 段の控えを読む。
    pub fn read(&self, tier: Tier) -> Result<Option<Snapshot>, String> {
        let Some(path) = self.path(tier) else { return Ok(None) };
        let decode = |p: &[u8]| Snapshot::decode(p).ok_or_else(|| format!("{}: 中身が壊れている", path.display()));
        match read_framed(&path, MAGIC, VERSION) {
            Loaded::Ok { payload, .. } => decode(&payload).map(Some),
            Loaded::FromBackup { payload, reason, .. } => match decode(&payload) {
                Ok(s) => Ok(Some(s)),
                Err(e) => Err(format!("{reason}; {e}")),
            },
            Loaded::Missing => Ok(None),
            Loaded::Corrupt(e) => Err(e),
        }
    }

    /// 起動時: 2 段のうち新しい控えを返す（ディスクの内容と同じ未保存の内容は除く）。
    /// 復元するものが何も無ければ `None`。
    pub fn restore(&self) -> Option<Restore> {
        let mut notes = Vec::new();
        let mut best: Option<(Tier, Snapshot)> = None;
        for t in Tier::ALL {
            match self.read(t) {
                Ok(Some(s)) => {
                    if best.as_ref().is_none_or(|(_, b)| s.saved_at_ms > b.saved_at_ms) {
                        best = Some((t, s));
                    }
                }
                Ok(None) => {}
                Err(e) => notes.push(format!("控えが読めない: {e}")),
            }
        }
        let (tier, mut snapshot) = match best {
            Some(b) => b,
            None if notes.is_empty() => return None,
            None => return Some(Restore { tier: Tier::Ram, snapshot: Snapshot::default(), notes }),
        };
        for d in &mut snapshot.docs {
            if let (Some(p), Some(u)) = (&d.path, &d.unsaved) {
                if std::fs::read(p).ok().as_deref() == Some(&u[..]) {
                    d.unsaved = None;
                }
            }
        }
        Some(Restore { tier, snapshot, notes })
    }

    /// 控えを全部消す（復元を断った・復元して保存し終えたとき）。
    pub fn discard(&mut self) -> io::Result<()> {
        for t in Tier::ALL {
            if let Some(p) = self.path(t) {
                remove_framed(&p)?;
            }
        }
        self.last = [None; 2];
        Ok(())
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(1).max(1)
}

/// いつ控えを書くか（design.md §8.4）。イベントループが持ち、時刻（µs、`zc_core::time`）を渡す。
///
/// ```
/// use zc_store::{BackupPolicy, Tier};
/// let mut p = BackupPolicy::default();
/// p.note_edit(0);
/// assert!(p.due(1_000_000).is_empty());
/// assert_eq!(p.next_deadline(), Some(2_000_000));
/// assert_eq!(p.due(2_000_000), vec![Tier::Ram]);
/// p.note_written(Tier::Ram);
/// assert_eq!(p.due(60_000_000), vec![Tier::Disk]);
/// ```
#[derive(Clone, Debug)]
pub struct BackupPolicy {
    /// 段 1 を書くまでの、操作の止まっている時間（µs）。
    pub ram_idle_us: u64,
    /// 段 2 を書くまでの、操作の止まっている時間（µs）。
    pub disk_idle_us: u64,
    last_edit_us: u64,
    dirty: [bool; 2],
    disk_now: bool,
}

impl Default for BackupPolicy {
    fn default() -> Self {
        BackupPolicy { ram_idle_us: 2_000_000, disk_idle_us: 60_000_000, last_edit_us: 0, dirty: [false; 2], disk_now: false }
    }
}

impl BackupPolicy {
    /// 内容かセッション（カーソル・タブ）が変わった。
    pub fn note_edit(&mut self, now_us: u64) {
        self.last_edit_us = now_us;
        self.dirty = [true; 2];
    }
    /// タブを切り替えた（段 2 をすぐ書く。変化があれば）。
    pub fn note_tab_switch(&mut self) {
        self.disk_now = true;
    }
    /// 電池が少ない（段 2 をすぐ書く。変化があれば）。
    pub fn note_low_battery(&mut self) {
        self.disk_now = true;
    }
    /// 書いた（依頼を出した）。
    pub fn note_written(&mut self, tier: Tier) {
        self.dirty[tier as usize] = false;
        if tier == Tier::Disk {
            self.disk_now = false;
        }
    }
    /// 保存などで、控えに残すものが無くなった（未保存の文書が無い）。
    pub fn note_clean(&mut self) {
        self.dirty = [false; 2];
        self.disk_now = false;
    }
    /// 今書くべき段。
    pub fn due(&self, now_us: u64) -> Vec<Tier> {
        let idle = now_us.saturating_sub(self.last_edit_us);
        let mut v = Vec::new();
        if self.dirty[0] && idle >= self.ram_idle_us {
            v.push(Tier::Ram);
        }
        if self.dirty[1] && (self.disk_now || idle >= self.disk_idle_us) {
            v.push(Tier::Disk);
        }
        v
    }
    /// 次に [`BackupPolicy::due`] を見るべき時刻（timerfd に仕掛ける）。
    pub fn next_deadline(&self) -> Option<u64> {
        let mut t: Option<u64> = None;
        let mut take = |x: u64| t = Some(t.map_or(x, |y| y.min(x)));
        if self.dirty[0] {
            take(self.last_edit_us + self.ram_idle_us);
        }
        if self.dirty[1] {
            take(if self.disk_now { self.last_edit_us } else { self.last_edit_us + self.disk_idle_us });
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::bak_path;
    use crate::testutil::TempDir;

    fn snap(text: &str) -> Snapshot {
        Snapshot {
            saved_at_ms: 0,
            active: Some(1),
            docs: vec![
                DocBackup { path: Some("/x/a.py".into()), title: "a.py".into(), cursor: Pos::new(3, 4), scroll_line: 1, unsaved: None },
                DocBackup { path: None, title: "無題-1".into(), cursor: Pos::new(0, 2), scroll_line: 0, unsaved: Some(text.as_bytes().to_vec()) },
            ],
        }
    }

    fn store(d: &TempDir) -> BackupStore {
        BackupStore::new(BackupDirs { ram: Some(d.path().join("run/zcode")), disk: Some(d.path().join("cache/zcode")) }).without_fsync()
    }

    #[test]
    fn snapshot_roundtrip_and_corruption() {
        let s = Snapshot { saved_at_ms: 42, ..snap("hello") };
        let b = s.encode();
        assert_eq!(Snapshot::decode(&b), Some(s));
        for n in 0..b.len() {
            assert_eq!(Snapshot::decode(&b[..n]), None);
        }
        let mut c = b.clone();
        c.push(0);
        assert_eq!(Snapshot::decode(&c), None);
    }

    #[test]
    fn two_tiers_newest_wins_and_unchanged_is_skipped() {
        let d = TempDir::new("backup");
        let mut st = store(&d);
        assert_eq!(st.restore(), None);
        let s1 = Snapshot { saved_at_ms: 100, ..snap("v1") };
        assert!(st.write(Tier::Ram, &s1).unwrap());
        assert!(st.write(Tier::Disk, &s1).unwrap());
        // 中身が同じなら書かない（時刻だけ違っても）
        assert!(!st.write(Tier::Ram, &Snapshot { saved_at_ms: 999, ..s1.clone() }).unwrap());
        // 段 1 だけ新しい
        let s2 = Snapshot { saved_at_ms: 200, ..snap("v2") };
        assert!(st.write(Tier::Ram, &s2).unwrap());
        let r = st.restore().unwrap();
        assert_eq!(r.tier, Tier::Ram);
        assert_eq!(r.unsaved_docs().next().unwrap().unsaved.as_deref(), Some(&b"v2"[..]));
        // 段 1 が消えた（再起動で tmpfs が空）→ 段 2
        std::fs::remove_dir_all(d.path().join("run")).unwrap();
        let r = st.restore().unwrap();
        assert_eq!(r.tier, Tier::Disk);
        assert_eq!(r.snapshot.saved_at_ms, 100);
        st.discard().unwrap();
        assert_eq!(st.restore(), None);
        // 捨てた後は同じ中身でも書く
        assert!(st.write(Tier::Disk, &s1).unwrap());
    }

    #[test]
    fn saved_files_are_not_offered_and_corruption_is_reported() {
        let d = TempDir::new("backup2");
        let mut st = store(&d);
        let f = d.path().join("a.py");
        std::fs::write(&f, b"saved text").unwrap();
        let s = Snapshot {
            saved_at_ms: 5,
            active: None,
            docs: vec![DocBackup { path: Some(f.to_string_lossy().into()), unsaved: Some(b"saved text".to_vec()), ..Default::default() }],
        };
        st.write(Tier::Disk, &s).unwrap();
        let r = st.restore().unwrap();
        assert_eq!(r.unsaved_docs().count(), 0, "ディスクと同じ内容は復元の対象にしない");
        assert_eq!(r.snapshot.docs.len(), 1, "セッション（タブ）は残す");
        // 本体が壊れた → .bak も無い → 知らせる（panic しない）
        let p = st.path(Tier::Disk).unwrap();
        std::fs::write(&p, b"ZCBK broken").unwrap();
        let r = st.restore().unwrap();
        assert!(r.notes.iter().any(|n| n.contains("読めない")), "{:?}", r.notes);
        // 2 世代目を書くと .bak ができ、本体が切れても .bak から戻せる
        let mut st = store(&d);
        st.discard().unwrap();
        st.write(Tier::Disk, &Snapshot { saved_at_ms: 1, ..snap("g1") }).unwrap();
        st.write(Tier::Disk, &Snapshot { saved_at_ms: 2, ..snap("g2") }).unwrap();
        assert!(bak_path(&p).exists());
        let full = std::fs::read(&p).unwrap();
        std::fs::write(&p, &full[..full.len() / 2]).unwrap();
        let r = st.restore().unwrap();
        assert_eq!(r.snapshot.saved_at_ms, 1);
    }

    #[test]
    fn policy_timing() {
        let mut p = BackupPolicy::default();
        assert!(p.due(10_000_000).is_empty());
        assert_eq!(p.next_deadline(), None);
        p.note_edit(1_000_000);
        assert!(p.due(2_000_000).is_empty());
        assert_eq!(p.next_deadline(), Some(3_000_000));
        assert_eq!(p.due(3_000_000), vec![Tier::Ram]);
        p.note_written(Tier::Ram);
        assert_eq!(p.next_deadline(), Some(61_000_000));
        // 打ち続けている間は書かない
        p.note_edit(50_000_000);
        assert!(p.due(51_000_000).is_empty());
        // タブを切り替えたら段 2 をすぐ
        p.note_tab_switch();
        assert_eq!(p.due(50_500_000), vec![Tier::Disk]);
        assert_eq!(p.due(52_000_000), vec![Tier::Ram, Tier::Disk]);
        p.note_written(Tier::Ram);
        p.note_written(Tier::Disk);
        assert!(p.due(200_000_000).is_empty());
        // 電池が少ない
        p.note_edit(300_000_000);
        p.note_low_battery();
        assert_eq!(p.due(300_000_001), vec![Tier::Disk]);
        p.note_clean();
        assert_eq!(p.next_deadline(), None);
    }

    #[test]
    fn dirs_from_env_shape() {
        let d = BackupDirs::from_env();
        if let Some(r) = &d.ram {
            assert!(r.ends_with("zcode"));
        }
        if let Some(c) = &d.disk {
            assert!(c.ends_with("zcode"));
        }
    }
}
