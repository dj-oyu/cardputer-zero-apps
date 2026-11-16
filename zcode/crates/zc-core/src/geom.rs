//! 画面の大きさと矩形。

/// 液晶の幅（画素）。
pub const SCREEN_W: u16 = 320;
/// 液晶の高さ（画素）。
pub const SCREEN_H: u16 = 170;

/// 画素行の半開区間 `start..end`。液晶への転送は行単位（横は常に全幅）なので、
/// 変更の単位は行範囲で表す。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct RowRange {
    pub start: u16,
    pub end: u16,
}

impl RowRange {
    pub const fn new(start: u16, end: u16) -> Self {
        Self { start, end }
    }
    pub const fn is_empty(&self) -> bool {
        self.end <= self.start
    }
    pub const fn len(&self) -> u16 {
        self.end.saturating_sub(self.start)
    }
    /// 両方を含む最小の範囲。空の範囲は無視する。
    pub fn union(self, o: RowRange) -> RowRange {
        if self.is_empty() {
            return o;
        }
        if o.is_empty() {
            return self;
        }
        RowRange::new(self.start.min(o.start), self.end.max(o.end))
    }
    pub fn intersect(self, o: RowRange) -> RowRange {
        let s = self.start.max(o.start);
        let e = self.end.min(o.end);
        RowRange::new(s, e.max(s))
    }
    /// 重なるか、ちょうど隣り合っているか。
    pub fn touches(self, o: RowRange) -> bool {
        self.start <= o.end && o.start <= self.end
    }
    /// 2 つの範囲のあいだの行数（重なっていれば 0）。
    pub fn gap(self, o: RowRange) -> u16 {
        if self.end <= o.start {
            o.start - self.end
        } else if o.end <= self.start {
            self.start - o.end
        } else {
            0
        }
    }
    pub fn contains(self, y: u16) -> bool {
        self.start <= y && y < self.end
    }
}

/// 画面上の矩形。原点は左上。画面の外にはみ出してもよい（描画側で切り取る）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: i16,
    pub y: i16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    /// 画面全体。
    pub const SCREEN: Rect = Rect::new(0, 0, SCREEN_W, SCREEN_H);

    pub const fn new(x: i16, y: i16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }
    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    /// 右端の次の x。
    pub const fn right(&self) -> i32 {
        self.x as i32 + self.w as i32
    }
    /// 下端の次の y。
    pub const fn bottom(&self) -> i32 {
        self.y as i32 + self.h as i32
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32 && x < self.right() && y >= self.y as i32 && y < self.bottom()
    }
    /// 共通部分。重ならなければ空の矩形。
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = (self.x as i32).max(o.x as i32);
        let y0 = (self.y as i32).max(o.y as i32);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        if x1 <= x0 || y1 <= y0 {
            Rect::new(x0 as i16, y0 as i16, 0, 0)
        } else {
            Rect::new(x0 as i16, y0 as i16, (x1 - x0) as u16, (y1 - y0) as u16)
        }
    }
    /// この矩形が占める行範囲（負の y は 0 に切り上げる）。
    pub fn rows(&self) -> RowRange {
        let s = self.y.max(0) as u16;
        let e = self.bottom().max(0) as u16;
        RowRange::new(s, e.max(s))
    }
    /// 上から `h` 行と残りに分ける。
    pub fn split_top(&self, h: u16) -> (Rect, Rect) {
        let h = h.min(self.h);
        (
            Rect::new(self.x, self.y, self.w, h),
            Rect::new(self.x, self.y + h as i16, self.w, self.h - h),
        )
    }
    /// 残りと、下から `h` 行に分ける。
    pub fn split_bottom(&self, h: u16) -> (Rect, Rect) {
        let h = h.min(self.h);
        (
            Rect::new(self.x, self.y, self.w, self.h - h),
            Rect::new(self.x, self.y + (self.h - h) as i16, self.w, h),
        )
    }
    /// 左から `w` 列と残りに分ける。
    pub fn split_left(&self, w: u16) -> (Rect, Rect) {
        let w = w.min(self.w);
        (
            Rect::new(self.x, self.y, w, self.h),
            Rect::new(self.x + w as i16, self.y, self.w - w, self.h),
        )
    }
    /// 残りと、右から `w` 列に分ける。
    pub fn split_right(&self, w: u16) -> (Rect, Rect) {
        let w = w.min(self.w);
        (
            Rect::new(self.x, self.y, self.w - w, self.h),
            Rect::new(self.x + (self.w - w) as i16, self.y, w, self.h),
        )
    }
    /// 四辺を内側に詰める。
    pub fn inset(&self, dx: u16, dy: u16) -> Rect {
        let w = self.w.saturating_sub(dx * 2);
        let h = self.h.saturating_sub(dy * 2);
        Rect::new(self.x + dx as i16, self.y + dy as i16, w, h)
    }
    pub fn translate(&self, dx: i16, dy: i16) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_intersect_and_rows() {
        let a = Rect::new(-5, -3, 20, 10);
        let c = a.intersect(&Rect::SCREEN);
        assert_eq!(c, Rect::new(0, 0, 15, 7));
        assert_eq!(a.rows(), RowRange::new(0, 7));
        assert!(Rect::new(400, 0, 5, 5).intersect(&Rect::SCREEN).is_empty());
    }

    #[test]
    fn rect_split() {
        let (body, status) = Rect::SCREEN.split_bottom(11);
        assert_eq!(body, Rect::new(0, 0, 320, 159));
        assert_eq!(status, Rect::new(0, 159, 320, 11));
        let (tabs, rest) = body.split_top(13);
        assert_eq!(tabs.h, 13);
        assert_eq!(rest.y, 13);
        let (side, ed) = rest.split_left(84);
        assert_eq!(side.w, 84);
        assert_eq!(ed.x, 84);
        assert_eq!(ed.w, 236);
    }

    #[test]
    fn row_range_ops() {
        let a = RowRange::new(10, 20);
        let b = RowRange::new(30, 35);
        assert_eq!(a.gap(b), 10);
        assert_eq!(b.gap(a), 10);
        assert_eq!(a.union(b), RowRange::new(10, 35));
        assert!(a.touches(RowRange::new(20, 22)));
        assert!(!a.touches(RowRange::new(21, 22)));
        assert_eq!(a.intersect(b).len(), 0);
        assert_eq!(RowRange::default().union(a), a);
    }
}
