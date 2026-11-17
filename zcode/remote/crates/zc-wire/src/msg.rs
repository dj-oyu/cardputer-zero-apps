//! メッセージ（docs/stage3-design.md §5.2〜§5.4）。
//!
//! テキストフレーム 1 つ = JSON のオブジェクト 1 つ。`"type"` で種類を分ける。知らない
//! `type` と知らない項目は無視する（版を上げずに足せるように）。
//!
//! 型の定義はここで確定（雛形）。課題 W2 が書くのは [`encode`] / [`decode`]・
//! [`BinHeader`] の符号化と、正解の JSON との突き合わせ試験。

use serde::{Deserialize, Serialize};

/// ジョブの種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Build,
    Check,
    Test,
}

/// 言語の扱い。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Rust,
    C,
    Node,
}

/// ジョブのオプション。**job id の計算に入る**ので、項目を足すときは既定値を
/// 「書かない」（`skip_serializing_if`）にして、古い id を変えない。
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct JobOpts {
    /// 配布用のプロファイル（`--release`）。既定は F5 用（opt-level 2・LTO なし・strip）。
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub release: bool,
    /// 起点のファイル（木の根からの相対）。根に `Cargo.toml` / `package.json` が無い
    /// 「1 ファイルだけ」のプロジェクトと、C・TS の入口の指定に使う。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// 試験の絞り込み（`cargo test <filter>`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

/// 目録の 1 行。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// 木の根からの相対・`/` 区切り・正規化済み（`.`・`..`・空の要素・先頭の `/` なし）。
    #[serde(rename = "p")]
    pub path: String,
    /// 大きさ（バイト）。
    #[serde(rename = "s")]
    pub size: u64,
    /// 中身の SHA-256（16 進 64 桁・小文字）。
    #[serde(rename = "h")]
    pub sha256: String,
    /// 実行権限（所有者の x）。
    #[serde(rename = "x", default, skip_serializing_if = "core::ops::Not::not")]
    pub exec: bool,
}

/// 診断の重さ。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// 診断 1 件。行・桁は 0 始まり、桁は行の中の UTF-8 のバイト位置（受付が直してから送る）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diag {
    pub line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    /// 範囲の終わり `[行, 桁]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<(u32, u32)>,
    pub severity: Severity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
}

/// 1 ファイルぶんの診断。`path` が空文字列なら「位置なし」（ステータスバーにだけ出す）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiags {
    pub path: String,
    pub diags: Vec<Diag>,
}

/// 試験 1 件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCase {
    pub name: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

/// 成果物の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    /// aarch64 の実行ファイル。
    Exe,
    /// 1 ファイルにまとめた JS。
    Js,
}

/// ジョブの終わり方。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    /// コンパイルエラー・試験の失敗（`exit` は道具の終了コード）。
    Failed,
    Cancelled,
    /// 時間切れ（受付が止めた）。
    Timeout,
    /// ジョブを失くした（コンテナが止まった・受付が再起動した）。同じ依頼を出し直せばよい。
    Lost,
    /// 受付の中の失敗（道具が無い・ディスクがいっぱい）。
    Error,
}

/// `error` の `code`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// 約束の版が合わない。
    Version,
    /// JSON として読めない・順番がおかしい。
    Protocol,
    /// 上限を超えた（大きさ・ファイル数）。
    TooLarge,
    /// 回数・同時実行の上限（Worker / DO / 受付）。`retry_after_s` が付く。
    Limit,
    /// 知らないジョブ（`attach` / `cancel`）。
    UnknownJob,
    /// 木がそろっていない（`job` の前に `ready` を受けていない）、tree id / job id が合わない。
    BadTree,
    /// 中身のハッシュが合わない。
    BadBlob,
    /// ビルド係を起こせない（Cloudflare 側。コンテナの起動の失敗）。
    Unavailable,
    Internal,
}

/// LSP の問い合わせの種類（docs/lsp-design.md）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LspQuery {
    /// カーソルの位置の型と説明（`textDocument/hover`）。
    Hover,
    /// 定義へ（`textDocument/definition`）。
    Definition,
    /// 使っている所（`textDocument/references`、宣言も含む）。
    References,
    /// プロジェクト全体の名前の検索（`workspace/symbol`。`text` が検索の文字列）。
    Symbols,
    /// 補完（`textDocument/completion`）。
    Completion,
}

/// rust-analyzer の状態（`lsp_state`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LspPhase {
    /// 起動して、プロジェクトを読んでいる・索引を作っている（問い合わせには「準備中」と返す）。
    Loading,
    /// 最初の索引が済んだ（問い合わせに答えられる）。
    Ready,
    /// 起動できない・落ちた（`message` に理由）。次の `lsp_warm` で起動し直す。
    Failed,
}

/// LSP の答えの 1 項目（場所・補完の候補）。行・桁は 0 始まり、桁は行の中の UTF-8 の
/// バイト位置（診断と同じ。受付が UTF-16 から直してから送る）。
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LspItem {
    /// 木の根からの相対パス（プロジェクトの中の場所）。プロジェクトの外なら空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// プロジェクトの外の場所の見せ方（`core/src/option.rs`・`regex-1.11.1/src/lib.rs` の形）。
    /// 実機にはそのファイルが無いので、開かずに見せるだけ。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ext: String,
    #[serde(default)]
    pub line: u32,
    #[serde(default)]
    pub col: u32,
    /// 範囲の終わり `[行, 桁]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<(u32, u32)>,
    /// 名前（symbols の名前・補完の見出し）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// LSP の SymbolKind / CompletionItemKind の番号（0 = 無い）。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub kind: u32,
    /// 添え書き（場所ならその行の中身、symbols なら入れ物の名前、補完なら型）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// 補完で入れる文字列（`col` から今のカーソルまでを置き換える）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub insert: String,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// 実機 → ビルド係。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// 接続の最初に 1 回。
    Hello {
        v: u32,
        /// `zc-remote/0.1.0` の形。
        client: String,
    },
    /// 目録。返事は `need`。`project` はプロジェクトの根のパスの SHA-256 の先頭 16 桁
    /// （受付は作業の木をこの名前で分ける。パスそのものは送らない）。
    Manifest { project: String, tree: String, files: Vec<FileEntry> },
    /// ジョブを頼む。`tree` は `ready` を受けた木。`job` は [`crate::tree::job_id`]。
    Job {
        job: String,
        project: String,
        tree: String,
        kind: Kind,
        lang: Lang,
        #[serde(default)]
        opts: JobOpts,
        /// ログも欲しいか（保存時の check は `false`: 受付はログを残すが送らない）。
        #[serde(default)]
        log: bool,
    },
    /// つなぎ直した後、続きを受け取る。`log` / `artifact` は受け取り済みのバイト数。
    Attach {
        job: String,
        #[serde(default)]
        log: u64,
        #[serde(default)]
        artifact: u64,
    },
    /// 取り消す。受付は道具をプロセスグループごと止め、`done {status:"cancelled"}` を返す。
    Cancel { job: String },
    /// rust-analyzer を先に起こしておく（Rust のファイルを開いた）。`tree` は `ready` を受けた木、
    /// `path` は開いたファイル（木の根からの相対）。返事は `lsp_state`（状態が変わるたびにも届く）。
    LspWarm { project: String, tree: String, path: String },
    /// LSP の問い合わせ。`tree` は `ready` を受けた木（未保存の中身を重ねた目録）。`path` は木の
    /// 根からの相対、`line` / `col` は 0 始まり・`col` は UTF-8 のバイト位置。`text` は symbols の
    /// 検索の文字列。返事は同じ `id` の `lsp_result`（必ず 1 つ）。
    Lsp {
        id: u32,
        project: String,
        tree: String,
        query: LspQuery,
        #[serde(default)]
        path: String,
        #[serde(default)]
        line: u32,
        #[serde(default)]
        col: u32,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        text: String,
    },
}

/// ビルド係 → 実機。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// `hello` への返事。
    Hello {
        v: u32,
        /// `zc-buildd/0.1.0` の形。
        server: String,
    },
    /// 持っていない中身（SHA-256）。空なら全部そろっている（このあとすぐ `ready`）。
    /// 実機は**この順に**バイナリフレーム（[`BinKind::Blob`]、`id` = この配列の添字）で送る。
    Need { tree: String, blobs: Vec<String> },
    /// 木がそろった（全部の中身を受け取り、ハッシュを確かめ、作業の木に写した）。
    Ready { tree: String },
    /// ジョブを受け付けた。`cached` は、同じ job id の結果が残っていて実行しないとき。
    /// `queued` は前に並んでいるジョブの数。
    Accepted {
        job: String,
        #[serde(default)]
        cached: bool,
        #[serde(default)]
        queued: u32,
    },
    /// 実行を始めた。
    Started { job: String },
    /// ログ。`offset` はジョブのログ全体の中での位置（バイト）。50〜100ms か 4KB ごとに
    /// まとめて送る。`text` は UTF-8（壊れたバイトは U+FFFD に置き換え済み）。
    Log { job: String, offset: u64, text: String },
    /// 診断。1 ジョブにつき 1 回、`done` の前に送る。`complete` は道具が最後まで走ったとき
    /// だけ `true`（取り消し・時間切れは `false`）。
    Diagnostics {
        job: String,
        complete: bool,
        #[serde(default)]
        truncated: bool,
        files: Vec<FileDiags>,
    },
    /// 試験の結果（kind = test）。
    Tests { job: String, passed: u32, failed: u32, ignored: u32, cases: Vec<TestCase> },
    /// 実機のプロジェクトに書き戻すファイル（lock ファイルが無かった・古かったとき）。
    File { job: String, path: String, sha256: String, text: String },
    /// 成果物を送り始める。このあと [`BinKind::Artifact`] のバイナリフレーム（`id` = `chan`）が
    /// 続く。中身は zstd で圧縮済み（`zbytes`）。`sha256` / `bytes` は展開後のもの。
    /// `offset` は `attach` で途中から送るときの、圧縮後の位置。
    Artifact {
        job: String,
        chan: u32,
        kind: ArtifactKind,
        name: String,
        sha256: String,
        bytes: u64,
        zbytes: u64,
        #[serde(default)]
        offset: u64,
    },
    /// ジョブが終わった。成果物があれば、その最後のバイナリフレームの後に送る。
    Done {
        job: String,
        status: Status,
        #[serde(default)]
        exit: i32,
        /// 待ち・実行にかかった時間（ms）。
        #[serde(default)]
        queue_ms: u32,
        #[serde(default)]
        run_ms: u32,
    },
    /// 失敗。`job` が付けばそのジョブだけの話、無ければ接続全体（このあと切る）。
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        job: Option<String>,
        code: ErrorCode,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_s: Option<u32>,
    },
    /// rust-analyzer の状態（`lsp_warm` への返事と、状態が変わったとき）。`cpu_ms` は、この受付で
    /// 動かした rust-analyzer（子のプロセスも）が使った CPU の合計（ms。受付が起動してからの
    /// 通算で、減らない）。DO が費用の計りに使うので、`type` の直後に置く（覗くだけで読める）。
    /// `error` は DO が断ったとき（`limit`）だけ付く。
    LspState {
        #[serde(default)]
        cpu_ms: u64,
        project: String,
        state: LspPhase,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ErrorCode>,
    },
    /// `lsp` への返事。`error` があれば失敗（`unavailable` = rust-analyzer が準備中・動いていない、
    /// `limit` = 費用の上限、`bad-tree` = 木がそろっていない、`internal`）で、`message` に理由。
    /// 成功なら `text`（hover）と `items`（場所・候補）。何も無ければどちらも空。
    LspResult {
        #[serde(default)]
        cpu_ms: u64,
        id: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ErrorCode>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        message: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        items: Vec<LspItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_s: Option<u32>,
    },
}

/// JSON にする。
pub fn encode<T: Serialize>(msg: &T) -> String {
    serde_json::to_string(msg).expect("メッセージは必ず JSON にできる")
}

/// JSON から読む。知らない `type` は `Ok(None)`（無視する）。
///
/// 知っている `type` で項目が足りない・型が違うものは `Err`（約束の間違い）。オブジェクトで
/// ない・`type` が無い・`type` が文字列でないものも `Err`。知らない項目は無視する。
///
/// 見分け方: まず `T` として読む。失敗したら、`type` だけのオブジェクト `{"type": …}` を
/// `T` として読み直し、それが「知らない種類」で落ちるなら知らない `type` とみなす
/// （中の項目の知らない値、たとえば `"kind":"deploy"` と取り違えない）。
pub fn decode<'a, T: Deserialize<'a>>(text: &'a str) -> Result<Option<T>, serde_json::Error> {
    let err = match serde_json::from_str::<T>(text) {
        Ok(v) => return Ok(Some(v)),
        Err(e) => e,
    };
    if !err.is_data() {
        // 構文の誤り・途中で切れた JSON
        return Err(err);
    }
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(text) else {
        return Err(err);
    };
    let Some(serde_json::Value::String(ty)) = obj.get("type") else {
        return Err(err);
    };
    let mut probe = serde_json::Map::new();
    probe.insert("type".into(), serde_json::Value::String(ty.clone()));
    match T::deserialize(serde_json::Value::Object(probe)) {
        Err(e) if e.to_string().starts_with("unknown variant") => Ok(None),
        _ => Err(err),
    }
}

/// バイナリフレームの種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BinKind {
    /// 実機 → ビルド係: ファイルの中身（`id` = 直前の `need` の添字）。
    Blob = 1,
    /// ビルド係 → 実機: 成果物（`id` = `artifact` の `chan`）。
    Artifact = 2,
}

/// バイナリフレームの頭（16 バイト、リトルエンディアン）。このあとに中身が続く
/// （[`crate::limits::CHUNK_BYTES`] まで）。
///
/// ```text
///   u8  kind      BinKind
///   u8  flags     bit0 = この流れの最後のフレーム
///   u16 reserved  0
///   u32 id        流れの番号（need の添字 / chan）
///   u64 offset    この中身の、流れの中での位置
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinHeader {
    pub kind: BinKind,
    pub last: bool,
    pub id: u32,
    pub offset: u64,
}

impl BinHeader {
    pub const LEN: usize = 16;

    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[0] = self.kind as u8;
        b[1] = self.last as u8;
        b[4..8].copy_from_slice(&self.id.to_le_bytes());
        b[8..16].copy_from_slice(&self.offset.to_le_bytes());
        b
    }

    /// フレームを頭と中身に分ける。短い・知らない種類・中身が
    /// [`crate::limits::CHUNK_BYTES`] を超えるものは `None`。
    ///
    /// `flags` の bit0 以外と `reserved` は見ない（版を上げずに足せるように）。
    pub fn split(frame: &[u8]) -> Option<(BinHeader, &[u8])> {
        if frame.len() < Self::LEN || frame.len() - Self::LEN > crate::limits::CHUNK_BYTES {
            return None;
        }
        let kind = match frame[0] {
            1 => BinKind::Blob,
            2 => BinKind::Artifact,
            _ => return None,
        };
        let last = frame[1] & 1 != 0;
        let id = u32::from_le_bytes(frame[4..8].try_into().unwrap());
        let offset = u64::from_le_bytes(frame[8..16].try_into().unwrap());
        Some((BinHeader { kind, last, id, offset }, &frame[Self::LEN..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 形は serde の属性で決まるので、雛形の時点で確かめておく（docs の例と同じ文字列）。
    #[test]
    fn json_shape_is_fixed() {
        let m = ClientMsg::Job { job: "j1".into(), project: "p1".into(), tree: "t1".into(), kind: Kind::Check, lang: Lang::Rust, opts: JobOpts::default(), log: false };
        assert_eq!(encode(&m), r#"{"type":"job","job":"j1","project":"p1","tree":"t1","kind":"check","lang":"rust","opts":{},"log":false}"#);
        let d = ServerMsg::Diagnostics {
            job: "j1".into(),
            complete: true,
            truncated: false,
            files: vec![FileDiags { path: "src/main.rs".into(), diags: vec![Diag { line: 11, col: Some(4), end: None, severity: Severity::Error, code: Some("E0308".into()), message: "mismatched types".into() }] }],
        };
        assert_eq!(
            encode(&d),
            r#"{"type":"diagnostics","job":"j1","complete":true,"truncated":false,"files":[{"path":"src/main.rs","diags":[{"line":11,"col":4,"severity":"error","code":"E0308","message":"mismatched types"}]}]}"#
        );
        let e = ServerMsg::Error { job: None, code: ErrorCode::TooLarge, message: "x".into(), retry_after_s: None };
        assert_eq!(encode(&e), r#"{"type":"error","code":"too-large","message":"x"}"#);
        let f = FileEntry { path: "a.rs".into(), size: 3, sha256: "00".into(), exec: false };
        assert_eq!(encode(&f), r#"{"p":"a.rs","s":3,"h":"00"}"#);
    }

    #[test]
    fn unknown_type_is_ignored_and_bin_header_round_trips() {
        assert_eq!(decode::<ServerMsg>(r#"{"type":"future_thing","x":1}"#).unwrap(), None);
        assert!(decode::<ServerMsg>(r#"{"type":"ready","tree":"t","extra":1}"#).unwrap().is_some());
        let h = BinHeader { kind: BinKind::Artifact, last: true, id: 3, offset: 0x1_0000 };
        let b = h.encode();
        assert_eq!(b, [2, 1, 0, 0, 3, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
        let mut frame = b.to_vec();
        frame.extend_from_slice(b"xyz");
        assert_eq!(BinHeader::split(&frame), Some((h, &b"xyz"[..])));
        assert_eq!(BinHeader::split(&frame[..10]), None);
    }

    /// 文字列 → 型 → 文字列が元に戻る（正解の JSON と 1 文字も違わない）。
    fn same_both_ways<T: Serialize + for<'a> Deserialize<'a> + PartialEq + core::fmt::Debug>(json: &str, msg: T) {
        assert_eq!(encode(&msg), json);
        assert_eq!(decode::<T>(json).unwrap(), Some(msg));
    }

    /// docs/stage3-design.md §5.4 の例（「…」は具体的な値に置き換えた）。
    #[test]
    fn design_doc_example_session() {
        let (p, t, j) = ("3f9a000000000000", "b1c2".repeat(16), "7f3a".repeat(8));
        let h = |c: &str| c.repeat(64);
        same_both_ways(r#"{"type":"hello","v":1,"client":"zc-remote/0.1.0"}"#, ClientMsg::Hello { v: 1, client: "zc-remote/0.1.0".into() });
        same_both_ways(r#"{"type":"hello","v":1,"server":"zc-buildd/0.1.0"}"#, ServerMsg::Hello { v: 1, server: "zc-buildd/0.1.0".into() });
        same_both_ways(
            &format!(r#"{{"type":"manifest","project":"{p}","tree":"{t}","files":[{{"p":"Cargo.toml","s":120,"h":"{}"}},{{"p":"src/main.rs","s":812,"h":"{}","x":true}}]}}"#, h("1"), h("2")),
            ClientMsg::Manifest {
                project: p.into(),
                tree: t.clone(),
                files: vec![
                    FileEntry { path: "Cargo.toml".into(), size: 120, sha256: h("1"), exec: false },
                    FileEntry { path: "src/main.rs".into(), size: 812, sha256: h("2"), exec: true },
                ],
            },
        );
        same_both_ways(&format!(r#"{{"type":"need","tree":"{t}","blobs":["{}"]}}"#, h("9")), ServerMsg::Need { tree: t.clone(), blobs: vec![h("9")] });
        same_both_ways(&format!(r#"{{"type":"ready","tree":"{t}"}}"#), ServerMsg::Ready { tree: t.clone() });
        same_both_ways(
            &format!(r#"{{"type":"job","job":"{j}","project":"{p}","tree":"{t}","kind":"build","lang":"rust","opts":{{}},"log":true}}"#),
            ClientMsg::Job { job: j.clone(), project: p.into(), tree: t.clone(), kind: Kind::Build, lang: Lang::Rust, opts: JobOpts::default(), log: true },
        );
        same_both_ways(&format!(r#"{{"type":"accepted","job":"{j}","cached":false,"queued":0}}"#), ServerMsg::Accepted { job: j.clone(), cached: false, queued: 0 });
        same_both_ways(&format!(r#"{{"type":"started","job":"{j}"}}"#), ServerMsg::Started { job: j.clone() });
        same_both_ways(
            &format!(r#"{{"type":"log","job":"{j}","offset":0,"text":"   Compiling foo v0.1.0\n"}}"#),
            ServerMsg::Log { job: j.clone(), offset: 0, text: "   Compiling foo v0.1.0\n".into() },
        );
        same_both_ways(
            &format!(r#"{{"type":"diagnostics","job":"{j}","complete":true,"truncated":false,"files":[]}}"#),
            ServerMsg::Diagnostics { job: j.clone(), complete: true, truncated: false, files: vec![] },
        );
        same_both_ways(
            &format!(r#"{{"type":"artifact","job":"{j}","chan":1,"kind":"exe","name":"foo","sha256":"{}","bytes":912384,"zbytes":301220,"offset":0}}"#, h("a")),
            ServerMsg::Artifact { job: j.clone(), chan: 1, kind: ArtifactKind::Exe, name: "foo".into(), sha256: h("a"), bytes: 912384, zbytes: 301220, offset: 0 },
        );
        same_both_ways(
            &format!(r#"{{"type":"done","job":"{j}","status":"ok","exit":0,"queue_ms":3,"run_ms":4210}}"#),
            ServerMsg::Done { job: j.clone(), status: Status::Ok, exit: 0, queue_ms: 3, run_ms: 4210 },
        );
    }

    #[test]
    fn the_other_messages() {
        same_both_ways(r#"{"type":"attach","job":"j","log":4096,"artifact":65536}"#, ClientMsg::Attach { job: "j".into(), log: 4096, artifact: 65536 });
        same_both_ways(r#"{"type":"cancel","job":"j"}"#, ClientMsg::Cancel { job: "j".into() });
        same_both_ways(
            r#"{"type":"job","job":"j","project":"p","tree":"t","kind":"test","lang":"node","opts":{"release":true,"entry":"src/a.ts","filter":"x"},"log":true}"#,
            ClientMsg::Job {
                job: "j".into(),
                project: "p".into(),
                tree: "t".into(),
                kind: Kind::Test,
                lang: Lang::Node,
                opts: JobOpts { release: true, entry: Some("src/a.ts".into()), filter: Some("x".into()) },
                log: true,
            },
        );
        same_both_ways(
            r#"{"type":"tests","job":"j","passed":1,"failed":1,"ignored":0,"cases":[{"name":"a","passed":true},{"name":"b","passed":false,"path":"src/lib.rs","line":3,"message":"assert"}]}"#,
            ServerMsg::Tests {
                job: "j".into(),
                passed: 1,
                failed: 1,
                ignored: 0,
                cases: vec![
                    TestCase { name: "a".into(), passed: true, path: None, line: None, message: String::new() },
                    TestCase { name: "b".into(), passed: false, path: Some("src/lib.rs".into()), line: Some(3), message: "assert".into() },
                ],
            },
        );
        same_both_ways(
            r##"{"type":"file","job":"j","path":"Cargo.lock","sha256":"00","text":"# lock\n"}"##,
            ServerMsg::File { job: "j".into(), path: "Cargo.lock".into(), sha256: "00".into(), text: "# lock\n".into() },
        );
        same_both_ways(
            r#"{"type":"error","job":"j","code":"limit","message":"1 日の上限","retry_after_s":60}"#,
            ServerMsg::Error { job: Some("j".into()), code: ErrorCode::Limit, message: "1 日の上限".into(), retry_after_s: Some(60) },
        );
        same_both_ways(
            r#"{"type":"diagnostics","job":"j","complete":false,"truncated":true,"files":[{"path":"","diags":[{"line":0,"col":2,"end":[0,5],"severity":"warning","message":"m"}]}]}"#,
            ServerMsg::Diagnostics {
                job: "j".into(),
                complete: false,
                truncated: true,
                files: vec![FileDiags { path: String::new(), diags: vec![Diag { line: 0, col: Some(2), end: Some((0, 5)), severity: Severity::Warning, code: None, message: "m".into() }] }],
            },
        );
        for (s, c) in [
            ("version", ErrorCode::Version),
            ("protocol", ErrorCode::Protocol),
            ("too-large", ErrorCode::TooLarge),
            ("unknown-job", ErrorCode::UnknownJob),
            ("bad-tree", ErrorCode::BadTree),
            ("bad-blob", ErrorCode::BadBlob),
            ("unavailable", ErrorCode::Unavailable),
            ("internal", ErrorCode::Internal),
        ] {
            same_both_ways(&format!(r#"{{"type":"error","code":"{s}","message":""}}"#), ServerMsg::Error { job: None, code: c, message: String::new(), retry_after_s: None });
        }
        for (s, st) in [("failed", Status::Failed), ("cancelled", Status::Cancelled), ("timeout", Status::Timeout), ("lost", Status::Lost), ("error", Status::Error)] {
            same_both_ways(&format!(r#"{{"type":"done","job":"j","status":"{s}","exit":1,"queue_ms":0,"run_ms":0}}"#), ServerMsg::Done { job: "j".into(), status: st, exit: 1, queue_ms: 0, run_ms: 0 });
        }
    }

    /// LSP のメッセージ（docs/lsp-design.md の例と同じ文字列）。
    #[test]
    fn lsp_messages() {
        same_both_ways(r#"{"type":"lsp_warm","project":"p","tree":"t","path":"src/main.rs"}"#, ClientMsg::LspWarm { project: "p".into(), tree: "t".into(), path: "src/main.rs".into() });
        same_both_ways(
            r#"{"type":"lsp","id":7,"project":"p","tree":"t","query":"hover","path":"src/main.rs","line":3,"col":12}"#,
            ClientMsg::Lsp { id: 7, project: "p".into(), tree: "t".into(), query: LspQuery::Hover, path: "src/main.rs".into(), line: 3, col: 12, text: String::new() },
        );
        same_both_ways(
            r#"{"type":"lsp","id":8,"project":"p","tree":"t","query":"symbols","path":"","line":0,"col":0,"text":"Foo"}"#,
            ClientMsg::Lsp { id: 8, project: "p".into(), tree: "t".into(), query: LspQuery::Symbols, path: String::new(), line: 0, col: 0, text: "Foo".into() },
        );
        for (s, q) in [("definition", LspQuery::Definition), ("references", LspQuery::References), ("completion", LspQuery::Completion)] {
            let m = decode::<ClientMsg>(&format!(r#"{{"type":"lsp","id":1,"project":"p","tree":"t","query":"{s}"}}"#)).unwrap();
            assert!(matches!(m, Some(ClientMsg::Lsp { query, .. }) if query == q), "{s}");
        }
        same_both_ways(r#"{"type":"lsp_state","cpu_ms":1500,"project":"p","state":"loading"}"#, ServerMsg::LspState { cpu_ms: 1500, project: "p".into(), state: LspPhase::Loading, message: String::new(), error: None });
        same_both_ways(
            r#"{"type":"lsp_state","cpu_ms":0,"project":"p","state":"failed","message":"上限","error":"limit"}"#,
            ServerMsg::LspState { cpu_ms: 0, project: "p".into(), state: LspPhase::Failed, message: "上限".into(), error: Some(ErrorCode::Limit) },
        );
        same_both_ways(r#"{"type":"lsp_state","cpu_ms":2,"project":"p","state":"ready"}"#, ServerMsg::LspState { cpu_ms: 2, project: "p".into(), state: LspPhase::Ready, message: String::new(), error: None });
        same_both_ways(
            r#"{"type":"lsp_result","cpu_ms":9,"id":7,"text":"fn len(&self) -> usize"}"#,
            ServerMsg::LspResult { cpu_ms: 9, id: 7, error: None, message: String::new(), text: "fn len(&self) -> usize".into(), items: vec![], retry_after_s: None },
        );
        same_both_ways(
            r#"{"type":"lsp_result","cpu_ms":9,"id":7,"items":[{"path":"src/a.rs","line":4,"col":7,"end":[4,10],"detail":"fn foo() {}"},{"ext":"core/src/option.rs","line":570,"col":0,"name":"Option","kind":10,"insert":"Some"}]}"#,
            ServerMsg::LspResult {
                cpu_ms: 9,
                id: 7,
                error: None,
                message: String::new(),
                text: String::new(),
                items: vec![
                    LspItem { path: "src/a.rs".into(), line: 4, col: 7, end: Some((4, 10)), detail: "fn foo() {}".into(), ..Default::default() },
                    LspItem { ext: "core/src/option.rs".into(), line: 570, name: "Option".into(), kind: 10, insert: "Some".into(), ..Default::default() },
                ],
                retry_after_s: None,
            },
        );
        same_both_ways(
            r#"{"type":"lsp_result","cpu_ms":0,"id":1,"error":"limit","message":"上限","retry_after_s":60}"#,
            ServerMsg::LspResult { cpu_ms: 0, id: 1, error: Some(ErrorCode::Limit), message: "上限".into(), text: String::new(), items: vec![], retry_after_s: Some(60) },
        );
        // cpu_ms が無くても読める（DO が作る返事）
        assert!(matches!(decode::<ServerMsg>(r#"{"type":"lsp_result","id":3,"error":"unavailable"}"#).unwrap(), Some(ServerMsg::LspResult { id: 3, cpu_ms: 0, .. })));
        // 知らない query は約束の間違い（知らない type ではない）
        assert!(decode::<ClientMsg>(r#"{"type":"lsp","id":1,"project":"p","tree":"t","query":"rename"}"#).is_err());
    }

    #[test]
    fn defaults_and_extra_fields() {
        // 省いてよい項目
        assert_eq!(
            decode::<ClientMsg>(r#"{"type":"job","job":"j","project":"p","tree":"t","kind":"check","lang":"c"}"#).unwrap(),
            Some(ClientMsg::Job { job: "j".into(), project: "p".into(), tree: "t".into(), kind: Kind::Check, lang: Lang::C, opts: JobOpts::default(), log: false })
        );
        assert_eq!(decode::<ClientMsg>(r#"{"type":"attach","job":"j"}"#).unwrap(), Some(ClientMsg::Attach { job: "j".into(), log: 0, artifact: 0 }));
        assert_eq!(
            decode::<ServerMsg>(r#"{"type":"done","job":"j","status":"cancelled"}"#).unwrap(),
            Some(ServerMsg::Done { job: "j".into(), status: Status::Cancelled, exit: 0, queue_ms: 0, run_ms: 0 })
        );
        // 知らない項目は、どの深さでも無視する
        assert_eq!(
            decode::<ClientMsg>(r#"{"later":[1,{"a":null}],"type":"manifest","project":"p","tree":"t","files":[{"p":"a","s":1,"h":"00","mode":420}]}"#).unwrap(),
            Some(ClientMsg::Manifest { project: "p".into(), tree: "t".into(), files: vec![FileEntry { path: "a".into(), size: 1, sha256: "00".into(), exec: false }] })
        );
        assert!(decode::<ClientMsg>(r#"{"type":"job","job":"j","project":"p","tree":"t","kind":"build","lang":"rust","opts":{"future":true}}"#)
            .unwrap()
            .is_some());
    }

    #[test]
    fn unknown_types_per_direction() {
        // 向きが逆のメッセージは、その向きでは知らない type
        assert_eq!(decode::<ClientMsg>(r#"{"type":"need","tree":"t","blobs":[]}"#).unwrap(), None);
        assert_eq!(decode::<ServerMsg>(r#"{"type":"cancel","job":"j"}"#).unwrap(), None);
        // 大文字小文字は区別する
        assert_eq!(decode::<ServerMsg>(r#"{"type":"Ready","tree":"t"}"#).unwrap(), None);
        // 知らない type なら、ほかの項目が何であっても無視する
        assert_eq!(decode::<ServerMsg>(r#"{"type":"x","tree":5,"job":[]}"#).unwrap(), None);
        assert_eq!(decode::<ServerMsg>(r#"{"type":""}"#).unwrap(), None);
    }

    #[test]
    fn broken_messages_are_errors() {
        for bad in [
            "",
            "{",
            r#"{"type":"ready","tree":"t""#,
            "[]",
            "null",
            r#""ready""#,
            r#"{"tree":"t"}"#,
            r#"{"type":5}"#,
            r#"{"type":null}"#,
            // 知っている type で項目が足りない・型が違う
            r#"{"type":"ready"}"#,
            r#"{"type":"ready","tree":5}"#,
            r#"{"type":"done","job":"j","status":"exploded"}"#,
            r#"{"type":"error","code":"nope","message":""}"#,
            // 中の項目の知らない値（type と同じ綴りでも、知らない type と取り違えない）
            r#"{"type":"job","job":"j","project":"p","tree":"t","kind":"job","lang":"rust"}"#,
            r#"{"type":"job","job":"j","project":"p","tree":"t","kind":"deploy","lang":"rust"}"#,
            r#"{"type":"job","job":"j","project":"p","tree":"t","kind":"build","lang":"go"}"#,
            // 数の範囲
            r#"{"type":"attach","job":"j","log":-1}"#,
            r#"{"type":"hello","v":4294967296,"client":"c"}"#,
            r#"{"type":"hello","v":"1","client":"c"}"#,
            // 後ろのごみ
            r#"{"type":"cancel","job":"j"} x"#,
        ] {
            // 向きが違えば「知らない type」で None になりうるので、少なくとも片方で Err
            assert!(decode::<ClientMsg>(bad).is_err() || decode::<ServerMsg>(bad).is_err(), "{bad:?}");
        }
        // 向きを決めて確かめる
        assert!(decode::<ServerMsg>(r#"{"type":"ready"}"#).is_err());
        assert!(decode::<ClientMsg>(r#"{"type":"job","job":"j","project":"p","tree":"t","kind":"job","lang":"rust"}"#).is_err());
        assert!(decode::<ClientMsg>(r#"{"type":5}"#).is_err());
    }

    /// 版が違う hello も読める（断るのは受ける側の仕事: `error {code:"version"}`）。
    #[test]
    fn version_mismatch_is_visible_to_the_caller() {
        let m = decode::<ClientMsg>(r#"{"type":"hello","v":2,"client":"zc-remote/9.0.0","caps":["zstd"]}"#).unwrap();
        assert_eq!(m, Some(ClientMsg::Hello { v: 2, client: "zc-remote/9.0.0".into() }));
        assert!(matches!(m, Some(ClientMsg::Hello { v, .. }) if v != crate::PROTOCOL_VERSION));
        same_both_ways(
            r#"{"type":"error","code":"version","message":"v2 は知らない"}"#,
            ServerMsg::Error { job: None, code: ErrorCode::Version, message: "v2 は知らない".into(), retry_after_s: None },
        );
    }

    #[test]
    fn json_escapes_survive() {
        let text = "a\"b\\c\n\t\u{1}日本語\u{1F600}";
        let m = ServerMsg::Log { job: "j".into(), offset: 7, text: text.into() };
        let s = encode(&m);
        assert!(!s.contains('\n'));
        assert_eq!(decode::<ServerMsg>(&s).unwrap(), Some(m));
        // \u の書き方で来ても同じ
        assert_eq!(
            decode::<ServerMsg>(r#"{"type":"ready","tree":"t"}"#).unwrap(),
            Some(ServerMsg::Ready { tree: "t".into() })
        );
        assert_eq!(
            decode::<ServerMsg>(r#"{"type":"ready","tree":"t"}"#).unwrap(),
            Some(ServerMsg::Ready { tree: "t".into() })
        );
    }

    #[test]
    fn bin_header_edges() {
        let h = BinHeader { kind: BinKind::Blob, last: false, id: u32::MAX, offset: u64::MAX };
        assert_eq!(h.encode(), [1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(BinHeader::split(&h.encode()), Some((h, &[][..])));
        // 頭だけちょうど・1 バイト足りない
        assert!(BinHeader::split(&[1; 16]).is_some());
        assert_eq!(BinHeader::split(&[1; 15]), None);
        assert_eq!(BinHeader::split(&[]), None);
        // 知らない種類
        for k in [0u8, 3, 255] {
            let mut f = h.encode();
            f[0] = k;
            assert_eq!(BinHeader::split(&f), None, "{k}");
        }
        // flags の bit0 以外と reserved は見ない
        let mut f = h.encode();
        f[1] = 0xfe;
        f[2] = 9;
        f[3] = 9;
        assert_eq!(BinHeader::split(&f).unwrap().0, h);
        f[1] = 0x03;
        assert!(BinHeader::split(&f).unwrap().0.last);
        // 中身の上限
        let mut f = h.encode().to_vec();
        f.resize(BinHeader::LEN + crate::limits::CHUNK_BYTES, 0);
        assert_eq!(BinHeader::split(&f).unwrap().1.len(), crate::limits::CHUNK_BYTES);
        f.push(0);
        assert_eq!(BinHeader::split(&f), None);
    }
}
