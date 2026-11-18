use super::*;
use zc_core::LineRange;

struct Doc(Vec<String>);

impl LineSource for Doc {
    fn len_lines(&self) -> u32 {
        self.0.len().max(1) as u32
    }
    fn line(&self, line: u32, out: &mut String) {
        out.clear();
        if let Some(s) = self.0.get(line as usize) {
            out.push_str(s);
        }
    }
}

fn doc(s: &str) -> Doc {
    Doc(s.split('\n').map(String::from).collect())
}

fn parse(d: &Doc) -> MdDocument {
    MdDocument::parse(d, &CancelToken::never()).unwrap().value
}

/// 行を「見える文字列」と「記号」に分けて、`[記号]文字` の形の文字列にする。
fn show(md: &MdDocument, d: &Doc, line: u32) -> String {
    let t = &d.0[line as usize];
    let l = md.line(line, t, None);
    let mut s = String::new();
    for r in &l.runs {
        let part = &t[r.range.start as usize..r.range.end as usize];
        if r.hidden {
            s.push('[');
            s.push_str(part);
            s.push(']');
        } else {
            s.push_str(part);
        }
    }
    s
}

fn run_of<'a>(l: &'a MdLine, t: &str, part: &str) -> &'a MdRun {
    let at = t.find(part).unwrap() as u32;
    l.runs.iter().find(|r| r.range.start <= at && at < r.range.end).unwrap()
}

/// 区間が行の全部を、前から順にすき間なく覆う。
fn assert_covers(l: &MdLine, t: &str) {
    let mut pos = 0u32;
    for r in &l.runs {
        assert_eq!(r.range.start, pos, "{t:?}: {:?}", l.runs);
        assert!(r.range.end > r.range.start, "{t:?}: 空の区間");
        assert!(t.is_char_boundary(r.range.end as usize), "{t:?}: 文字の途中");
        pos = r.range.end;
    }
    assert_eq!(pos, t.len() as u32, "{t:?}: {:?}", l.runs);
}

const SAMPLE: &str = "# 買い物と作業メモ\n- [x] 基板を発注する\n- [ ] ケースを **3D 印刷** する\n- [ ] `zcode` を実機で試す\n## メモ\n> CMA は **64MB** しかない。*カメラ*と同時に使うときは注意\n詳細は [development.md](../development.md) を参照\n```python\nimport zero_ui as ui\nui.font(12)\n```";

#[test]
fn mock_sample_blocks_and_markers() {
    let d = doc(SAMPLE);
    let md = parse(&d);
    let blocks: Vec<MdBlock> = (0..11).map(|i| md.line(i, &d.0[i as usize], None).block).collect();
    assert_eq!(blocks, vec![
        MdBlock::Heading(1),
        MdBlock::ListItem { depth: 1, checkbox: Some(true) },
        MdBlock::ListItem { depth: 1, checkbox: Some(false) },
        MdBlock::ListItem { depth: 1, checkbox: Some(false) },
        MdBlock::Heading(2),
        MdBlock::Quote,
        MdBlock::Paragraph,
        MdBlock::CodeFence,
        MdBlock::Code,
        MdBlock::Code,
        MdBlock::CodeFence,
    ]);
    assert_eq!(show(&md, &d, 0), "[# ]買い物と作業メモ");
    assert_eq!(show(&md, &d, 1), "[- [x] ]基板を発注する");
    assert_eq!(show(&md, &d, 2), "[- [ ] ]ケースを [**]3D 印刷[**] する");
    assert_eq!(show(&md, &d, 3), "[- [ ] `]zcode[`] を実機で試す");
    assert_eq!(show(&md, &d, 4), "[## ]メモ");
    assert_eq!(show(&md, &d, 5), "[> ]CMA は [**]64MB[**] しかない。[*]カメラ[*]と同時に使うときは注意");
    assert_eq!(show(&md, &d, 6), "詳細は [[]development.md[](../development.md)] を参照");
    assert_eq!(show(&md, &d, 7), "[```python]");
    assert_eq!(show(&md, &d, 8), "import zero_ui as ui");
    assert_eq!(show(&md, &d, 10), "[```]");
    for i in 0..11u32 {
        let t = &d.0[i as usize];
        assert_covers(&md.line(i, t, None), t);
        assert_covers(&md.line(i, t, Some(i)), t);
    }
}

#[test]
fn mock_sample_styles() {
    let d = doc(SAMPLE);
    let md = parse(&d);
    let line = |i: u32| md.line(i, &d.0[i as usize], None);
    // 見出し
    let l = line(0);
    let r = run_of(&l, &d.0[0], "買");
    assert_eq!((r.font, r.style.fg), (MdFont::Heading(1), colors::HEADING));
    assert!(r.style.attrs.contains(Attrs::BOLD));
    // チェック済み: 灰色の取り消し線
    let l = line(1);
    assert!(l.bullet);
    let r = run_of(&l, &d.0[1], "基板");
    assert_eq!(r.style.fg, colors::DONE);
    assert!(r.style.attrs.contains(Attrs::STRIKE));
    // 太字は白
    let l = line(2);
    let r = run_of(&l, &d.0[2], "3D");
    assert_eq!((r.font, r.style.fg), (MdFont::Body, colors::STRONG));
    assert!(r.style.attrs.contains(Attrs::BOLD));
    assert_eq!(run_of(&l, &d.0[2], "ケース").style, Style::new(palette::FG));
    // インラインコード
    let l = line(3);
    let r = run_of(&l, &d.0[3], "zcode");
    assert_eq!((r.font, r.style.fg, r.style.bg), (MdFont::Code, colors::CODE_FG, Some(colors::CODE_BG)));
    // 引用: 字は灰色、太字は白、斜体
    let l = line(5);
    assert_eq!(l.quote, 1);
    assert_eq!(run_of(&l, &d.0[5], "CMA").style.fg, colors::QUOTE);
    assert_eq!(run_of(&l, &d.0[5], "64MB").style.fg, colors::STRONG);
    assert!(run_of(&l, &d.0[5], "カメラ").style.attrs.contains(Attrs::ITALIC));
    // リンク
    let l = line(6);
    let r = run_of(&l, &d.0[6], "development.md");
    assert_eq!(r.style.fg, colors::LINK);
    assert!(r.style.attrs.contains(Attrs::UNDERLINE));
    assert!(run_of(&l, &d.0[6], "../dev").hidden);
    // コードブロック
    let l = line(7);
    assert!(l.collapse);
    let l = line(8);
    assert_eq!((l.runs.len(), l.runs[0].font, l.collapse), (1, MdFont::Code, false));
}

#[test]
fn cursor_line_shows_markers() {
    let d = doc(SAMPLE);
    let md = parse(&d);
    let t = &d.0[2];
    let hidden = md.line(2, t, None);
    let shown = md.line(2, t, Some(2));
    assert!(hidden.runs.iter().any(|r| r.hidden));
    assert!(shown.runs.iter().all(|r| !r.hidden));
    // 隠すかどうか以外は同じ
    assert_eq!(hidden.runs.len(), shown.runs.len());
    for (a, b) in hidden.runs.iter().zip(&shown.runs) {
        assert_eq!((a.range.clone(), a.font, a.style), (b.range.clone(), b.font, b.style));
    }
    // 記号は薄い等幅、太字は太字のまま
    let m = run_of(&shown, t, "**");
    assert_eq!((m.font, m.style.fg), (MdFont::Marker, colors::MARKER));
    assert!(run_of(&shown, t, "3D").style.attrs.contains(Attrs::BOLD));
    // ほかの行がカーソル行なら隠す
    assert_eq!(md.line(2, t, Some(3)), hidden);
}

#[test]
fn block_kinds() {
    let src = "Title\n===\n\n1. one\n2. two\n   - nested *it*\n\n> q1\n> - in quote\n\n    indented\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n<div>\nhtml\n</div>\n\n~~strike~~ \\* <b>x</b> &amp; ![img](a.png) <http://e.x>";
    let d = doc(src);
    let md = parse(&d);
    let line = |i: u32| md.line(i, &d.0[i as usize], None);
    // setext の見出し: 下線の行はつぶす
    assert_eq!(line(0).block, MdBlock::Heading(1));
    assert_eq!((line(1).block, line(1).collapse), (MdBlock::Heading(1), true));
    assert_eq!(show(&md, &d, 1), "[===]");
    assert_eq!(line(2).block, MdBlock::Blank);
    // 番号つき: 番号は隠さない
    let l = line(3);
    assert_eq!((l.block, l.bullet, l.indent), (MdBlock::ListItem { depth: 1, checkbox: None }, false, 1));
    assert_eq!(show(&md, &d, 3), "1.[ ]one");
    assert_eq!(l.runs[0].style.fg, colors::NUMBER);
    // 入れ子
    let l = line(5);
    assert_eq!((l.block, l.bullet, l.indent), (MdBlock::ListItem { depth: 2, checkbox: None }, true, 2));
    assert_eq!(show(&md, &d, 5), "[   - ]nested [*]it[*]");
    // 引用とその中の箇条書き
    assert_eq!((line(7).block, line(7).quote), (MdBlock::Quote, 1));
    let l = line(8);
    assert_eq!((l.block, l.quote, l.bullet), (MdBlock::ListItem { depth: 1, checkbox: None }, 1, true));
    // 字下げのコード
    assert_eq!(line(10).block, MdBlock::Code);
    assert_eq!(show(&md, &d, 10), "[    ]indented");
    // 水平線
    assert_eq!(line(12).block, MdBlock::Rule);
    assert_eq!(show(&md, &d, 12), "[---]");
    // 表はソースのまま
    for i in 14..17 {
        let l = line(i);
        assert_eq!((l.block, l.runs.len(), l.runs[0].font, l.runs[0].hidden), (MdBlock::Table, 1, MdFont::Code, false));
    }
    // HTML はそのまま
    assert_eq!(show(&md, &d, 18), "<div>");
    assert_eq!(show(&md, &d, 19), "html");
    // インライン
    assert_eq!(show(&md, &d, 22), "[~~]strike[~~] [\\]* <b>x</b> &amp; [![]img[](a.png)] [<]http://e.x[>]");
    let l = line(22);
    assert!(run_of(&l, &d.0[22], "strike").style.attrs.contains(Attrs::STRIKE));
    assert_eq!(run_of(&l, &d.0[22], "img").style.fg, colors::LINK);
    for i in 0..d.0.len() as u32 {
        assert_covers(&line(i), &d.0[i as usize]);
    }
}

#[test]
fn unclosed_and_long_fences() {
    let d = doc("````\n```\ncode\n```");
    let md = parse(&d);
    let blocks: Vec<MdBlock> = (0..4).map(|i| md.line(i, &d.0[i as usize], None).block).collect();
    // 開きより短い ``` は中身
    assert_eq!(blocks, vec![MdBlock::CodeFence, MdBlock::Code, MdBlock::Code, MdBlock::Code]);
    let d = doc("- item\n  ```\n  code\n  ```\n- next");
    let md = parse(&d);
    assert_eq!(md.line(1, &d.0[1], None).block, MdBlock::CodeFence);
    assert_eq!(md.line(2, &d.0[2], None).block, MdBlock::Code);
    assert_eq!(md.line(2, &d.0[2], None).indent, 1);
    assert_eq!(md.line(3, &d.0[3], None).block, MdBlock::CodeFence);
    assert_eq!(show(&md, &d, 2), "[  ]code");
}

#[test]
fn toggle_checkbox() {
    let d = doc("- [ ] a\n- [x] b\n  * [X] c\n- plain [ ] x\ntext [ ]\n> - [ ] q\n1. [ ] n");
    let md = parse(&d);
    let t = |i: u32| md.toggle_checkbox(i, &d.0[i as usize]);
    assert_eq!(t(0).as_deref(), Some("- [x] a"));
    assert_eq!(t(1).as_deref(), Some("- [ ] b"));
    assert_eq!(t(2).as_deref(), Some("  * [ ] c"));
    assert_eq!(t(3), None);
    assert_eq!(t(4), None);
    assert_eq!(t(5).as_deref(), Some("> - [x] q"));
    assert_eq!(t(6).as_deref(), Some("1. [x] n"));
    // 覚えている行と違う文字列でも、その場で解析して切り替える
    assert_eq!(md.toggle_checkbox(4, "- [ ] new").as_deref(), Some("- [x] new"));
    assert_eq!(md.toggle_checkbox(99, "- [x] far").as_deref(), Some("- [ ] far"));
}

#[test]
fn line_with_different_text_is_parsed_on_the_spot() {
    let d = doc(SAMPLE);
    let md = parse(&d);
    // 未確定文字列を挟んだ行: 記号の位置がずれても、正しく分ける
    let t = "- [ ] ケースを ▽てん**3D 印刷** する";
    let l = md.line(2, t, Some(2));
    assert_covers(&l, t);
    assert_eq!(l.block, MdBlock::ListItem { depth: 1, checkbox: Some(false) });
    assert!(run_of(&l, t, "3D").style.attrs.contains(Attrs::BOLD));
    assert_eq!(run_of(&l, t, "▽てん").style, Style::new(palette::FG));
    // コードブロックの中の行は、コードのまま
    let l = md.line(8, "import ▽ぜろ zero_ui", Some(8));
    assert_eq!((l.block, l.runs.len(), l.runs[0].font), (MdBlock::Code, 1, MdFont::Code));
    // 範囲外の行
    let l = md.line(500, "# far", None);
    assert_eq!(l.block, MdBlock::Heading(1));
}

#[test]
fn parse_cancel_and_replace() {
    let d = doc(SAMPLE);
    let c = zc_core::VersionCounter::new();
    let tok = c.token();
    c.bump();
    assert!(MdDocument::parse(&d, &tok).is_err());
    let tok = c.token();
    let parsed = MdDocument::parse(&d, &tok).unwrap();
    let mut md = MdDocument::new();
    assert!(!md.replace(Versioned::new(Version(99), MdDocument::new()), c.current()));
    assert!(md.replace(parsed, c.current()));
    assert_eq!(md.len_lines(), 11);
    assert_eq!(md.line(0, &d.0[0], None).block, MdBlock::Heading(1));
}

fn change(os: u32, oe: u32, ne: u32) -> LineChange {
    LineChange { old: LineRange::new(os, oe), new: LineRange::new(os, ne) }
}

#[test]
fn edited_inside_block_needs_no_reparse() {
    let mut d = doc(SAMPLE);
    let mut md = parse(&d);
    // 段落の中: その場で直る
    d.0[2] = "- [ ] ケースを *3D 印刷* する".into();
    assert!(!md.edited(&d, change(2, 3, 3)));
    assert_eq!(show(&md, &d, 2), "[- [ ] ]ケースを [*]3D 印刷[*] する");
    assert_eq!(md.lines, parse(&d).lines);
    // コードブロックの中
    d.0[8] = "import zero_ui".into();
    assert!(!md.edited(&d, change(8, 9, 9)));
    assert_eq!(md.lines, parse(&d).lines);
    // 行を足す
    d.0.insert(3, "- [ ] 新しい項目".into());
    assert!(!md.edited(&d, change(3, 3, 4)));
    assert_eq!(md.lines, parse(&d).lines);
    assert_eq!(md.line(3, &d.0[3], None).block, MdBlock::ListItem { depth: 1, checkbox: Some(false) });
    // 行を消す
    d.0.remove(5);
    assert!(!md.edited(&d, change(5, 6, 5)));
    assert_eq!(md.lines, parse(&d).lines);
    assert!(!md.needs_reparse());
}

#[test]
fn edited_fence_needs_reparse() {
    let mut d = doc("# t\n\ntext\n\nmore *text*\n\nend");
    let mut md = parse(&d);
    d.0[2] = "```".into();
    assert!(md.edited(&d, change(2, 3, 3)));
    assert!(md.needs_reparse());
    // その場の結果: 打った行は囲みになっている
    assert_eq!(md.line(2, &d.0[2], None).block, MdBlock::CodeFence);
    // 結果を受け取るまでは true のまま
    d.0[0] = "# tt".into();
    assert!(md.edited(&d, change(0, 1, 1)));
    let c = CancelToken::never();
    let p = MdDocument::parse(&d, &c).unwrap();
    assert!(md.replace(p, c.version()));
    assert!(!md.needs_reparse());
    assert_eq!(md.line(4, &d.0[4], None).block, MdBlock::Code);
    // 囲みを閉じる: 文書の終わりまで解析し直すので、その場で直る
    d.0[3] = "```".into();
    assert!(!md.edited(&d, change(3, 4, 4)));
    assert_eq!(md.line(4, &d.0[4], None).block, MdBlock::Paragraph);
    assert_eq!(md.lines, parse(&d).lines);
}

#[test]
fn edited_with_reference_definitions_always_asks() {
    let mut d = doc("see [foo]\n\n[foo]: http://example.com\n\nend");
    let mut md = parse(&d);
    assert_eq!(show(&md, &d, 0), "see [[]foo[]]");
    d.0[4] = "end [foo]".into();
    assert!(md.edited(&d, change(4, 5, 5)));
}

#[test]
fn edited_over_limit_falls_back_to_single_line() {
    let mut d = Doc((0..200).map(|i| format!("- item {i} with **bold** text")).collect());
    let mut md = parse(&d);
    md.set_region_limit(256);
    d.0[100] = "- changed *item*".into();
    assert!(md.edited(&d, change(100, 101, 101)));
    assert_eq!(show(&md, &d, 100), "[- ]changed [*]item[*]");
    assert_eq!(show(&md, &d, 101), "[- ]item 101 with [**]bold[**] text");
}

#[test]
fn edited_with_bad_ranges_resyncs() {
    let mut d = doc(SAMPLE);
    let mut md = parse(&d);
    d.0.push("x".into());
    d.0.push("# y".into());
    // 行数の合わない通知
    assert!(md.edited(&d, change(40, 41, 41)));
    assert_eq!(md.len_lines(), 13);
    assert_eq!(md.line(12, &d.0[12], None).block, MdBlock::Heading(1));
    assert_eq!(md.line(0, &d.0[0], None).block, MdBlock::Heading(1));
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const FRAGMENTS: &[&str] = &[
    "",
    "",
    "",
    "plain text",
    "text with **bold** and *it* and `code`",
    "日本語の **太字** と [リンク](a.md)",
    "# heading",
    "## 見出し *2*",
    "- item",
    "- [ ] task",
    "- [x] done **b**",
    "  - nested",
    "  continuation",
    "1. first",
    "2) second",
    "> quote",
    "> - quoted item",
    ">",
    "```",
    "```rust",
    "~~~",
    "    indented code",
    "---",
    "===",
    "***",
    "| a | b |",
    "|---|---|",
    "<div>",
    "</div>",
    "<!-- comment",
    "-->",
    "[ref]: http://x",
    "see [ref] and [other][ref]",
    "\\*escaped\\* &amp; <b>inline</b>",
    "* * *",
    "+ plus item",
    "trailing spaces  ",
    "\ttab",
    "![img](x.png) <http://auto.link>",
    "**unclosed *nest",
    "`` double ` tick ``",
    "$x$ ~~del~~ ^sup^",
    "[^1]: footnote",
    "Term\n: def",
];

fn random_line(r: &mut Rng) -> String {
    let f = FRAGMENTS[r.below(FRAGMENTS.len())];
    f.split('\n').next().unwrap().to_string()
}

/// どの行も、区間が行の全部を覆い、落ちない。
fn check_all(md: &MdDocument, d: &Doc) {
    for (i, t) in d.0.iter().enumerate() {
        let i = i as u32;
        assert_covers(&md.line(i, t, None), t);
        let l = md.line(i, t, Some(i));
        assert_covers(&l, t);
        assert!(l.runs.iter().all(|r| !r.hidden));
        let _ = md.toggle_checkbox(i, t);
    }
}

#[test]
fn edited_agrees_with_full_parse() {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut quiet, mut total) = (0u32, 0u32);
    for round in 0..1000 {
        let n = 1 + r.below(30);
        let mut d = Doc((0..n).map(|_| random_line(&mut r)).collect());
        let mut md = parse(&d);
        for step in 0..12 {
            let len = d.0.len();
            let at = r.below(len);
            let c = match r.below(6) {
                // 1 行を置き換える
                0 | 1 => {
                    d.0[at] = random_line(&mut r);
                    change(at as u32, at as u32 + 1, at as u32 + 1)
                }
                // 1 字足す・消す
                2 => {
                    let t = &mut d.0[at];
                    if r.below(2) == 0 || t.is_empty() {
                        let ch = ['a', ' ', '*', '`', '#', '-', '>', '[', ']', '日', '~', '|'][r.below(12)];
                        let cols: Vec<usize> = t.char_indices().map(|(i, _)| i).chain([t.len()]).collect();
                        t.insert(cols[r.below(cols.len())], ch);
                    } else {
                        let cols: Vec<usize> = t.char_indices().map(|(i, _)| i).collect();
                        t.remove(cols[r.below(cols.len())]);
                    }
                    change(at as u32, at as u32 + 1, at as u32 + 1)
                }
                // 行を足す
                3 => {
                    let k = 1 + r.below(3);
                    for _ in 0..k {
                        d.0.insert(at, random_line(&mut r));
                    }
                    change(at as u32, at as u32, (at + k) as u32)
                }
                // 行を消す（最後の 1 行は残す）
                4 if len > 1 => {
                    let k = (1 + r.below(3)).min(len - at).min(len - 1);
                    d.0.drain(at..at + k);
                    change(at as u32, (at + k) as u32, at as u32)
                }
                // 行を分ける（Enter）
                _ => {
                    let t = d.0[at].clone();
                    let cols: Vec<usize> = t.char_indices().map(|(i, _)| i).chain([t.len()]).collect();
                    let b = cols[r.below(cols.len())];
                    d.0[at] = t[..b].to_string();
                    d.0.insert(at + 1, t[b..].to_string());
                    change(at as u32, at as u32 + 1, at as u32 + 2)
                }
            };
            let need = md.edited(&d, c);
            assert_eq!(md.len_lines() as usize, d.0.len());
            check_all(&md, &d);
            total += 1;
            if need {
                // 全体を解析し直した結果を受け取る
                md = parse(&d);
            } else {
                quiet += 1;
                let fresh = parse(&d);
                if md.lines != fresh.lines {
                    let i = md.lines.iter().zip(&fresh.lines).position(|(a, b)| a != b).unwrap();
                    panic!(
                        "round {round} step {step}: 行 {i} が全体の解析と違う\n変更 {c:?}\n文書:\n{}\nその場: {:?}\n全体:   {:?}",
                        d.0.join("\n"),
                        md.lines[i],
                        fresh.lines[i]
                    );
                }
            }
        }
    }
    // 大半の編集は、全体の再解析なしで済む
    assert!(quiet * 2 > total, "再解析なしで済んだのは {quiet}/{total}");
}

#[test]
fn never_panics_on_odd_input() {
    let edge = [
        "",
        "\n\n\n",
        "#",
        "# ",
        "```",
        "```\n",
        "~~~\n~~~",
        ">",
        "> > > >",
        "- ",
        "- [ ]",
        "- [x]",
        "-\n-\n-",
        "1.",
        "***",
        "* * *\n- - -\n_ _ _",
        "[",
        "](",
        "[]()",
        "![",
        "`",
        "``",
        "**",
        "\\",
        "&",
        "<",
        "<!--",
        "|",
        "|\n|-",
        "| a |\n|---|",
        "\t",
        "    ",
        "a\r\nb\r\n",
        "\u{0}\u{1}\u{7f}",
        "\u{feff}# bom",
        "日本語\n===\n\n本文",
        "e\u{301}\u{200d}👨‍👩‍👧",
        "- a\n\n  b\n\n      c\n- d",
        "> ```\n> code\n> ```",
        "[a]: <b> 'c'\n[a]",
        "<details>\n\n**x**\n\n</details>",
    ];
    for s in edge {
        let d = doc(s);
        let md = parse(&d);
        assert_eq!(md.len_lines() as usize, d.0.len());
        check_all(&md, &d);
    }
    // でたらめな文字の並び
    let alphabet: Vec<char> = "#*_`~>-+[]()!|\\<>&: \t\nab1.=日本\"'$^".chars().collect();
    let mut r = Rng(0x1234_5678_9abc_def1);
    for _ in 0..2000 {
        let n = r.below(120);
        let s: String = (0..n).map(|_| alphabet[r.below(alphabet.len())]).collect();
        let d = doc(&s);
        let md = parse(&d);
        assert_eq!(md.len_lines() as usize, d.0.len());
        check_all(&md, &d);
        // 文書と違う文字列・違う行番号でも落ちない
        let other: String = (0..r.below(40)).map(|_| alphabet[r.below(alphabet.len())]).filter(|c| *c != '\n').collect();
        for i in 0..d.0.len() as u32 + 2 {
            assert_covers(&md.line(i, &other, Some(i)), &other);
        }
    }
}

#[test]
fn windowed_parse_agrees_with_whole_parse() {
    let mut r = Rng(0x1234_5678_9abc_def1);
    let never = CancelToken::never();
    let (mut windowed, mut total) = (0u32, 0u32);
    for _ in 0..1500 {
        let n = 20 + r.below(260);
        // 参照リンクの定義を含む行は、窓に分けない決まりなので、ここでは避ける
        let lines: Vec<String> = (0..n)
            .map(|_| loop {
                let l = random_line(&mut r);
                if !l.contains("]:") {
                    break l;
                }
            })
            .collect();
        let d = Doc(lines);
        let whole = parse_whole(&d, d.len_lines(), &never).unwrap();
        let window = 2 + r.below(14) as u32;
        total += 1;
        if let Some(w) = parse_windowed(&d, d.len_lines(), window, &never).unwrap() {
            windowed += 1;
            assert_eq!(w.len(), whole.len());
            for (i, (a, b)) in w.iter().zip(&whole).enumerate() {
                assert_eq!(a, b, "window={window} line {i}: {:?}", d.0);
            }
        }
    }
    assert!(windowed * 10 > total * 8, "窓に分けられた文書が少なすぎる: {windowed}/{total}");
}

#[test]
fn windowed_parse_falls_back_with_reference_definitions() {
    let mut lines: Vec<String> = (0..100).map(|i| format!("line {i} [ref]")).collect();
    lines.push("[ref]: http://x".into());
    let d = Doc(lines);
    assert!(parse_windowed(&d, d.len_lines(), 4, &CancelToken::never()).unwrap().is_none());
}

#[test]
fn windowed_parse_handles_blocks_longer_than_a_window() {
    let mut lines = vec!["intro".to_string(), String::new(), "```".to_string()];
    lines.extend((0..200).map(|i| format!("code {i}")));
    lines.push("```".into());
    lines.push(String::new());
    lines.extend((0..50).map(|i| format!("# h{i}")));
    let d = Doc(lines);
    let never = CancelToken::never();
    let whole = parse_whole(&d, d.len_lines(), &never).unwrap();
    let w = parse_windowed(&d, d.len_lines(), 8, &never).unwrap().expect("窓に分ける");
    assert_eq!(w, whole);
}
