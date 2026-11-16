//! 画面の割り付けと寸法（`docs/ui-mock.html` と同じ値）。

use zc_core::Rect;

/// タブバーの高さ。
pub const TAB_H: u16 = 13;
/// ステータスバーの高さ。
pub const STATUS_H: u16 = 11;
/// ファイルツリーの幅。
pub const SIDE_W: u16 = 84;
/// コードの 1 行の高さ。
pub const CODE_LINE_H: u16 = 12;
/// コードの文字の大きさ（画素）。フォント比較画面で 10px / 行 12px に決定（2026-11-16）。
pub const CODE_PX: f32 = 10.0;
/// タブ・ツリーの文字の大きさ（モックと同じ。コードより 1 段小さい）。
pub const UI_PX: f32 = 9.0;
/// ツリーの 1 項目の高さ。
pub const TREE_ROW_H: u16 = 11;
/// ツリーの見出し（EXPLORER）の高さ。
pub const TREE_HEAD_H: u16 = 10;
/// ステータスバーの文字の大きさ。
pub const STATUS_PX: f32 = 8.0;
/// 行番号の欄の最小幅（3 桁 × 字幅 6 + 左 2 + 右 3）。行数が 1000 以上なら桁数ぶん広がる。
pub const GUTTER_W: u16 = 23;

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
        // 全幅なら 146 / 12 = 12 行
        let r = Regions::compute(false, 0);
        assert_eq!(r.editor.w, 320);
        assert_eq!(r.editor.h / CODE_LINE_H, 12);
        let r = Regions::compute(false, 80);
        assert_eq!((r.editor.h, r.panel.y, r.panel.h), (66, 79, 80));
    }
}
