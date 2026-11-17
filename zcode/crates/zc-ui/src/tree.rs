//! ファイルツリー（左 84px。モック 1）。ツリーの中身（開いているフォルダと選択）と
//! 描画の両方を持つ。
//!
//! - フォルダの中身は、開いたときに `std::fs::read_dir` で読む（1 フォルダぶんだけ。
//!   開いていないフォルダの中は読まない）
//! - 並びは「フォルダ → ファイル」、それぞれ名前順。`.` で始まるものは出さない
//! - 描画は項目ごとの差分（選択を動かしても、変わった 2 行だけ描き直す）

use crate::layout::{TREE_HEAD_H, TREE_ROW_H, UI_PX};
use crate::{RenderCx, Widget};
use std::path::{Path, PathBuf};
use zc_core::{palette, Color, Damage, Rect};
use zc_font::TextStyle;

/// 1 つのフォルダから読む項目の上限（巨大なフォルダで固まらないように）。
const MAX_ENTRIES: usize = 500;

/// ツリーに見えている 1 項目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub path: PathBuf,
    pub name: String,
    /// 根の直下が 0。
    pub depth: u16,
    pub is_dir: bool,
    /// フォルダを開いているか。
    pub expanded: bool,
}

/// フォルダの中身を読む（フォルダ → ファイル、名前順）。
pub(crate) fn list_dir(dir: &Path, depth: u16) -> Vec<TreeNode> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<TreeNode> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            // リンクはたどった先の種類で見る（切れたリンクはファイル扱い）
            let is_dir = std::fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false);
            Some(TreeNode { path: e.path(), name, depth, is_dir, expanded: false })
        })
        .take(MAX_ENTRIES)
        .collect();
    v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.name.cmp(&b.name)));
    v
}

/// 見出しに出すパス（ホームの下なら `~/code` の形）。
pub fn display_path(p: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if p.starts_with(&home) => {
            let rest = p.strip_prefix(&home).unwrap_or(p);
            if rest.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", rest.display())
            }
        }
        _ => p.display().to_string(),
    }
}

/// ファイルツリー。
pub struct FileTree {
    root: PathBuf,
    title: String,
    /// 見えている項目（開いたフォルダの中身を、その直後に並べた平らな列）。
    nodes: Vec<TreeNode>,
    sel: usize,
    /// いちばん上に見えている項目。
    scroll: usize,
    focused: bool,
    /// エディタで開いているファイル（入力がツリーに無いあいだ、これを目立たせる）。
    current: Option<PathBuf>,
    /// 描いてある項目の印（行ごと）。
    painted: Vec<u64>,
    dirty_all: bool,
    /// 見出し（EXPLORER の行）だけ描き直す。
    head_dirty: bool,
}

impl FileTree {
    /// `root` を根にして、その直下を読む。
    pub fn new(root: &Path) -> FileTree {
        let title = display_path(root);
        FileTree {
            root: root.to_path_buf(),
            title,
            nodes: list_dir(root, 0),
            sel: 0,
            scroll: 0,
            focused: false,
            current: None,
            painted: Vec::new(),
            dirty_all: true,
            head_dirty: false,
        }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// 見出しに出す根の名前（既定は `~/code` の形）。
    pub fn set_title(&mut self, title: &str) {
        if self.title != title {
            self.title = title.into();
            self.dirty_all = true;
        }
    }
    /// 見えている項目。
    pub fn nodes(&self) -> &[TreeNode] {
        &self.nodes
    }
    /// 選んでいる項目。
    pub fn selected(&self) -> Option<&TreeNode> {
        self.nodes.get(self.sel)
    }
    pub fn selected_index(&self) -> usize {
        self.sel
    }
    /// 入力がツリーに向いているか。
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }
    pub fn is_focused(&self) -> bool {
        self.focused
    }
    /// 選択を `delta` 項目ぶん動かす（端で止まる）。
    pub fn move_sel(&mut self, delta: i32) {
        if self.nodes.is_empty() {
            return;
        }
        self.sel = (self.sel as i64 + delta as i64).clamp(0, self.nodes.len() as i64 - 1) as usize;
    }
    /// 選んでいるフォルダを開く。
    pub fn expand_selected(&mut self) {
        let Some(n) = self.nodes.get_mut(self.sel) else { return };
        if !n.is_dir || n.expanded {
            return;
        }
        n.expanded = true;
        let children = list_dir(&n.path.clone(), n.depth + 1);
        let at = self.sel + 1;
        self.nodes.splice(at..at, children);
    }
    /// 選んでいるフォルダを閉じる。開いていない項目なら、親のフォルダへ選択を移す。
    pub fn collapse_selected(&mut self) {
        let Some(n) = self.nodes.get(self.sel) else { return };
        let depth = n.depth;
        if n.is_dir && n.expanded {
            let end = self.nodes[self.sel + 1..].iter().position(|c| c.depth <= depth).map_or(self.nodes.len(), |i| self.sel + 1 + i);
            self.nodes.drain(self.sel + 1..end);
            self.nodes[self.sel].expanded = false;
        } else if depth > 0 {
            if let Some(i) = self.nodes[..self.sel].iter().rposition(|c| c.depth < depth) {
                self.sel = i;
            }
        }
    }
    /// Enter: フォルダなら開閉し、ファイルならそのパスを返す。
    pub fn activate(&mut self) -> Option<PathBuf> {
        let n = self.nodes.get(self.sel)?;
        if n.is_dir {
            if n.expanded {
                self.collapse_selected();
            } else {
                self.expand_selected();
            }
            None
        } else {
            Some(n.path.clone())
        }
    }
    /// エディタで開いているファイルを知らせる。根の下にあれば、途中のフォルダを開いて
    /// その項目を選ぶ。
    pub fn set_current(&mut self, path: Option<&Path>) {
        self.current = path.map(Path::to_path_buf);
        let Some(path) = path else { return };
        let Ok(rel) = path.strip_prefix(&self.root) else { return };
        let mut cur = self.root.clone();
        let parts: Vec<_> = rel.components().collect();
        for (i, part) in parts.iter().enumerate() {
            cur.push(part);
            let Some(idx) = self.nodes.iter().position(|n| n.path == cur) else { return };
            self.sel = idx;
            if i + 1 < parts.len() {
                self.expand_selected();
            }
        }
    }
    /// 開いているフォルダを読み直す（保存で新しいファイルができたとき）。開いていた
    /// フォルダと選択は、同じパスがあれば保つ。
    pub fn refresh(&mut self) {
        let open: Vec<PathBuf> = self.nodes.iter().filter(|n| n.expanded).map(|n| n.path.clone()).collect();
        let sel = self.selected().map(|n| n.path.clone());
        self.nodes = list_dir(&self.root, 0);
        let mut i = 0;
        while i < self.nodes.len() {
            if self.nodes[i].is_dir && open.contains(&self.nodes[i].path) {
                self.nodes[i].expanded = true;
                let children = list_dir(&self.nodes[i].path.clone(), self.nodes[i].depth + 1);
                self.nodes.splice(i + 1..i + 1, children);
            }
            i += 1;
        }
        self.sel = sel.and_then(|p| self.nodes.iter().position(|n| n.path == p)).unwrap_or(self.sel.min(self.nodes.len().saturating_sub(1)));
    }

    fn row_sig(&self, idx: usize) -> u64 {
        let Some(n) = self.nodes.get(idx) else { return 1 };
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: &[u8]| {
            for &x in b {
                h = (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        eat(n.name.as_bytes());
        let state = self.row_state(idx, n);
        eat(&[0xff, n.depth as u8, n.is_dir as u8, n.expanded as u8, state]);
        h | 2
    }
    /// 0: ふつう、1: 開いているファイル / 選択（入力なし）、2: 選択（入力あり）。
    fn row_state(&self, idx: usize, n: &TreeNode) -> u8 {
        if self.focused {
            if idx == self.sel {
                2
            } else {
                0
            }
        } else if self.current.as_deref() == Some(n.path.as_path()) {
            1
        } else {
            0
        }
    }
}

/// 項目の文字色（モックと同じ: フォルダは地の文字色、`.py` は黄、ほかは薄い灰）。
fn name_color(n: &TreeNode) -> Color {
    if n.is_dir {
        palette::FG
    } else if n.name.ends_with(".py") {
        Color::hex(0xe5c07b)
    } else {
        Color::hex(0xcccccc)
    }
}

impl Widget for FileTree {
    fn render(&mut self, cx: &mut RenderCx<'_>, area: Rect) -> Damage {
        if area.is_empty() {
            return Damage::none();
        }
        let ts = TextStyle::mono(UI_PX);
        let inner_w = area.w.saturating_sub(1);
        let rows = (area.h.saturating_sub(TREE_HEAD_H) / TREE_ROW_H) as usize;
        // 選択が見える位置までずらす
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if rows > 0 && self.sel >= self.scroll + rows {
            self.scroll = self.sel + 1 - rows;
        }
        self.scroll = self.scroll.min(self.nodes.len().saturating_sub(rows.min(self.nodes.len())));
        let mut damage = Damage::none();
        let all = self.dirty_all || self.painted.len() != rows;
        if all {
            self.dirty_all = false;
            self.painted.clear();
            self.painted.resize(rows, 0);
            cx.canvas.fill_rect(area, palette::SIDE);
            cx.canvas.vline(area.right() - 1, area.y as i32, area.h, Color::hex(0x333333));
            damage = Damage::rect(area);
        }
        if all || std::mem::take(&mut self.head_dirty) {
            let head_row = Rect::new(area.x, area.y, inner_w, TREE_HEAD_H.min(area.h));
            if !all {
                cx.canvas.fill_rect(head_row, palette::SIDE);
                damage.add(head_row.rows(), zc_core::Priority::Normal);
            }
            let head = Rect::new(area.x + 4, area.y, inner_w.saturating_sub(4), TREE_HEAD_H);
            let mut title = String::from("EXPLORER: ");
            title.push_str(&self.title);
            cx.text_in(head, &title, &TextStyle::mono(7.0), palette::DIM, palette::SIDE);
        }
        let cell = cx.metrics(&ts).cell_w as i16;
        for slot in 0..rows {
            let idx = self.scroll + slot;
            let sig = self.row_sig(idx);
            if self.painted[slot] == sig {
                continue;
            }
            self.painted[slot] = sig;
            let r = Rect::new(area.x, area.y + (TREE_HEAD_H + slot as u16 * TREE_ROW_H) as i16, inner_w, TREE_ROW_H);
            damage.add(r.rows(), zc_core::Priority::Normal);
            let Some(n) = self.nodes.get(idx) else {
                cx.canvas.fill_rect(r, palette::SIDE);
                continue;
            };
            let state = self.row_state(idx, n);
            let bg = match state {
                2 => palette::LIST_FOCUS,
                1 => palette::LIST_CUR,
                _ => palette::SIDE,
            };
            cx.canvas.fill_rect(r, bg);
            if state == 2 {
                cx.canvas.stroke_rect(r, palette::STATUS);
            }
            let x = r.x + 4 + n.depth as i16 * 2 * cell;
            let text = Rect::new(x, r.y, (r.right() - x as i32 - 1).max(0) as u16, r.h);
            let mut label = String::with_capacity(n.name.len() + 4);
            if n.is_dir {
                label.push_str(if n.expanded { "▾ " } else { "▸ " });
            }
            label.push_str(&n.name);
            cx.text_in(text, &label, &ts, name_color(n), bg);
        }
        damage
    }
    fn invalidate(&mut self) {
        self.dirty_all = true;
    }
}

impl FileTree {
    /// 見出しの行だけ描き直させる（フォーカスの縁取りを消すとき）。
    pub fn invalidate_head(&mut self) {
        self.head_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use zc_core::{Canvas, RowRange, Theme};
    use zc_font::{FontConfig, Fonts, RasterMode};

    /// モック 1 と同じ並びのフォルダを作る。
    fn sample_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zc-ui-tree-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("blink")).unwrap();
        fs::create_dir_all(dir.join("sensors/imu")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        for f in ["blink/main.py", "blink/utils.py", "sensors/imu/read.py", "sensors/temp.py", "README.md", "notes.txt", ".hidden"] {
            fs::write(dir.join(f), "x").unwrap();
        }
        dir
    }

    fn names(t: &FileTree) -> Vec<String> {
        t.nodes().iter().map(|n| format!("{}{}", "  ".repeat(n.depth as usize), n.name)).collect()
    }

    #[test]
    fn listing_expand_collapse_and_activate() {
        let dir = sample_dir("model");
        let mut t = FileTree::new(&dir);
        // フォルダが先、名前順、隠しファイルは出さない
        assert_eq!(names(&t), ["blink", "sensors", "notes.txt", "README.md"]);
        assert_eq!(t.activate(), None); // blink を開く
        assert_eq!(names(&t), ["blink", "  main.py", "  utils.py", "sensors", "notes.txt", "README.md"]);
        t.move_sel(1);
        assert_eq!(t.activate(), Some(dir.join("blink/main.py")));
        // ファイルの上で「閉じる」は親へ移る。もう 1 回で親を閉じる
        t.collapse_selected();
        assert_eq!(t.selected().unwrap().name, "blink");
        t.collapse_selected();
        assert_eq!(names(&t), ["blink", "sensors", "notes.txt", "README.md"]);
        // 入れ子: sensors → imu
        t.move_sel(1);
        t.expand_selected();
        t.move_sel(1);
        t.expand_selected();
        assert_eq!(names(&t), ["blink", "sensors", "  imu", "    read.py", "  temp.py", "notes.txt", "README.md"]);
        // 親を閉じると、孫までまとめて消える
        t.move_sel(-1);
        t.collapse_selected();
        assert_eq!(names(&t), ["blink", "sensors", "notes.txt", "README.md"]);
        // 端で止まる
        t.move_sel(100);
        assert_eq!(t.selected().unwrap().name, "README.md");
        t.move_sel(-100);
        assert_eq!(t.selected_index(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_current_reveals_and_refresh_keeps_state() {
        let dir = sample_dir("reveal");
        let mut t = FileTree::new(&dir);
        t.set_current(Some(&dir.join("sensors/imu/read.py")));
        assert_eq!(names(&t), ["blink", "sensors", "  imu", "    read.py", "  temp.py", "notes.txt", "README.md"]);
        assert_eq!(t.selected().unwrap().name, "read.py");
        // 根の外のファイルでは何も変わらない
        t.set_current(Some(Path::new("/nonexistent/x.py")));
        assert_eq!(t.selected().unwrap().name, "read.py");
        // ファイルが増えた
        fs::write(dir.join("sensors/imu/calib.py"), "x").unwrap();
        t.refresh();
        assert_eq!(names(&t), ["blink", "sensors", "  imu", "    calib.py", "    read.py", "  temp.py", "notes.txt", "README.md"]);
        assert_eq!(t.selected().unwrap().name, "read.py");
        // 無いフォルダを根にしても落ちない
        let t = FileTree::new(Path::new("/nonexistent/zc"));
        assert!(t.nodes().is_empty() && t.selected().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_diffs_rows() {
        let cfg = FontConfig::default();
        if !cfg.available() {
            return;
        }
        let dir = sample_dir("render");
        let mut fonts = Fonts::load(&cfg, RasterMode::Inline).unwrap();
        let theme = Theme::dark_plus();
        let mut canvas = Canvas::new();
        let area = Rect::new(0, 13, 84, 146);
        let mut t = FileTree::new(&dir);
        t.set_focused(true);
        let d = t.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme), area);
        assert_eq!(d.bounds(), RowRange::new(13, 159));
        assert!(t.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme), area).is_empty());
        // 選択を 1 つ下へ: 2 行だけ
        t.move_sel(1);
        let d = t.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme), area);
        assert_eq!(d.bounds(), RowRange::new(23, 45));
        assert_eq!(canvas.get(40, 23 + 11 + 1), Some(palette::LIST_FOCUS));
        assert_eq!(canvas.get(0, 23 + 11), Some(palette::STATUS));
        // 右端の境界線と、領域の外
        assert_eq!(canvas.get(83, 100), Some(Color::hex(0x333333)));
        assert_eq!(canvas.get(84, 100), Some(Color::BLACK));
        // フォルダを開く: その行から下が変わる
        t.move_sel(-1);
        t.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme), area);
        t.expand_selected();
        let d = t.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme), area);
        assert_eq!(d.bounds(), RowRange::new(23, 23 + 6 * 11));
        // 入力が外れると、開いているファイルの行が目立つ
        t.set_focused(false);
        t.set_current(Some(&dir.join("blink/utils.py")));
        t.render(&mut RenderCx::new(&mut canvas, &mut fonts, &theme), area);
        assert_eq!(canvas.get(60, 23 + 2 * 11 + 1), Some(palette::LIST_CUR));
        assert_eq!(canvas.get(60, 23 + 1), Some(palette::SIDE));
        let _ = fs::remove_dir_all(&dir);
    }
}
