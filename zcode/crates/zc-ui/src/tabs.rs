//! タブバー（最上段、高さ 13。モック 1・2）。

use crate::layout::UI_PX;
use crate::{RenderCx, Widget};
use zc_core::{palette, Color, Damage, DocId, Rect};
use zc_font::TextStyle;

/// タブ 1 つ。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub doc: DocId,
    pub title: String,
    /// 未保存（● を付ける）。
    pub modified: bool,
}

/// タブバー。タブが入りきらないときは、選択中のタブが見える位置まで横にずらす。
#[derive(Default)]
pub struct TabBar {
    tabs: Vec<Tab>,
    active: usize,
    /// 左の空き（ツリーを開いているあいだ、タブをエディタの上から並べる）。
    inset: u16,
    scroll: i32,
    dirty: bool,
}

/// タブの左右の余白。
const PAD: i32 = 5;

impl TabBar {
    pub fn new() -> TabBar {
        TabBar { dirty: true, ..Default::default() }
    }
    /// タブの並びと選択中のタブ。変わったときだけ描き直す。
    pub fn set_tabs(&mut self, tabs: Vec<Tab>, active: usize) {
        if self.tabs != tabs || self.active != active {
            self.tabs = tabs;
            self.active = active;
            self.dirty = true;
        }
    }
    /// 左の空き。
    pub fn set_inset(&mut self, inset: u16) {
        if self.inset != inset {
            self.inset = inset;
            self.dirty = true;
        }
    }
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }
    pub fn active(&self) -> usize {
        self.active
    }

    /// タブの幅（余白と右の境界線込み）。
    fn tab_width(cx: &mut RenderCx<'_>, tab: &Tab, ts: &TextStyle) -> i32 {
        let dot = if tab.modified { cx.fonts.measure(" ●", ts) } else { 0 };
        PAD + cx.fonts.measure(&tab.title, ts) + dot + PAD + 1
    }
}

impl Widget for TabBar {
    fn render(&mut self, cx: &mut RenderCx<'_>, area: Rect) -> Damage {
        if !self.dirty {
            return Damage::none();
        }
        self.dirty = false;
        let ts = TextStyle::mono(UI_PX);
        cx.canvas.fill_rect(area, palette::TABBAR);
        let clip = Rect::new(area.x + self.inset as i16, area.y, area.w.saturating_sub(self.inset), area.h);
        let widths: Vec<i32> = self.tabs.iter().map(|t| TabBar::tab_width(cx, t, &ts)).collect();
        // 選択中のタブが見えるようにずらす
        let start: i32 = widths.iter().take(self.active).sum();
        let end = start + widths.get(self.active).copied().unwrap_or(0);
        if start < self.scroll {
            self.scroll = start;
        } else if end > self.scroll + clip.w as i32 {
            self.scroll = end - clip.w as i32;
        }
        let total: i32 = widths.iter().sum();
        self.scroll = self.scroll.clamp(0, (total - clip.w as i32).max(0));
        let mut x = clip.x as i32 - self.scroll;
        let base = area.y as i32 + 1 + cx.baseline_in(&ts, area.h - 1);
        for (i, (tab, &w)) in self.tabs.iter().zip(&widths).enumerate() {
            if x + w > clip.x as i32 && x < clip.right() {
                let on = i == self.active;
                let r = Rect::new(x as i16, area.y, w as u16, area.h);
                let (bg, fg) = if on { (palette::TAB_ACTIVE, Color::WHITE) } else { (palette::TABBAR, palette::DIM) };
                if on {
                    cx.canvas.fill_rect(r.intersect(&clip), bg);
                    cx.canvas.fill_rect(Rect::new(r.x, r.y, r.w, 1).intersect(&clip), palette::STATUS);
                }
                cx.canvas.fill_rect(Rect::new((x + w - 1) as i16, area.y, 1, area.h).intersect(&clip), palette::SIDE);
                let text = Rect::new((x + PAD) as i16, area.y + 1, (w - PAD - 1) as u16, area.h - 1).intersect(&clip);
                let tx = cx.text(x + PAD, base, &tab.title, &ts, fg, bg, text);
                if tab.modified {
                    cx.text(tx, base, " ●", &ts, Color::WHITE, bg, text);
                }
            }
            x += w;
        }
        Damage::rect(area)
    }
    fn invalidate(&mut self) {
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::{Canvas, Theme};
    use zc_font::{FontConfig, Fonts, RasterMode};

    fn tab(i: u32, title: &str, modified: bool) -> Tab {
        Tab { doc: DocId(i), title: title.into(), modified }
    }

    #[test]
    fn renders_once_and_scrolls_to_active() {
        let cfg = FontConfig::default();
        if !cfg.available() {
            return;
        }
        let mut fonts = Fonts::load(&cfg, RasterMode::Inline).unwrap();
        let theme = Theme::dark_plus();
        let mut canvas = Canvas::new();
        let area = Rect::new(0, 0, 320, 13);
        let mut bar = TabBar::new();
        bar.set_tabs(vec![tab(1, "utils.py", false), tab(2, "main.py", true)], 1);
        let d = bar.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme), area);
        assert_eq!(d.bounds(), zc_core::RowRange::new(0, 13));
        assert!(bar.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme), area).is_empty());
        // 選択中のタブの上端に青い線、その下は本文と同じ地の色
        let w0 = 5 + 8 * 5 + 5 + 1;
        assert_eq!(canvas.get(w0 + 2, 0), Some(palette::STATUS));
        assert_eq!(canvas.get(w0 + 2, 12), Some(palette::TAB_ACTIVE));
        assert_eq!(canvas.get(2, 0), Some(palette::TABBAR));
        // 同じ内容を入れ直しても描き直さない
        bar.set_tabs(vec![tab(1, "utils.py", false), tab(2, "main.py", true)], 1);
        assert!(bar.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme), area).is_empty());
        // たくさん開く: 選択中のタブが右端に収まる
        let many: Vec<Tab> = (0..12).map(|i| tab(i, &format!("file_{i}.py"), false)).collect();
        bar.set_tabs(many, 11);
        bar.set_inset(84);
        bar.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme), area);
        assert!(bar.scroll > 0);
        assert_eq!(canvas.get(317, 0), Some(palette::STATUS));
        // 左の空きにはタブを描かない
        assert!((0..84).all(|x| canvas.get(x, 0) == Some(palette::TABBAR)));
    }
}
