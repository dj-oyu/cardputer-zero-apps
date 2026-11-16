//! 色（RGB565）と、文字の合成に使う 16 段階の色の表。

/// RGB565 の色。fb0 にそのまま書ける値（リトルエンディアンの u16）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct Color(pub u16);

impl Color {
    pub const BLACK: Color = Color(0);
    pub const WHITE: Color = Color(0xFFFF);

    /// 8bit の RGB から作る。
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color(((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3))
    }
    /// `0xRRGGBB` から作る（CSS の `#rrggbb` と同じ並び）。
    pub const fn hex(v: u32) -> Color {
        Color::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
    /// 8bit の RGB に戻す（下位ビットは上位ビットの繰り返しで埋める）。
    pub const fn to_rgb8(self) -> [u8; 3] {
        let r = (self.0 >> 11) as u8 & 0x1f;
        let g = (self.0 >> 5) as u8 & 0x3f;
        let b = self.0 as u8 & 0x1f;
        [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
    }
    /// `self` から `to` へ `t / 255` だけ寄せた色。
    pub const fn mix(self, to: Color, t: u8) -> Color {
        const fn ch(a: u32, b: u32, t: u32, shift: u32, mask: u32) -> u32 {
            let x = (a >> shift) & mask;
            let y = (b >> shift) & mask;
            (x * (255 - t) + y * t + 127) / 255
        }
        let (a, b, t) = (self.0 as u32, to.0 as u32, t as u32);
        Color(((ch(a, b, t, 11, 0x1f) << 11) | (ch(a, b, t, 5, 0x3f) << 5) | ch(a, b, t, 0, 0x1f)) as u16)
    }
}

/// 背景色と文字色の組に対する 16 段階の色の表（design.md §5.4）。
///
/// 字形の濃度（8bit）を 4bit に丸めてこの表を引くだけで合成できる。
/// `lut.0[0]` は背景色、`lut.0[15]` は文字色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlendLut(pub [u16; 16]);

impl BlendLut {
    pub const fn new(fg: Color, bg: Color) -> BlendLut {
        let mut t = [0u16; 16];
        let mut i = 0;
        while i < 16 {
            t[i] = bg.mix(fg, (i * 17) as u8).0;
            i += 1;
        }
        BlendLut(t)
    }
    /// 濃度 0〜255 に対する色。
    #[inline]
    pub const fn at(&self, coverage: u8) -> u16 {
        self.0[(coverage >> 4) as usize]
    }
    pub const fn fg(&self) -> Color {
        Color(self.0[15])
    }
    pub const fn bg(&self) -> Color {
        Color(self.0[0])
    }
}

/// VS Code Dark+ の配色（`docs/ui-mock.html` の CSS 変数と同じ値）。
pub mod palette {
    use super::Color;

    pub const BG: Color = Color::hex(0x1e1e1e);
    pub const SIDE: Color = Color::hex(0x252526);
    pub const TABBAR: Color = Color::hex(0x2d2d2d);
    pub const TAB_ACTIVE: Color = Color::hex(0x1e1e1e);
    pub const FG: Color = Color::hex(0xd4d4d4);
    pub const DIM: Color = Color::hex(0x858585);
    /// カーソル行の背景。
    pub const LINE: Color = Color::hex(0x2a2a2a);
    pub const SELECTION: Color = Color::hex(0x264f78);
    pub const STATUS: Color = Color::hex(0x007acc);
    pub const STATUS_RUN: Color = Color::hex(0xcc6633);
    pub const ACCENT: Color = Color::hex(0x3dff7a);
    pub const GUTTER: Color = Color::hex(0x5a5a5a);
    pub const GUTTER_CUR: Color = Color::hex(0xc6c6c6);
    pub const CURSOR: Color = Color::hex(0xaeafad);
    pub const BORDER: Color = Color::hex(0x444444);
    pub const LIST_FOCUS: Color = Color::hex(0x04395e);
    pub const LIST_CUR: Color = Color::hex(0x37373d);
    pub const ERROR: Color = Color::hex(0xf48771);
    pub const WARN: Color = Color::hex(0xffd479);

    // トークンの色
    pub const KEYWORD: Color = Color::hex(0x569cd6);
    pub const CONTROL: Color = Color::hex(0xc586c0);
    pub const STRING: Color = Color::hex(0xce9178);
    pub const NUMBER: Color = Color::hex(0xb5cea8);
    pub const FUNCTION: Color = Color::hex(0xdcdcaa);
    pub const COMMENT: Color = Color::hex(0x6a9955);
    pub const CLASS: Color = Color::hex(0x4ec9b0);
    pub const VARIABLE: Color = Color::hex(0x9cdcfe);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb565_roundtrip() {
        assert_eq!(Color::rgb(255, 255, 255), Color::WHITE);
        assert_eq!(Color::hex(0xff0000).0, 0xF800);
        assert_eq!(Color::hex(0x00ff00).0, 0x07E0);
        assert_eq!(Color::hex(0x0000ff).0, 0x001F);
        assert_eq!(Color::WHITE.to_rgb8(), [255, 255, 255]);
        assert_eq!(Color::hex(0x1e1e1e).to_rgb8(), [24, 28, 24]);
    }

    #[test]
    fn lut_endpoints_and_monotonic() {
        let lut = BlendLut::new(Color::WHITE, Color::BLACK);
        assert_eq!(lut.bg(), Color::BLACK);
        assert_eq!(lut.fg(), Color::WHITE);
        assert_eq!(lut.at(0), 0);
        assert_eq!(lut.at(255), 0xFFFF);
        let mut prev = 0;
        for i in 0..16 {
            let g = (lut.0[i] >> 5) & 0x3f;
            assert!(g >= prev);
            prev = g;
        }
    }
}
