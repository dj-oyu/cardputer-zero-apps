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
}

/// JSON にする。
pub fn encode<T: Serialize>(msg: &T) -> String {
    serde_json::to_string(msg).expect("メッセージは必ず JSON にできる")
}

/// JSON から読む。知らない `type` は `Ok(None)`（無視する）。
pub fn decode<'a, T: Deserialize<'a>>(text: &'a str) -> Result<Option<T>, serde_json::Error> {
    let _ = text;
    todo!("W2: 知らない type を見分けて None にする")
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
        todo!("W2")
    }

    /// フレームを頭と中身に分ける。短い・知らない種類は `None`。
    pub fn split(frame: &[u8]) -> Option<(BinHeader, &[u8])> {
        let _ = frame;
        todo!("W2")
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
    #[ignore = "W2"]
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
}
