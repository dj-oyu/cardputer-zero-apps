//! 画面の割り付けと寸法（`docs/ui-mock.html` と同じ値）。

use zc_core::Rect;

/// タブバーの高さ。
pub const TAB_H: u16 = 13;
/// ステータスバーの高さ。
pub const STATUS_H: u16 = 11;
/// ファイルツリーの幅。
pub const SIDE_W: u16 = 84;
/// コードの 1 行の高さ。
pub const CODE_LINE_H: u16 = 11;
/// コードの文字の大きさ（画素）。段階 1 のフォント比較で最終決定する。
pub const CODE_PX: f32 = 9.0;
/// ステータスバーの文字の大きさ。
pub const STATUS_PX: f32 = 8.0;
/// 行番号の欄の幅。
pub const GUTTER_W: u16 = 20;

/// 画面全体の割り付け。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Regions {
    pub tabs: Rect,
    /// ファイルツリー（閉じていれば幅 0）。
    pub side: Rect,
    pub editor: Rect,
    /// 下のパネル（ターミナル。閉じていれば高さ 0）。
    pub panel: Rect,
    pub status: Rect,
}

impl Regions {
    /// `tree_open`: ツリーを開いているか。`panel_h`: 下のパネルの高さ（0 で閉じる。
    /// 本文の高さ以上なら全画面）。
    pub fn compute(tree_open: bool, panel_h: u16) -> Regions {
        let (rest, status) = Rect::SCREEN.split_bottom(STATUS_H);
        let (tabs, body) = rest.split_top(TAB_H);
        let (body, panel) = body.split_bottom(panel_h);
        let (side, editor) = body.split_left(if tree_open { SIDE_W } else { 0 });
        Regions { tabs, side, editor, panel, status }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_match_mock() {
        let r = Regions::compute(true, 0);
        assert_eq!(r.tabs, Rect::new(0, 0, 320, 13));
        assert_eq!(r.side, Rect::new(0, 13, 84, 146));
        assert_eq!(r.editor, Rect::new(84, 13, 236, 146));
        assert_eq!(r.status, Rect::new(0, 159, 320, 11));
        assert!(r.panel.is_empty());
        // 全幅なら 146 / 11 = 13 行
        let r = Regions::compute(false, 0);
        assert_eq!(r.editor.w, 320);
        assert_eq!(r.editor.h / CODE_LINE_H, 13);
        let r = Regions::compute(false, 80);
        assert_eq!((r.editor.h, r.panel.y, r.panel.h), (66, 79, 80));
    }
}
