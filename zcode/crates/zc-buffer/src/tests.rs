use super::*;

fn p(l: u32, c: u32) -> Pos {
    Pos::new(l, c)
}

fn lines(b: &Buffer) -> Vec<String> {
    let mut out = Vec::new();
    let mut s = String::new();
    for i in 0..b.len_lines() {
        b.line(i, &mut s);
        out.push(s.clone());
    }
    out
}

fn saved(b: &Buffer) -> Vec<u8> {
    let mut v = Vec::new();
    b.write_to(&mut v).unwrap();
    v
}

#[test]
fn selection_helpers() {
    let s = Selection { anchor: Pos::new(3, 4), head: Pos::new(1, 9) };
    assert_eq!(s.start(), Pos::new(1, 9));
    assert_eq!(s.end(), Pos::new(3, 4));
    assert!(Selection::caret(Pos::new(2, 2)).is_empty());
    let b = Buffer::new();
    assert_eq!(b.cursor(), Pos::default());
    assert!(!b.token().is_stale());
    assert_eq!(b.len_lines(), 1);
}

#[test]
fn open_and_line_source() {
    let b = Buffer::from_text("abc\n日本語\n\nlast");
    assert_eq!(lines(&b), ["abc", "日本語", "", "last"]);
    assert_eq!(b.line_len(1), 3);
    assert_eq!(b.line_len(99), 0);
    assert_eq!(b.len_chars(), 4 + 4 + 1 + 4);
    // 末尾の改行は最後の空行として残る
    let b = Buffer::from_text("a\n");
    assert_eq!(b.len_lines(), 2);
    assert_eq!(saved(&b), b"a\n");
    // 範囲外の行は空
    let mut s = String::from("x");
    b.line(50, &mut s);
    assert_eq!(s, "");
}

#[test]
fn crlf_detected_and_preserved() {
    let b = Buffer::from_bytes(b"a\r\nb\r\nc").unwrap();
    assert_eq!(b.line_ending(), LineEnding::CrLf);
    assert_eq!(lines(&b), ["a", "b", "c"]);
    assert_eq!(saved(&b), b"a\r\nb\r\nc");
    // 末尾の改行つき
    let b = Buffer::from_bytes("あ\r\nい\r\n".as_bytes()).unwrap();
    assert_eq!(b.len_lines(), 3);
    assert_eq!(saved(&b), "あ\r\nい\r\n".as_bytes());
    // 編集後も CRLF で保存される
    let mut b = Buffer::from_bytes(b"x\r\ny").unwrap();
    b.set_selection(Selection::caret(p(0, 1)));
    b.insert("Z\nQ");
    assert_eq!(saved(&b), b"xZ\r\nQ\r\ny");
    // LF のファイル
    let b = Buffer::from_bytes(b"a\nb\n").unwrap();
    assert_eq!(b.line_ending(), LineEnding::Lf);
    assert_eq!(saved(&b), b"a\nb\n");
    // 混在は多い方
    let b = Buffer::from_bytes(b"a\nb\nc\r\nd").unwrap();
    assert_eq!(b.line_ending(), LineEnding::Lf);
    assert_eq!(saved(&b), b"a\nb\nc\nd");
    // 貼り付けの CRLF / CR は LF に
    let mut b = Buffer::new();
    b.insert("1\r\n2\r3");
    assert_eq!(lines(&b), ["1", "2", "3"]);
    // 保存を CRLF に切り替えられる
    let mut b = Buffer::from_text("a\nb");
    b.set_line_ending(LineEnding::CrLf);
    assert_eq!(saved(&b), b"a\r\nb");
}

#[test]
fn invalid_utf8_is_flagged_not_panicking() {
    let bad = [b'a', 0xff, 0xfe, b'b'];
    let e = Buffer::from_bytes(&bad).err().unwrap();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    let (b, lossy) = Buffer::from_bytes_lossy(&bad);
    assert!(lossy);
    assert_eq!(b.to_text(), "a\u{fffd}\u{fffd}b");
    let (b, lossy) = Buffer::from_bytes_lossy("ok".as_bytes());
    assert!(!lossy);
    assert_eq!(b.to_text(), "ok");
}

#[test]
fn bom_is_kept() {
    let b = Buffer::from_bytes(b"\xEF\xBB\xBFhi\n").unwrap();
    assert_eq!(b.to_text(), "hi\n");
    assert_eq!(saved(&b), b"\xEF\xBB\xBFhi\n");
}

#[test]
fn load_from_file_and_cancel() {
    let dir = std::env::temp_dir().join(format!("zc-buffer-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.txt");
    let big: String = "日本語の行です。\n".repeat(20000);
    std::fs::write(&path, &big).unwrap();
    let vc = VersionCounter::new();
    let b = Buffer::load(&path, &vc.token()).unwrap();
    assert_eq!(b.len_lines(), 20001);
    assert_eq!(b.to_text(), big);
    // 古くなった依頼は途中でやめる
    let tok = vc.token();
    vc.bump();
    let e = Buffer::load(&path, &tok).err().unwrap();
    assert_eq!(e.kind(), io::ErrorKind::Interrupted);
    // UTF-8 でないファイル
    std::fs::write(&path, [0x80u8, 0x81]).unwrap();
    assert_eq!(Buffer::load(&path, &vc.token()).err().unwrap().kind(), io::ErrorKind::InvalidData);
    assert!(Buffer::load(&dir.join("none"), &vc.token()).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn insert_and_change_ranges() {
    let mut b = Buffer::from_text("ab\ncd\nef");
    b.set_selection(Selection::caret(p(1, 1)));
    // 1 行内の入力: 1 行だけ変わる
    let ch = b.insert("あ").unwrap();
    assert_eq!(ch.old, LineRange::new(1, 2));
    assert_eq!(ch.new, LineRange::new(1, 2));
    assert_eq!(b.cursor(), p(1, 2));
    assert_eq!(lines(&b), ["ab", "cあd", "ef"]);
    // 改行を入れる: 1 行が 2 行に
    let ch = b.insert("\n").unwrap();
    assert_eq!(ch.old, LineRange::new(1, 2));
    assert_eq!(ch.new, LineRange::new(1, 3));
    assert_eq!(ch.delta(), 1);
    assert_eq!(b.cursor(), p(2, 0));
    // 改行を消す: 2 行が 1 行に
    let ch = b.delete_backward().unwrap();
    assert_eq!(ch.old, LineRange::new(1, 3));
    assert_eq!(ch.new, LineRange::new(1, 2));
    assert_eq!(b.cursor(), p(1, 2));
    // 空の挿入・文頭の BackSpace・文末の Delete は何もしない
    assert!(b.insert("").is_none());
    b.set_selection(Selection::caret(p(0, 0)));
    assert!(b.delete_backward().is_none());
    b.set_selection(Selection::caret(p(2, 2)));
    assert!(b.delete_forward().is_none());
    // 複数行の選択を置き換える
    b.set_selection(Selection { anchor: p(0, 1), head: p(2, 1) });
    let ch = b.insert("X").unwrap();
    assert_eq!(ch.old, LineRange::new(0, 3));
    assert_eq!(ch.new, LineRange::new(0, 1));
    assert_eq!(lines(&b), ["aXf"]);
}

#[test]
fn version_advances_only_on_change() {
    let mut b = Buffer::new();
    let v0 = b.version();
    let tok = b.token();
    b.insert("a");
    assert!(b.version() > v0);
    assert!(tok.is_stale());
    let v1 = b.version();
    b.delete_forward();
    assert_eq!(b.version(), v1);
    let snap = b.snapshot();
    assert_eq!(snap.version(), v1);
    b.insert("b");
    // 写しは編集の影響を受けない
    assert_eq!(snap.len_chars(), 1);
    assert_eq!(snap.line_len(0), 1);
    assert_eq!(b.len_chars(), 2);
    let mut s = String::new();
    snap.line(0, &mut s);
    assert_eq!(s, "a");
    assert_eq!(snap.len_lines(), 1);
}

#[test]
fn snapshot_is_send_and_works_in_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<Snapshot>();
    let mut b = Buffer::from_text("hello world\nhello");
    let snap = b.snapshot();
    let h = std::thread::spawn(move || snap.find("hello", p(0, 1), false));
    b.insert("zzz");
    assert_eq!(h.join().unwrap(), Some(Selection { anchor: p(1, 0), head: p(1, 5) }));
}

#[test]
fn backspace_and_delete_japanese() {
    let mut b = Buffer::from_text("日本語🙂x");
    b.set_selection(Selection::caret(p(0, 4)));
    b.delete_backward();
    assert_eq!(b.to_text(), "日本語x");
    b.delete_backward();
    assert_eq!(b.to_text(), "日本x");
    b.set_selection(Selection::caret(p(0, 0)));
    b.delete_forward();
    assert_eq!(b.to_text(), "本x");
    assert_eq!(b.cursor(), p(0, 0));
}

#[test]
fn newline_auto_indent() {
    let mut b = Buffer::from_text("    foo");
    b.set_selection(Selection::caret(p(0, 7)));
    b.newline("    ", ":");
    assert_eq!(lines(&b), ["    foo", "    "]);
    assert_eq!(b.cursor(), p(1, 4));
    // 行末が ':' なら 1 段足す
    let mut b = Buffer::from_text("  if x:");
    b.set_selection(Selection::caret(p(0, 7)));
    b.newline("    ", ":{([");
    assert_eq!(lines(&b), ["  if x:", "      "]);
    assert_eq!(b.cursor(), p(1, 6));
    // タブの字下げを写す。カーソルより右の字下げは写さない（行の途中で切る）
    let mut b = Buffer::from_text("\t\tab");
    b.set_selection(Selection::caret(p(0, 3)));
    b.newline("\t", "");
    assert_eq!(lines(&b), ["\t\ta", "\t\tb"]);
    b.set_selection(Selection::caret(p(1, 1)));
    b.newline("\t", "");
    assert_eq!(lines(&b), ["\t\ta", "\t", "\t\tb"]);
    // 選択は置き換える
    let mut b = Buffer::from_text("  abcd");
    b.set_selection(Selection { anchor: p(0, 3), head: p(0, 5) });
    b.newline("  ", "");
    assert_eq!(lines(&b), ["  a", "  d"]);
    b.undo();
    assert_eq!(b.to_text(), "  abcd");
}

#[test]
fn undo_groups_typing() {
    let mut b = Buffer::new();
    for c in "hello".chars() {
        b.insert(&c.to_string());
    }
    b.insert("\n");
    for c in "世界".chars() {
        b.insert(&c.to_string());
    }
    assert_eq!(b.to_text(), "hello\n世界");
    // 改行を挟むので 3 単位: 世界 / 改行 / hello
    b.undo();
    assert_eq!(b.to_text(), "hello\n");
    assert_eq!(b.cursor(), p(1, 0));
    b.undo();
    assert_eq!(b.to_text(), "hello");
    b.undo();
    assert_eq!(b.to_text(), "");
    assert!(b.undo().is_none());
    b.redo();
    assert_eq!(b.to_text(), "hello");
    assert_eq!(b.cursor(), p(0, 5));
    b.redo();
    b.redo();
    assert_eq!(b.to_text(), "hello\n世界");
    assert!(b.redo().is_none());
}

#[test]
fn undo_group_breaks_on_cursor_jump() {
    let mut b = Buffer::new();
    b.insert("a");
    b.insert("b");
    b.move_cursor(Motion::Left, false, 10);
    b.insert("X");
    assert_eq!(b.to_text(), "aXb");
    b.undo();
    assert_eq!(b.to_text(), "ab");
    b.undo();
    assert_eq!(b.to_text(), "");
    // set_selection でも切れる
    b.redo();
    b.set_selection(Selection::caret(p(0, 2)));
    b.insert("c");
    b.undo();
    assert_eq!(b.to_text(), "ab");
    // break_undo_group で明示的に切る
    let mut b = Buffer::new();
    b.insert("a");
    b.break_undo_group();
    b.insert("b");
    b.undo();
    assert_eq!(b.to_text(), "a");
}

#[test]
fn undo_groups_backspace_and_delete() {
    let mut b = Buffer::from_text("abcdef");
    b.set_selection(Selection::caret(p(0, 6)));
    b.delete_backward();
    b.delete_backward();
    b.delete_backward();
    assert_eq!(b.to_text(), "abc");
    b.undo();
    assert_eq!(b.to_text(), "abcdef");
    assert_eq!(b.cursor(), p(0, 6));
    b.redo();
    assert_eq!(b.to_text(), "abc");
    // Delete の連続
    let mut b = Buffer::from_text("abcdef");
    b.delete_forward();
    b.delete_forward();
    assert_eq!(b.to_text(), "cdef");
    b.undo();
    assert_eq!(b.to_text(), "abcdef");
    assert_eq!(b.cursor(), p(0, 0));
    // 選択の置き換えは 1 単位で、選択が戻る
    let mut b = Buffer::from_text("hello world");
    b.set_selection(Selection { anchor: p(0, 0), head: p(0, 5) });
    b.insert("bye");
    assert_eq!(b.to_text(), "bye world");
    b.undo();
    assert_eq!(b.to_text(), "hello world");
    assert_eq!(b.selection(), Selection { anchor: p(0, 0), head: p(0, 5) });
}

#[test]
fn new_edit_clears_redo() {
    let mut b = Buffer::new();
    b.insert("a");
    b.break_undo_group();
    b.insert("b");
    b.undo();
    assert!(b.can_redo());
    b.insert("c");
    assert!(!b.can_redo());
    assert_eq!(b.to_text(), "ac");
}

#[test]
fn undo_returns_line_change() {
    let mut b = Buffer::from_text("a\nb\nc");
    b.set_selection(Selection { anchor: p(0, 1), head: p(2, 0) });
    b.insert("");
    assert_eq!(b.to_text(), "ac");
    let ch = b.undo().unwrap();
    assert_eq!(ch.old, LineRange::new(0, 1));
    assert_eq!(ch.new, LineRange::new(0, 3));
    let ch = b.redo().unwrap();
    assert_eq!(ch.old, LineRange::new(0, 3));
    assert_eq!(ch.new, LineRange::new(0, 1));
}

#[test]
fn modified_flag_follows_undo() {
    let mut b = Buffer::from_text("x");
    assert!(!b.is_modified());
    b.insert("a");
    assert!(b.is_modified());
    b.mark_saved();
    assert!(!b.is_modified());
    // 保存のあとに同じ種類の入力を続けても、保存時点の単位は変えない
    b.insert("b");
    assert!(b.is_modified());
    b.undo();
    assert!(!b.is_modified());
    assert_eq!(b.to_text(), "ax");
    b.undo();
    assert!(b.is_modified());
    b.redo();
    assert!(!b.is_modified());
}

#[test]
fn undo_history_is_capped() {
    let mut b = Buffer::new();
    for _ in 0..(UNDO_LIMIT + 50) {
        b.insert("a");
        b.break_undo_group();
    }
    assert_eq!(b.undo.len(), UNDO_LIMIT);
    for _ in 0..UNDO_LIMIT {
        b.undo();
    }
    assert!(b.undo().is_none());
    assert_eq!(b.len_chars(), 50);
}

#[test]
fn motions_basic() {
    let mut b = Buffer::from_text("ab\n\ncdef");
    b.move_cursor(Motion::Right, false, 5);
    assert_eq!(b.cursor(), p(0, 1));
    b.move_cursor(Motion::Right, false, 5);
    b.move_cursor(Motion::Right, false, 5);
    assert_eq!(b.cursor(), p(1, 0));
    b.move_cursor(Motion::Left, false, 5);
    assert_eq!(b.cursor(), p(0, 2));
    b.move_cursor(Motion::DocEnd, false, 5);
    assert_eq!(b.cursor(), p(2, 4));
    b.move_cursor(Motion::Right, false, 5);
    assert_eq!(b.cursor(), p(2, 4));
    b.move_cursor(Motion::DocStart, false, 5);
    assert_eq!(b.cursor(), p(0, 0));
    b.move_cursor(Motion::Left, false, 5);
    assert_eq!(b.cursor(), p(0, 0));
    b.move_cursor(Motion::LineEnd, false, 5);
    assert_eq!(b.cursor(), p(0, 2));
}

#[test]
fn vertical_motion_keeps_goal_column() {
    let mut b = Buffer::from_text("abcdef\n\nabcdef");
    b.set_selection(Selection::caret(p(0, 5)));
    b.move_cursor(Motion::Down, false, 5);
    assert_eq!(b.cursor(), p(1, 0));
    b.move_cursor(Motion::Down, false, 5);
    assert_eq!(b.cursor(), p(2, 5));
    // 端の行
    b.move_cursor(Motion::Down, false, 5);
    assert_eq!(b.cursor(), p(2, 6));
    b.move_cursor(Motion::Up, false, 5);
    b.move_cursor(Motion::Up, false, 5);
    b.move_cursor(Motion::Up, false, 5);
    assert_eq!(b.cursor(), p(0, 0));
}

#[test]
fn vertical_motion_with_wide_chars_and_tabs() {
    // 全角 = 2 桁。2 行目の 3 文字目の手前は表示桁 4
    let mut b = Buffer::from_text("日本語です\nabcdefgh");
    b.set_selection(Selection::caret(p(0, 2)));
    assert_eq!(b.display_col(0, 2), 4);
    b.move_cursor(Motion::Down, false, 5);
    assert_eq!(b.cursor(), p(1, 4));
    b.move_cursor(Motion::Up, false, 5);
    assert_eq!(b.cursor(), p(0, 2));
    // 全角の真ん中の桁は手前の文字へ
    b.set_selection(Selection::caret(p(1, 5)));
    b.move_cursor(Motion::Up, false, 5);
    assert_eq!(b.cursor(), p(0, 2));
    // タブ
    let mut b = Buffer::from_text("\tx\n12345678");
    assert_eq!(b.display_col(0, 1), 4);
    assert_eq!(b.display_col(0, 2), 5);
    assert_eq!(b.col_at_display(0, 4), 1);
    assert_eq!(b.col_at_display(0, 2), 0);
    b.set_selection(Selection::caret(p(0, 2)));
    b.move_cursor(Motion::Down, false, 5);
    assert_eq!(b.cursor(), p(1, 5));
    b.set_tab_width(8);
    assert_eq!(b.display_col(0, 1), 8);
    // 桁 ↔ 文字位置は往復できる
    let b = Buffer::from_text("a\tb日c\td");
    for col in 0..=7 {
        let d = b.display_col(0, col);
        assert_eq!(b.col_at_display(0, d), col, "col {col}");
    }
}

#[test]
fn page_motion_and_selection_extend() {
    let text: String = (0..30).map(|i| format!("line{i}\n")).collect();
    let mut b = Buffer::from_text(&text);
    b.move_cursor(Motion::PageDown, false, 10);
    assert_eq!(b.cursor(), p(10, 0));
    b.move_cursor(Motion::PageDown, true, 10);
    assert_eq!(b.selection(), Selection { anchor: p(10, 0), head: p(20, 0) });
    b.move_cursor(Motion::PageUp, false, 10);
    assert_eq!(b.cursor(), p(10, 0));
    b.move_cursor(Motion::PageUp, false, 100);
    assert_eq!(b.cursor(), p(0, 0));
    // Shift+Right で選択を伸ばし、Right で端に寄せる
    b.set_selection(Selection::caret(p(0, 1)));
    b.move_cursor(Motion::Right, true, 1);
    b.move_cursor(Motion::Right, true, 1);
    assert_eq!(b.selection(), Selection { anchor: p(0, 1), head: p(0, 3) });
    b.move_cursor(Motion::Left, false, 1);
    assert_eq!(b.selection(), Selection::caret(p(0, 1)));
    b.select_all();
    assert_eq!(b.selection().start(), p(0, 0));
    assert_eq!(b.selection().end(), p(30, 0));
    b.move_cursor(Motion::Right, false, 1);
    assert_eq!(b.cursor(), p(30, 0));
}

#[test]
fn word_motions_with_japanese() {
    let mut b = Buffer::from_text("foo_bar  baz.qux(x)");
    b.move_cursor(Motion::WordRight, false, 1);
    assert_eq!(b.cursor(), p(0, 7));
    b.move_cursor(Motion::WordRight, false, 1);
    assert_eq!(b.cursor(), p(0, 12));
    b.move_cursor(Motion::WordRight, false, 1);
    assert_eq!(b.cursor(), p(0, 13));
    b.move_cursor(Motion::WordLeft, false, 1);
    assert_eq!(b.cursor(), p(0, 12));
    b.move_cursor(Motion::WordLeft, false, 1);
    assert_eq!(b.cursor(), p(0, 9));
    b.move_cursor(Motion::WordLeft, false, 1);
    assert_eq!(b.cursor(), p(0, 0));
    // 日本語は文字種の境目で止まる
    let mut b = Buffer::from_text("今日はテストabc");
    let mut stops = Vec::new();
    for _ in 0..4 {
        b.move_cursor(Motion::WordRight, false, 1);
        stops.push(b.cursor().col);
    }
    assert_eq!(stops, [2, 3, 6, 9]);
    // 行をまたぐ
    let mut b = Buffer::from_text("ab\ncd");
    b.set_selection(Selection::caret(p(0, 2)));
    b.move_cursor(Motion::WordRight, false, 1);
    assert_eq!(b.cursor(), p(1, 0));
    b.move_cursor(Motion::WordLeft, false, 1);
    assert_eq!(b.cursor(), p(0, 2));
}

#[test]
fn smart_home() {
    let mut b = Buffer::from_text("    abc");
    b.set_selection(Selection::caret(p(0, 6)));
    b.move_cursor(Motion::LineStart, false, 1);
    assert_eq!(b.cursor(), p(0, 4));
    b.move_cursor(Motion::LineStart, false, 1);
    assert_eq!(b.cursor(), p(0, 0));
    b.move_cursor(Motion::LineStart, false, 1);
    assert_eq!(b.cursor(), p(0, 4));
}

#[test]
fn delete_lines_cases() {
    let mut b = Buffer::from_text("a\nb\nc");
    b.set_selection(Selection::caret(p(1, 1)));
    let ch = b.delete_lines().unwrap();
    assert_eq!(b.to_text(), "a\nc");
    assert_eq!(ch.old, LineRange::new(1, 3));
    assert_eq!(ch.new, LineRange::new(1, 2));
    assert_eq!(b.cursor(), p(1, 1));
    // 最後の行は、直前の改行も消す
    b.delete_lines();
    assert_eq!(b.to_text(), "a");
    assert_eq!(b.cursor(), p(0, 1));
    // 1 行だけなら空になる
    b.delete_lines();
    assert_eq!(b.to_text(), "");
    assert!(b.delete_lines().is_none());
    // 複数行の選択（終わりが行頭ならその行は残す）
    let mut b = Buffer::from_text("1\n2\n3\n4");
    b.set_selection(Selection { anchor: p(0, 1), head: p(2, 0) });
    b.delete_lines();
    assert_eq!(b.to_text(), "3\n4");
    b.undo();
    assert_eq!(b.to_text(), "1\n2\n3\n4");
}

#[test]
fn indent_and_outdent() {
    let mut b = Buffer::from_text("a\n\nb\nc");
    b.set_selection(Selection { anchor: p(0, 1), head: p(2, 1) });
    let ch = b.indent_lines("    ", false).unwrap();
    assert_eq!(lines(&b), ["    a", "", "    b", "c"]);
    assert_eq!(ch.old, LineRange::new(0, 3));
    assert_eq!(ch.new, LineRange::new(0, 3));
    // 選択の桁が一緒にずれる
    assert_eq!(b.selection(), Selection { anchor: p(0, 5), head: p(2, 5) });
    b.indent_lines("    ", true);
    assert_eq!(lines(&b), ["a", "", "b", "c"]);
    assert_eq!(b.selection(), Selection { anchor: p(0, 1), head: p(2, 1) });
    // 1 回の undo で全行が戻る
    b.undo();
    assert_eq!(lines(&b), ["    a", "", "    b", "c"]);
    b.undo();
    assert_eq!(lines(&b), ["a", "", "b", "c"]);
    // 選択なしの Tab は、カーソル位置に入れる。Shift+Tab は行を戻す
    let mut b = Buffer::from_text("xy");
    b.set_selection(Selection::caret(p(0, 1)));
    b.indent_lines("  ", false);
    assert_eq!(b.to_text(), "x  y");
    assert_eq!(b.cursor(), p(0, 3));
    b.indent_lines("  ", true);
    assert_eq!(b.to_text(), "x  y");
    b.set_selection(Selection::caret(p(0, 0)));
    b.insert("  ");
    b.indent_lines("  ", true);
    assert_eq!(b.to_text(), "x  y");
    // 空白が足りないときは取れるだけ・タブも戻せる・戻すものが無ければ None
    let mut b = Buffer::from_text(" a\n\tb\nc");
    b.set_selection(Selection { anchor: p(0, 0), head: p(2, 1) });
    b.indent_lines("    ", true);
    assert_eq!(lines(&b), ["a", "b", "c"]);
    assert!(b.indent_lines("    ", true).is_none());
    // 1 行の選択は、その行を字下げする
    let mut b = Buffer::from_text("abcd");
    b.set_selection(Selection { anchor: p(0, 1), head: p(0, 3) });
    b.indent_lines("\t", false);
    assert_eq!(b.to_text(), "\tabcd");
    assert_eq!(b.selection(), Selection { anchor: p(0, 2), head: p(0, 4) });
}

#[test]
fn toggle_comment() {
    let mut b = Buffer::from_text("def f():\n    x = 1\n\n    y = 2");
    b.select_all();
    b.toggle_line_comment("#");
    assert_eq!(lines(&b), ["# def f():", "#     x = 1", "", "#     y = 2"]);
    b.toggle_line_comment("#");
    assert_eq!(lines(&b), ["def f():", "    x = 1", "", "    y = 2"]);
    // 字下げの桁にそろえる
    let mut b = Buffer::from_text("    a\n      b");
    b.select_all();
    b.toggle_line_comment("//");
    assert_eq!(lines(&b), ["    // a", "    //   b"]);
    // 一部だけコメントなら全部コメントにする
    let mut b = Buffer::from_text("# a\nb");
    b.select_all();
    b.toggle_line_comment("#");
    assert_eq!(lines(&b), ["# # a", "# b"]);
    // 空白なしのコメントも外せる
    let mut b = Buffer::from_text("#a\n  #b");
    b.select_all();
    b.toggle_line_comment("#");
    assert_eq!(lines(&b), ["a", "  b"]);
    // 空行だけでもコメントにできる。マーカーが空なら何もしない
    let mut b = Buffer::from_text("");
    assert!(b.toggle_line_comment("").is_none());
    b.toggle_line_comment("#");
    assert_eq!(b.to_text(), "# ");
    // 日本語の行・undo
    let mut b = Buffer::from_text("あい\nうえ");
    b.set_selection(Selection { anchor: p(0, 1), head: p(1, 1) });
    b.toggle_line_comment("#");
    assert_eq!(lines(&b), ["# あい", "# うえ"]);
    assert_eq!(b.selection(), Selection { anchor: p(0, 3), head: p(1, 3) });
    b.undo();
    assert_eq!(lines(&b), ["あい", "うえ"]);
    assert_eq!(b.selection(), Selection { anchor: p(0, 1), head: p(1, 1) });
}

#[test]
fn text_in_selection() {
    let b = Buffer::from_text("日本語\nabc\nxyz");
    assert_eq!(b.text_in(Selection { anchor: p(0, 1), head: p(1, 2) }), "本語\nab");
    assert_eq!(b.text_in(Selection { anchor: p(2, 3), head: p(0, 2) }), "語\nabc\nxyz");
    assert_eq!(b.text_in(Selection::caret(p(1, 1))), "");
    // 範囲外は丸める
    assert_eq!(b.text_in(Selection { anchor: p(2, 1), head: p(99, 99) }), "yz");
}

#[test]
fn find_forward_backward_wrap() {
    let b = Buffer::from_text("foo bar foo\nbaz\nfoo");
    let s = |a: (u32, u32), h: (u32, u32)| Some(Selection { anchor: p(a.0, a.1), head: p(h.0, h.1) });
    assert_eq!(b.find("foo", p(0, 0), false), s((0, 0), (0, 3)));
    assert_eq!(b.find("foo", p(0, 3), false), s((0, 8), (0, 11)));
    assert_eq!(b.find("foo", p(0, 11), false), s((2, 0), (2, 3)));
    // 巻き戻し
    assert_eq!(b.find("foo", p(2, 3), false), s((0, 0), (0, 3)));
    // 後方
    assert_eq!(b.find("foo", p(2, 0), true), s((0, 8), (0, 11)));
    assert_eq!(b.find("foo", p(0, 11), true), s((0, 8), (0, 11)));
    assert_eq!(b.find("foo", p(0, 10), true), s((0, 0), (0, 3)));
    assert_eq!(b.find("foo", p(0, 0), true), s((2, 0), (2, 3)));
    assert!(b.find("nope", p(0, 0), false).is_none());
    assert!(b.find("", p(0, 0), false).is_none());
    // 範囲外の from
    assert_eq!(b.find("baz", p(99, 99), false), s((1, 0), (1, 3)));
}

#[test]
fn find_japanese_and_case() {
    let b = Buffer::from_text("日本語のTextと日本語\nText text TEXT");
    let r = b.find("日本語", p(0, 1), false).unwrap();
    assert_eq!(r, Selection { anchor: p(0, 9), head: p(0, 12) });
    assert_eq!(b.find("text", p(0, 0), false).unwrap().anchor, p(1, 5));
    assert_eq!(b.find_with("text", p(0, 0), false, false).unwrap().anchor, p(0, 4));
    assert_eq!(b.find_with("TEXT", p(1, 9), true, false).unwrap().anchor, p(1, 5));
    assert_eq!(b.find_with("TEXT", p(1, 9), false, false).unwrap().anchor, p(1, 10));
    // 列は文字数（バイトではない）
    assert_eq!(b.find("と", p(0, 0), false).unwrap(), Selection { anchor: p(0, 8), head: p(0, 9) });
}

#[test]
fn find_multiline_needle() {
    let b = Buffer::from_text("a\nb\nc\nb\nc");
    let r = b.find("b\nc", p(0, 0), false).unwrap();
    assert_eq!(r, Selection { anchor: p(1, 0), head: p(2, 1) });
    let r = b.find("b\nc", p(2, 1), false).unwrap();
    assert_eq!(r, Selection { anchor: p(3, 0), head: p(4, 1) });
    let r = b.find("b\nc", p(3, 0), true).unwrap();
    assert_eq!(r, Selection { anchor: p(1, 0), head: p(2, 1) });
    assert_eq!(b.find("b\nc", p(4, 1), false).unwrap().anchor, p(1, 0));
    assert!(b.find("c\na", p(0, 0), false).is_none());
}

#[test]
fn set_selection_clamps() {
    let mut b = Buffer::from_text("ab\ncd");
    b.set_selection(Selection { anchor: p(9, 9), head: p(0, 99) });
    assert_eq!(b.selection(), Selection { anchor: p(1, 2), head: p(0, 2) });
    assert_eq!(b.clamp(p(1, 50)), p(1, 2));
    assert_eq!(b.char_index(p(1, 1)), 4);
    assert_eq!(b.pos_at(4), p(1, 1));
}

#[test]
fn undo_after_complex_sequence_restores_exactly() {
    let mut b = Buffer::from_text("fn main() {\n    println!(\"こんにちは\");\n}\n");
    let original = b.to_text();
    b.set_selection(Selection::caret(p(1, 4)));
    b.newline("    ", "{");
    b.insert("let x = 1;");
    b.delete_lines();
    b.select_all();
    b.toggle_line_comment("//");
    b.indent_lines("    ", false);
    b.set_selection(Selection::caret(p(0, 0)));
    b.insert("日本語");
    b.delete_backward();
    while b.undo().is_some() {}
    assert_eq!(b.to_text(), original);
    assert!(!b.is_modified());
    while b.redo().is_some() {}
    assert!(b.is_modified());
}

#[test]
fn merge_change_is_conservative_and_exact() {
    let a = LineChange { old: LineRange::new(2, 3), new: LineRange::new(2, 4) };
    let c = LineChange { old: LineRange::new(5, 6), new: LineRange::new(5, 5) };
    let m = merge_change(Some(a), c);
    assert_eq!(m.old, LineRange::new(2, 5));
    assert_eq!(m.new, LineRange::new(2, 5));
    assert_eq!(m.delta(), a.delta() + c.delta());
    // 先の範囲の中
    let c2 = LineChange { old: LineRange::new(3, 4), new: LineRange::new(3, 6) };
    let m = merge_change(Some(a), c2);
    assert_eq!(m.old, LineRange::new(2, 3));
    assert_eq!(m.new, LineRange::new(2, 6));
}

#[test]
fn bulk_edit_2000_lines() {
    let text: String = (0..2000).map(|i| format!("    line {i} あいう\n")).collect();
    let mut b = Buffer::from_text(&text);
    b.set_selection(Selection::caret(p(1000, 5)));
    let t = std::time::Instant::now();
    for _ in 0..200 {
        b.insert("x");
    }
    b.select_all();
    b.toggle_line_comment("#");
    let el = t.elapsed();
    assert!(el.as_millis() < 2000, "{el:?}");
    assert_eq!(b.len_lines(), 2001);
    b.undo();
    b.undo();
    assert_eq!(b.to_text(), text);
}
