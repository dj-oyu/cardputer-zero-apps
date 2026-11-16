// `languages/<言語>/language.toml` と `languages/theme.toml` の形
// （書き方は `zcode/languages/README.md`）。
//
// `build.rs`（ビルド時に DFA へ変換する）と、ライブラリの `compile` 機能（道具
// `zcode-langc` が実行時に読む）の両方から使う。実機の実行ファイルには入らない。
// 知らない項目は誤記として止める（`deny_unknown_fields`）。

use std::collections::BTreeMap;

/// `language.toml` 1 つ。
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct LanguageFile {
    /// 表示名（ステータスバー）。
    pub name: String,
    /// 拡張子（ドットなし、大文字小文字は区別しない）。
    #[serde(default)]
    pub extensions: Vec<String>,
    /// 拡張子で決まらないファイル名（`Makefile` など。完全一致）。
    #[serde(default)]
    pub filenames: Vec<String>,
    /// 1 行目（shebang など）で判定する正規表現。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_line: Option<String>,
    /// 行コメントの記号（Ctrl+/）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_comment: Option<String>,
    /// 字下げ 1 段ぶん（既定は空白 4 つ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indent: Option<String>,
    /// F5 で実行するコマンド（`{file}` をパスに置き換える）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// ビルド係での扱い（`rust` / `c` / `node`。docs/stage3-design.md §10.1）。あれば保存時に
    /// ビルド係の check を頼み、「試験を実行」が使える。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// 保存時の構文チェックのコマンド（design.md §10.2）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// チェックの出力からエラー行を拾う正規表現（zc-intel が使う。ここでは素通し）。
    /// 括弧の組 1 = 行（1 始まり）、2 = 桁（省略可）、3 = メッセージ（省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_pattern: Option<String>,
    /// 単語補完に出す言語のキーワード（組み込みの名前も含めてよい）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// アウトライン（zc-intel の `OutlineRules` の元）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<OutlineFile>,
    /// 正規表現の中の `{{名前}}` を置き換える部品。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variables: BTreeMap<String, String>,
    /// 語の表: トークンの種類 → 語の列。`words = true` の規則に一致した語がここにあれば、
    /// その種類にする（キーワードの長い選択を正規表現に書くと DFA が大きくなるため）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub words: BTreeMap<String, Vec<String>>,
    /// 語 → 積む文脈。`[words]` で見つかった語がここにあれば、その文脈も積む
    /// （`def` → 関数名の文脈、など。キーワードを正規表現に書かずに済む）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub word_push: BTreeMap<String, String>,
    /// 文脈の地のトークン（規則に当たらない文字と、`token` を書かない規則の色）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub default_token: BTreeMap<String, String>,
    /// 文脈 → 規則の列。`main` が必須で、行頭の文脈の出発点。
    pub contexts: BTreeMap<String, Vec<Rule>>,
}

/// `[outline]`。
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OutlineFile {
    /// 行の最初の語 → 項目の種類（`function` / `class` / `heading`）。
    #[serde(default)]
    pub items: BTreeMap<String, String>,
    /// キーワードの前に付いてよい語（`async`・`pub` など）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<String>,
    /// 見出しの記号（Markdown の `#`。1 文字）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_char: Option<String>,
    /// 字下げ 1 段の空白の数（省略すると `indent` の長さ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indent_width: Option<u32>,
}

/// アウトラインの項目の種類の名前。
pub const OUTLINE_KINDS: [&str; 3] = ["function", "class", "heading"];

/// 規則 1 つ。`match` か `include` のどちらか一方を書く。
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// 正規表現（行の今の位置に先頭一致。上の規則ほど優先）。
    #[serde(rename = "match", default, skip_serializing_if = "Option::is_none")]
    pub match_: Option<String>,
    /// トークンの種類（`zc_core::TokenKind` の名前）。省略すると文脈の地のトークン。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// 一致した後に積む文脈（1 つか、並べた順に積む配列）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<OneOrMany>,
    /// 一致した後に文脈を降ろす（`true` = 1 段、数 = その段数）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pop: Option<PopSpec>,
    /// 今の文脈を降ろしてから積む（`pop = true` + `push` と同じ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<OneOrMany>,
    /// 後ろにこれが続くときだけ一致する（続く部分は消費しない）。先読みの代わり。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub followed_by: Option<String>,
    /// 一致した文字列を `[words]` で引き、あればその種類にする。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<bool>,
    /// ほかの文脈の規則をここに展開する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s.clone()],
            OneOrMany::Many(v) => v.clone(),
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum PopSpec {
    Flag(bool),
    Count(u8),
}

impl PopSpec {
    pub fn count(&self) -> u8 {
        match self {
            PopSpec::Flag(b) => *b as u8,
            PopSpec::Count(n) => *n,
        }
    }
}

/// `theme.toml`。
#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ThemeFile {
    #[serde(default)]
    pub name: Option<String>,
    /// トークンの種類 → 色（`"#rrggbb"`）か `{ fg, bg, bold, italic, underline }`。
    pub tokens: BTreeMap<String, ThemeEntry>,
}

#[derive(serde::Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum ThemeEntry {
    Color(String),
    Style(ThemeStyle),
}

#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ThemeStyle {
    pub fg: String,
    #[serde(default)]
    pub bg: Option<String>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
}

/// `"#rrggbb"` → 0xRRGGBB。
pub fn parse_hex_color(s: &str) -> Option<u32> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    u32::from_str_radix(h, 16).ok()
}
