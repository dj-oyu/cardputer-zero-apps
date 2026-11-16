//! zcode 本体（design.md §4）。イベントループ・スレッド構成・コマンド表と、
//! 段階 1 の画面（キー確認・フォント比較・計測）。
//!
//! 実行ファイル（`main.rs`）は引数を読んで [`app::run`] に画面を渡すだけ。
//! 画面の描画は実機なしで試せる（[`render_screen`]、`tests/render.rs`）。

pub mod app;
pub mod bench;
pub mod commands;
pub mod crash;
pub mod fonttest;
pub mod keylog;
pub mod keytest;
pub mod launcher_config;
pub mod sample;
pub mod stats;

use zc_core::{Canvas, Theme};
use zc_font::{FontConfig, Fonts, RasterMode};
use zc_ui::RenderCx;

/// 画面を 1 回描いて `Canvas` にする（render-test 用。実機もスレッドも要らない）。
pub fn render_screen(screen: &mut dyn app::Screen, fonts: &FontConfig) -> std::io::Result<Canvas> {
    let mut fonts = Fonts::load(fonts, RasterMode::Inline)?;
    let theme = Theme::dark_plus();
    let mut canvas = Canvas::new();
    screen.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme));
    Ok(canvas)
}
