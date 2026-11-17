//! zcode（と `zc-remote` の CLI）⇔ `zc-remote serve` の間の約束（docs/stage3-design.md §8.1）。
//!
//! - 通り道は UNIX ソケット（[`socket_path`]）。zcode 側に JSON も TLS も入れないための層
//! - 1 メッセージ = 12 バイトの頭 + 本体。全部リトルエンディアン
//!
//! ```text
//!   u32 len      このあとに続くバイト数（頭の残り 8 バイト + 本体。上限 MAX_FRAME、依頼は MAX_REQUEST_FRAME）
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
//! - 本体の長さは種類ごとにちょうど。足りない・余るのは [`DecodeError::Length`]
//! - `req` を持たない種類（`Hello`・`Warm`・`Status`・`Shutdown` と、返事の `Hello`・`Status`）は
//!   頭の `req` を 0 で送り、読むときは見ない。頭の `flags` も読むときは見ない
//! - 書く側は長さを確かめない（`u16` を超える文字列だけは切る）。1 メッセージが
//!   [`MAX_FRAME`] を超えないように分けるのは送る側の役目（ログは [`MAX_LOG_BYTES`] まで）
//!
//! 種類ごとの細かい決まり:
//!
//! - `Log` の `bytes` は長さを付けず、本体の残り全部
//! - 診断: `col` が無ければ `u32::MAX`。`end` が無ければ `end_line` も `end_col` も
//!   `u32::MAX`。`severity` は 1 = エラー、2 = 警告。`code` が無ければ空の文字列
//!   （空の `code` は「無い」と読む）
//! - 試験: 落ちた場所が無ければ `path` は空・`line` は `u32::MAX`（`line` で判断する）
//! - `Submit` の後ろの重ね（版 2 で足した）: `u16 件数`、件ごとに `path file`・`u32 長さ`・
//!   バイト列。版 1 の `Submit`（`file` で終わる）も読める（重ね無しと読む）。書くときは
//!   重ねが空でも件数 0 を書く
//! - 依頼（Request）だけは [`MAX_REQUEST_FRAME`] まで受け取る（重ねの分だけ大きい）。読む側は
//!   [`FrameReader::for_requests`] を使う
//!
//! この crate は I/O を持たない（バイト列 ⇔ 型だけ）。ソケットを読む・書くのは
//! `zcode/src/remote.rs`（専用スレッド）と `zc-remote`（serve / CLI）。

use std::path::{Path, PathBuf};
use zc_core::{Diagnostic, Severity};

/// 約束の版。合わない相手とは話さない（serve だけは [`MIN_IPC_VERSION`] からの相手も受ける）。
///
/// 版 2: `Submit` に未保存の中身の重ね（[`OverlayFile`]）と [`SubmitFlags::AUTO`] を足した。
pub const IPC_VERSION: u16 = 2;
/// serve が受ける、いちばん古い相手の版（版 1 の `Submit` も読める）。
pub const MIN_IPC_VERSION: u16 = 1;
/// [`Request::Submit`] の重ねの中身の合計の上限。超える重ねは送らない（打鍵が止まったときの
/// check は飛ばす）。
pub const MAX_OVERLAY_BYTES: usize = 1 << 20;
/// 依頼 1 メッセージの上限（重ねの分だけ [`MAX_FRAME`] より大きい）。
pub const MAX_REQUEST_FRAME: usize = MAX_FRAME + MAX_OVERLAY_BYTES;
/// 1 メッセージの上限（頭の `len` の値の上限。`len` そのものの 4 バイトは含まない）。
/// 超えるログ・診断は送る側が分ける。
pub const MAX_FRAME: usize = 256 * 1024;
/// 頭の長さ。
pub const HEADER_LEN: usize = 12;
/// [`Response::Log`] 1 つに入れてよいバイト数（[`MAX_FRAME`] から頭の残りと `tag` を引いたもの）。
pub const MAX_LOG_BYTES: usize = MAX_FRAME - (HEADER_LEN - 4) - 4;

/// ソケットの場所: `$XDG_RUNTIME_DIR/zcode/remote.sock`
/// （`XDG_RUNTIME_DIR` が無い・空なら `/run/user/<uid>/zcode/remote.sock`）。
pub fn socket_path() -> PathBuf {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(format!("/run/user/{}", current_uid())),
    };
    dir.join("zcode").join("remote.sock")
}

/// 自分の uid（libc に頼らず、`/proc/self` の持ち主で調べる）。
fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = std::fs::metadata("/proc/self") {
            return m.uid();
        }
    }
    1000
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
    /// 打鍵が止まったときの check（zcode が未保存の中身を重ねて頼む。版 2）。serve は設定
    /// `check_on_save` が `idle` のときだけ受け、ほかは [`DoneStatus::NotApplicable`] で返す。
    pub const AUTO: SubmitFlags = SubmitFlags(1 << 4);

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
    /// （build は build だけ、check は check だけを取り消す）。`overlay` は、ディスクの代わりに
    /// 使う未保存の中身（根の外のもの・木に無いものは serve が捨てる）。
    Submit { req: ReqId, kind: JobKind, lang: RemoteLang, flags: SubmitFlags, file: PathBuf, overlay: Vec<OverlayFile> },
    /// 依頼を取り消す。
    Cancel { req: ReqId },
    /// 状態を聞く。返事は [`Response::Status`]。
    Status,
    /// serve を終わらせる（試験用）。
    Shutdown,
}

/// ディスクの代わりに使う、開いている文書の未保存の中身（[`Request::Submit`]）。
#[derive(Clone, PartialEq, Eq)]
pub struct OverlayFile {
    /// 絶対パス（zcode は根を知らない。serve が根からの相対に直す）。
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

impl core::fmt::Debug for OverlayFile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "OverlayFile({:?}, {} バイト)", self.path, self.bytes.len())
    }
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
    /// `len` が上限（[`MAX_FRAME`]、依頼は [`MAX_REQUEST_FRAME`]）を超えている（相手が壊れている。切る）。
    TooLarge(u32),
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for DecodeError {}

// 種類の番号（docs/stage3-design.md §8.2 の表）。
const K_HELLO: u16 = 0x0001;
const K_WARM: u16 = 0x0002;
const K_SUBMIT: u16 = 0x0003;
const K_CANCEL: u16 = 0x0004;
const K_STATUS: u16 = 0x0005;
const K_SHUTDOWN: u16 = 0x0006;
const R_HELLO: u16 = 0x8001;
const R_STATUS: u16 = 0x8002;
const R_PHASE: u16 = 0x8003;
const R_LOG: u16 = 0x8004;
const R_DIAGNOSTICS: u16 = 0x8005;
const R_TESTS: u16 = 0x8006;
const R_ARTIFACT: u16 = 0x8007;
const R_FILE_WRITTEN: u16 = 0x8008;
const R_DONE: u16 = 0x8009;

/// 「無い」を表す `u32`。
const NONE_U32: u32 = u32::MAX;

/// `out` の末尾に 1 メッセージ（頭つき）を足す。
pub fn encode_request(msg: &Request, out: &mut Vec<u8>) {
    let start = out.len();
    match msg {
        Request::Hello { version, client } => {
            begin(out, K_HELLO, ReqId::NONE);
            put_u16(out, *version);
            out.push(*client as u8);
        }
        Request::Warm { file } => {
            begin(out, K_WARM, ReqId::NONE);
            put_path(out, file);
        }
        Request::Submit { req, kind, lang, flags, file, overlay } => {
            begin(out, K_SUBMIT, *req);
            out.push(*kind as u8);
            out.push(*lang as u8);
            put_u16(out, flags.0);
            put_path(out, file);
            let overlay = &overlay[..overlay.len().min(u16::MAX as usize)];
            put_u16(out, overlay.len() as u16);
            for o in overlay {
                put_path(out, &o.path);
                put_u32(out, o.bytes.len() as u32);
                out.extend_from_slice(&o.bytes);
            }
        }
        Request::Cancel { req } => begin(out, K_CANCEL, *req),
        Request::Status => begin(out, K_STATUS, ReqId::NONE),
        Request::Shutdown => begin(out, K_SHUTDOWN, ReqId::NONE),
    }
    finish(out, start);
}

/// `out` の末尾に 1 メッセージ（頭つき）を足す。
pub fn encode_response(msg: &Response, out: &mut Vec<u8>) {
    let start = out.len();
    match msg {
        Response::Hello { version } => {
            begin(out, R_HELLO, ReqId::NONE);
            put_u16(out, *version);
        }
        Response::Status { backend, connected, running_jobs } => {
            begin(out, R_STATUS, ReqId::NONE);
            out.push(*backend as u8);
            out.push(*connected as u8);
            put_u16(out, *running_jobs);
        }
        Response::Phase { req, tag, root, phase, done, total } => {
            begin(out, R_PHASE, *req);
            put_u32(out, tag.0);
            out.push(*phase as u8);
            put_u32(out, *done);
            put_u32(out, *total);
            put_path(out, root);
        }
        Response::Log { req, tag, bytes } => {
            begin(out, R_LOG, *req);
            put_u32(out, tag.0);
            out.extend_from_slice(bytes);
        }
        Response::Diagnostics { req, tag, root, complete, last, files } => {
            begin(out, R_DIAGNOSTICS, *req);
            put_u32(out, tag.0);
            out.push(*complete as u8);
            out.push(*last as u8);
            put_path(out, root);
            let files = &files[..files.len().min(u16::MAX as usize)];
            put_u16(out, files.len() as u16);
            for f in files {
                put_str(out, &f.path);
                let diags = &f.diags[..f.diags.len().min(u16::MAX as usize)];
                put_u16(out, diags.len() as u16);
                for d in diags {
                    put_diag(out, d);
                }
            }
        }
        Response::Tests { req, tag, root, report } => {
            begin(out, R_TESTS, *req);
            put_u32(out, tag.0);
            put_path(out, root);
            put_u32(out, report.passed);
            put_u32(out, report.failed);
            put_u32(out, report.ignored);
            let cases = &report.cases[..report.cases.len().min(u16::MAX as usize)];
            put_u16(out, cases.len() as u16);
            for c in cases {
                put_str(out, &c.name);
                out.push(c.passed as u8);
                match &c.location {
                    Some((path, line)) => {
                        put_str(out, path);
                        put_u32(out, *line);
                    }
                    None => {
                        put_str(out, "");
                        put_u32(out, NONE_U32);
                    }
                }
                put_str(out, &c.message);
            }
        }
        Response::Artifact { req, tag, kind, stale, path } => {
            begin(out, R_ARTIFACT, *req);
            put_u32(out, tag.0);
            out.push(*kind as u8);
            out.push(*stale as u8);
            put_path(out, path);
        }
        Response::FileWritten { req, tag, path } => {
            begin(out, R_FILE_WRITTEN, *req);
            put_u32(out, tag.0);
            put_path(out, path);
        }
        Response::Done { req, tag, status, exit, backend, elapsed_ms, message } => {
            begin(out, R_DONE, *req);
            put_u32(out, tag.0);
            out.push(*status as u8);
            out.extend_from_slice(&exit.to_le_bytes());
            out.push(*backend as u8);
            put_u32(out, *elapsed_ms);
            put_str(out, message);
        }
    }
    finish(out, start);
}

/// 1 メッセージ（頭の `len` より後ろ = `kind` から本体の終わりまで）を読む。
pub fn decode_request(frame: &[u8]) -> Result<Request, DecodeError> {
    let (kind, req, mut r) = split_header(frame, MAX_REQUEST_FRAME)?;
    let msg = match kind {
        K_HELLO => {
            let version = r.u16()?;
            let client = match r.u8()? {
                1 => ClientKind::Editor,
                2 => ClientKind::Cli,
                _ => return Err(DecodeError::Invalid("client")),
            };
            Request::Hello { version, client }
        }
        K_WARM => Request::Warm { file: r.path()? },
        K_SUBMIT => {
            let kind = match r.u8()? {
                1 => JobKind::Build,
                2 => JobKind::Check,
                3 => JobKind::Test,
                _ => return Err(DecodeError::Invalid("job kind")),
            };
            let lang = match r.u8()? {
                1 => RemoteLang::Rust,
                2 => RemoteLang::C,
                3 => RemoteLang::Node,
                _ => return Err(DecodeError::Invalid("lang")),
            };
            let flags = SubmitFlags(r.u16()?);
            let file = r.path()?;
            // 版 1 の Submit は file で終わる（重ね無し）
            let mut overlay = Vec::new();
            if r.remaining() > 0 {
                let n = r.u16()? as usize;
                overlay.reserve(n.min(r.remaining() / 6));
                for _ in 0..n {
                    let path = r.path()?;
                    let len = r.u32()? as usize;
                    overlay.push(OverlayFile { path, bytes: r.take(len)?.to_vec() });
                }
            }
            Request::Submit { req, kind, lang, flags, file, overlay }
        }
        K_CANCEL => Request::Cancel { req },
        K_STATUS => Request::Status,
        K_SHUTDOWN => Request::Shutdown,
        other => return Err(DecodeError::UnknownKind(other)),
    };
    r.end()?;
    Ok(msg)
}

/// 1 メッセージ（`kind` から本体の終わりまで）を読む。
pub fn decode_response(frame: &[u8]) -> Result<Response, DecodeError> {
    let (kind, req, mut r) = split_header(frame, MAX_FRAME)?;
    let msg = match kind {
        R_HELLO => Response::Hello { version: r.u16()? },
        R_STATUS => {
            let backend = backend(r.u8()?)?;
            let connected = r.bool()?;
            Response::Status { backend, connected, running_jobs: r.u16()? }
        }
        R_PHASE => {
            let tag = JobTag(r.u32()?);
            let phase = match r.u8()? {
                1 => Phase::Connecting,
                2 => Phase::Scanned,
                3 => Phase::Uploading,
                4 => Phase::Queued,
                5 => Phase::Running,
                6 => Phase::Downloading,
                _ => return Err(DecodeError::Invalid("phase")),
            };
            let done = r.u32()?;
            let total = r.u32()?;
            Response::Phase { req, tag, root: r.path()?, phase, done, total }
        }
        R_LOG => {
            let tag = JobTag(r.u32()?);
            Response::Log { req, tag, bytes: r.rest().to_vec() }
        }
        R_DIAGNOSTICS => {
            let tag = JobTag(r.u32()?);
            let complete = r.bool()?;
            let last = r.bool()?;
            let root = r.path()?;
            let n = r.u16()? as usize;
            let mut files = Vec::with_capacity(n.min(r.remaining() / 4));
            for _ in 0..n {
                let path = r.str()?;
                let m = r.u16()? as usize;
                let mut diags = Vec::with_capacity(m.min(r.remaining() / 21));
                for _ in 0..m {
                    diags.push(r.diag()?);
                }
                files.push(FileDiags { path, diags });
            }
            Response::Diagnostics { req, tag, root, complete, last, files }
        }
        R_TESTS => {
            let tag = JobTag(r.u32()?);
            let root = r.path()?;
            let passed = r.u32()?;
            let failed = r.u32()?;
            let ignored = r.u32()?;
            let n = r.u16()? as usize;
            let mut cases = Vec::with_capacity(n.min(r.remaining() / 13));
            for _ in 0..n {
                let name = r.str()?;
                let ok = r.bool()?;
                let path = r.str()?;
                let line = r.u32()?;
                let message = r.str()?;
                let location = if line == NONE_U32 { None } else { Some((path, line)) };
                cases.push(TestCase { name, passed: ok, location, message });
            }
            Response::Tests { req, tag, root, report: TestReport { passed, failed, ignored, cases } }
        }
        R_ARTIFACT => {
            let tag = JobTag(r.u32()?);
            let kind = match r.u8()? {
                1 => ArtifactKind::Exe,
                2 => ArtifactKind::Js,
                _ => return Err(DecodeError::Invalid("artifact kind")),
            };
            let stale = r.bool()?;
            Response::Artifact { req, tag, kind, stale, path: r.path()? }
        }
        R_FILE_WRITTEN => {
            let tag = JobTag(r.u32()?);
            Response::FileWritten { req, tag, path: r.path()? }
        }
        R_DONE => {
            let tag = JobTag(r.u32()?);
            let status = match r.u8()? {
                0 => DoneStatus::Ok,
                1 => DoneStatus::Failed,
                2 => DoneStatus::Cancelled,
                3 => DoneStatus::NotApplicable,
                4 => DoneStatus::Offline,
                5 => DoneStatus::Auth,
                6 => DoneStatus::Clock,
                7 => DoneStatus::Limit,
                8 => DoneStatus::Error,
                _ => return Err(DecodeError::Invalid("done status")),
            };
            let exit = r.u32()? as i32;
            let backend = backend(r.u8()?)?;
            let elapsed_ms = r.u32()?;
            Response::Done { req, tag, status, exit, backend, elapsed_ms, message: r.str()? }
        }
        other => return Err(DecodeError::UnknownKind(other)),
    };
    r.end()?;
    Ok(msg)
}

fn backend(v: u8) -> Result<Backend, DecodeError> {
    match v {
        0 => Ok(Backend::Unknown),
        1 => Ok(Backend::Pc),
        2 => Ok(Backend::Cloudflare),
        _ => Err(DecodeError::Invalid("backend")),
    }
}

/// 頭の残り（`kind`・`flags`・`req`）を読み、本体の読み手を返す。
fn split_header(frame: &[u8], limit: usize) -> Result<(u16, ReqId, Rd<'_>), DecodeError> {
    if frame.len() > limit {
        return Err(DecodeError::TooLarge(frame.len().min(u32::MAX as usize) as u32));
    }
    let mut r = Rd { b: frame, pos: 0 };
    let kind = r.u16()?;
    let _flags = r.u16()?;
    let req = ReqId(r.u32()?);
    Ok((kind, req, r))
}

/// 頭を書く（`len` は仮の 0。[`finish`] で埋める）。
fn begin(out: &mut Vec<u8>, kind: u16, req: ReqId) {
    put_u32(out, 0);
    put_u16(out, kind);
    put_u16(out, 0);
    put_u32(out, req.0);
}

fn finish(out: &mut [u8], start: usize) {
    let len = (out.len() - start - 4) as u32;
    out[start..start + 4].copy_from_slice(&len.to_le_bytes());
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// `u16` の長さ + バイト列。長すぎれば `u16::MAX` で切る。
fn put_bytes16(out: &mut Vec<u8>, b: &[u8]) {
    let b = &b[..b.len().min(u16::MAX as usize)];
    put_u16(out, b.len() as u16);
    out.extend_from_slice(b);
}

/// 文字列。長すぎれば文字の境目で切る。
fn put_str(out: &mut Vec<u8>, s: &str) {
    let mut n = s.len().min(u16::MAX as usize);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    put_bytes16(out, &s.as_bytes()[..n]);
}

fn put_path(out: &mut Vec<u8>, p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        put_bytes16(out, p.as_os_str().as_bytes());
    }
    #[cfg(not(unix))]
    put_str(out, &p.to_string_lossy());
}

fn put_diag(out: &mut Vec<u8>, d: &Diagnostic) {
    put_u32(out, d.line);
    put_u32(out, d.col.unwrap_or(NONE_U32));
    let (el, ec) = d.end.unwrap_or((NONE_U32, NONE_U32));
    put_u32(out, el);
    put_u32(out, ec);
    out.push(match d.severity {
        Severity::Error => 1,
        Severity::Warning => 2,
    });
    put_str(out, d.code.as_deref().unwrap_or(""));
    put_str(out, &d.message);
}

/// 本体の読み手。
struct Rd<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.b.len() - self.pos < n {
            return Err(DecodeError::Length);
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn remaining(&self) -> usize {
        self.b.len() - self.pos
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError::Invalid("bool")),
        }
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn bytes16(&mut self) -> Result<&'a [u8], DecodeError> {
        let n = self.u16()? as usize;
        self.take(n)
    }
    fn str(&mut self) -> Result<String, DecodeError> {
        let b = self.bytes16()?;
        std::str::from_utf8(b).map(str::to_owned).map_err(|_| DecodeError::Invalid("utf-8"))
    }
    fn path(&mut self) -> Result<PathBuf, DecodeError> {
        let b = self.bytes16()?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Ok(PathBuf::from(std::ffi::OsStr::from_bytes(b)))
        }
        #[cfg(not(unix))]
        std::str::from_utf8(b).map(PathBuf::from).map_err(|_| DecodeError::Invalid("utf-8"))
    }
    fn diag(&mut self) -> Result<Diagnostic, DecodeError> {
        let line = self.u32()?;
        let col = self.u32()?;
        let end_line = self.u32()?;
        let end_col = self.u32()?;
        let severity = match self.u8()? {
            1 => Severity::Error,
            2 => Severity::Warning,
            _ => return Err(DecodeError::Invalid("severity")),
        };
        let code = self.str()?;
        let message = self.str()?;
        Ok(Diagnostic {
            line,
            col: (col != NONE_U32).then_some(col),
            end: (end_line != NONE_U32).then_some((end_line, end_col)),
            severity,
            code: (!code.is_empty()).then(|| code.into_boxed_str()),
            message,
        })
    }
    fn rest(&mut self) -> &'a [u8] {
        let s = &self.b[self.pos..];
        self.pos = self.b.len();
        s
    }
    /// 本体を読み切ったか（余りがあれば [`DecodeError::Length`]）。
    fn end(&self) -> Result<(), DecodeError> {
        if self.pos == self.b.len() {
            Ok(())
        } else {
            Err(DecodeError::Length)
        }
    }
}

/// ソケットから読んだバイト列を溜めて、メッセージごとに切り出す。
///
/// 返したメッセージのぶんは、次の [`FrameReader::push`] で詰める（読むたびに詰め直さない）。
/// [`DecodeError::TooLarge`] / [`DecodeError::Length`] を返したあとは続きを読めない（相手が
/// 壊れている。接続を切る）。
#[derive(Debug)]
pub struct FrameReader {
    buf: Vec<u8>,
    /// `buf` の中の、まだ返していない位置。
    pos: usize,
    /// 1 メッセージの上限（頭の `len` の値）。
    limit: usize,
}

impl Default for FrameReader {
    fn default() -> FrameReader {
        FrameReader { buf: Vec::new(), pos: 0, limit: MAX_FRAME }
    }
}

impl FrameReader {
    /// 返事（Response）を読む側。上限は [`MAX_FRAME`]。
    pub fn new() -> FrameReader {
        FrameReader::default()
    }

    /// 依頼（Request）を読む側（serve）。上限は [`MAX_REQUEST_FRAME`]（重ねの分だけ大きい）。
    pub fn for_requests() -> FrameReader {
        FrameReader { limit: MAX_REQUEST_FRAME, ..FrameReader::default() }
    }

    /// 読んだバイト列を足す。
    pub fn push(&mut self, bytes: &[u8]) {
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// 溜まっていて、まだ返していないバイト数。
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// そろっているメッセージを 1 つ返す（`kind` から本体の終わりまで。次の `push` /
    /// `next_frame` まで有効）。足りなければ `Ok(None)`。
    pub fn next_frame(&mut self) -> Result<Option<&[u8]>, DecodeError> {
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
            return Ok(None);
        }
        let avail = &self.buf[self.pos..];
        if avail.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([avail[0], avail[1], avail[2], avail[3]]);
        if len as usize > self.limit {
            return Err(DecodeError::TooLarge(len));
        }
        if (len as usize) < HEADER_LEN - 4 {
            return Err(DecodeError::Length);
        }
        let len = len as usize;
        if avail.len() < 4 + len {
            return Ok(None);
        }
        let start = self.pos + 4;
        self.pos = start + len;
        Ok(Some(&self.buf[start..start + len]))
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
        assert!(SubmitFlags(0b10001).contains(SubmitFlags::AUTO));
        assert!(!SubmitFlags::NONE.contains(SubmitFlags::WANT_LOG));
    }

    // 受け入れ試験（課題 I1）: 全部の種類の往復、正解のバイト列（docs/stage3-design.md §8.2 の例）、
    // 半端に届いたバイト列の切り出し、知らない kind の読み飛ばし、MAX_FRAME 超えの拒否。
    #[test]
    fn submit_round_trips_and_matches_golden_bytes() {
        let msg = Request::Submit { req: ReqId(7), kind: JobKind::Check, lang: RemoteLang::Rust, flags: SubmitFlags::NONE, file: "/a/b.rs".into(), overlay: vec![] };
        let mut buf = Vec::new();
        encode_request(&msg, &mut buf);
        // len=23, kind=0x0003, flags=0, req=7, kind=2, lang=1, flags=0, 長さ 7 + "/a/b.rs", 重ね 0 件
        assert_eq!(buf, [23, 0, 0, 0, 3, 0, 0, 0, 7, 0, 0, 0, 2, 1, 0, 0, 7, 0, b'/', b'a', b'/', b'b', b'.', b'r', b's', 0, 0]);
        let mut r = FrameReader::new();
        r.push(&buf[..5]);
        assert_eq!(r.next_frame().unwrap(), None);
        r.push(&buf[5..]);
        let frame = r.next_frame().unwrap().unwrap().to_vec();
        assert_eq!(decode_request(&frame).unwrap(), msg);

        // 版 1 の Submit（file で終わる）も読める。重ね無し
        let v1 = [21, 0, 0, 0, 3, 0, 0, 0, 7, 0, 0, 0, 2, 1, 0, 0, 7, 0, b'/', b'a', b'/', b'b', b'.', b'r', b's'];
        assert_eq!(decode_request(&v1[4..]).unwrap(), msg);
    }

    #[test]
    fn submit_overlay_matches_golden_bytes() {
        let msg = Request::Submit {
            req: ReqId(1),
            kind: JobKind::Check,
            lang: RemoteLang::Rust,
            flags: SubmitFlags::AUTO,
            file: "/p/a.rs".into(),
            overlay: vec![OverlayFile { path: "/p/a.rs".into(), bytes: b"fn".to_vec() }, OverlayFile { path: "/p/e".into(), bytes: vec![] }],
        };
        let mut buf = Vec::new();
        encode_request(&msg, &mut buf);
        #[rustfmt::skip]
        let body = [
            2, 1, 0x10, 0,                                      // kind, lang, flags = AUTO
            7, 0, b'/', b'p', b'/', b'a', b'.', b'r', b's',     // file
            2, 0,                                               // 重ね 2 件
            7, 0, b'/', b'p', b'/', b'a', b'.', b'r', b's',     // path
            2, 0, 0, 0, b'f', b'n',                             // 長さ + 中身
            4, 0, b'/', b'p', b'/', b'e',
            0, 0, 0, 0,                                         // 空の中身
        ];
        assert_eq!(&buf[HEADER_LEN..], body);
        assert_eq!(decode_request(&buf[4..]).unwrap(), msg);
    }

    /// 依頼は重ねの分だけ大きくてよい（上限 MAX_REQUEST_FRAME）。返事は MAX_FRAME のまま。
    #[test]
    fn requests_may_carry_a_megabyte_overlay() {
        let bytes: Vec<u8> = (0..MAX_OVERLAY_BYTES).map(|i| (i % 251) as u8).collect();
        let msg = Request::Submit { req: ReqId(3), kind: JobKind::Check, lang: RemoteLang::Node, flags: SubmitFlags::AUTO, file: "/p/a.ts".into(), overlay: vec![OverlayFile { path: "/p/a.ts".into(), bytes }] };
        let mut buf = Vec::new();
        encode_request(&msg, &mut buf);
        assert!(buf.len() - 4 > MAX_FRAME && buf.len() - 4 <= MAX_REQUEST_FRAME);
        let mut r = FrameReader::for_requests();
        for chunk in buf.chunks(64 * 1024) {
            r.push(chunk);
        }
        assert_eq!(decode_request(r.next_frame().unwrap().unwrap()).unwrap(), msg);
        // 返事を読む側は、同じ大きさを断る
        let mut r = FrameReader::new();
        r.push(&buf);
        assert!(matches!(r.next_frame(), Err(DecodeError::TooLarge(_))));
        assert!(matches!(decode_response(&buf[4..]), Err(DecodeError::TooLarge(_))));
        // 依頼の上限を超えるもの
        let mut r = FrameReader::for_requests();
        r.push(&((MAX_REQUEST_FRAME as u32) + 1).to_le_bytes());
        assert!(matches!(r.next_frame(), Err(DecodeError::TooLarge(_))));
    }

    fn requests() -> Vec<Request> {
        vec![
            Request::Hello { version: IPC_VERSION, client: ClientKind::Editor },
            Request::Hello { version: 9, client: ClientKind::Cli },
            Request::Warm { file: "/home/u/code/a/src/main.rs".into() },
            Request::Submit { req: ReqId(1), kind: JobKind::Build, lang: RemoteLang::C, flags: SubmitFlags(0b1011), file: "/x/日本語.c".into(), overlay: vec![] },
            Request::Submit { req: ReqId(u32::MAX - 1), kind: JobKind::Test, lang: RemoteLang::Node, flags: SubmitFlags::WANT_LOG, file: "".into(), overlay: vec![] },
            Request::Submit {
                req: ReqId(2),
                kind: JobKind::Check,
                lang: RemoteLang::Rust,
                flags: SubmitFlags::AUTO,
                file: "/x/src/main.rs".into(),
                overlay: vec![OverlayFile { path: "/x/src/main.rs".into(), bytes: "fn main() { let あ = 1; }\n".into() }, OverlayFile { path: "/x/src/lib.rs".into(), bytes: vec![0, 0xff] }],
            },
            Request::Cancel { req: ReqId(42) },
            Request::Status,
            Request::Shutdown,
        ]
    }

    fn responses() -> Vec<Response> {
        let d_full = Diagnostic { line: 3, col: Some(4), end: Some((3, 9)), severity: Severity::Error, code: Some("E0308".into()), message: "型が合わない".into() };
        let d_bare = Diagnostic::at_line(0, Severity::Warning, "");
        let d_end_only = Diagnostic { line: 7, col: None, end: Some((8, 0)), severity: Severity::Warning, code: None, message: "w".into() };
        vec![
            Response::Hello { version: IPC_VERSION },
            Response::Status { backend: Backend::Unknown, connected: false, running_jobs: 0 },
            Response::Status { backend: Backend::Cloudflare, connected: true, running_jobs: 513 },
            Response::Phase { req: ReqId(3), tag: JobTag(9), root: "/p".into(), phase: Phase::Connecting, done: 0, total: 0 },
            Response::Phase { req: ReqId::NONE, tag: JobTag(9), root: "/p".into(), phase: Phase::Scanned, done: 1, total: 2 },
            Response::Phase { req: ReqId(3), tag: JobTag(9), root: "/p".into(), phase: Phase::Uploading, done: 100, total: 4000 },
            Response::Phase { req: ReqId(3), tag: JobTag(9), root: "".into(), phase: Phase::Queued, done: 0, total: 0 },
            Response::Phase { req: ReqId(3), tag: JobTag(9), root: "/p".into(), phase: Phase::Running, done: 0, total: 0 },
            Response::Phase { req: ReqId(3), tag: JobTag(9), root: "/p".into(), phase: Phase::Downloading, done: u32::MAX, total: u32::MAX },
            Response::Log { req: ReqId(3), tag: JobTag(9), bytes: vec![] },
            Response::Log { req: ReqId(3), tag: JobTag(9), bytes: vec![0xff, 0x1b, b'[', b'm', b'\n'] },
            Response::Diagnostics { req: ReqId(3), tag: JobTag(9), root: "/p".into(), complete: true, last: true, files: vec![] },
            Response::Diagnostics {
                req: ReqId::NONE,
                tag: JobTag(10),
                root: "/p".into(),
                complete: false,
                last: false,
                files: vec![
                    FileDiags { path: "src/main.rs".into(), diags: vec![d_full, d_bare.clone(), d_end_only] },
                    FileDiags { path: "".into(), diags: vec![d_bare] },
                    FileDiags { path: "src/empty.rs".into(), diags: vec![] },
                ],
            },
            Response::Tests { req: ReqId(4), tag: JobTag(11), root: "/p".into(), report: TestReport::default() },
            Response::Tests {
                req: ReqId(4),
                tag: JobTag(11),
                root: "/p".into(),
                report: TestReport {
                    passed: 1,
                    failed: 1,
                    ignored: 2,
                    cases: vec![
                        TestCase { name: "a::tests::ok".into(), passed: true, location: None, message: "".into() },
                        TestCase { name: "a::tests::ng".into(), passed: false, location: Some(("src/a.rs".into(), 0)), message: "assertion failed".into() },
                        TestCase { name: "b".into(), passed: false, location: Some(("".into(), 5)), message: "".into() },
                    ],
                },
            },
            Response::Artifact { req: ReqId(5), tag: JobTag(12), kind: ArtifactKind::Exe, stale: false, path: "/run/user/1000/zcode/bin/foo-1234abcd".into() },
            Response::Artifact { req: ReqId(5), tag: JobTag(12), kind: ArtifactKind::Js, stale: true, path: "/x.js".into() },
            Response::FileWritten { req: ReqId(5), tag: JobTag(12), path: "/p/Cargo.lock".into() },
            Response::Done { req: ReqId(5), tag: JobTag(12), status: DoneStatus::Ok, exit: 0, backend: Backend::Pc, elapsed_ms: 4210, message: "".into() },
            Response::Done { req: ReqId(5), tag: JobTag(12), status: DoneStatus::Failed, exit: -1, backend: Backend::Cloudflare, elapsed_ms: 1, message: "失敗".into() },
            Response::Done { req: ReqId(5), tag: JobTag(12), status: DoneStatus::Cancelled, exit: 130, backend: Backend::Pc, elapsed_ms: 0, message: "".into() },
            Response::Done { req: ReqId(5), tag: JobTag(12), status: DoneStatus::NotApplicable, exit: 0, backend: Backend::Unknown, elapsed_ms: 0, message: "".into() },
            Response::Done { req: ReqId(5), tag: JobTag(12), status: DoneStatus::Offline, exit: 3, backend: Backend::Unknown, elapsed_ms: 0, message: "".into() },
            Response::Done { req: ReqId(5), tag: JobTag(12), status: DoneStatus::Auth, exit: 4, backend: Backend::Unknown, elapsed_ms: 0, message: "".into() },
            Response::Done { req: ReqId(5), tag: JobTag(12), status: DoneStatus::Clock, exit: 4, backend: Backend::Unknown, elapsed_ms: 0, message: "".into() },
            Response::Done { req: ReqId(5), tag: JobTag(12), status: DoneStatus::Limit, exit: 5, backend: Backend::Unknown, elapsed_ms: 0, message: "".into() },
            Response::Done { req: ReqId(5), tag: JobTag(12), status: DoneStatus::Error, exit: i32::MIN, backend: Backend::Unknown, elapsed_ms: u32::MAX, message: "x".into() },
        ]
    }

    /// 全部を 1 本の流れに書き、1 バイトずつ足しながら切り出して読む。
    #[test]
    fn every_kind_round_trips_byte_by_byte() {
        let reqs = requests();
        let mut buf = Vec::new();
        for m in &reqs {
            encode_request(m, &mut buf);
        }
        let mut r = FrameReader::new();
        let mut got = Vec::new();
        for b in &buf {
            r.push(std::slice::from_ref(b));
            while let Some(f) = r.next_frame().unwrap() {
                got.push(decode_request(f).unwrap());
            }
        }
        assert_eq!(got, reqs);
        assert_eq!(r.buffered(), 0);

        let resps = responses();
        let mut buf = Vec::new();
        for m in &resps {
            encode_response(m, &mut buf);
        }
        let mut r = FrameReader::new();
        let mut got = Vec::new();
        // 刻み方を変える（7 バイトずつ）
        for chunk in buf.chunks(7) {
            r.push(chunk);
            while let Some(f) = r.next_frame().unwrap() {
                got.push(decode_response(f).unwrap());
            }
        }
        assert_eq!(got, resps);
    }

    /// 1 回の push に全部が入っていても、順に切り出せる。
    #[test]
    fn many_frames_in_one_push() {
        let resps = responses();
        let mut buf = Vec::new();
        for m in &resps {
            encode_response(m, &mut buf);
        }
        let mut r = FrameReader::new();
        r.push(&buf);
        let mut got = Vec::new();
        while let Some(f) = r.next_frame().unwrap() {
            got.push(decode_response(f).unwrap());
        }
        assert_eq!(got, resps);
        assert_eq!(r.next_frame().unwrap(), None);
    }

    #[test]
    fn done_matches_golden_bytes() {
        let msg = Response::Done { req: ReqId(2), tag: JobTag(5), status: DoneStatus::Failed, exit: -1, backend: Backend::Pc, elapsed_ms: 258, message: "e".into() };
        let mut buf = Vec::new();
        encode_response(&msg, &mut buf);
        #[rustfmt::skip]
        let want = [
            25, 0, 0, 0, 0x09, 0x80, 0, 0, 2, 0, 0, 0, // 頭
            5, 0, 0, 0,                                 // tag
            1,                                          // status
            0xff, 0xff, 0xff, 0xff,                     // exit = -1
            1,                                          // backend
            2, 1, 0, 0,                                 // elapsed_ms = 258
            1, 0, b'e',                                 // message
        ];
        assert_eq!(buf, want);
    }

    #[test]
    fn diagnostic_layout_matches_table() {
        let d = Diagnostic { line: 1, col: None, end: None, severity: Severity::Warning, code: None, message: "m".into() };
        let msg = Response::Diagnostics { req: ReqId::NONE, tag: JobTag(1), root: "/r".into(), complete: true, last: false, files: vec![FileDiags { path: "a".into(), diags: vec![d] }] };
        let mut buf = Vec::new();
        encode_response(&msg, &mut buf);
        #[rustfmt::skip]
        let body = [
            1, 0, 0, 0,             // tag
            1, 0,                   // complete, last
            2, 0, b'/', b'r',       // root
            1, 0,                   // ファイル数
            1, 0, b'a',             // path
            1, 0,                   // 件数
            1, 0, 0, 0,             // line
            0xff, 0xff, 0xff, 0xff, // col = 無い
            0xff, 0xff, 0xff, 0xff, // end_line = 無い
            0xff, 0xff, 0xff, 0xff, // end_col = 無い
            2,                      // severity = 警告
            0, 0,                   // code = 無い
            1, 0, b'm',             // message
        ];
        assert_eq!(&buf[HEADER_LEN..], body);
        assert_eq!(&buf[4..6], [0x05, 0x80]);
        assert_eq!(u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize, buf.len() - 4);
    }

    /// 知らない kind は、その 1 つだけ飛ばして次を読める。
    #[test]
    fn unknown_kind_is_skippable() {
        let mut buf = Vec::new();
        // kind 0x0077、本体 3 バイト
        buf.extend_from_slice(&[11, 0, 0, 0, 0x77, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3]);
        encode_request(&Request::Status, &mut buf);
        let mut r = FrameReader::new();
        r.push(&buf);
        let f = r.next_frame().unwrap().unwrap();
        assert_eq!(decode_request(f), Err(DecodeError::UnknownKind(0x77)));
        let f = r.next_frame().unwrap().unwrap();
        assert_eq!(decode_request(f), Ok(Request::Status));
        // 向きの違う種類も「知らない」
        let mut buf = Vec::new();
        encode_request(&Request::Status, &mut buf);
        assert_eq!(decode_response(&buf[4..]), Err(DecodeError::UnknownKind(K_STATUS)));
        let mut buf = Vec::new();
        encode_response(&Response::Hello { version: 1 }, &mut buf);
        assert_eq!(decode_request(&buf[4..]), Err(DecodeError::UnknownKind(R_HELLO)));
    }

    #[test]
    fn oversized_and_undersized_frames_are_rejected() {
        let mut r = FrameReader::new();
        r.push(&((MAX_FRAME as u32) + 1).to_le_bytes());
        assert_eq!(r.next_frame(), Err(DecodeError::TooLarge(MAX_FRAME as u32 + 1)));

        // ちょうど上限は受け取る（本体が届くまで待つ）
        let mut r = FrameReader::new();
        r.push(&(MAX_FRAME as u32).to_le_bytes());
        assert_eq!(r.next_frame(), Ok(None));
        let mut frame = vec![0u8; MAX_FRAME];
        frame[..2].copy_from_slice(&R_LOG.to_le_bytes());
        r.push(&frame);
        let f = r.next_frame().unwrap().unwrap();
        match decode_response(f).unwrap() {
            Response::Log { bytes, .. } => assert_eq!(bytes.len(), MAX_LOG_BYTES),
            other => panic!("{other:?}"),
        }

        // 頭の残り 8 バイトにも満たない
        let mut r = FrameReader::new();
        r.push(&[3, 0, 0, 0, 1, 0, 0]);
        assert_eq!(r.next_frame(), Err(DecodeError::Length));
    }

    #[test]
    fn short_or_long_bodies_are_length_errors() {
        for m in requests() {
            let mut buf = Vec::new();
            encode_request(&m, &mut buf);
            let f = &buf[4..];
            assert_eq!(decode_request(&f[..f.len() - 1]), Err(DecodeError::Length), "{m:?}");
            let mut long = f.to_vec();
            long.push(0);
            assert_eq!(decode_request(&long), Err(DecodeError::Length), "{m:?}");
        }
        for m in responses() {
            if matches!(m, Response::Log { .. }) {
                continue; // 本体の残り全部がログ
            }
            let mut buf = Vec::new();
            encode_response(&m, &mut buf);
            let f = &buf[4..];
            assert_eq!(decode_response(&f[..f.len() - 1]), Err(DecodeError::Length), "{m:?}");
            let mut long = f.to_vec();
            long.push(0);
            assert_eq!(decode_response(&long), Err(DecodeError::Length), "{m:?}");
        }
        assert_eq!(decode_request(&[1, 0, 0]), Err(DecodeError::Length));
    }

    #[test]
    fn bad_values_are_invalid() {
        let mut buf = Vec::new();
        encode_request(&Request::Hello { version: 1, client: ClientKind::Editor }, &mut buf);
        buf[HEADER_LEN + 2] = 3;
        assert!(matches!(decode_request(&buf[4..]), Err(DecodeError::Invalid(_))));

        let mut buf = Vec::new();
        encode_request(&Request::Submit { req: ReqId(1), kind: JobKind::Build, lang: RemoteLang::Rust, flags: SubmitFlags::NONE, file: "/a".into(), overlay: vec![] }, &mut buf);
        let mut b = buf.clone();
        b[HEADER_LEN] = 0;
        assert!(matches!(decode_request(&b[4..]), Err(DecodeError::Invalid(_))));
        let mut b = buf.clone();
        b[HEADER_LEN + 1] = 4;
        assert!(matches!(decode_request(&b[4..]), Err(DecodeError::Invalid(_))));

        let mut buf = Vec::new();
        encode_response(&Response::Done { req: ReqId(1), tag: JobTag(1), status: DoneStatus::Ok, exit: 0, backend: Backend::Pc, elapsed_ms: 0, message: "ab".into() }, &mut buf);
        let mut b = buf.clone();
        b[HEADER_LEN + 4] = 9; // status
        assert!(matches!(decode_response(&b[4..]), Err(DecodeError::Invalid(_))));
        let mut b = buf.clone();
        b[HEADER_LEN + 9] = 3; // backend
        assert!(matches!(decode_response(&b[4..]), Err(DecodeError::Invalid(_))));
        let mut b = buf.clone();
        let n = b.len();
        b[n - 1] = 0xff; // UTF-8 でない
        assert_eq!(decode_response(&b[4..]), Err(DecodeError::Invalid("utf-8")));

        let mut buf = Vec::new();
        encode_response(&Response::Status { backend: Backend::Pc, connected: true, running_jobs: 0 }, &mut buf);
        buf[HEADER_LEN + 1] = 2; // 真偽が 0 / 1 でない
        assert!(matches!(decode_response(&buf[4..]), Err(DecodeError::Invalid(_))));
    }

    /// `u16` を超える文字列は、文字の境目で切って書く（壊れた UTF-8 にしない）。
    #[test]
    fn long_strings_are_cut_at_char_boundary() {
        let msg = "あ".repeat(30000); // 90000 バイト
        let mut buf = Vec::new();
        encode_response(&Response::Done { req: ReqId(1), tag: JobTag(1), status: DoneStatus::Error, exit: 1, backend: Backend::Pc, elapsed_ms: 0, message: msg.clone() }, &mut buf);
        match decode_response(&buf[4..]).unwrap() {
            Response::Done { message, .. } => {
                assert_eq!(message.len(), 65535);
                assert!(msg.starts_with(&message));
            }
            other => panic!("{other:?}"),
        }
    }

    /// 返したメッセージのぶんを詰めても、半端な続きは失われない。
    #[test]
    fn reader_keeps_partial_tail_across_pushes() {
        let mut buf = Vec::new();
        encode_request(&Request::Cancel { req: ReqId(1) }, &mut buf);
        encode_request(&Request::Cancel { req: ReqId(2) }, &mut buf);
        let mut r = FrameReader::new();
        r.push(&buf[..15]);
        assert_eq!(decode_request(r.next_frame().unwrap().unwrap()), Ok(Request::Cancel { req: ReqId(1) }));
        assert_eq!(r.next_frame().unwrap(), None);
        assert_eq!(r.buffered(), 3);
        r.push(&buf[15..]);
        assert_eq!(decode_request(r.next_frame().unwrap().unwrap()), Ok(Request::Cancel { req: ReqId(2) }));
        assert_eq!(r.next_frame().unwrap(), None);
        assert_eq!(r.buffered(), 0);
    }

    #[test]
    fn socket_path_uses_xdg_runtime_dir() {
        // 環境変数を触るのはこの試験だけ
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp/zc-ipc-test");
        assert_eq!(socket_path(), PathBuf::from("/tmp/zc-ipc-test/zcode/remote.sock"));
        std::env::set_var("XDG_RUNTIME_DIR", "");
        let p = socket_path();
        assert!(p.starts_with("/run/user/"), "{p:?}");
        assert!(p.ends_with("zcode/remote.sock"));
        std::env::remove_var("XDG_RUNTIME_DIR");
    }
}
