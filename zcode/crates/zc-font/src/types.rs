//! フォントまわりの値の型。

use std::path::PathBuf;

/// 書体の系統。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    /// 等幅（コード・端末）。DejaVu Sans Mono、無い字は Noto Sans Mono CJK JP。
    /// 全角の字は半角 2 つぶんの幅にそろえる。
    Mono,
    /// プロポーショナル（Markdown の本文）。Noto Sans CJK JP。
    Sans,
    /// コマンドラインで渡したビットマップフォント（BDF）。比較画面用。
    Bitmap,
}

/// 文字の大きさ。1/4 画素単位で持つ（モックの 7.5px・8.5px・10.5px を表せる）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Size(pub u16);

impl Size {
    /// 画素数から（1/4 画素に丸める）。
    pub fn px(px: f32) -> Size {
        Size((px * 4.0).round().clamp(4.0, 4.0 * 255.0) as u16)
    }
    pub fn as_px(self) -> f32 {
        self.0 as f32 / 4.0
    }
}

/// 字形を選ぶ条件。キャッシュの鍵の一部。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextStyle {
    pub face: Face,
    pub size: Size,
    /// 太字（いまは字形を太らせて作る）。
    pub bold: bool,
    /// 斜体（字形を傾けて作る。CJK に斜体の字形が無いため。design.md §5.4）。
    pub italic: bool,
}

impl TextStyle {
    /// 等幅の `px` 画素。
    pub fn mono(px: f32) -> TextStyle {
        TextStyle { face: Face::Mono, size: Size::px(px), bold: false, italic: false }
    }
    /// プロポーショナルの `px` 画素。
    pub fn sans(px: f32) -> TextStyle {
        TextStyle { face: Face::Sans, size: Size::px(px), bold: false, italic: false }
    }
    pub fn bold(mut self) -> TextStyle {
        self.bold = true;
        self
    }
    pub fn italic(mut self) -> TextStyle {
        self.italic = true;
        self
    }
}

/// グリフキャッシュの鍵。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub ch: char,
    pub style: TextStyle,
}

/// 字形がどのフォントから来たか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphSource {
    /// その書体の 1 番目のフォント。
    Primary,
    /// フォールバック（等幅での CJK）。
    Fallback,
    /// どのフォントにも無い（豆腐）。
    Missing,
}

/// ラスタライズ済みの字形（8bit の濃度）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub width: u16,
    pub height: u16,
    /// ペン位置から濃度マスクの左端まで（右が正）。
    pub left: i16,
    /// ベースラインから濃度マスクの上端まで（上が正）。
    pub top: i16,
    /// 送り幅（画素）。等幅では半角の幅の整数倍。
    pub advance: u16,
    /// 濃度（行優先、`width * height` バイト）。
    pub data: Box<[u8]>,
    pub source: GlyphSource,
}

impl Glyph {
    /// キャッシュの中で占める大きさの目安（バイト）。
    pub fn cost(&self) -> usize {
        self.data.len() + 64
    }
}

/// ある書体・大きさの行の寸法（画素）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// ベースラインから上。
    pub ascent: i16,
    /// ベースラインから下（正の値）。
    pub descent: i16,
    /// 半角 1 字の送り幅（等幅のマス目の幅。プロポーショナルでは `0` の幅）。
    pub cell_w: u16,
}

impl Metrics {
    /// 行間を足さない最小の行の高さ。
    pub fn height(&self) -> u16 {
        (self.ascent + self.descent).max(1) as u16
    }
}

/// 読み込むフォントの指定。既定値は実機（と開発コンテナ）のパス。
#[derive(Clone, Debug)]
pub struct FontConfig {
    /// 等幅の 1 番目（TrueType、ヒンティングを使う）。
    pub mono: PathBuf,
    /// CJK のコレクション（.ttc）。
    pub cjk: PathBuf,
    /// `cjk` の中の、等幅の書体の family 名。
    pub cjk_mono_family: String,
    /// `cjk` の中の、プロポーショナルの書体の family 名。
    pub cjk_sans_family: String,
    /// ビットマップフォント（BDF、ISO10646 のもの）。あれば [`Face::Bitmap`] で使える。
    pub bdf: Option<PathBuf>,
    /// 等幅を `code_px` の大きさのときだけビットマップフォント（BDF、ISO10646 のもの）で描く。
    /// そのときの行の寸法はこの BDF のもの。BDF に無い字は `mono` と CJK に回る。ほかの大きさ
    /// （タブ・ツリー・ステータスバーの小さな字）は `mono` のまま。
    pub mono_bdf: Option<PathBuf>,
    /// 等幅の `code_px` の大きさの中の日本語（等幅のフォントに無い字）を描くフォント。無ければ
    /// `cjk` の `cjk_mono_family`（半角 2 つぶんの大きさ）。
    pub mono_cjk: Option<CjkFont>,
    /// `mono_bdf` と `mono_cjk` を使う等幅の大きさ（画素。コードの字の大きさ）。
    pub code_px: f32,
}

/// 等幅の中の日本語を描くフォント（[`FontConfig::mono_cjk`]）。
#[derive(Clone, Debug, PartialEq)]
pub struct CjkFont {
    /// TrueType / OpenType（.ttc なら 1 番目）。
    pub path: PathBuf,
    /// 描く大きさ（画素）。点で描いたフォント（PixelMplus など）は、その決まった大きさにする。
    pub px: f32,
    /// 濃度を 0 か 255 にする（点で描いたフォントのぼけを消す）。
    pub crisp: bool,
}

impl Default for FontConfig {
    fn default() -> Self {
        FontConfig {
            mono: "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf".into(),
            cjk: "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into(),
            cjk_mono_family: "Noto Sans Mono CJK JP".into(),
            cjk_sans_family: "Noto Sans CJK JP".into(),
            bdf: None,
            mono_bdf: None,
            mono_cjk: None,
            code_px: 10.0,
        }
    }
}

impl FontConfig {
    /// 既定のフォントファイルがこの環境にあるか（render-test を飛ばす判断用）。
    pub fn available(&self) -> bool {
        self.mono.exists() && self.cjk.exists()
    }
}

/// 全角（半角 2 つぶん）で表示する字か。字形がまだ無いとき（assist 待ち）の
/// 送り幅の見積もりに使う。East Asian Width の W / F の主な範囲だけを見る。
pub fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F | 0x20000..=0x3FFFD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_quarters() {
        assert_eq!(Size::px(9.0), Size(36));
        assert_eq!(Size::px(7.5), Size(30));
        assert_eq!(Size::px(8.5).as_px(), 8.5);
    }

    #[test]
    fn wide_chars() {
        assert!(is_wide('漢'));
        assert!(is_wide('あ'));
        assert!(is_wide('ー'));
        assert!(is_wide('（'));
        assert!(!is_wide('a'));
        assert!(!is_wide('▽'));
        assert!(!is_wide('ｱ'));
    }
}
