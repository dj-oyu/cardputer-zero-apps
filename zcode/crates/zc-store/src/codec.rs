//! 独自形式の中身を書く・読む道具（リトルエンディアン、長さつきのバイト列）と、
//! 内容の比較に使う 64 ビットのハッシュ。
//!
//! 読む側は範囲を全部確かめ、壊れていれば `None` を返す（panic しない）。

/// 中身を組み立てる。
#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn new() -> Writer {
        Writer(Vec::new())
    }
    pub fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    pub fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
    pub fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    pub fn opt_bytes(&mut self, b: Option<&[u8]>) {
        match b {
            Some(b) => {
                self.u8(1);
                self.bytes(b);
            }
            None => self.u8(0),
        }
    }
    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }
}

/// 中身を読む。
pub struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b, at: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    pub fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    pub fn str(&mut self) -> Option<&'a str> {
        std::str::from_utf8(self.bytes()?).ok()
    }
    pub fn opt_bytes(&mut self) -> Option<Option<&'a [u8]>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.bytes()?)),
            _ => None,
        }
    }
    /// 全部読み終えたか。
    pub fn is_done(&self) -> bool {
        self.at == self.b.len()
    }
}

/// 内容の比較に使う 64 ビットのハッシュ（「内容が変わっていなければ書かない」用）。
/// 実行をまたいで同じ値になる（控えのファイルに書いても比べられる）。暗号用ではない。
pub fn hash64(data: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h: u64 = 0x243F_6A88_85A3_08D3 ^ (data.len() as u64).wrapping_mul(K);
    let (chunks, r) = data.as_chunks::<8>();
    for c in chunks {
        let v = u64::from_le_bytes(*c);
        h = (h ^ v.wrapping_mul(K)).rotate_left(29).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    }
    let mut tail = [0u8; 8];
    tail[..r.len()].copy_from_slice(r);
    h = (h ^ u64::from_le_bytes(tail).wrapping_mul(K) ^ r.len() as u64).rotate_left(29).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    // 仕上げ（splitmix64）
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_reader_roundtrip_and_truncation() {
        let mut w = Writer::new();
        w.u8(7);
        w.u32(0xdead_beef);
        w.u64(1 << 40);
        w.str("日本語");
        w.opt_bytes(None);
        w.opt_bytes(Some(b"abc"));
        let b = w.into_inner();
        let mut r = Reader::new(&b);
        assert_eq!(r.u8(), Some(7));
        assert_eq!(r.u32(), Some(0xdead_beef));
        assert_eq!(r.u64(), Some(1 << 40));
        assert_eq!(r.str(), Some("日本語"));
        assert_eq!(r.opt_bytes(), Some(None));
        assert_eq!(r.opt_bytes(), Some(Some(&b"abc"[..])));
        assert!(r.is_done());
        // どこで切れても panic しない
        for n in 0..b.len() {
            let mut r = Reader::new(&b[..n]);
            let ok = (|| {
                r.u8()?;
                r.u32()?;
                r.u64()?;
                r.str()?;
                r.opt_bytes()?;
                r.opt_bytes()
            })();
            assert!(ok.is_none());
        }
    }

    const PINNED_ZCODE: u64 = 0xf1aa_7f1d_7023_3b20;

    #[test]
    fn hash_differs_and_is_stable() {
        assert_ne!(hash64(b""), hash64(b"\0"));
        assert_ne!(hash64(b"abc"), hash64(b"abd"));
        assert_ne!(hash64(b"12345678a"), hash64(b"12345678b"));
        assert_ne!(hash64(&[0u8; 8]), hash64(&[0u8; 9]));
        // 値が変わると控えのファイルとの比較が狂うので、固定値で確かめる
        assert_eq!(hash64(b"zcode"), PINNED_ZCODE, "hash64 の値が変わった: {:#x}", hash64(b"zcode"));
    }
}
