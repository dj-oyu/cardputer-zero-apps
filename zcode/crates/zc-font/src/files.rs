//! フォントファイル（mmap）とラスタライズ。**mmap に触る処理はすべてここ**にある。
//!
//! SD からの読み込みが起きうる（1 回 5〜16ms 止まる）ので、実機ではこのモジュールの
//! 関数を assist スレッドで呼ぶ（design.md §3「共通の約束」）。[`FontFiles`] は
//! 読むだけなので、`Arc` で包んでスレッド間で共有できる。

use crate::bdf::BdfFont;
use crate::types::{Face, FontConfig, Glyph, GlyphKey, GlyphSource, Metrics, Size};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::{Angle, Format, Transform};
use swash::{FontDataRef, FontRef, StringId};

/// 斜体の傾き（度）。
const ITALIC_DEGREES: f32 = 12.0;
/// 太字で太らせる量（文字の大きさに対する割合）。
const BOLD_RATIO: f32 = 0.025;

/// ファイルを読み取り専用で mmap し、プロセスが終わるまで残す。
/// 必要なページだけが SD から読まれ、ページキャッシュ扱いになる（RSS を増やさない）。
fn map_file(path: &Path) -> io::Result<&'static [u8]> {
    let f = File::open(path)?;
    let len = f.metadata()?.len() as usize;
    if len == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{}: 空のファイル", path.display())));
    }
    // SAFETY: 読み取り専用の私有マッピング。unmap しないので 'static として扱える。
    let p = unsafe { libc::mmap(core::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0) };
    if p == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: p は len バイトの読める領域で、解放しない。
    Ok(unsafe { core::slice::from_raw_parts(p as *const u8, len) })
}

/// コレクション（.ttc）の中から、family 名が一致する書体を探す。
fn find_family(data: &'static [u8], family: &str) -> Option<FontRef<'static>> {
    let col = FontDataRef::new(data)?;
    col.fonts().find(|f| {
        f.localized_strings()
            .find_by_id(StringId::Family, None)
            .is_some_and(|s| s.chars().eq(family.chars()))
    })
}

/// 読み込んだフォント一式。
pub struct FontFiles {
    mono: FontRef<'static>,
    cjk_mono: Option<FontRef<'static>>,
    cjk_sans: Option<FontRef<'static>>,
    bdf: Option<BdfFont>,
    /// 等幅をビットマップで描くときの BDF（[`FontConfig::mono_bdf`]）。
    mono_bdf: Option<BdfFont>,
    /// 等幅の中の日本語のフォント（[`FontConfig::mono_cjk`]）と、その大きさ・点にするか。
    mono_cjk: Option<(FontRef<'static>, f32, bool)>,
    /// `mono_bdf` と `mono_cjk` を使う等幅の大きさ（[`FontConfig::code_px`]）。
    code_size: Size,
}

/// 読み込みで起きた、致命的でない問題（CJK が無い、など）。
pub type Warnings = Vec<String>;

impl FontFiles {
    /// フォントを開く。等幅の 1 番目が開けなければエラー。CJK や BDF が無いのは
    /// 警告にとどめる（その字は豆腐になる）。
    pub fn load(cfg: &FontConfig) -> io::Result<(FontFiles, Warnings)> {
        let mut warn = Warnings::new();
        let mono_data = map_file(&cfg.mono).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", cfg.mono.display())))?;
        let mono = FontRef::from_index(mono_data, 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("{}: フォントとして読めない", cfg.mono.display())))?;
        let (mut cjk_mono, mut cjk_sans) = (None, None);
        match map_file(&cfg.cjk) {
            Ok(data) => {
                cjk_mono = find_family(data, &cfg.cjk_mono_family);
                cjk_sans = find_family(data, &cfg.cjk_sans_family);
                if cjk_mono.is_none() {
                    warn.push(format!("{}: {} が無い", cfg.cjk.display(), cfg.cjk_mono_family));
                    cjk_mono = cjk_sans.or_else(|| FontRef::from_index(data, 0));
                }
                if cjk_sans.is_none() {
                    warn.push(format!("{}: {} が無い", cfg.cjk.display(), cfg.cjk_sans_family));
                    cjk_sans = cjk_mono;
                }
            }
            Err(e) => warn.push(format!("{}: {e}（日本語は表示できない）", cfg.cjk.display())),
        }
        let mut bdf = None;
        if let Some(p) = &cfg.bdf {
            match std::fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|s| BdfFont::parse(&s)) {
                Ok(f) => bdf = Some(f),
                Err(e) => warn.push(format!("{}: {e}", p.display())),
            }
        }
        let mut mono_bdf = None;
        if let Some(p) = &cfg.mono_bdf {
            match std::fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|s| BdfFont::parse(&s)) {
                Ok(f) => mono_bdf = Some(f),
                Err(e) => warn.push(format!("{}: {e}（等幅は TrueType で描く）", p.display())),
            }
        }
        let mut mono_cjk = None;
        if let Some(c) = &cfg.mono_cjk {
            match map_file(&c.path).ok().and_then(|d| FontRef::from_index(d, 0)) {
                Some(f) => mono_cjk = Some((f, c.px, c.crisp)),
                None => warn.push(format!("{}: 読めない（日本語は {} で描く）", c.path.display(), cfg.cjk_mono_family)),
            }
        }
        Ok((FontFiles { mono, cjk_mono, cjk_sans, bdf, mono_bdf, mono_cjk, code_size: Size::px(cfg.code_px) }, warn))
    }

    /// ビットマップフォントを読み込んであるか。
    pub fn has_bitmap(&self) -> bool {
        self.bdf.is_some()
    }

    /// この大きさの等幅を描く BDF（コードの大きさのときだけ）。
    fn code_bdf(&self, size: Size) -> Option<&BdfFont> {
        self.mono_bdf.as_ref().filter(|_| size == self.code_size)
    }

    /// 等幅の半角 1 字の幅（画素）。
    fn mono_cell(&self, size: Size) -> u16 {
        if let Some(b) = self.code_bdf(size) {
            return b.metrics().cell_w.max(1);
        }
        let gid = self.mono.charmap().map('0');
        let adv = self.mono.glyph_metrics(&[]).scale(size.as_px()).advance_width(gid);
        adv.round().max(1.0) as u16
    }

    /// 行の寸法。
    pub fn metrics(&self, face: Face, size: Size) -> Metrics {
        let px = size.as_px();
        match face {
            Face::Mono if self.code_bdf(size).is_some() => self.code_bdf(size).map(|b| b.metrics()).expect("mono_bdf"),
            Face::Mono => {
                let m = self.mono.metrics(&[]).scale(px);
                Metrics { ascent: m.ascent.round() as i16, descent: m.descent.round() as i16, cell_w: self.mono_cell(size) }
            }
            Face::Sans => match self.cjk_sans {
                Some(f) => {
                    let m = f.metrics(&[]).scale(px);
                    let adv = f.glyph_metrics(&[]).scale(px).advance_width(f.charmap().map('0'));
                    Metrics {
                        // Noto CJK の ascent / descent は行送り用で大きい（1.16 / 0.29 em）。
                        // 字面に合わせて 0.88 / 0.12 em を使う
                        ascent: (px * 0.88).round().min(m.ascent.round()) as i16,
                        descent: (px * 0.12).round().max(1.0) as i16,
                        cell_w: adv.round().max(1.0) as u16,
                    }
                }
                None => self.metrics(Face::Mono, size),
            },
            Face::Bitmap => match &self.bdf {
                Some(b) => b.metrics(),
                None => self.metrics(Face::Mono, size),
            },
        }
    }

    /// 字形を 1 つ作る。どのフォントにも無ければ豆腐（枠）を返す。
    pub fn rasterize(&self, ctx: &mut ScaleContext, key: GlyphKey) -> Glyph {
        let st = key.style;
        let px = st.size.as_px();
        match st.face {
            Face::Bitmap => {
                if let Some(g) = self.bdf.as_ref().and_then(|b| b.glyph(key.ch)) {
                    return g.clone();
                }
                self.tofu(self.metrics(Face::Bitmap, st.size), 1)
            }
            Face::Mono => {
                let cell = self.mono_cell(st.size);
                // ビットマップの等幅: BDF の字をそのまま（全角は半角 2 つぶん）
                if let Some(g) = self.code_bdf(st.size).and_then(|b| b.glyph(key.ch)) {
                    let mut g = g.clone();
                    g.advance = if g.advance > cell + cell / 5 { cell * 2 } else { cell };
                    return g;
                }
                let gid = self.mono.charmap().map(key.ch);
                if gid != 0 {
                    let mut g = render(ctx, self.mono, gid, px, true, st.bold, st.italic);
                    // 等幅のフォント自身に全角の字がある（BIZ UD ゴシックなど）なら半角 2 つぶん
                    let adv = self.mono.glyph_metrics(&[]).scale(px).advance_width(gid);
                    g.advance = if adv > cell as f32 * 1.2 { cell * 2 } else { cell };
                    return g;
                }
                // 日本語のフォントの指定があれば、その大きさで描いて、半角 2 つぶんの中央に置く
                if let Some((f, cpx, crisp)) = self.mono_cjk.filter(|_| st.size == self.code_size) {
                    let gid = f.charmap().map(key.ch);
                    if gid != 0 {
                        let adv = f.glyph_metrics(&[]).scale(cpx).advance_width(gid);
                        let mut g = render(ctx, f, gid, cpx, !crisp, st.bold, st.italic);
                        g.advance = if adv > cell as f32 * 1.2 { cell * 2 } else { cell };
                        g.left += ((g.advance as f32 - adv) / 2.0).round() as i16;
                        // 等幅の行より背が高ければ、下の余り（等幅の descent）の分まで下げて、上が
                        // 切れないようにする（Cozette 6×13 の行に PixelMplus12 を入れると上が 1px 出る）
                        let m = self.metrics(Face::Mono, st.size);
                        let over = g.top as i32 - m.ascent as i32;
                        if over > 0 {
                            let room = (m.descent as i32 - (g.height as i32 - g.top as i32)).max(0);
                            g.top -= over.min(room) as i16;
                        }
                        if crisp {
                            for a in g.data.iter_mut() {
                                *a = if *a >= 128 { 255 } else { 0 };
                            }
                        }
                        g.source = GlyphSource::Fallback;
                        return g;
                    }
                }
                // 無い字は CJK の等幅。全角が半角 2 つぶんになる大きさで描く
                if let Some(f) = self.cjk_mono {
                    let gid = f.charmap().map(key.ch);
                    if gid != 0 {
                        let cjk_px = cell as f32 * 2.0;
                        let adv = f.glyph_metrics(&[]).scale(cjk_px).advance_width(gid);
                        let mut g = render(ctx, f, gid, cjk_px, false, st.bold, st.italic);
                        g.advance = if adv > cell as f32 * 1.2 { cell * 2 } else { cell };
                        g.source = GlyphSource::Fallback;
                        return g;
                    }
                }
                let cells = if crate::types::is_wide(key.ch) { 2 } else { 1 };
                self.tofu(self.metrics(Face::Mono, st.size), cells)
            }
            Face::Sans => {
                let Some(f) = self.cjk_sans else {
                    return self.rasterize(ctx, GlyphKey { ch: key.ch, style: crate::TextStyle { face: Face::Mono, ..st } });
                };
                let gid = f.charmap().map(key.ch);
                if gid == 0 {
                    return self.tofu(self.metrics(Face::Sans, st.size), 1);
                }
                let adv = f.glyph_metrics(&[]).scale(px).advance_width(gid);
                let mut g = render(ctx, f, gid, px, false, st.bold, st.italic);
                g.advance = adv.round().max(1.0) as u16;
                g
            }
        }
    }

    /// 豆腐（字形が無いことを示す枠）。
    fn tofu(&self, m: Metrics, cells: u16) -> Glyph {
        let advance = m.cell_w * cells;
        let w = advance.saturating_sub(1).max(2);
        let h = (m.ascent.max(3) as u16).saturating_sub(1).max(3);
        let mut data = vec![0u8; w as usize * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                if y == 0 || y == h as usize - 1 || x == 0 || x == w as usize - 1 {
                    data[y * w as usize + x] = 160;
                }
            }
        }
        Glyph { width: w, height: h, left: 0, top: h as i16, advance, data: data.into_boxed_slice(), source: GlyphSource::Missing }
    }
}

fn render(ctx: &mut ScaleContext, font: FontRef<'static>, gid: u16, px: f32, hint: bool, bold: bool, italic: bool) -> Glyph {
    let mut scaler = ctx.builder(font).size(px).hint(hint).build();
    let mut r = Render::new(&[Source::Outline]);
    r.format(Format::Alpha);
    if bold {
        r.embolden(px * BOLD_RATIO);
    }
    if italic {
        r.transform(Some(Transform::skew(Angle::from_degrees(ITALIC_DEGREES), Angle::from_degrees(0.0))));
    }
    match r.render(&mut scaler, gid) {
        Some(img) => Glyph {
            width: img.placement.width as u16,
            height: img.placement.height as u16,
            left: img.placement.left as i16,
            top: img.placement.top as i16,
            advance: 0,
            data: img.data.into_boxed_slice(),
            source: GlyphSource::Primary,
        },
        // 輪郭の無い字（空白など）
        None => Glyph { width: 0, height: 0, left: 0, top: 0, advance: 0, data: Box::new([]), source: GlyphSource::Primary },
    }
}
