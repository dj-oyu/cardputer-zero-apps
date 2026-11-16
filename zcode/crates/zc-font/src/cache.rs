//! グリフキャッシュ（design.md §5.4）。8bit の濃度で持つ LRU。上限は既定 512KB
//! （漢字 1 字 約 140 バイトなので 3,000 字以上入る）。メインスレッドだけが持つ。

use crate::types::{Glyph, GlyphKey};
use std::collections::HashMap;

/// キャッシュの上限の既定値（バイト）。
pub const DEFAULT_CACHE_BYTES: usize = 512 * 1024;

struct Entry {
    glyph: Glyph,
    used: u64,
}

/// LRU のグリフキャッシュ。
pub struct GlyphCache {
    map: HashMap<GlyphKey, Entry>,
    bytes: usize,
    limit: usize,
    tick: u64,
}

impl GlyphCache {
    pub fn new(limit_bytes: usize) -> GlyphCache {
        GlyphCache { map: HashMap::new(), bytes: 0, limit: limit_bytes, tick: 0 }
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    /// 使っているバイト数の目安。
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn contains(&self, key: &GlyphKey) -> bool {
        self.map.contains_key(key)
    }
    /// 字形を取り出す（使った印を付ける）。
    pub fn get(&mut self, key: &GlyphKey) -> Option<&Glyph> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(key).map(|e| {
            e.used = tick;
            &e.glyph
        })
    }
    /// 字形を入れる。上限を超えたら、長く使っていないものから捨てる
    /// （上限の 3/4 まで減らす。毎回 1 つずつ捨てると走査が増えるため）。
    pub fn insert(&mut self, key: GlyphKey, glyph: Glyph) {
        self.tick += 1;
        self.bytes += glyph.cost();
        if let Some(old) = self.map.insert(key, Entry { glyph, used: self.tick }) {
            self.bytes -= old.glyph.cost();
        }
        if self.bytes > self.limit {
            let mut order: Vec<(u64, GlyphKey)> = self.map.iter().map(|(k, e)| (e.used, *k)).collect();
            order.sort_unstable_by_key(|&(used, _)| used);
            let target = self.limit / 4 * 3;
            for (_, k) in order {
                if self.bytes <= target || k == key {
                    break;
                }
                if let Some(e) = self.map.remove(&k) {
                    self.bytes -= e.glyph.cost();
                }
            }
        }
    }
    pub fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GlyphSource, TextStyle};

    fn key(ch: char) -> GlyphKey {
        GlyphKey { ch, style: TextStyle::mono(9.0) }
    }
    fn glyph(n: usize) -> Glyph {
        Glyph {
            width: n as u16,
            height: 1,
            left: 0,
            top: 0,
            advance: 5,
            data: vec![0; n].into_boxed_slice(),
            source: GlyphSource::Primary,
        }
    }

    #[test]
    fn evicts_least_recently_used() {
        // 1 字 = 36 + 64 = 100 バイト。上限 1000 → 11 字目で 750 以下まで捨てる
        let mut c = GlyphCache::new(1000);
        for i in 0..10u8 {
            c.insert(key((b'a' + i) as char), glyph(36));
        }
        assert_eq!((c.len(), c.bytes()), (10, 1000));
        assert!(c.get(&key('a')).is_some()); // a を使う → いちばん新しくなる
        c.insert(key('z'), glyph(36));
        assert!(c.bytes() <= 750);
        assert!(c.contains(&key('a')));
        assert!(c.contains(&key('z')));
        assert!(!c.contains(&key('b')));
        assert!(!c.contains(&key('c')));
        assert!(c.contains(&key('j')));
    }

    #[test]
    fn replace_keeps_byte_count() {
        let mut c = GlyphCache::new(10_000);
        c.insert(key('a'), glyph(10));
        c.insert(key('a'), glyph(30));
        assert_eq!((c.len(), c.bytes()), (1, 94));
        c.clear();
        assert_eq!(c.bytes(), 0);
    }
}
