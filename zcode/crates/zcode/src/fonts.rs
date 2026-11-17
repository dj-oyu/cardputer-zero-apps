//! 同梱のコードフォント: 英字は Cozette（BDF、6×13）、日本語は PixelMplus12（点で描いた
//! TrueType を 12px で、ぼかさずに）。.deb は実行ファイルの隣の `fonts/` に入れる
//! （`/usr/share/APPLaunch/lib/zcode/fonts/`。`build.sh`）。無ければ DejaVu Sans Mono のまま。
//!
//! 同梱のフォントはコードの大きさ（[`zc_ui::layout::CODE_PX`]）のときだけ使う。タブ・ツリー・
//! ステータスバーの小さな字は DejaVu Sans Mono のまま（[`FontConfig::code_px`]）。

use std::path::{Path, PathBuf};
use zc_font::{CjkFont, FontConfig};
use zc_ui::layout::CODE_PX;

/// 英字の BDF のファイル名。
pub const BDF: &str = "cozette.bdf";
/// 日本語のファイル名。
pub const CJK: &str = "PixelMplus12-Regular.ttf";
/// 日本語を描く大きさ（PixelMplus12 は 12px で点の字そのままになる）。
pub const CJK_PX: f32 = 12.0;
/// 同梱のフォントのときのコードと端末の 1 行の高さ（Cozette の 13px）。
pub const LINE_H: u16 = 13;

/// 実行ファイルの隣の `fonts/`。
pub fn default_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("fonts"))
}

/// `dir` に同梱のフォントがあれば `cfg` に入れて、コードの 1 行の高さを返す。英字の BDF が
/// 無ければ何もしない（日本語だけあっても使わない）。日本語が無ければ Noto Sans Mono CJK のまま。
pub fn apply(cfg: &mut FontConfig, dir: &Path) -> Option<u16> {
    let bdf = dir.join(BDF);
    if !bdf.is_file() {
        return None;
    }
    cfg.mono_bdf = Some(bdf);
    cfg.code_px = CODE_PX;
    let cjk = dir.join(CJK);
    cfg.mono_cjk = cjk.is_file().then(|| CjkFont { path: cjk, px: CJK_PX, crisp: true });
    Some(LINE_H)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_needs_the_bdf() {
        let dir = std::env::temp_dir().join(format!("zcode-fonts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FontConfig::default();
        assert_eq!(apply(&mut cfg, &dir), None);
        std::fs::write(dir.join(CJK), b"x").unwrap();
        assert_eq!(apply(&mut cfg, &dir), None);
        assert!(cfg.mono_bdf.is_none() && cfg.mono_cjk.is_none());
        std::fs::write(dir.join(BDF), b"x").unwrap();
        assert_eq!(apply(&mut cfg, &dir), Some(LINE_H));
        assert_eq!(cfg.mono_bdf, Some(dir.join(BDF)));
        assert_eq!(cfg.mono_cjk.as_ref().map(|c| (c.px, c.crisp)), Some((CJK_PX, true)));
        std::fs::remove_file(dir.join(CJK)).unwrap();
        let mut cfg = FontConfig::default();
        assert_eq!(apply(&mut cfg, &dir), Some(LINE_H));
        assert!(cfg.mono_cjk.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
