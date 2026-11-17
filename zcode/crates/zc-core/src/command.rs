//! 操作。すべての操作は [`Command`] で表す（design.md §3「共通の約束」）。
//!
//! キー割り当てとショートカット一覧は `zcode/src/commands.rs` の 1 つの表から作る。
//! ここには列挙型だけを置く。操作を足すときは、ここに項目を足して、表に 1 行足す。

/// カーソルの動き。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    PageUp,
    PageDown,
    DocStart,
    DocEnd,
}

impl Motion {
    /// 画面がスクロールしうる動きか（キーリピートを 20 回/秒にする対象。§5.3）。
    pub const fn scrolls(self) -> bool {
        matches!(self, Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown)
    }
}

/// 操作。`docs/ui-mock.html` の「ショートカット一覧の全項目」に対応する。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Command {
    // --- ファイル ---
    Save,
    NewFile,
    CloseTab,
    NextTab,
    PrevTab,
    /// ショートカット一覧（コマンドパレットを兼ねる）を開く。
    ShowShortcuts,
    Quit,

    // --- 文字入力 ---
    /// 文字を入れる（SKK の確定文字列は 1 文字ずつこれで届く）。
    InsertChar(char),
    Newline,
    Backspace,
    DeleteForward,
    /// Esc。開いている一覧・入力欄を閉じる、ツリーからエディタへ戻るなど。
    Cancel,

    // --- 編集 ---
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Indent,
    Outdent,
    DeleteLine,
    ToggleLineComment,
    Find,
    FindNext,
    FindPrev,
    GotoLine,
    /// 単語補完の候補を出す（§10.2）。
    Complete,

    // --- 移動 ---
    /// カーソルを動かす。`select` なら選択範囲を伸ばす（Shift つき）。
    Move { motion: Motion, select: bool },

    // --- 表示 ---
    /// ツリーの開閉（Ctrl+B。どこにいても。開いたらツリーへ移る）。
    ToggleTree,
    /// ツリーへ移る（Ctrl+1。閉じていれば開く）。
    FocusTree,
    /// エディタへ移る（Ctrl+2）。
    FocusEditor,
    /// ターミナルへ移る（Ctrl+3。閉じていれば開く）。
    FocusTerminal,
    /// いまの区画を最大化 / 戻す（F11。ツリーではフォルダを開く画面）。
    Maximize,
    /// ツリーで選んでいる項目を開く。
    TreeOpen,
    /// アウトライン（§10.2）。
    ShowOutline,

    // --- 型の情報（LSP。ビルド係の rust-analyzer。docs/lsp-design.md） ---
    /// カーソルの位置の型と説明（Ctrl+H）。
    Hover,
    /// 定義へ（Ctrl+D）。
    GotoDefinition,
    /// 使っている所の一覧（Ctrl+R）。
    FindReferences,
    /// プロジェクトの名前を探す（Ctrl+T）。
    FindSymbol,
    /// 次の診断（波線）へ（F8。手元の診断だけを見る。オフラインでも効く）。
    NextDiagnostic,
    /// 前の診断へ（Shift+F8）。
    PrevDiagnostic,

    // --- 実行 ---
    /// 保存して実行（F5）。
    Run,
    StopRun,
    /// 試験をビルド係で実行し、結果を一覧に出す（F6。docs/stage3-design.md §8.5）。
    RunTests,
    /// ターミナルの開閉（F12。どこにいても。開いたらターミナルへ移る）。
    ToggleTerminal,

    // --- Markdown ---
    /// ライブプレビュー ⇔ ソース表示。
    TogglePreview,
    ToggleCheckbox,
    Bold,
    Italic,

    // --- 日本語入力 ---
    /// かなモードへ（Ctrl+J）。かなモード中のキーは `zc-skk` が直接受け取る。
    ImeOn,
    ImeOff,
}

impl Command {
    /// 押しっぱなしで繰り返してよい操作か。`false` の操作には、リピートで作られた
    /// 打鍵（`KeyEvent::repeat`）を渡さない。
    pub const fn repeatable(&self) -> bool {
        matches!(
            self,
            Command::InsertChar(_)
                | Command::Newline
                | Command::Backspace
                | Command::DeleteForward
                | Command::Move { .. }
                | Command::Undo
                | Command::Redo
                | Command::FindNext
                | Command::FindPrev
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeatable_and_scroll() {
        assert!(Command::Backspace.repeatable());
        assert!(!Command::Save.repeatable());
        assert!(Motion::PageDown.scrolls());
        assert!(!Motion::Left.scrolls());
    }
}
