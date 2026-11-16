//! 版番号と取り消し（design.md §4.2「古い結果の扱い」）。
//!
//! 使い方:
//!
//! 1. 文書ごとに [`VersionCounter`] を 1 つ持つ。編集のたびに [`VersionCounter::bump`]
//! 2. ワーカーへの依頼に [`VersionCounter::token`] を付ける
//! 3. ワーカーは数百行ごとに [`CancelToken::check`] を呼び、古ければやめる
//! 4. 結果は [`Versioned`] に包んで返す。メインは [`VersionCounter::is_current`] で
//!    確かめ、版が違う結果は捨てる
//!
//! ```
//! use zc_core::version::*;
//! let counter = VersionCounter::new();
//! let token = counter.token();          // 依頼に付ける
//! assert!(token.check().is_ok());
//! counter.bump();                       // その後に編集された
//! assert!(token.check().is_err());      // ワーカーはここでやめる
//! let result = Versioned::new(token.version(), "古い結果");
//! assert!(result.take_if_current(&counter).is_none()); // メインは捨てる
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 開いている文書（タブ）の識別子。結果がどの文書のものかを見分ける。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct DocId(pub u32);

/// 版番号。編集のたびに 1 増える。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Version(pub u64);

/// 現在の版を持つカウンタ。メインが増やし、ワーカーは [`CancelToken`] 越しに読む。
#[derive(Clone, Debug, Default)]
pub struct VersionCounter(Arc<AtomicU64>);

impl VersionCounter {
    pub fn new() -> VersionCounter {
        VersionCounter::default()
    }
    /// 現在の版。
    pub fn current(&self) -> Version {
        Version(self.0.load(Ordering::Acquire))
    }
    /// 版を 1 進めて、新しい版を返す。進める前に出した依頼はすべて古くなる。
    pub fn bump(&self) -> Version {
        Version(self.0.fetch_add(1, Ordering::AcqRel) + 1)
    }
    /// 現在の版に対する取り消しトークン。
    pub fn token(&self) -> CancelToken {
        CancelToken { shared: self.0.clone(), version: self.current() }
    }
    /// `v` が現在の版か。
    pub fn is_current(&self, v: Version) -> bool {
        self.current() == v
    }
}

/// 依頼が古くなったことを表すエラー。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

impl core::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("依頼が古くなったので取り消した")
    }
}

impl std::error::Error for Cancelled {}

/// ワーカーに渡す取り消しトークン。依頼を出した時点の版を覚えている。
#[derive(Clone, Debug)]
pub struct CancelToken {
    shared: Arc<AtomicU64>,
    version: Version,
}

impl CancelToken {
    /// 依頼を出した時点の版。結果に付けて返す。
    pub fn version(&self) -> Version {
        self.version
    }
    /// 依頼が古くなったか（その後に版が進んだか）。
    pub fn is_stale(&self) -> bool {
        self.shared.load(Ordering::Acquire) != self.version.0
    }
    /// 古ければ `Err(Cancelled)`。長い処理の中で `token.check()?;` と書く。
    pub fn check(&self) -> Result<(), Cancelled> {
        if self.is_stale() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
    /// 決して古くならないトークン（テストや、取り消しの要らない依頼用）。
    pub fn never() -> CancelToken {
        CancelToken { shared: Arc::new(AtomicU64::new(0)), version: Version(0) }
    }
}

/// 版番号つきの結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Versioned<T> {
    pub version: Version,
    pub value: T,
}

impl<T> Versioned<T> {
    pub fn new(version: Version, value: T) -> Versioned<T> {
        Versioned { version, value }
    }
    /// 版が現在のものなら中身を返す。古ければ捨てて `None`。
    pub fn take_if_current(self, counter: &VersionCounter) -> Option<T> {
        if counter.is_current(self.version) {
            Some(self.value)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_results_are_discarded() {
        let c = VersionCounter::new();
        let t1 = c.token();
        assert!(!t1.is_stale());
        let r1 = Versioned::new(t1.version(), 1);
        assert_eq!(c.bump(), Version(1));
        assert!(t1.is_stale());
        assert_eq!(t1.check(), Err(Cancelled));
        let t2 = c.token();
        let r2 = Versioned::new(t2.version(), 2);
        assert_eq!(r1.take_if_current(&c), None);
        assert_eq!(r2.take_if_current(&c), Some(2));
    }

    #[test]
    fn token_sees_bump_from_another_thread() {
        let c = VersionCounter::new();
        let t = c.token();
        let c2 = c.clone();
        std::thread::spawn(move || {
            c2.bump();
        })
        .join()
        .unwrap();
        assert!(t.is_stale());
        assert!(!CancelToken::never().is_stale());
    }
}
