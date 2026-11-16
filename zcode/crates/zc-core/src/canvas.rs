//! 描画先。fb0 も PNG も同じ `Canvas` を写すだけなので、実機が無くても
//! render-test で確認できる（design.md §3「共通の約束」）。

use crate::color::{BlendLut, Color};
use crate::geom::{Rect, RowRange, SCREEN_H, SCREEN_W};

/// RGB565 の画素の配列。既定は液晶と同じ 320×170。
///
/// 描画関数はすべて範囲外を切り取るので、呼ぶ側は座標のはみ出しを気にしなくてよい。
/// 「どの行を描き変えたか」は `Canvas` では追わない。部品が [`crate::Damage`] で返す。
#[derive(Clone, PartialEq, Eq)]
pub struct Canvas {
    w: u16,
    h: u16,
    px: Box<[u16]>,
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for Canvas {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Canvas({}x{})", self.w, self.h)
    }
}

impl Canvas {
    /// 液晶と同じ大きさ（320×170）の、黒で埋めた Canvas。
    pub fn new() -> Canvas {
        Canvas::with_size(SCREEN_W, SCREEN_H)
    }
    /// 任意の大きさ（テストや部品単体の描画用）。
    pub fn with_size(w: u16, h: u16) -> Canvas {
        Canvas { w, h, px: vec![0u16; w as usize * h as usize].into_boxed_slice() }
    }
    pub fn width(&self) -> u16 {
        self.w
    }
    pub fn height(&self) -> u16 {
        self.h
    }
    /// 全体の矩形。
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }
    /// 全画素（行優先、1 行 `width()` 個）。
    pub fn pixels(&self) -> &[u16] {
        &self.px
    }
    pub fn pixels_mut(&mut self) -> &mut [u16] {
        &mut self.px
    }
    /// 全画素をバイト列として見る（fb0 に書く並びそのまま）。
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: u16 のスライスを同じ長さのバイト列として読むだけ。
        unsafe { core::slice::from_raw_parts(self.px.as_ptr() as *const u8, self.px.len() * 2) }
    }
    /// 1 行ぶんの画素。
    pub fn row(&self, y: u16) -> &[u16] {
        let s = y as usize * self.w as usize;
        &self.px[s..s + self.w as usize]
    }
    pub fn row_mut(&mut self, y: u16) -> &mut [u16] {
        let s = y as usize * self.w as usize;
        &mut self.px[s..s + self.w as usize]
    }
    /// 行範囲ぶんの画素。
    pub fn rows(&self, r: RowRange) -> &[u16] {
        let w = self.w as usize;
        &self.px[r.start as usize * w..r.end as usize * w]
    }

    pub fn get(&self, x: i32, y: i32) -> Option<Color> {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return None;
        }
        Some(Color(self.px[y as usize * self.w as usize + x as usize]))
    }
    pub fn put(&mut self, x: i32, y: i32, c: Color) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        self.px[y as usize * self.w as usize + x as usize] = c.0;
    }

    /// 全体を 1 色で埋める。
    pub fn clear(&mut self, c: Color) {
        self.px.fill(c.0);
    }
    /// 矩形を 1 色で埋める。
    pub fn fill_rect(&mut self, r: Rect, c: Color) {
        let r = r.intersect(&self.bounds());
        if r.is_empty() {
            return;
        }
        let w = self.w as usize;
        for y in r.y as usize..r.bottom() as usize {
            let s = y * w + r.x as usize;
            self.px[s..s + r.w as usize].fill(c.0);
        }
    }
    /// 横線（太さ 1）。
    pub fn hline(&mut self, x: i32, y: i32, len: u16, c: Color) {
        if y < i16::MIN as i32 || y > i16::MAX as i32 {
            return;
        }
        let x0 = x.max(0);
        let x1 = (x + len as i32).min(self.w as i32);
        if x1 > x0 {
            self.fill_rect(Rect::new(x0 as i16, y as i16, (x1 - x0) as u16, 1), c);
        }
    }
    /// 縦線（太さ 1）。
    pub fn vline(&mut self, x: i32, y: i32, len: u16, c: Color) {
        if x < i16::MIN as i32 || x > i16::MAX as i32 {
            return;
        }
        let y0 = y.max(0);
        let y1 = (y + len as i32).min(self.h as i32);
        if y1 > y0 {
            self.fill_rect(Rect::new(x as i16, y0 as i16, 1, (y1 - y0) as u16), c);
        }
    }
    /// 矩形の枠線（太さ 1、内側に描く）。
    pub fn stroke_rect(&mut self, r: Rect, c: Color) {
        if r.is_empty() {
            return;
        }
        self.hline(r.x as i32, r.y as i32, r.w, c);
        self.hline(r.x as i32, r.bottom() - 1, r.w, c);
        self.vline(r.x as i32, r.y as i32, r.h, c);
        self.vline(r.right() - 1, r.y as i32, r.h, c);
    }

    /// 字形などの濃度マスク（8bit、`mw`×`mh`、行優先）を、色の表を引いて合成する。
    ///
    /// 左上を `(x, y)` に置き、`clip` と Canvas の両方で切り取る。濃度が 0 の画素は
    /// 触らない（背景は呼ぶ側が先に塗っておく）。濃度の上位 4bit だけを使う。
    pub fn blit_mask(&mut self, x: i32, y: i32, mw: u16, mh: u16, mask: &[u8], lut: &BlendLut, clip: Rect) {
        debug_assert!(mask.len() >= mw as usize * mh as usize);
        let clip = clip.intersect(&self.bounds());
        let x0 = x.max(clip.x as i32);
        let y0 = y.max(clip.y as i32);
        let x1 = (x + mw as i32).min(clip.right());
        let y1 = (y + mh as i32).min(clip.bottom());
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let w = self.w as usize;
        let n = (x1 - x0) as usize;
        for yy in y0..y1 {
            let ms = (yy - y) as usize * mw as usize + (x0 - x) as usize;
            let ds = yy as usize * w + x0 as usize;
            let src = &mask[ms..ms + n];
            let dst = &mut self.px[ds..ds + n];
            for (d, &m) in dst.iter_mut().zip(src) {
                let i = m >> 4;
                if i != 0 {
                    *d = lut.0[i as usize];
                }
            }
        }
    }

    /// `area` の中身を `dy` 行だけ縦にずらす（正で下へ）。空いた行は `fill` で埋める。
    /// スクロールに使う（design.md §5.3。全画面でも 43µs 程度）。
    pub fn scroll_rows(&mut self, area: Rect, dy: i32, fill: Color) {
        let a = area.intersect(&self.bounds());
        if a.is_empty() || dy == 0 {
            return;
        }
        let h = a.h as i32;
        if dy.abs() >= h {
            self.fill_rect(a, fill);
            return;
        }
        let w = self.w as usize;
        let (ax, aw, ay) = (a.x as usize, a.w as usize, a.y as usize);
        let full_width = ax == 0 && aw == w;
        let n = (h - dy.abs()) as usize;
        if full_width {
            let (src, dst) = if dy > 0 { (ay, ay + dy as usize) } else { (ay + (-dy) as usize, ay) };
            self.px.copy_within(src * w..(src + n) * w, dst * w);
        } else if dy > 0 {
            for i in (0..n).rev() {
                let s = (ay + i) * w + ax;
                self.px.copy_within(s..s + aw, (ay + i + dy as usize) * w + ax);
            }
        } else {
            let d = (-dy) as usize;
            for i in 0..n {
                let s = (ay + i + d) * w + ax;
                self.px.copy_within(s..s + aw, (ay + i) * w + ax);
            }
        }
        let gap = if dy > 0 {
            Rect::new(a.x, a.y, a.w, dy as u16)
        } else {
            Rect::new(a.x, a.y + n as i16, a.w, (-dy) as u16)
        };
        self.fill_rect(gap, fill);
    }

    /// 同じ大きさの Canvas から丸ごと写す。
    pub fn copy_from(&mut self, o: &Canvas) {
        assert_eq!((self.w, self.h), (o.w, o.h));
        self.px.copy_from_slice(&o.px);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_rect_clips() {
        let mut c = Canvas::with_size(8, 4);
        c.fill_rect(Rect::new(-2, -1, 5, 3), Color(7));
        assert_eq!(c.row(0), &[7, 7, 7, 0, 0, 0, 0, 0]);
        assert_eq!(c.row(1), &[7, 7, 7, 0, 0, 0, 0, 0]);
        assert_eq!(c.row(2), &[0; 8]);
        c.fill_rect(Rect::new(100, 100, 5, 5), Color(9));
        c.put(-1, 0, Color(1));
        c.put(7, 3, Color(1));
        assert_eq!(c.get(7, 3), Some(Color(1)));
        assert_eq!(c.get(8, 3), None);
    }

    #[test]
    fn blit_mask_uses_lut_and_clip() {
        let mut c = Canvas::with_size(4, 2);
        let lut = BlendLut::new(Color::WHITE, Color::BLACK);
        let mask = [0u8, 255, 128, 255, 255, 0, 0, 255];
        c.blit_mask(1, 0, 4, 2, &mask, &lut, Rect::new(0, 0, 4, 2));
        assert_eq!(c.row(0), &[0, 0, 0xFFFF, lut.0[8]]);
        assert_eq!(c.row(1), &[0, 0xFFFF, 0, 0]);
        // clip で切り取る
        let mut c = Canvas::with_size(4, 2);
        c.blit_mask(0, 0, 4, 2, &[255; 8], &lut, Rect::new(1, 1, 2, 1));
        assert_eq!(c.row(0), &[0; 4]);
        assert_eq!(c.row(1), &[0, 0xFFFF, 0xFFFF, 0]);
    }

    #[test]
    fn scroll_rows_both_directions() {
        let mut c = Canvas::with_size(2, 5);
        for y in 0..5 {
            c.row_mut(y).fill(y + 1);
        }
        let mut up = c.clone();
        up.scroll_rows(Rect::new(0, 0, 2, 5), -2, Color(99));
        assert_eq!(up.pixels().iter().step_by(2).copied().collect::<Vec<_>>(), [3, 4, 5, 99, 99]);
        let mut down = c.clone();
        down.scroll_rows(Rect::new(0, 1, 2, 3), 1, Color(99));
        assert_eq!(down.pixels().iter().step_by(2).copied().collect::<Vec<_>>(), [1, 99, 2, 3, 5]);
        // 一部の列だけ
        let mut part = c.clone();
        part.scroll_rows(Rect::new(1, 0, 1, 5), 1, Color(0));
        assert_eq!(part.row(1), &[2, 1]);
        assert_eq!(part.row(0), &[1, 0]);
    }
}
