//! 書き込み先。flush スレッドだけが持ち、flush スレッドだけが呼ぶ。

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zc_core::RowRange;

/// 表示の書き込み先。
pub trait Backend: Send {
    /// 幅と高さ（画素）。
    fn size(&self) -> (u16, u16);
    /// いま表示されている内容を読む（提示用バッファの初期値にする）。
    fn read_all(&mut self, out: &mut [u16]);
    /// `y0` 行目から、`px`（幅の倍数ぶん）を書く。
    fn write_rows(&mut self, y0: u16, px: &[u16]);
    /// 書いた内容の転送を始めさせる（fb0 では `fsync`）。
    fn sync(&mut self) -> io::Result<()>;
}

/// [`MemBackend`] が受けた操作の記録。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemOp {
    Write(RowRange),
    Sync,
}

/// [`MemBackend`] の中身。テストから覗ける。
#[derive(Debug, Default)]
pub struct MemLog {
    pub px: Vec<u16>,
    pub ops: Vec<MemOp>,
}

impl MemLog {
    /// `Sync` で区切った、1 回ごとの書き込み範囲。
    pub fn frames(&self) -> Vec<Vec<RowRange>> {
        let mut out = vec![];
        let mut cur = vec![];
        for op in &self.ops {
            match op {
                MemOp::Write(r) => cur.push(*r),
                MemOp::Sync => out.push(std::mem::take(&mut cur)),
            }
        }
        out
    }
}

/// メモリ上の書き込み先（テスト用）。操作を記録する。
pub struct MemBackend {
    w: u16,
    h: u16,
    log: Arc<Mutex<MemLog>>,
    sync_delay: Duration,
}

impl MemBackend {
    /// `w`×`h` の書き込み先と、その中身を覗く口。
    pub fn new(w: u16, h: u16) -> (MemBackend, Arc<Mutex<MemLog>>) {
        let log = Arc::new(Mutex::new(MemLog { px: vec![0; w as usize * h as usize], ops: vec![] }));
        (MemBackend { w, h, log: log.clone(), sync_delay: Duration::ZERO }, log)
    }
    /// `sync` にかかる時間を足す（flush が忙しい間の併合を試すため）。
    pub fn with_sync_delay(mut self, d: Duration) -> MemBackend {
        self.sync_delay = d;
        self
    }
}

impl Backend for MemBackend {
    fn size(&self) -> (u16, u16) {
        (self.w, self.h)
    }
    fn read_all(&mut self, out: &mut [u16]) {
        out.copy_from_slice(&self.log.lock().unwrap().px);
    }
    fn write_rows(&mut self, y0: u16, px: &[u16]) {
        let mut log = self.log.lock().unwrap();
        let s = y0 as usize * self.w as usize;
        log.px[s..s + px.len()].copy_from_slice(px);
        let rows = (px.len() / self.w as usize) as u16;
        log.ops.push(MemOp::Write(RowRange::new(y0, y0 + rows)));
    }
    fn sync(&mut self) -> io::Result<()> {
        if !self.sync_delay.is_zero() {
            std::thread::sleep(self.sync_delay);
        }
        self.log.lock().unwrap().ops.push(MemOp::Sync);
        Ok(())
    }
}
