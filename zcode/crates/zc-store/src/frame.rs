//! 独自形式のファイル（控え・セッション）の枠: マジック・版・長さ・CRC32（design.md §8.3）。
//!
//! ```text
//!  0  magic    [u8; 4]   ファイルの種類
//!  4  version  u16       中身の形の版
//!  6  flags    u16       0（予約）
//!  8  len      u32       中身のバイト数
//! 12  crc      u32       CRC32（先頭 12 バイト + 中身）
//! 16  中身
//! ```
//!
//! 途中で切れたファイル・壊れたファイルは読み込みで見分けて、1 世代前（`.bak`）に
//! 切り替える。読めなければ「無い」として既定値で続ける（呼ぶ側がステータスバーで知らせる）。

use crate::atomic::{write_atomic, WriteOptions};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const HEADER_LEN: usize = 16;
/// 中身の上限（壊れた長さで巨大な確保をしない）。
pub const MAX_PAYLOAD: usize = 64 << 20;

/// 枠が読めない理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// 先頭の 16 バイトに満たない。
    TooShort(usize),
    /// マジックが違う（別の種類のファイル）。
    BadMagic,
    /// 読めない版（新しい zcode が書いた）。
    UnsupportedVersion(u16),
    /// 長さの分だけ中身が無い（書き込みの途中で切れた）。
    Truncated { want: usize, got: usize },
    /// 長さより後ろに余計なバイトがある。
    Trailing(usize),
    /// CRC が合わない（中身が壊れた）。
    Crc,
    /// 長さがおかしい。
    TooLarge(usize),
}

impl core::fmt::Display for FrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FrameError::TooShort(n) => write!(f, "短すぎる（{n} バイト）"),
            FrameError::BadMagic => write!(f, "種類が違う"),
            FrameError::UnsupportedVersion(v) => write!(f, "読めない版（{v}）"),
            FrameError::Truncated { want, got } => write!(f, "途中で切れている（{got}/{want} バイト）"),
            FrameError::Trailing(n) => write!(f, "後ろに余計な {n} バイト"),
            FrameError::Crc => write!(f, "CRC が合わない"),
            FrameError::TooLarge(n) => write!(f, "長さがおかしい（{n}）"),
        }
    }
}

impl std::error::Error for FrameError {}

fn crc(head: &[u8], payload: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(head);
    h.update(payload);
    h.finalize()
}

/// 枠を付ける。
pub fn encode(magic: [u8; 4], version: u16, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(HEADER_LEN + payload.len());
    v.extend_from_slice(&magic);
    v.extend_from_slice(&version.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    let c = crc(&v[..12], payload);
    v.extend_from_slice(&c.to_le_bytes());
    v.extend_from_slice(payload);
    v
}

/// 枠を確かめて中身を返す（`max_version` より新しい版は読まない）。panic しない。
pub fn decode(bytes: &[u8], magic: [u8; 4], max_version: u16) -> Result<(u16, &[u8]), FrameError> {
    if bytes.len() < HEADER_LEN {
        return Err(FrameError::TooShort(bytes.len()));
    }
    if bytes[0..4] != magic {
        return Err(FrameError::BadMagic);
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    let len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(FrameError::TooLarge(len));
    }
    let got = bytes.len() - HEADER_LEN;
    if got < len {
        return Err(FrameError::Truncated { want: len, got });
    }
    if got > len {
        return Err(FrameError::Trailing(got - len));
    }
    let want = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    let payload = &bytes[HEADER_LEN..];
    if crc(&bytes[..12], payload) != want {
        return Err(FrameError::Crc);
    }
    // 版は CRC の後に見る（壊れたファイルを「新しい版」と取り違えない）
    if version > max_version {
        return Err(FrameError::UnsupportedVersion(version));
    }
    Ok((version, payload))
}

/// 1 世代前のファイルの名前（`<名前>.bak`）。
pub fn bak_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// 読み込みの結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Loaded {
    /// 読めた。
    Ok { version: u16, payload: Vec<u8> },
    /// 本体が読めず、1 世代前（`.bak`）を使った。`reason` は本体が読めなかった理由。
    FromBackup { version: u16, payload: Vec<u8>, reason: String },
    /// どちらも無い。
    Missing,
    /// どちらも読めない（理由）。既定値で続ける。
    Corrupt(String),
}

impl Loaded {
    /// 中身（読めたときだけ）。
    pub fn payload(&self) -> Option<(u16, &[u8])> {
        match self {
            Loaded::Ok { version, payload } | Loaded::FromBackup { version, payload, .. } => Some((*version, payload)),
            _ => None,
        }
    }
}

fn try_read(path: &Path, magic: [u8; 4], max_version: u16) -> Result<Option<(u16, Vec<u8>)>, String> {
    match fs::read(path) {
        Ok(b) => match decode(&b, magic, max_version) {
            Ok((v, p)) => Ok(Some((v, p.to_vec()))),
            Err(e) => Err(format!("{}: {e}", path.display())),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// 枠つきのファイルを読む。本体が無いか壊れていれば `.bak` を読む。panic しない。
pub fn read_framed(path: &Path, magic: [u8; 4], max_version: u16) -> Loaded {
    let primary = try_read(path, magic, max_version);
    if let Ok(Some((version, payload))) = primary {
        return Loaded::Ok { version, payload };
    }
    let reason = match &primary {
        Err(e) => e.clone(),
        _ => format!("{}: 無い", path.display()),
    };
    match try_read(&bak_path(path), magic, max_version) {
        Ok(Some((version, payload))) => Loaded::FromBackup { version, payload, reason },
        Ok(None) if primary.as_ref().is_ok_and(|p| p.is_none()) => Loaded::Missing,
        Ok(None) => Loaded::Corrupt(reason),
        Err(e) => Loaded::Corrupt(format!("{reason}; {e}")),
    }
}

/// 枠つきのファイルを書く。中身が今の本体と同じなら書かない（`Ok(false)`）。
///
/// 手順: 一時ファイルに書いて fsync → 本体が正しく読めるなら `.bak` に名前を変える
/// （壊れた本体で良い `.bak` を上書きしない）→ 一時ファイルを本体に → フォルダを fsync。
/// 2 つの rename の間で電源が落ちると本体が無くなるが、読むときに `.bak` を使う。
pub fn write_framed(path: &Path, magic: [u8; 4], version: u16, payload: &[u8], opt: WriteOptions) -> io::Result<bool> {
    let bytes = encode(magic, version, payload);
    let current = fs::read(path).ok();
    let current_ok = current.as_deref().is_some_and(|c| decode(c, magic, u16::MAX).is_ok());
    if current.as_deref() == Some(&bytes[..]) {
        return Ok(false);
    }
    if current_ok {
        // 一時ファイル → fsync を先に済ませてから、本体を .bak に回す
        let tmp = crate::atomic::tmp_path(path);
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(opt.new_mode).open(&tmp)?;
            f.write_all(&bytes)?;
            if !opt.skip_fsync {
                f.sync_all()?;
            }
        }
        let r = fs::rename(path, bak_path(path)).and_then(|_| fs::rename(&tmp, path));
        if r.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        r?;
        if !opt.skip_fsync {
            crate::atomic::fsync_dir(path.parent().unwrap_or(Path::new(".")))?;
        }
        Ok(true)
    } else {
        write_atomic(path, &bytes, opt).map(|_| true)
    }
}

/// 本体と `.bak` を消す（控えを捨てるとき）。無ければ何もしない。
pub fn remove_framed(path: &Path) -> io::Result<()> {
    for p in [path.to_path_buf(), bak_path(path), crate::atomic::tmp_path(path)] {
        match fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const M: [u8; 4] = *b"ZTST";
    const OPT: WriteOptions = WriteOptions { new_mode: 0o600, skip_fsync: false };

    #[test]
    fn roundtrip_and_every_truncation_and_bitflip_is_detected() {
        let payload: Vec<u8> = (0..200u32).map(|i| (i * 7 + 3) as u8).collect();
        let b = encode(M, 3, &payload);
        assert_eq!(decode(&b, M, 3).unwrap(), (3, &payload[..]));
        assert_eq!(decode(&b, M, 2), Err(FrameError::UnsupportedVersion(3)));
        assert_eq!(decode(&b, *b"XXXX", 3), Err(FrameError::BadMagic));
        // 途中で切れたもの（書き込み中の電源断）
        for n in 0..b.len() {
            assert!(decode(&b[..n], M, 3).is_err(), "長さ {n} で読めてしまった");
        }
        // 1 ビットでも壊れたもの
        for i in 0..b.len() {
            for bit in 0..8 {
                let mut c = b.clone();
                c[i] ^= 1 << bit;
                assert!(decode(&c, M, 3).is_err(), "{i} バイト目の {bit} ビットを壊しても読めた");
            }
        }
        // 後ろにごみ
        let mut c = b.clone();
        c.push(0);
        assert_eq!(decode(&c, M, 3), Err(FrameError::Trailing(1)));
        // 長さが巨大
        let mut c = b.clone();
        c[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(decode(&c, M, 3), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn bak_generation_and_tolerant_load() {
        let d = TempDir::new("frame");
        let p = d.path().join("session.zcs");
        assert_eq!(read_framed(&p, M, 1), Loaded::Missing);
        assert!(write_framed(&p, M, 1, b"gen1", OPT).unwrap());
        assert!(!bak_path(&p).exists());
        // 同じ中身なら書かない
        assert!(!write_framed(&p, M, 1, b"gen1", OPT).unwrap());
        assert!(write_framed(&p, M, 1, b"gen2", OPT).unwrap());
        assert_eq!(read_framed(&p, M, 1), Loaded::Ok { version: 1, payload: b"gen2".to_vec() });
        assert_eq!(read_framed(&bak_path(&p), M, 1).payload().unwrap().1, b"gen1");

        // 本体が途中で切れた → .bak（1 世代前）を使う
        let full = fs::read(&p).unwrap();
        fs::write(&p, &full[..full.len() - 2]).unwrap();
        match read_framed(&p, M, 1) {
            Loaded::FromBackup { payload, reason, .. } => {
                assert_eq!(payload, b"gen1");
                assert!(reason.contains("切れて"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        // 壊れた本体の上に書いても、良い .bak は残す
        assert!(write_framed(&p, M, 1, b"gen3", OPT).unwrap());
        assert_eq!(read_framed(&p, M, 1).payload().unwrap().1, b"gen3");
        assert_eq!(read_framed(&bak_path(&p), M, 1).payload().unwrap().1, b"gen1");

        // rename の間で電源が落ちた（本体が無く、.bak と一時ファイルがある）
        fs::rename(&p, bak_path(&p)).unwrap();
        fs::write(crate::atomic::tmp_path(&p), encode(M, 1, b"half")).unwrap();
        assert!(matches!(read_framed(&p, M, 1), Loaded::FromBackup { ref payload, .. } if payload == b"gen3"));
        // 次の書き込みで元に戻る
        assert!(write_framed(&p, M, 1, b"gen4", OPT).unwrap());
        assert_eq!(read_framed(&p, M, 1).payload().unwrap().1, b"gen4");

        // 両方壊れている → Corrupt（panic しない）
        fs::write(&p, b"garbage").unwrap();
        fs::write(bak_path(&p), b"ZTST\x01\x00").unwrap();
        assert!(matches!(read_framed(&p, M, 1), Loaded::Corrupt(_)));
        // 新しい版で書かれたもの
        fs::write(&p, encode(M, 9, b"future")).unwrap();
        assert!(matches!(read_framed(&p, M, 1), Loaded::Corrupt(ref s) if s.contains("版")));

        remove_framed(&p).unwrap();
        assert_eq!(read_framed(&p, M, 1), Loaded::Missing);
        remove_framed(&p).unwrap();
    }
}
