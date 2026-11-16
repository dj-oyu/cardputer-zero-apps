//! zcode（と `zc-remote` の CLI）⇔ `zc-remote serve` の間の約束（docs/stage3-design.md §8.1）。
//!
//! 雛形（段階 3 の設計）。型と関数の形はここで確定。符号化の中身は課題 I1 が書く。
//!
//! - 通り道は UNIX ソケット（[`socket_path`]）。zcode 側に JSON も TLS も入れないための層
//! - 1 メッセージ = 12 バイトの頭 + 本体。全部リトルエンディアン
//!
//! ```text
//!   u32 len      このあとに続くバイト数（頭の残り 8 バイト + 本体。上限 MAX_FRAME）
//!   u16 kind     メッセージの種類（Request は 0x00xx、Response は 0x80xx）
//!   u16 flags    予約（0）
//!   u32 req      依頼の番号（依頼した側が決める。0 は「自分の依頼ではない知らせ」）
//!   ...          本体（種類ごとに固定の並び。docs/stage3-design.md §8.2 の表）
//! ```
//!
//! - 文字列・パスは `u16 長さ + バイト列`（UTF-8。パスは OS のバイト列のまま）
//! - `Option<u32>` は `u32::MAX` が「無い」。真偽は `u8`（0 / 1）
//! - 知らない `kind` は読み飛ばす（`len` があるので飛ばせる）。版が違えば [`Response::Hello`]
//!   のあとで切る
//!
//! この crate は I/O を持たない（バイト列 ⇔ 型だけ）。ソケットを読む・書くのは
//! `zcode/src/remote.rs`（専用スレッド）と `zc-remote`（serve / CLI）。

use std::path::PathBuf;
use zc_core::Diagnostic;

/// 約束の版。合わない相手とは話さない。
pub const IPC_VERSION: u16 = 1;
/// 1 メッセージの上限（頭を含まない）。超えるログ・診断は送る側が分ける。
pub const MAX_FRAME: usize = 256 * 1024;
/// 頭の長さ。
pub const HEADER_LEN: usize = 12;

/// ソケットの場所: `$XDG_RUNTIME_DIR/zcode/remote.sock`
/// （`XDG_RUNTIME_DIR` が無ければ `/run/user/<uid>/zcode/remote.sock`）。
pub fn socket_path() -> PathBuf {
    todo!("I1")
}

/// 依頼の番号。接続ごとに、依頼した側が 1 から順に振る。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReqId(pub u32);

impl ReqId {
    /// 「自分の依頼ではない」知らせに付く番号（ほかの接続が頼んだジョブの診断など）。
    pub const NONE: ReqId = ReqId(0);
}

/// serve が振るジョブの通し番号（serve が動いている間は重ならない）。同じジョブに
/// ついての知らせ（[`Response::Phase`]・[`Response::Diagnostics`]・[`Response::Done`]）を
/// 突き合わせるのに使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobTag(pub u32);

/// ジョブの種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum JobKind {
    /// 実機用の実行ファイル（または 1 ファイルの JS）を作る。
    Build = 1,
    /// 型検査だけ。
    Check = 2,
    /// 試験をビルド係で実行する。
    Test = 3,
}

/// ビルド係での言語の扱い（`language.toml` の `remote`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RemoteLang {
    Rust = 1,
    C = 2,
    /// JavaScript / TypeScript（npm の依存があるプロジェクト）。
    Node = 3,
}

impl RemoteLang {
    /// `language.toml` の `remote` の値から。
    pub fn parse(s: &str) -> Option<RemoteLang> {
        match s {
            "rust" => Some(RemoteLang::Rust),
            "c" => Some(RemoteLang::C),
            "node" => Some(RemoteLang::Node),
            _ => None,
        }
    }
}

/// つないできた相手の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ClientKind {
    /// zcode の専用スレッド。ほかの接続が頼んだジョブの診断も受け取る。
    Editor = 1,
    /// `zc-remote build|check|test|run`。自分の依頼の知らせだけ受け取る。
    Cli = 2,
}

/// 使っているビルド係。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Backend {
    /// まだ決まっていない（つないでいない）。
    Unknown = 0,
    /// PC の開発コンテナ（Tailscale、TLS なし）。
    Pc = 1,
    /// Cloudflare（Access → Worker → Durable Object → コンテナ）。
    Cloudflare = 2,
}

/// ジョブの進み具合。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    /// つないでいる（DNS・TCP・TLS・Upgrade）。
    Connecting = 1,
    /// 木を歩いて目録を作り終えた。**メインはこの知らせで、開いているタブの「保存済みの版」を
    /// 控える**（診断の基準の版。docs/stage3-design.md §9.3）。
    Scanned = 2,
    /// 足りない中身を送っている（`done` / `total` はバイト数）。
    Uploading = 3,
    /// ビルド係の順番待ち。
    Queued = 4,
    /// ビルド係で実行中。
    Running = 5,
    /// 実行ファイルを受け取っている（`done` / `total` はバイト数）。
    Downloading = 6,
}

/// ジョブの終わり方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DoneStatus {
    /// 成功（build は実行ファイルを置いた、check / test はエラーなし）。
    Ok = 0,
    /// ビルド・型検査・試験が失敗した（診断か試験の結果が先に届いている）。
    Failed = 1,
    /// 取り消した（新しい依頼に置き換わった、[`Request::Cancel`]）。
    Cancelled = 2,
    /// このファイルはビルド係の対象ではない（プロジェクトの根が無い、npm の依存が無い）。
    /// zcode は実機の `check` に回す。
    NotApplicable = 3,
    /// ネットワークが無い・届かない。build で前の版が残っていれば
    /// [`Response::Artifact`]（`stale = true`）が先に届く。
    Offline = 4,
    /// 認証できない（トークンが無い・権限が広すぎる・断られた）。
    Auth = 5,
    /// 時計が合っていない（証明書の検証が「まだ有効でない」で落ちた）。
    Clock = 6,
    /// ビルド係の上限に当たった（回数・同時実行・大きさ）。
    Limit = 7,
    /// ビルド係の中の失敗・約束の食い違い・時間切れ。
    Error = 8,
}

/// 受け取った成果物の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ArtifactKind {
    /// aarch64 の実行ファイル（そのまま実行する）。
    Exe = 1,
    /// 1 ファイルにまとめた JS（`llrt <パス>` で実行する）。
    Js = 2,
}

/// [`Request::Submit`] の指定（ビットの和）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SubmitFlags(pub u16);

impl SubmitFlags {
    pub const NONE: SubmitFlags = SubmitFlags(0);
    /// ログ（[`Response::Log`]）も欲しい（CLI と「試験を実行」）。保存時の check は付けない。
    pub const WANT_LOG: SubmitFlags = SubmitFlags(1 << 0);
    /// 配布用のプロファイルで作る（`--release` 相当）。F5 は付けない。
    pub const RELEASE: SubmitFlags = SubmitFlags(1 << 1);
    /// 接続が温まっているときだけ頼む（冷えていたら [`DoneStatus::Offline`] ですぐ返す。
    /// 設定「保存時の check: 接続が温かいときだけ」用）。
    pub const ONLY_IF_WARM: SubmitFlags = SubmitFlags(1 << 2);
    /// ネットワークが無いとき、前に成功した実行ファイルで代用してよい（build）。
    pub const ALLOW_STALE: SubmitFlags = SubmitFlags(1 << 3);

    pub const fn contains(self, f: SubmitFlags) -> bool {
        self.0 & f.0 == f.0
    }
}

/// zcode / CLI → serve。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// 接続の最初に 1 回。
    Hello { version: u16, client: ClientKind },
    /// 先につないでおく（プロジェクトを開いた・最初の保存）。返事は [`Response::Status`]。
    Warm { file: PathBuf },
    /// ジョブを頼む。`file` は保存したファイルの絶対パス（プロジェクトの根は serve が探す）。
    /// 同じプロジェクト・同じ種類の前の依頼が動いていれば、serve が取り消してから始める
    /// （build は build だけ、check は check だけを取り消す）。
    Submit { req: ReqId, kind: JobKind, lang: RemoteLang, flags: SubmitFlags, file: PathBuf },
    /// 依頼を取り消す。
    Cancel { req: ReqId },
    /// 状態を聞く。返事は [`Response::Status`]。
    Status,
    /// serve を終わらせる（試験用）。
    Shutdown,
}

/// 1 ファイルぶんの診断（[`Response::Diagnostics`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiags {
    /// プロジェクトの根からの相対・`/` 区切り。
    pub path: String,
    pub diags: Vec<Diagnostic>,
}

/// 試験 1 件の結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestCase {
    /// 試験の名前（`mod::tests::name`）。
    pub name: String,
    pub passed: bool,
    /// 落ちた場所（プロジェクト相対のパスと 0 始まりの行。分かれば）。
    pub location: Option<(String, u32)>,
    /// 落ちたときのメッセージ（上限 2KB。超えたら末尾を切る）。
    pub message: String,
}

/// 試験の結果。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TestReport {
    pub passed: u32,
    pub failed: u32,
    pub ignored: u32,
    /// 落ちた試験（全部）と、通った試験（多ければ先頭の 200 件まで）。
    pub cases: Vec<TestCase>,
}

/// serve → zcode / CLI。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    /// [`Request::Hello`] への返事。
    Hello { version: u16 },
    /// [`Request::Warm`] / [`Request::Status`] への返事。
    Status { backend: Backend, connected: bool, running_jobs: u16 },
    /// ジョブの進み具合。serve は同じジョブについて最短 33ms に 1 回しか送らない
    /// （段階が変わったときはすぐ送る）。
    Phase { req: ReqId, tag: JobTag, root: PathBuf, phase: Phase, done: u32, total: u32 },
    /// ログ（UTF-8 とは限らないバイト列。端末にそのまま流せる）。serve は 33ms か 4KB ごとに
    /// まとめる。
    Log { req: ReqId, tag: JobTag, bytes: Vec<u8> },
    /// 診断。1 ジョブにつき 1 回（多ければ `MAX_FRAME` に収まるように分け、最後だけ
    /// `last = true`）。`complete` は取り消し・失敗の結果では `false`。
    Diagnostics { req: ReqId, tag: JobTag, root: PathBuf, complete: bool, last: bool, files: Vec<FileDiags> },
    /// 試験の結果。
    Tests { req: ReqId, tag: JobTag, root: PathBuf, report: TestReport },
    /// 成果物を tmpfs に置いた（SHA-256 を確かめ、実行権限を付け、rename した後）。
    /// `stale` は、ネットワークが無くて前に成功した版を戻したとき。
    Artifact { req: ReqId, tag: JobTag, kind: ArtifactKind, stale: bool, path: PathBuf },
    /// ビルド係が作った lock ファイルをプロジェクトに書き戻した（知らせるだけ）。
    FileWritten { req: ReqId, tag: JobTag, path: PathBuf },
    /// ジョブが終わった。必ず最後に 1 回届く。`message` は人に見せる 1 行（空でもよい）。
    Done { req: ReqId, tag: JobTag, status: DoneStatus, exit: i32, backend: Backend, elapsed_ms: u32, message: String },
}

/// 読めなかった理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// 長さが足りない・余る。
    Length,
    /// 知らない種類（読み飛ばしてよい）。
    UnknownKind(u16),
    /// 値が範囲の外（列挙の番号、UTF-8 でない文字列）。
    Invalid(&'static str),
    /// `len` が [`MAX_FRAME`] を超えている（相手が壊れている。切る）。
    TooLarge(u32),
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for DecodeError {}

/// `out` の末尾に 1 メッセージ（頭つき）を足す。
pub fn encode_request(msg: &Request, out: &mut Vec<u8>) {
    let _ = (msg, out);
    todo!("I1")
}

/// `out` の末尾に 1 メッセージ（頭つき）を足す。
pub fn encode_response(msg: &Response, out: &mut Vec<u8>) {
    let _ = (msg, out);
    todo!("I1")
}

/// 1 メッセージ（頭の `len` より後ろ = `kind` から本体の終わりまで）を読む。
pub fn decode_request(frame: &[u8]) -> Result<Request, DecodeError> {
    let _ = frame;
    todo!("I1")
}

/// 1 メッセージ（`kind` から本体の終わりまで）を読む。
pub fn decode_response(frame: &[u8]) -> Result<Response, DecodeError> {
    let _ = frame;
    todo!("I1")
}

/// ソケットから読んだバイト列を溜めて、メッセージごとに切り出す。
#[derive(Debug, Default)]
pub struct FrameReader {
    _private: (),
}

impl FrameReader {
    pub fn new() -> FrameReader {
        FrameReader::default()
    }

    /// 読んだバイト列を足す。
    pub fn push(&mut self, bytes: &[u8]) {
        let _ = bytes;
        todo!("I1")
    }

    /// そろっているメッセージを 1 つ返す（`kind` から本体の終わりまで。次の `push` /
    /// `next_frame` まで有効）。足りなければ `Ok(None)`。
    pub fn next_frame(&mut self) -> Result<Option<&[u8]>, DecodeError> {
        todo!("I1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_names_match_language_toml() {
        assert_eq!(RemoteLang::parse("rust"), Some(RemoteLang::Rust));
        assert_eq!(RemoteLang::parse("node"), Some(RemoteLang::Node));
        assert_eq!(RemoteLang::parse("python"), None);
        assert!(SubmitFlags(0b101).contains(SubmitFlags::ONLY_IF_WARM));
        assert!(!SubmitFlags::NONE.contains(SubmitFlags::WANT_LOG));
    }

    // 受け入れ試験（課題 I1）: 全部の種類の往復、正解のバイト列（docs/stage3-design.md §8.2 の例）、
    // 半端に届いたバイト列の切り出し、知らない kind の読み飛ばし、MAX_FRAME 超えの拒否。
    #[test]
    #[ignore = "I1"]
    fn submit_round_trips_and_matches_golden_bytes() {
        let msg = Request::Submit { req: ReqId(7), kind: JobKind::Check, lang: RemoteLang::Rust, flags: SubmitFlags::NONE, file: "/a/b.rs".into() };
        let mut buf = Vec::new();
        encode_request(&msg, &mut buf);
        // len=21, kind=0x0003, flags=0, req=7, kind=2, lang=1, flags=0, 長さ 7 + "/a/b.rs"
        assert_eq!(buf, [21, 0, 0, 0, 3, 0, 0, 0, 7, 0, 0, 0, 2, 1, 0, 0, 7, 0, b'/', b'a', b'/', b'b', b'.', b'r', b's']);
        let mut r = FrameReader::new();
        r.push(&buf[..5]);
        assert_eq!(r.next_frame().unwrap(), None);
        r.push(&buf[5..]);
        let frame = r.next_frame().unwrap().unwrap().to_vec();
        assert_eq!(decode_request(&frame).unwrap(), msg);
    }
}
