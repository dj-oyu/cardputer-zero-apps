// キー配列ファイル（`zcode/keymap/*.toml`）の形。
//
// このファイルは `build.rs`（ビルド時に埋め込み用の表を作る）と、ライブラリの
// `parse` 機能（テスト・道具用に実行時に TOML を読む）の両方から `include!` される。
// 意味の検査（キーの名前、重複など）は `keymap.rs` の `Keymap::from_table` が行う。

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawFile {
    pub version: u32,
    #[serde(default)]
    pub device: Option<String>,
    pub modifiers: RawMods,
    pub keys: Vec<RawKey>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawMods {
    #[serde(default)]
    pub shift: Vec<u16>,
    #[serde(default)]
    pub ctrl: Vec<u16>,
    #[serde(default)]
    pub alt: Vec<u16>,
    #[serde(default)]
    pub sym: Vec<u16>,
    #[serde(default, rename = "fn")]
    pub fn_: Vec<u16>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawKey {
    pub code: u16,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub shift: Option<String>,
    #[serde(default)]
    pub layer: Option<String>,
    #[serde(default)]
    pub pos: Option<[u8; 2]>,
    #[serde(default)]
    pub provisional: bool,
}
