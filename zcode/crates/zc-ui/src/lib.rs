//! 画面部品（design.md §3 / §5、`docs/ui-mock.html`）。
//!
//! - 描画はすべて [`RenderCx`]（`Canvas` + フォント + 配色）に対して行う。fb0 も
//!   PNG も同じ `Canvas` なので、実機が無くても render-test で確認できる
//! - 各部品は [`Widget`] を実装する: `render(&mut self, cx, area) -> Damage`。
//!   **自分が描き変えた行範囲だけ**を返し、何も変わらなければ `Damage::none()` を返す
//!   （部品は「前回描いた内容から変わったか」を自分で覚えておく）
//! - 部品は入力を直接受け取らない。アプリ（`zcode`）が `Command` に直してから、
//!   部品の操作用の関数を呼ぶ
//!
//! 実装済み: [`RenderCx`] の文字描画、[`StatusBar`]・[`TitleBar`]、エディタ本体
//! （[`EditorView`]。行の組み方は [`textlayout`]）、[`TabBar`]、[`FileTree`]。
//! ほかの部品は雛形（[`widgets`]）。

// 段階 2 以降の部品が使う crate（依存の向きを先に決めておく）
use zc_buffer as _;
use zc_highlight as _;
use zc_intel as _;
use zc_skk as _;

pub mod editor;
pub mod layout;
pub mod mdlayout;
pub mod picker;
pub mod skkui;
pub mod status;
pub mod tabs;
pub mod terminal;
pub mod textlayout;
pub mod tree;
pub mod widgets;

pub use editor::{EditorView, LineMark, NoSpans, PreeditSeg, PreeditStyle, PreeditView, RenderStats, SpanSource, Viewport};
pub use mdlayout::MdLayouter;
pub use picker::{CompletionPopup, PickRow, PickerList, PopupItem};
pub use skkui::{CandidatePopup, CandidateRow, MiniBuffer, MINI_H};
pub use status::{Chip, EditorStatus, ImeMode, StatusBar, TitleBar};
pub use tabs::{Tab, TabBar};
pub use terminal::{TermScreen, TerminalPanel, TERM_PANEL_H};
pub use textlayout::{Caret, CodeLayouter, Deco, DecoKind, LineInput, LineLayout, LineLayouter, VisualRow, VisualRun};
pub use tree::{FileTree, TreeNode};

use zc_core::{BlendLut, Canvas, Color, Damage, Rect, Theme};
use zc_font::{Fonts, Metrics, TextStyle};

/// 描画に要るもの一式。フレームごとにアプリが作って、部品に渡す。
pub struct RenderCx<'a> {
    pub canvas: &'a mut Canvas,
    pub fonts: &'a mut Fonts,
    pub theme: &'a Theme,
}

/// 画面部品。
pub trait Widget {
    /// `area` に自分を描き、描き変えた行範囲を返す。何も変わらなければ空を返す。
    fn render(&mut self, cx: &mut RenderCx<'_>, area: Rect) -> Damage;
    /// 次の `render` で必ず全体を描き直させる（別の画面から戻ったとき、
    /// `area` が変わったとき）。
    fn invalidate(&mut self);
}

impl<'a> RenderCx<'a> {
    pub fn new(canvas: &'a mut Canvas, fonts: &'a mut Fonts, theme: &'a Theme) -> RenderCx<'a> {
        RenderCx { canvas, fonts, theme }
    }

    /// 行の寸法。
    pub fn metrics(&mut self, ts: &TextStyle) -> Metrics {
        self.fonts.metrics(ts)
    }

    /// 高さ `h` の行の中で、文字が上下の中央に来るベースラインの位置（行の上端から）。
    pub fn baseline_in(&mut self, ts: &TextStyle, h: u16) -> i32 {
        let m = self.fonts.metrics(ts);
        (h as i32 - m.height() as i32).div_euclid(2) + m.ascent as i32
    }

    /// 文字列を描く（背景は塗らない。`bg` は合成の相手の色）。描き終わりの x を返す。
    #[allow(clippy::too_many_arguments)]
    pub fn text(&mut self, x: i32, baseline: i32, s: &str, ts: &TextStyle, fg: Color, bg: Color, clip: Rect) -> i32 {
        let lut = BlendLut::new(fg, bg);
        self.fonts.draw_text(self.canvas, x, baseline, s, ts, &lut, clip)
    }

    /// `area` の左端から、上下の中央に文字列を描く。描き終わりの x を返す。
    pub fn text_in(&mut self, area: Rect, s: &str, ts: &TextStyle, fg: Color, bg: Color) -> i32 {
        let base = area.y as i32 + self.baseline_in(ts, area.h);
        self.text(area.x as i32, base, s, ts, fg, bg, area)
    }

    /// `area` の右端に寄せて文字列を描く。描き始めの x を返す。
    pub fn text_right(&mut self, area: Rect, s: &str, ts: &TextStyle, fg: Color, bg: Color) -> i32 {
        let w = self.fonts.measure(s, ts);
        let x = (area.right() - w).max(area.x as i32);
        let base = area.y as i32 + self.baseline_in(ts, area.h);
        self.text(x, base, s, ts, fg, bg, area);
        x
    }
}
