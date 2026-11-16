# 言語の定義（ハイライト・実行・コメント・アウトライン）

zcode の言語はここに **ファイルを置くだけ** で増える（コードは変えない。design.md §7.2）。

```
languages/
  theme.toml          トークンの種類 → 色（Dark+）
  <言語>/
    language.toml     言語の設定と規則（テキスト）
    sample.<拡張子>   見本（自動でテストされる）
```

`cargo build` のときに `crates/zc-highlight/build.rs` が全部の `language.toml` を読み、規則を
**DFA（状態遷移表）に変換して実行ファイルに埋め込む**。実機ではパースも正規表現の
コンパイルもしない（syntect は 1 行 1.8ms・初回 200ms 止まった。DFA は 1 行 数 µs）。

## 言語を足す手順

1. `languages/<言語>/language.toml` を書く（下の形。`python/` を手本にする）。
   Sublime Text の `.sublime-syntax` があれば、下書きを道具で作れる:
   ```sh
   cargo run -p zcode-langc -- import Foo.sublime-syntax > languages/foo/language.toml
   ```
   変換できなかった規則はファイルの先頭のコメントと標準エラーに一覧で出るので、手で直す
2. 規則を確かめる（DFA の大きさも出る。1 言語 100KB 以下が目安）:
   ```sh
   cargo run -p zcode-langc -- check languages/foo/language.toml
   cargo run -p zcode-langc -- show languages/foo/language.toml languages/foo/sample.foo
   ```
3. 見本 `languages/<言語>/sample.<拡張子>` を置く。拡張子（か 1 行目）で言語が決まること
4. `cargo test -p zc-highlight --test golden` を 1 回走らせると、正解
   `tests/golden/highlight/<言語>.txt` が作られる（その回は失敗する）。中身を見て、色の付き方が
   正しければコミットする。規則を直したら `ZCODE_UPDATE_GOLDEN=1` を付けて作り直す

言語のフォルダを足すと、`cargo test` が見本・埋め込み・大きさの上限を全部自動で確かめる。

## `language.toml` の形

```toml
name = "Python"                 # 表示名（ステータスバー）
extensions = ["py", "pyw"]      # 拡張子（ドットなし。大文字小文字は区別しない）
filenames = ["SConstruct"]      # 拡張子で決まらないファイル名（完全一致。省略可）
first_line = '^#!.*\bpython'    # 拡張子で決まらないとき、1 行目に一致すればこの言語（省略可）
line_comment = "#"              # Ctrl+/ のコメント切り替え（省略可）
indent = "    "                 # 字下げ 1 段（省略すると空白 4 つ）
run = "python3 {file}"          # F5 で実行するコマンド（省略可）
remote = "rust"                 # ビルド係での扱い: rust / c / node（省略可。下の「ビルド係と実行環境」）
check = "python3 -I -S -c '...' {file}"  # 保存時の構文チェック（省略可。design.md §10.2）
check_pattern = 'line (\d+)'    # チェックの出力からエラーを拾う正規表現（省略可）
keywords = ["print", "len"]     # 単語補完に足す語（[words] の語は自動で入る。省略可）

[outline]                       # アウトライン（省略可）
items = { def = "function", class = "class" }   # 行の最初の語 → function / class / heading
modifiers = ["async"]           # その語の前に付いてよい語（pub・export など）
heading_char = "#"              # 見出しの記号（Markdown）
indent_width = 4                # 字下げ 1 段の空白の数（省略すると indent の長さ）

[variables]                     # 正規表現の中の {{名前}} を置き換える部品
ident = '[A-Za-z_{{nonascii}}][A-Za-z0-9_{{nonascii}}]*'

[words]                         # 識別子の表（下の「キーワードは words に」）
keyword = ["def", "class", "and"]
control = ["if", "for", "return"]

[word_push]                     # その語のあとに積む文脈
def = "funcname"

[default_token]                 # 文脈の地のトークン（規則に当たらない文字の色）
string_dq = "string"

[[contexts.main]]               # 文脈ごとに規則を上から順に並べる。main が出発点
match = '#.*'
token = "comment"
```

- `check` は `{file}` をファイルのパスに置き換える。**シェルは通さない**
  （引用符 `'` `"` で区切りをまとめられる。`{file}` は引数ごとに置き換える）。`$HOME` などを
  展開したいときは `bash -c '… "$0"' {file}` で包む
- `run` は、F5 の端末の bash が `eval` する（作業フォルダはファイルのフォルダ、`{file}` は
  引用済みのファイル名に置き換わる）。`exec` を直に書くと終了コードを返す zcode の包みごと
  置き換わるので、`exec` を使うときは `bash -c '…' {file}` で包む
- 長いコマンドは TOML の `"""…"""` で折って書ける。行末の `\` は「改行と次の行の字下げを消す」
  なので、値は 1 行のまま（`\` の前に空白を 1 つ残す。端末の見出しにも 1 行で出る）
- `check_pattern` の括弧の組は **1 = 行（1 始まり）、2 = 桁（省略可）、3 = メッセージ（省略可）**。
  これは zc-intel が読む（`line (\d+)` のように 1 つだけでもよい）

### 規則（`[[contexts.<文脈>]]`）

| 項目 | 意味 |
|---|---|
| `match` | 正規表現。行の今の位置に **先頭一致** する規則を、**上から順に** 探す（最初に当たった規則が勝つ） |
| `token` | トークンの種類（下の表）。省略するとその文脈の `default_token`（無ければ `text`） |
| `push` | 一致したあと文脈を積む。`"名前"` か `["a", "b"]`（並べた順に積む。最後が一番上） |
| `pop` | 一致したあと文脈を降ろす。`true` で 1 段、数でその段数（`main` は降ろせない） |
| `set` | 今の文脈を降ろしてから積む（`pop = true` + `push` と同じ） |
| `followed_by` | 正規表現。**後ろにこれが続くときだけ** 一致する。続く部分は消費しない（先読みの代わり。例: 名前の後ろに `(` → 関数の色） |
| `words` | `true` なら、一致した文字列を `[words]` で引き、あればその種類にする（`[word_push]` の文脈も積む） |
| `include` | ほかの文脈の規則をここに展開する（この規則にはほかの項目を書けない） |

- 文脈スタックは行をまたいで続く（三重引用符の文字列、複数行の引数など）。行ごとに「行頭の
  文脈スタック」を覚えて、編集した行から下は、行頭が前回と同じになったところで計算をやめる
- 各行は **末尾に `\n` を付けて** 照合する。`match = '\n'` + `pop = true` で「行末で文脈を
  抜ける」（一重引用符の文字列など）。`$` は行末（`\n` の手前）、`^` は行頭
- `match = ''`（空）は文脈を変える規則（`pop` / `push` / `set`）にだけ使える。文脈の最後に
  置くと「どれにも当たらなければ抜ける」になる。同じ位置で空の一致が続いたら、エンジンが
  1 文字進めて止まらないようにする
- 文脈の **最後の** `include` は、DFA を複製せずに共有する（結果は展開と同じ）。大きな文脈
  （式など）を取り込むときは最後に置くと表が小さくなる

### 正規表現で使えないもの（DFA にできない）

| 書き方 | 代わり |
|---|---|
| 先読み `(?=..)` `(?!..)` | `followed_by`。「どれにも当たらなければ抜ける」は空の `match` |
| 後読み `(?<=..)` `(?<!..)` | 文脈を分ける（前の記号で push する） |
| 後方参照 `\1` `\k<..>` | 使えない（ヒアドキュメントの終端などはエンジンに専用の規則を足す） |
| Unicode の文字クラス `\p{L}` など | 非 ASCII は「識別子の一部」として `[{{nonascii}}]`（= `\x80-\xFF`）で書く |
| アトミック `(?>..)`・強欲 `*+` | `(?:..)`・`*` |
| `\h` `\G` `\Z` | `[0-9a-fA-F]`・使えない・`$` |

- Unicode は使わない設定なので `\w` `\d` `\s` `\b` は ASCII だけ。非 ASCII の文字そのもの
  （`「`、`日本` など）は UTF-8 のバイト列として照合できる
- 使えない書き方は、**ビルドがファイル名と規則の位置を出して止まる**
  （例: `languages/foo/language.toml: contexts.main[3] (match = '(?=x)'): 先読み…`）

### 表を小さく保つ（1 言語 100KB 以下が目安）

DFA の表の大きさは「状態の数 × 文字の種類の数（2 の冪に切り上げ）× 4 バイト」。
`cargo run -p zcode-langc -- check` が文脈ごとの大きさと文字の種類の数を出す。

- **キーワードは `[words]` に書く**。`(?:if|elif|else|...)\b` を正規表現に書くと、文字ごとに
  状態ができ、表が数十〜数百 KB になる（Python で 1.1MB になった）。識別子を 1 つの規則
  （`words = true`）で取ってから表で引く。文脈を積むキーワード（`def` など）は `[word_push]`
- 文字を細かく区別しない。`0[xX][0-9a-fA-F]+` と `[eE][+-]?` と `[jJ]` を全部書くより、
  「数字で始まる英数字の並び」1 つの方が小さい。`**=` `//=` のような演算子も
  `[-+*/%@&|^~<>!=]+` 1 つにまとめる（文字の種類が 64 を超えると表の幅が 2 倍になる）
- 前置き × 引用符のような組み合わせは、前置きだけ見て push し、引用符は次の文脈で分ける
- 大きな文脈を取り込むときは、文脈の **最後に** `include` を置く（DFA を共有する）

## ビルド係と実行環境（`remote`・`run`・`check`。docs/stage3-design.md §10）

`remote` は、その言語をビルド係（`zc-remote` の先。Cloudflare か PC）で扱う種類。あれば
保存時にビルド係の check を頼み、「試験を実行」が使える。ビルド係に頼めないとき（対象では
ない・オフライン・`zc-remote` が無い）は、実機の `check` に回る。

| 言語 | `remote` | `run`（F5） | `check`（実機） |
|---|---|---|---|
| Rust | `rust` | `zc-remote run {file}`（ビルド係で aarch64 向けに作って、その場で実行） | 無し（実機に rustc を置かない） |
| C | `c` | `zc-remote run {file}` | `gcc -fsyntax-only`（実機に gcc があれば） |
| JavaScript | `node` | npm の依存があれば `zc-remote run`、無ければ `llrt` | `tsc --allowJs`（構文だけ） |
| TypeScript | `node` | npm の依存があれば `zc-remote run`、無ければ `tsc` で型検査して JS を出し `llrt` で実行 | `tsc --noEmit` |
| Python・Shell | — | `python3` / `bash` | `ast.parse` / `bash -n` |

- 「npm の依存があるか」は `zc-remote applies <file>`（終了コード 0 = ビルド係の対象。通信しない）
  で決める。`zc-remote` が入っていなければ対象ではない扱いになり、実機で動かす
- TS の F5 は、JS を RAM（`$XDG_RUNTIME_DIR/zcode/js/`）に出す（SD に書かない）。型エラーなら
  出さず（`--noEmitOnError`）、実行しない。`.mts` → `.mjs`、`.cts` → `.cjs`。型を剥がすだけの
  esbuild は使わない
- JS / TS の check と TS の F5 は、グローバルの型に DOM ではなく **LLRT の型定義**
  （`~/.local/share/zcode/types/llrt/index.d.ts`）を使い、`--lib es2022` にする（`--lib es2022,dom`
  は実機で 3.8 秒・91MB だった）。型定義が入っていなければ、もとの `--lib es2022,dom` に戻す
- LLRT の型定義は、組み込みのモジュールを **`"fs"` `"os"` の名前で** 宣言している（v0.10.0-beta。
  `node:` 付きで宣言しているのは `node:dgram`・`node:module`・`node:stream/web` だけ）。
  `import … from "node:fs"` と書くと TS2591 で型検査が通らないので、`"fs"` と書く
  （LLRT はどちらでも動く）。`structuredClone` も型定義に無い（開発コンテナの x64 の tsc 7.0.2 と
  LLRT で確かめた、2026-11-17）
- `llrt`・`tsc`・LLRT の型定義・`zc-remote` は `zcode/tools/install-runtimes.sh` で
  `~/.local/bin` などに入れる（版と SHA-256 は `zcode/tools/runtimes.lock`。sudo は要らない）。
  `language.toml` は版によらない名前だけを使う
- `tsc` が入っていないと、JS / TS の check は bash が「not found」で終わり、診断は出ない
  （`bash -c` で包んだので、zcode からは起動の失敗に見えない）

## トークンの種類（`token = "..."`）

`zc_core::TokenKind` の名前。色は `theme.toml`。

| 名前 | Dark+ | 使いどころ |
|---|---|---|
| `text` | #d4d4d4 | 地の文（色を付けない） |
| `comment` | #6a9955 | コメント |
| `string` | #ce9178 | 文字列 |
| `escape` | #d7ba7d | 文字列の中のエスケープ |
| `number` | #b5cea8 | 数 |
| `keyword` | #569cd6 | 宣言のキーワード（`def` `class` `let`）、論理演算子 |
| `control` | #c586c0 | 制御のキーワード（`if` `for` `return` `import`） |
| `function` | #dcdcaa | 関数名・呼び出し |
| `type` | #4ec9b0 | 型・クラス名 |
| `variable` | #9cdcfe | 引数・特別な変数 |
| `constant` | #569cd6 | `True` `None` `self`、書式の置き場所 |
| `operator` | #d4d4d4 | 演算子 |
| `punctuation` | #d4d4d4 | 区切り |
| `attribute` | #dcdcaa | デコレータ・属性・マクロ |
| `heading` | #569cd6 太字 | Markdown の見出し、TOML のテーブル名 |
| `key` | #9cdcfe | JSON / TOML のキー |
| `link` | #3794ff 下線 | リンク |
| `invalid` | #f48771 | 構文エラー |

## `theme.toml`

```toml
[tokens]
comment = "#6a9955"                       # 文字色だけ
heading = { fg = "#569cd6", bold = true } # fg / bg / bold / italic / underline
```

書かなかった種類は `zc_core::Theme::dark_plus()` のまま。`zc_highlight::theme()` で読む。

## 実行時の仕組み（参考）

- 埋め込みの形は `crates/zc-highlight/src/format.rs`（先頭にマジック・形式の版・ビルド ID、
  言語ごとに 4096 バイト境界の連続した領域。DFA はそのままの形でゼロコピーで使う）
- 言語は初めて使うときに読む（その言語の領域に `madvise(WILLNEED)`）。ほかの言語の表は
  SD から読まれない
- 実機の `~/.config/zcode/languages/` に置いた言語を実行時に変換する機能は、実行時に TOML を
  読む件（design.md §12 #10）が片づいてから入れる（変換のコードは `zc-highlight` の
  `compile` 機能にあり、形は埋め込みと同じ）
