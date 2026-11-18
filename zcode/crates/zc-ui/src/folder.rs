//! フォルダを開く画面（ツリーで F11。ツリーの根を選び直す）。
//!
//! いまいるフォルダの中身を、フォルダは明るく・ファイルは薄く（選べない）並べる。先頭は `.`
//! （いまいるフォルダ。Enter でそこに決める）、次に `..`（上のフォルダ）。↑↓ で選び、Enter で
//! 入る（`.` なら決める）、BS で上へ、Space で選んだフォルダに決める（ここは見た目と状態だけ。
//! 決めた後のことはエディタの画面が受け持つ）。

use crate::layout::{CODE_LINE_H, CODE_PX, TAB_H, UI_PX};
use crate::tree::{display_path, list_dir, TreeNode};
use crate::{RenderCx, Widget};
use std::path::{Path, PathBuf};
use zc_core::{palette, Color, Damage, Rect};
use zc_font::TextStyle;

/// 1 行（`.` は `path` がいまいるフォルダ、`..` は親のフォルダ）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

/// フォルダを開く画面。
pub struct FolderPicker {
    dir: PathBuf,
    entries: Vec<Entry>,
    sel: usize,
    scroll: usize,
    /// いまツリーの根になっているフォルダ（印を出す）。
    current: PathBuf,
    font: TextStyle,
    row_h: u16,
    dirty: bool,
}

impl FolderPicker {
    /// `current`（いまの根）の中身から始める。選択は最初のフォルダ（無ければ `.`）。
    pub fn new(current: &Path) -> FolderPicker {
        let mut p = FolderPicker {
            dir: current.to_path_buf(),
            entries: Vec::new(),
            sel: 0,
            scroll: 0,
            current: current.to_path_buf(),
            font: TextStyle::mono(CODE_PX),
            row_h: CODE_LINE_H,
            dirty: true,
        };
        p.load(None);
        p
    }
    /// 行の字と高さ（コードと同じにする）。
    pub fn set_font(&mut self, px: f32, row_h: u16) {
        self.font = TextStyle::mono(px);
        self.row_h = row_h.max(1);
        self.dirty = true;
    }
    /// いまいるフォルダ。
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    /// 選んでいる行のフォルダ（`.` ならいまいるフォルダ、`..` なら上のフォルダ）。ファイルなら `None`。
    pub fn selected(&self) -> Option<&Path> {
        self.entries.get(self.sel).filter(|e| e.is_dir).map(|e| e.path.as_path())
    }
    /// 並んでいる名前（試験用。`.`・`..` を含む）。
    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// `dir` を読み直す。`select` があればその行を選ぶ（上へ戻ったとき、いたフォルダを選ぶ）。
    fn load(&mut self, select: Option<&Path>) {
        let mut v = vec![Entry { name: ".".into(), path: self.dir.clone(), is_dir: true }];
        if let Some(parent) = self.dir.parent() {
            v.push(Entry { name: "..".into(), path: parent.to_path_buf(), is_dir: true });
        }
        v.extend(list_dir(&self.dir, 0).into_iter().map(|TreeNode { name, path, is_dir, .. }| Entry { name, path, is_dir }));
        self.entries = v;
        self.scroll = 0;
        let first_dir = self.entries.iter().position(|e| e.is_dir && !is_dot(&e.name)).unwrap_or(0);
        self.sel = select.and_then(|s| self.entries.iter().position(|e| e.path == s && !is_dot(&e.name))).unwrap_or(first_dir);
        self.dirty = true;
    }

    /// 選択を動かす（ファイルは飛ばす。端で止まる）。
    pub fn move_sel(&mut self, delta: i32) {
        let dirs: Vec<usize> = (0..self.entries.len()).filter(|&i| self.entries[i].is_dir).collect();
        if dirs.is_empty() {
            return;
        }
        let at = dirs.iter().position(|&i| i == self.sel).unwrap_or(0);
        let to = (at as i64 + delta as i64).clamp(0, dirs.len() as i64 - 1) as usize;
        if dirs[to] != self.sel {
            self.sel = dirs[to];
            self.dirty = true;
        }
    }
    /// 選んでいるのが `.`（いまいるフォルダ）か。
    pub fn on_here(&self) -> bool {
        self.entries.get(self.sel).is_some_and(|e| e.name == ".")
    }
    /// Enter: 選んだフォルダに入る（`..` なら上へ）。`.` は入る先が無いので何もしない
    /// （決めるのは呼ぶ側。[`FolderPicker::on_here`] を見る）。
    pub fn enter(&mut self) {
        let Some(e) = self.entries.get(self.sel).filter(|e| e.is_dir).cloned() else { return };
        match e.name.as_str() {
            "." => {}
            ".." => self.up(),
            _ => {
                self.dir = e.path;
                self.load(None);
            }
        }
    }
    /// BS: 上のフォルダへ（いたフォルダを選んだまま）。
    pub fn up(&mut self) {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else { return };
        let was = std::mem::replace(&mut self.dir, parent);
        self.load(Some(&was));
    }
}

impl Widget for FolderPicker {
    /// `area` は画面の上端からステータスバーの上まで（見出し・パス・一覧）。
    fn render(&mut self, cx: &mut RenderCx<'_>, area: Rect) -> Damage {
        if !std::mem::take(&mut self.dirty) || area.is_empty() {
            return Damage::none();
        }
        let ui = TextStyle::mono(UI_PX);
        cx.canvas.fill_rect(area, palette::SIDE);
        // 見出し
        let head = Rect::new(area.x, area.y, area.w, TAB_H);
        cx.canvas.fill_rect(head, palette::TABBAR);
        cx.text_in(Rect::new(head.x + 4, head.y, head.w / 2, head.h), "フォルダを開く", &ui, Color::WHITE, palette::TABBAR);
        cx.text_right(Rect::new(head.x, head.y, head.w.saturating_sub(4), head.h), "ツリーの根を選ぶ", &ui, palette::DIM, palette::TABBAR);
        // いまいるフォルダ
        let mut path = display_path(&self.dir);
        if !path.ends_with('/') {
            path.push('/');
        }
        let path_row = Rect::new(area.x, head.bottom() as i16, area.w, self.row_h);
        cx.text_in(Rect::new(path_row.x + 4, path_row.y, path_row.w.saturating_sub(8), path_row.h), &path, &self.font, palette::FG, palette::SIDE);
        cx.canvas.hline(area.x as i32, path_row.bottom(), area.w, palette::BORDER);
        // 一覧（選択が見える位置までずらす）
        let top = path_row.bottom() + 1;
        let rows = ((area.bottom() - top).max(0) as u16 / self.row_h) as usize;
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if rows > 0 && self.sel >= self.scroll + rows {
            self.scroll = self.sel + 1 - rows;
        }
        for (i, e) in self.entries.iter().enumerate().skip(self.scroll).take(rows) {
            let r = Rect::new(area.x, (top + ((i - self.scroll) as u16 * self.row_h) as i32) as i16, area.w, self.row_h);
            let on = i == self.sel;
            let bg = if on { palette::LIST_FOCUS } else { palette::SIDE };
            cx.canvas.fill_rect(r, bg);
            if on {
                cx.canvas.stroke_rect(r, palette::STATUS);
            }
            let label = if e.is_dir && !is_dot(&e.name) { format!("{}/", e.name) } else { e.name.clone() };
            let fg = if e.is_dir { palette::FG } else { palette::DIM };
            cx.text_in(Rect::new(r.x + 10, r.y, r.w.saturating_sub(12), r.h), &label, &self.font, fg, bg);
            if e.is_dir && e.name != ".." && e.path == self.current {
                cx.text_right(Rect::new(r.x, r.y, r.w.saturating_sub(6), r.h), "いま開いている", &ui, palette::DIM, bg);
            }
        }
        Damage::rect(area)
    }
    fn invalidate(&mut self) {
        self.dirty = true;
    }
}

/// `.` か `..`（名前の後ろに `/` を付けない・戻ってきたときに選ばない行）。
fn is_dot(name: &str) -> bool {
    name == "." || name == ".."
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zc-folder-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for p in ["code/blink/lib", "code/qr-test", "code/rust-hello"] {
            std::fs::create_dir_all(d.join(p)).unwrap();
        }
        std::fs::write(d.join("code/blink/main.py"), "").unwrap();
        std::fs::write(d.join("code/todo.md"), "").unwrap();
        d
    }

    #[test]
    fn walks_up_and_down_and_skips_files() {
        let d = tmp("walk");
        let blink = d.join("code/blink");
        let mut p = FolderPicker::new(&blink);
        assert_eq!(p.names(), [".", "..", "lib", "main.py"]);
        assert_eq!(p.selected(), Some(blink.join("lib").as_path()));
        // ファイルは選べない（下へ動いても lib のまま）
        p.move_sel(1);
        assert_eq!(p.selected(), Some(blink.join("lib").as_path()));
        // 上へ: いたフォルダ（blink）を選んだまま
        p.up();
        assert_eq!(p.dir(), d.join("code"));
        assert_eq!(p.names(), [".", "..", "blink", "qr-test", "rust-hello", "todo.md"]);
        assert_eq!(p.selected(), Some(blink.as_path()));
        p.move_sel(1);
        p.enter();
        assert_eq!(p.dir(), d.join("code/qr-test"));
        assert_eq!(p.names(), [".", ".."]);
        // 下にフォルダが無ければ `.` を選ぶ（Enter でそこに決められる。入る先は無い）
        assert!(p.on_here());
        assert_eq!(p.selected(), Some(d.join("code/qr-test").as_path()));
        p.enter();
        assert_eq!(p.dir(), d.join("code/qr-test"));
        // `..` で上へ（いたフォルダを選んだまま）
        p.move_sel(1);
        p.enter();
        assert_eq!(p.dir(), d.join("code"));
        assert_eq!(p.selected(), Some(d.join("code/qr-test").as_path()));
        // 先頭まで上げると `.`（いまいるフォルダ）
        p.move_sel(i32::MIN / 2);
        assert!(p.on_here());
        assert_eq!(p.selected(), Some(d.join("code").as_path()));
        let _ = std::fs::remove_dir_all(&d);
    }
}
