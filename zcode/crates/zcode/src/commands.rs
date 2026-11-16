//! 操作の表。**キー割り当てとショートカット一覧は、この 1 つの表から作る**
//! （design.md §3。一覧だけ古くなるのを防ぐ）。
//!
//! `/` と `` ` `` は Sym レイヤーのキー（Ctrl と Sym を同時に押す操作になる）。
//! 押しやすさを実機で確かめてから最終決定する。

use std::sync::OnceLock;
use zc_core::{Command, KeyChord, KeyEvent, Motion};

/// 表の 1 行。
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    /// ショートカット一覧の区分。
    pub category: &'static str,
    /// ショートカット一覧に出す名前。
    pub title: &'static str,
    /// キー（`KeyChord::parse` の書き方）。空なら割り当てなし（一覧から選んで実行する）。
    pub keys: &'static str,
    pub command: Command,
}

const fn b(category: &'static str, title: &'static str, keys: &'static str, command: Command) -> Binding {
    Binding { category, title, keys, command }
}

const fn mv(motion: Motion, select: bool) -> Command {
    Command::Move { motion, select }
}

/// 操作の表（`docs/ui-mock.html` の「ショートカット一覧の全項目」）。
pub const BINDINGS: &[Binding] = &[
    b("ファイル", "保存", "Ctrl+S", Command::Save),
    b("ファイル", "新規ファイル", "Ctrl+N", Command::NewFile),
    b("ファイル", "タブを閉じる", "Ctrl+W", Command::CloseTab),
    b("ファイル", "次のタブ", "Ctrl+Tab", Command::NextTab),
    b("ファイル", "前のタブ", "Ctrl+Shift+Tab", Command::PrevTab),
    b("ファイル", "ショートカット一覧", "F1", Command::ShowShortcuts),
    b("ファイル", "zcode を終了", "Ctrl+Q", Command::Quit),
    b("編集", "元に戻す", "Ctrl+Z", Command::Undo),
    b("編集", "やり直す", "Ctrl+Y", Command::Redo),
    b("編集", "切り取り", "Ctrl+X", Command::Cut),
    b("編集", "コピー", "Ctrl+C", Command::Copy),
    b("編集", "貼り付け", "Ctrl+V", Command::Paste),
    b("編集", "すべて選択", "Ctrl+A", Command::SelectAll),
    b("編集", "インデント", "Tab", Command::Indent),
    b("編集", "インデントを戻す", "Shift+Tab", Command::Outdent),
    b("編集", "行を削除", "Ctrl+Shift+K", Command::DeleteLine),
    b("編集", "行コメント切替", "Ctrl+/", Command::ToggleLineComment),
    b("編集", "検索", "Ctrl+F", Command::Find),
    b("編集", "次を検索", "F3", Command::FindNext),
    b("編集", "前を検索", "Shift+F3", Command::FindPrev),
    b("編集", "行へ移動", "Ctrl+G", Command::GotoLine),
    b("編集", "補完", "Ctrl+Space", Command::Complete),
    b("編集", "改行", "Enter", Command::Newline),
    b("編集", "前の文字を消す", "Backspace", Command::Backspace),
    b("編集", "次の文字を消す", "Delete", Command::DeleteForward),
    b("移動", "左へ", "Left", mv(Motion::Left, false)),
    b("移動", "右へ", "Right", mv(Motion::Right, false)),
    b("移動", "上へ", "Up", mv(Motion::Up, false)),
    b("移動", "下へ", "Down", mv(Motion::Down, false)),
    b("移動", "行頭", "Home", mv(Motion::LineStart, false)),
    b("移動", "行末", "End", mv(Motion::LineEnd, false)),
    b("移動", "1 画面上", "PageUp", mv(Motion::PageUp, false)),
    b("移動", "1 画面下", "PageDown", mv(Motion::PageDown, false)),
    b("移動", "ファイル先頭", "Ctrl+Home", mv(Motion::DocStart, false)),
    b("移動", "ファイル末尾", "Ctrl+End", mv(Motion::DocEnd, false)),
    b("移動", "左へ選択", "Shift+Left", mv(Motion::Left, true)),
    b("移動", "右へ選択", "Shift+Right", mv(Motion::Right, true)),
    b("移動", "上へ選択", "Shift+Up", mv(Motion::Up, true)),
    b("移動", "下へ選択", "Shift+Down", mv(Motion::Down, true)),
    b("移動", "前の単語", "Ctrl+Left", mv(Motion::WordLeft, false)),
    b("移動", "次の単語", "Ctrl+Right", mv(Motion::WordRight, false)),
    b("移動", "前の単語まで選択", "Ctrl+Shift+Left", mv(Motion::WordLeft, true)),
    b("移動", "次の単語まで選択", "Ctrl+Shift+Right", mv(Motion::WordRight, true)),
    b("移動", "行頭まで選択", "Shift+Home", mv(Motion::LineStart, true)),
    b("移動", "行末まで選択", "Shift+End", mv(Motion::LineEnd, true)),
    b("移動", "1 画面上まで選択", "Shift+PageUp", mv(Motion::PageUp, true)),
    b("移動", "1 画面下まで選択", "Shift+PageDown", mv(Motion::PageDown, true)),
    b("移動", "ファイル先頭まで選択", "Ctrl+Shift+Home", mv(Motion::DocStart, true)),
    b("移動", "ファイル末尾まで選択", "Ctrl+Shift+End", mv(Motion::DocEnd, true)),
    b("表示", "ツリー開閉", "Ctrl+B", Command::ToggleTree),
    b("表示", "ツリーへ移る", "Ctrl+E", Command::FocusTree),
    b("表示", "閉じる / エディタへ戻る", "Esc", Command::Cancel),
    b("表示", "ツリーで選んだファイルを開く", "", Command::TreeOpen),
    b("表示", "アウトライン", "Ctrl+O", Command::ShowOutline),
    b("実行", "保存して実行", "F5", Command::Run),
    b("実行", "ターミナル開閉", "Ctrl+`", Command::ToggleTerminal),
    b("実行", "ターミナル全画面", "F11", Command::ToggleTerminalFullscreen),
    b("Markdown", "プレビュー ⇔ ソース", "Ctrl+Shift+V", Command::TogglePreview),
    b("Markdown", "チェックボックスを切り替え", "Ctrl+Enter", Command::ToggleCheckbox),
    b("Markdown", "太字", "Alt+B", Command::Bold),
    b("Markdown", "斜体", "Alt+I", Command::Italic),
    b("日本語入力", "かなモードへ", "Ctrl+J", Command::ImeOn),
];

/// 打鍵に割り当てられた操作。割り当てが無く、文字の打鍵なら `InsertChar`。
///
/// ツリーの中の Enter（開く）や ↑↓ は、同じ操作（`Newline`・`Move`）を、入力が
/// ツリーに向いているときに画面側が読み替える。
pub fn lookup(ev: &KeyEvent) -> Option<Command> {
    // 表の文字列を読むのは最初の 1 回だけ（打鍵のたびに読むと、数十 µs かかる）
    static TABLE: OnceLock<Vec<(KeyChord, Command)>> = OnceLock::new();
    let table = TABLE.get_or_init(|| BINDINGS.iter().filter_map(|bd| Some((KeyChord::parse(bd.keys)?, bd.command))).collect());
    let chord = ev.chord();
    if let Some(&(_, cmd)) = table.iter().find(|(c, _)| *c == chord) {
        return Some(cmd);
    }
    ev.text.map(Command::InsertChar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use zc_core::{Key, Mods};

    #[test]
    fn every_binding_parses_and_is_unique() {
        let mut seen = HashSet::new();
        for bd in BINDINGS {
            if bd.keys.is_empty() {
                continue;
            }
            let chord = KeyChord::parse(bd.keys).unwrap_or_else(|| panic!("読めないキー: {}", bd.keys));
            assert!(seen.insert(chord), "キーが重なっている: {}", bd.keys);
        }
    }

    #[test]
    fn lookup_bindings_and_text() {
        assert_eq!(lookup(&KeyEvent::new(Key::Char('s'), Mods::CTRL)), Some(Command::Save));
        assert_eq!(lookup(&KeyEvent::new(Key::F(5), Mods::NONE)), Some(Command::Run));
        assert_eq!(lookup(&KeyEvent::new(Key::Left, Mods::SHIFT)), Some(mv(Motion::Left, true)));
        assert_eq!(lookup(&KeyEvent::char('A')), Some(Command::InsertChar('A')));
        assert_eq!(lookup(&KeyEvent::char(' ')), Some(Command::InsertChar(' ')));
        assert_eq!(lookup(&KeyEvent::new(Key::Char('k'), Mods::ALT)), None);
        // Sym / Fn は照合に関係しない
        assert_eq!(lookup(&KeyEvent::new(Key::F(1), Mods::FN)), Some(Command::ShowShortcuts));
        // エディタの移動と選択
        assert_eq!(lookup(&KeyEvent::new(Key::Right, Mods::CTRL | Mods::SHIFT)), Some(mv(Motion::WordRight, true)));
        assert_eq!(lookup(&KeyEvent::new(Key::End, Mods::SHIFT)), Some(mv(Motion::LineEnd, true)));
        assert_eq!(lookup(&KeyEvent::new(Key::Enter, Mods::NONE)), Some(Command::Newline));
        assert_eq!(lookup(&KeyEvent::new(Key::Tab, Mods::SHIFT)), Some(Command::Outdent));
        assert_eq!(lookup(&KeyEvent::new(Key::Char('b'), Mods::CTRL)), Some(Command::ToggleTree));
        assert_eq!(lookup(&KeyEvent::new(Key::Char('e'), Mods::CTRL)), Some(Command::FocusTree));
    }
}
