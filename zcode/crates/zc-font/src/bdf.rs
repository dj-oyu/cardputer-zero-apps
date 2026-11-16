//! ビットマップフォント（BDF）の最小限の読み込み。
//!
//! 段階 1 の「TrueType とビットマップの見比べ」用の入口。`zcode --fonttest --bdf <パス>`
//! で渡したファイルだけを読む（実機にはビットマップフォントが入っていない）。
//!
//! - `ENCODING` を Unicode の符号位置として扱う（`CHARSET_REGISTRY` が ISO10646 の
//!   フォント向け）。東雲のような JIS X 0208 のフォントは、先に Unicode へ変換した
//!   BDF を用意する必要がある（未対応）
//! - 採用するなら、ライセンスの確認と、配布の方法（.deb に同梱するか）を先に決める

use crate::types::{Glyph, GlyphSource, Metrics};
use std::collections::HashMap;

/// 読み込んだ BDF フォント。
#[derive(Debug, Default)]
pub struct BdfFont {
    glyphs: HashMap<u32, Glyph>,
    ascent: i16,
    descent: i16,
    default_width: u16,
}

impl BdfFont {
    /// BDF のテキストを読む。字形が 1 つも無ければエラー。
    pub fn parse(src: &str) -> Result<BdfFont, String> {
        let mut font = BdfFont::default();
        let mut lines = src.lines();
        let (mut enc, mut dwidth, mut bbx) = (None::<u32>, 0u16, (0u16, 0u16, 0i16, 0i16));
        let nums = |s: &str| -> Vec<i32> { s.split_whitespace().filter_map(|t| t.parse().ok()).collect() };
        while let Some(line) = lines.next() {
            let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
            match word {
                "FONT_ASCENT" => font.ascent = rest.trim().parse().unwrap_or(0),
                "FONT_DESCENT" => font.descent = rest.trim().parse().unwrap_or(0),
                "FONTBOUNDINGBOX" => {
                    let v = nums(rest);
                    if v.len() == 4 {
                        font.default_width = v[0].max(0) as u16;
                        if font.ascent == 0 && font.descent == 0 {
                            font.descent = (-v[3]).max(0) as i16;
                            font.ascent = (v[1] + v[3]).max(0) as i16;
                        }
                    }
                }
                "STARTCHAR" => (enc, dwidth, bbx) = (None, 0, (0, 0, 0, 0)),
                "ENCODING" => enc = nums(rest).first().and_then(|&v| u32::try_from(v).ok()),
                "DWIDTH" => dwidth = nums(rest).first().map_or(0, |&v| v.max(0) as u16),
                "BBX" => {
                    let v = nums(rest);
                    if v.len() == 4 {
                        bbx = (v[0].max(0) as u16, v[1].max(0) as u16, v[2] as i16, v[3] as i16);
                    }
                }
                "BITMAP" => {
                    let (w, h, xoff, yoff) = bbx;
                    let mut data = vec![0u8; w as usize * h as usize];
                    for row in 0..h as usize {
                        let Some(hex) = lines.next() else { break };
                        let hex = hex.trim().as_bytes();
                        for x in 0..w as usize {
                            let nib = hex.get(x / 4).and_then(|&c| (c as char).to_digit(16)).unwrap_or(0);
                            if nib >> (3 - x % 4) & 1 == 1 {
                                data[row * w as usize + x] = 255;
                            }
                        }
                    }
                    if let Some(cp) = enc {
                        font.glyphs.insert(
                            cp,
                            Glyph {
                                width: w,
                                height: h,
                                left: xoff,
                                top: yoff + h as i16,
                                advance: if dwidth > 0 { dwidth } else { w },
                                data: data.into_boxed_slice(),
                                source: GlyphSource::Primary,
                            },
                        );
                    }
                }
                _ => {}
            }
        }
        if font.glyphs.is_empty() {
            return Err("BDF に字形が無い".into());
        }
        if font.default_width == 0 {
            font.default_width = font.glyphs.get(&0x30).map_or(6, |g| g.advance);
        }
        Ok(font)
    }

    pub fn glyph(&self, ch: char) -> Option<&Glyph> {
        self.glyphs.get(&(ch as u32))
    }
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }
    pub fn metrics(&self) -> Metrics {
        let cell_w = self.glyphs.get(&0x30).map_or(self.default_width, |g| g.advance);
        Metrics { ascent: self.ascent, descent: self.descent, cell_w }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "STARTFONT 2.1\nFONT -test-\nSIZE 8 75 75\nFONTBOUNDINGBOX 5 8 0 -2\n\
STARTPROPERTIES 2\nFONT_ASCENT 6\nFONT_DESCENT 2\nENDPROPERTIES\nCHARS 2\n\
STARTCHAR zero\nENCODING 48\nSWIDTH 500 0\nDWIDTH 5 0\nBBX 4 3 0 0\nBITMAP\nF0\n90\nF0\nENDCHAR\n\
STARTCHAR a_hira\nENCODING 12354\nDWIDTH 10 0\nBBX 9 2 1 -1\nBITMAP\nFF80\n8080\nENDCHAR\nENDFONT\n";

    #[test]
    fn parses_glyphs_and_metrics() {
        let f = BdfFont::parse(SAMPLE).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f.metrics(), Metrics { ascent: 6, descent: 2, cell_w: 5 });
        let z = f.glyph('0').unwrap();
        assert_eq!((z.width, z.height, z.left, z.top, z.advance), (4, 3, 0, 3, 5));
        assert_eq!(&z.data[..], &[255, 255, 255, 255, 255, 0, 0, 255, 255, 255, 255, 255]);
        let a = f.glyph('あ').unwrap();
        assert_eq!((a.width, a.advance, a.left, a.top), (9, 10, 1, 1));
        assert_eq!(&a.data[..9], &[255; 9]);
        assert_eq!(&a.data[9..], &[255, 0, 0, 0, 0, 0, 0, 0, 255]);
        assert!(f.glyph('x').is_none());
        assert!(BdfFont::parse("STARTFONT 2.1\nENDFONT\n").is_err());
    }
}
