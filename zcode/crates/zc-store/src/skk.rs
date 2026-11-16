//! SKK 個人辞書（uim-skk と共有する `~/.skk-uim-jisyo`）の書き戻し（design.md §8.3・§9）。
//! 段階 4（zc-skk）で使う。ここは storage スレッドで動く書き込みだけを持つ。
//!
//! - 形式は uim-skk / SKK-JISYO と同じテキスト（EUC-JP）。`;; okuri-ari entries.` と
//!   `;; okuri-nasi entries.` の 2 つの区画に `よみ /候補1/候補2;注釈/.../` が並ぶ
//! - 書くときは **ロックファイルを取ってから、その時点のファイルを読み直し**、変更を
//!   重ねて丸ごと書き直す（uim-skk が学習した分を消さない）。書き方は一時ファイル →
//!   fsync → rename → フォルダを fsync
//! - ロックは uim と同じ `fcntl` の書き込みロック（ファイル全体）。既定では
//!   `<辞書>.lock`（uim の `open_lock()` が作る名前）と `~/.skk-jisyo.lock`（design.md §8.3）の
//!   両方を取る。取れなければ待って（既定 2 秒）、だめなら諦めて後で書き直す
//! - 学習した語は区画の先頭へ（新しいものが上。uim-skk・ddskk と同じ並べ方）。
//!   知らない行（コメントなど）と、候補の `[送り仮名/…/]` の塊はそのまま残す
//! - 中身が変わらなければ書かない

use crate::atomic::{write_atomic, WriteOptions};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const OKURI_ARI: &[u8] = b";; okuri-ari entries.";
const OKURI_NASI: &[u8] = b";; okuri-nasi entries.";

/// 辞書への変更 1 つ（文字列は UTF-8。書くときに EUC-JP にする）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DictChange {
    /// `reading` の候補 `candidate` を確定した（候補を先頭へ、項目を区画の先頭へ。無ければ足す）。
    /// 辞書登録もこれ。送りありの読みは `おくr` のように最後が英小文字。
    Learn { reading: String, candidate: String },
    /// 候補を消す（候補が無くなったら項目ごと消す）。
    Purge { reading: String, candidate: String },
}

/// 個人辞書の場所とロック。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkkUserDict {
    pub path: PathBuf,
    /// 書く前に取るロックファイル（並べた順に取る）。
    pub lock_paths: Vec<PathBuf>,
    /// ロックを待つ時間。
    pub lock_timeout: Duration,
}

/// 書き戻しの結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkkOutcome {
    /// 書いたか（中身が同じなら書かない）。
    pub written: bool,
    /// EUC-JP にできずに飛ばした変更。
    pub skipped: Vec<DictChange>,
    /// 書いた後の項目の数（送りあり, 送りなし）。
    pub entries: (usize, usize),
}

/// 書き戻しの失敗。
#[derive(Debug)]
pub enum SkkError {
    /// ロックが取れなかった（uim-skk が書いている）。後でもう一度。
    LockTimeout(PathBuf),
    Io(io::Error),
}

impl core::fmt::Display for SkkError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SkkError::LockTimeout(p) => write!(f, "{} のロックが取れない", p.display()),
            SkkError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SkkError {}

impl From<io::Error> for SkkError {
    fn from(e: io::Error) -> Self {
        SkkError::Io(e)
    }
}

impl SkkUserDict {
    /// uim-skk の既定: `<home>/.skk-uim-jisyo`、ロックは `<home>/.skk-uim-jisyo.lock` と
    /// `<home>/.skk-jisyo.lock`。
    pub fn uim_default(home: &Path) -> SkkUserDict {
        let path = home.join(".skk-uim-jisyo");
        SkkUserDict {
            lock_paths: vec![lock_path_for(&path), home.join(".skk-jisyo.lock")],
            path,
            lock_timeout: Duration::from_secs(2),
        }
    }

    /// 変更を重ねて書き戻す（storage スレッドで呼ぶ）。
    pub fn apply(&self, changes: &[DictChange]) -> Result<SkkOutcome, SkkError> {
        let _locks = self.lock()?;
        let old = match fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        let mut dict = UserDictFile::parse(&old);
        let mut skipped = Vec::new();
        for c in changes {
            if !dict.apply(c) {
                skipped.push(c.clone());
            }
        }
        let new = dict.serialize();
        let entries = dict.counts();
        if new == old {
            return Ok(SkkOutcome { written: false, skipped, entries });
        }
        write_atomic(&self.path, &new, WriteOptions::PRIVATE)?;
        Ok(SkkOutcome { written: true, skipped, entries })
    }

    /// ロックを全部取る（取れなければ `lock_timeout` まで待つ）。
    fn lock(&self) -> Result<Vec<File>, SkkError> {
        let deadline = Instant::now() + self.lock_timeout;
        let mut held = Vec::new();
        for p in &self.lock_paths {
            let f = OpenOptions::new().write(true).create(true).truncate(false).mode(0o600).open(p)?;
            loop {
                // SAFETY: flock 構造体を 0 で埋めてから設定する（全体の書き込みロック）。
                let mut fl: libc::flock = unsafe { std::mem::zeroed() };
                fl.l_type = libc::F_WRLCK as _;
                fl.l_whence = libc::SEEK_SET as _;
                // SAFETY: 有効な fd と flock を渡すだけ。
                let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETLK, &fl) };
                if r == 0 {
                    break;
                }
                let e = io::Error::last_os_error();
                if !matches!(e.raw_os_error(), Some(libc::EAGAIN) | Some(libc::EACCES) | Some(libc::EINTR)) {
                    return Err(e.into());
                }
                if Instant::now() >= deadline {
                    return Err(SkkError::LockTimeout(p.clone()));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            held.push(f);
        }
        // fd を閉じるとロックが外れる
        Ok(held)
    }
}

/// uim-skk がロックに使う名前（`<辞書>.lock`）。
pub fn lock_path_for(dict: &Path) -> PathBuf {
    let mut s = dict.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

/// 区画の中の 1 行。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    /// 項目（読み, 候補などの並び）。
    Entry { reading: Vec<u8>, items: Vec<Vec<u8>> },
    /// コメントや、読めない行（そのまま残す）。
    Raw(Vec<u8>),
}

/// 個人辞書を読んだもの（EUC-JP のバイト列のまま扱う。区切りの `/` ` ` `;` `[` `]` は
/// ASCII で、EUC-JP の 2 バイト文字には現れない）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct UserDictFile {
    /// 最初の区画の見出しより前の行。
    header: Vec<Vec<u8>>,
    ari: Vec<Line>,
    nasi: Vec<Line>,
    /// 見出しがあったか（無い・空のファイルは書くときに付ける）。
    had_markers: bool,
    /// 最後に改行があったか。
    trailing_newline: bool,
}

fn parse_entry(line: &[u8]) -> Option<Line> {
    let sp = line.iter().position(|&b| b == b' ')?;
    let reading = line[..sp].to_vec();
    let body = &line[sp + 1..];
    let body = body.strip_prefix(b"/")?;
    let body = body.strip_suffix(b"/").unwrap_or(body);
    let mut items: Vec<Vec<u8>> = Vec::new();
    let mut parts = body.split(|&b| b == b'/').peekable();
    while let Some(p) = parts.next() {
        if p.starts_with(b"[") {
            // [送り仮名/候補/…/] の塊は 1 つにまとめる
            let mut block = p.to_vec();
            if !p.ends_with(b"]") || p.len() == 1 {
                for q in parts.by_ref() {
                    block.push(b'/');
                    block.extend_from_slice(q);
                    if q == b"]" || q.ends_with(b"]") {
                        break;
                    }
                }
            }
            items.push(block);
        } else {
            items.push(p.to_vec());
        }
    }
    if reading.is_empty() {
        return None;
    }
    Some(Line::Entry { reading, items })
}

/// 候補の本体（`;注釈` を除く）。
fn cand_base(item: &[u8]) -> &[u8] {
    match item.iter().position(|&b| b == b';') {
        Some(i) => &item[..i],
        None => item,
    }
}

/// 送りありの読みか（`おくr` のように、非 ASCII の後ろが英小文字 1 つで終わる）。
pub fn is_okuri_ari(reading: &[u8]) -> bool {
    reading.len() >= 2 && reading[0] >= 0x80 && reading.last().is_some_and(|b| b.is_ascii_lowercase())
}

impl UserDictFile {
    /// 読む（どんなバイト列でも panic しない。読めない行はそのまま残す）。
    pub fn parse(b: &[u8]) -> UserDictFile {
        let mut f = UserDictFile { trailing_newline: b.is_empty() || b.ends_with(b"\n"), ..Default::default() };
        let text = b.strip_suffix(b"\n").unwrap_or(b);
        if b.is_empty() {
            return f;
        }
        // 0: 見出しの前、1: 送りあり、2: 送りなし
        let mut sec = 0;
        for line in text.split(|&c| c == b'\n') {
            if line == OKURI_ARI {
                sec = 1;
                f.had_markers = true;
                continue;
            }
            if line == OKURI_NASI {
                sec = 2;
                f.had_markers = true;
                continue;
            }
            let item = if line.starts_with(b";") || line.is_empty() {
                Line::Raw(line.to_vec())
            } else {
                parse_entry(line).unwrap_or_else(|| Line::Raw(line.to_vec()))
            };
            match sec {
                0 => match item {
                    // 見出しが無いファイルの項目は、読みから区画を決める
                    Line::Entry { ref reading, .. } => {
                        if is_okuri_ari(reading) {
                            f.ari.push(item)
                        } else {
                            f.nasi.push(item)
                        }
                    }
                    Line::Raw(r) => f.header.push(r),
                },
                1 => f.ari.push(item),
                _ => f.nasi.push(item),
            }
        }
        f
    }

    /// 書く形にする（EUC-JP のバイト列）。
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let line = |out: &mut Vec<u8>, l: &Line| {
            match l {
                Line::Raw(r) => out.extend_from_slice(r),
                Line::Entry { reading, items } => {
                    out.extend_from_slice(reading);
                    out.extend_from_slice(b" /");
                    for i in items {
                        out.extend_from_slice(i);
                        out.push(b'/');
                    }
                }
            }
            out.push(b'\n');
        };
        for h in &self.header {
            out.extend_from_slice(h);
            out.push(b'\n');
        }
        out.extend_from_slice(OKURI_ARI);
        out.push(b'\n');
        for l in &self.ari {
            line(&mut out, l);
        }
        out.extend_from_slice(OKURI_NASI);
        out.push(b'\n');
        for l in &self.nasi {
            line(&mut out, l);
        }
        if !self.trailing_newline && self.had_markers {
            out.pop();
        }
        out
    }

    fn counts(&self) -> (usize, usize) {
        let n = |v: &Vec<Line>| v.iter().filter(|l| matches!(l, Line::Entry { .. })).count();
        (n(&self.ari), n(&self.nasi))
    }

    /// 候補の一覧（UTF-8。注釈は除く。テスト・確認用）。
    pub fn candidates(&self, reading: &str) -> Option<Vec<String>> {
        let r = euc(reading)?;
        let sec = if is_okuri_ari(&r) { &self.ari } else { &self.nasi };
        sec.iter().find_map(|l| match l {
            Line::Entry { reading, items } if *reading == r => Some(
                items.iter().filter(|i| !i.starts_with(b"[")).map(|i| encoding_rs::EUC_JP.decode(cand_base(i)).0.into_owned()).collect(),
            ),
            _ => None,
        })
    }

    /// 変更を 1 つ重ねる。EUC-JP にできなければ `false`。
    pub fn apply(&mut self, c: &DictChange) -> bool {
        let (reading, cand, learn) = match c {
            DictChange::Learn { reading, candidate } => (reading, candidate, true),
            DictChange::Purge { reading, candidate } => (reading, candidate, false),
        };
        let (Some(r), Some(cd)) = (euc(reading), euc(cand)) else { return false };
        if r.is_empty() || cd.is_empty() || r.contains(&b' ') || cd.contains(&b'/') || cd.contains(&b'\n') {
            return false;
        }
        let sec = if is_okuri_ari(&r) { &mut self.ari } else { &mut self.nasi };
        let pos = sec.iter().position(|l| matches!(l, Line::Entry { reading, .. } if *reading == r));
        if learn {
            let mut entry = match pos {
                Some(i) => sec.remove(i),
                None => Line::Entry { reading: r.clone(), items: Vec::new() },
            };
            if let Line::Entry { items, .. } = &mut entry {
                let existing = items.iter().position(|i| !i.starts_with(b"[") && cand_base(i) == &cd[..]);
                let item = match existing {
                    Some(i) => items.remove(i),
                    None => cd.clone(),
                };
                items.insert(0, item);
            }
            // 区画の先頭（最初の項目の前。区画の頭のコメントは前に残す）
            let at = sec.iter().position(|l| matches!(l, Line::Entry { .. })).unwrap_or(sec.len());
            sec.insert(at, entry);
        } else if let Some(i) = pos {
            if let Line::Entry { items, .. } = &mut sec[i] {
                items.retain(|it| it.starts_with(b"[") || cand_base(it) != &cd[..]);
                if items.iter().all(|it| it.starts_with(b"[")) {
                    sec.remove(i);
                }
            }
        }
        self.had_markers = true;
        true
    }
}

/// UTF-8 → EUC-JP（表せない文字があれば `None`）。
fn euc(s: &str) -> Option<Vec<u8>> {
    let (b, _, bad) = encoding_rs::EUC_JP.encode(s);
    (!bad).then(|| b.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn e(s: &str) -> Vec<u8> {
        euc(s).unwrap()
    }

    fn sample() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend(e(";; -*- mode: fundamental; coding: euc-jp -*-\n"));
        v.extend(e(";; okuri-ari entries.\n"));
        v.extend(e("おくr /送/贈/\n"));
        v.extend(e("かえs /返/[す/返/]/帰/\n"));
        v.extend(e(";; okuri-nasi entries.\n"));
        v.extend(e("かんじ /漢字/感じ;feeling/幹事/\n"));
        v.extend(e("にほん /日本/\n"));
        v
    }

    #[test]
    fn parse_serialize_is_exact() {
        let s = sample();
        assert_eq!(UserDictFile::parse(&s).serialize(), s);
        // 見出しが無い・空
        let empty = UserDictFile::parse(b"");
        assert_eq!(empty.serialize(), b";; okuri-ari entries.\n;; okuri-nasi entries.\n");
        // ごみでも panic しない
        for junk in [&b"\xff\xfe\n/ /\n["[..], b"a", b"\n\n", b"x /[/", b";; okuri-nasi entries."] {
            let _ = UserDictFile::parse(junk).serialize();
        }
        let f = UserDictFile::parse(&s);
        assert_eq!(f.candidates("かえs").unwrap(), vec!["返", "帰"]);
        assert_eq!(f.counts(), (2, 2));
    }

    #[test]
    fn learn_purge_and_order() {
        let mut f = UserDictFile::parse(&sample());
        // 既にある候補 → 先頭へ、項目も区画の先頭へ（注釈は残す）
        assert!(f.apply(&DictChange::Learn { reading: "にほん".into(), candidate: "二本".into() }));
        assert!(f.apply(&DictChange::Learn { reading: "かんじ".into(), candidate: "感じ".into() }));
        let out = encoding_rs::EUC_JP.decode(&f.serialize()).0.into_owned();
        let nasi: Vec<&str> = out.split(";; okuri-nasi entries.\n").nth(1).unwrap().lines().collect();
        assert_eq!(nasi, vec!["かんじ /感じ;feeling/漢字/幹事/", "にほん /二本/日本/"]);
        // 送りあり（新しい読み）は送りありの区画へ
        assert!(f.apply(&DictChange::Learn { reading: "かくs".into(), candidate: "隠".into() }));
        assert_eq!(f.counts(), (3, 2));
        // [送り仮名] の塊は残る
        assert!(f.apply(&DictChange::Learn { reading: "かえs".into(), candidate: "帰".into() }));
        let out = encoding_rs::EUC_JP.decode(&f.serialize()).0.into_owned();
        assert!(out.contains("かえs /帰/返/[す/返/]/\n"), "{out}");
        // 消す
        assert!(f.apply(&DictChange::Purge { reading: "おくr".into(), candidate: "贈".into() }));
        assert_eq!(f.candidates("おくr").unwrap(), vec!["送"]);
        assert!(f.apply(&DictChange::Purge { reading: "おくr".into(), candidate: "送".into() }));
        assert_eq!(f.candidates("おくr"), None);
        // EUC-JP にできない文字・区切りを含む候補は飛ばす
        assert!(!f.apply(&DictChange::Learn { reading: "えもじ".into(), candidate: "😀".into() }));
        assert!(!f.apply(&DictChange::Learn { reading: "すらっしゅ".into(), candidate: "a/b".into() }));
    }

    #[test]
    fn apply_rereads_under_lock_and_skips_unchanged() {
        let d = TempDir::new("skk");
        let dict = SkkUserDict::uim_default(d.path());
        // 辞書が無い → 作る
        let o = dict.apply(&[DictChange::Learn { reading: "てすと".into(), candidate: "試験".into() }]).unwrap();
        assert!(o.written);
        // ほかのアプリ（uim-skk）が書き足した
        let mut b = fs::read(&dict.path).unwrap();
        b.extend(e("うぃむ /uim/\n"));
        fs::write(&dict.path, &b).unwrap();
        // 次の書き戻しでその分は消えない
        dict.apply(&[DictChange::Learn { reading: "てすと".into(), candidate: "テスト".into() }]).unwrap();
        let f = UserDictFile::parse(&fs::read(&dict.path).unwrap());
        assert_eq!(f.candidates("うぃむ").unwrap(), vec!["uim"]);
        assert_eq!(f.candidates("てすと").unwrap(), vec!["テスト", "試験"]);
        // 同じ変更をもう一度 → 中身が同じなので書かない
        let o = dict.apply(&[DictChange::Learn { reading: "てすと".into(), candidate: "テスト".into() }]).unwrap();
        assert!(!o.written);
        // 権限は本人だけ
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&dict.path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(lock_path_for(&dict.path).exists());
    }

    #[test]
    fn lock_held_by_someone_else_times_out() {
        let d = TempDir::new("skk-lock");
        let mut dict = SkkUserDict::uim_default(d.path());
        dict.lock_timeout = Duration::from_millis(100);
        // ほかのプロセスのロックの代わりに OFD ロックを持つ（同じプロセスでも
        // fcntl の F_SETLK とぶつかる）
        let lp = &dict.lock_paths[0];
        let f = OpenOptions::new().write(true).create(true).truncate(false).open(lp).unwrap();
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = libc::F_WRLCK as _;
        fl.l_whence = libc::SEEK_SET as _;
        assert_eq!(unsafe { libc::fcntl(f.as_raw_fd(), libc::F_OFD_SETLK, &fl) }, 0);
        let t = Instant::now();
        match dict.apply(&[DictChange::Learn { reading: "a".into(), candidate: "b".into() }]) {
            Err(SkkError::LockTimeout(p)) => assert_eq!(&p, lp),
            other => panic!("{other:?}"),
        }
        assert!(t.elapsed() >= Duration::from_millis(100));
        assert!(!dict.path.exists(), "ロックが取れないときは書かない");
        drop(f);
        assert!(dict.apply(&[DictChange::Learn { reading: "a".into(), candidate: "b".into() }]).unwrap().written);
    }
}
