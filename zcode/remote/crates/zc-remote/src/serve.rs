//! serve: UNIX ソケットの相手・ジョブの取り回し・バックエンドの選択
//! （docs/stage3-design.md §7.1・§7.6）。課題 D5。
//!
//! ```text
//!   poll( UNIX ソケット（待ち受け + つないできた相手） / ビルド係への接続 / signalfd )
//!     相手の依頼 ─▶ scan（根・目録）─▶ Session（状態機械）─▶ Conn（送る）
//!     Conn（届いた）─▶ Session ─▶ 知らせ ─▶ 相手（IPC）・成果物の置き場
//! ```
//!
//! - スレッドは 1 本。`poll()` の 1 回の起床でやる仕事には上限を付ける（ビルド係から読むのは
//!   64KB まで）。打鍵を受けるのは zcode で、serve は nice +5（付けるのは [`crate::cli`]）
//! - 相手は何人でもよい（zcode の専用スレッド、F5 の `zc-remote run`、ssh からの CLI）。
//!   **`Editor` の相手には、ほかの相手が頼んだジョブの進み具合・診断・終了も流す**
//!   （`req` = 0。F5 のビルドの診断を波線にするため）。ログと成果物は頼んだ相手にだけ
//! - 進み具合とログは、相手ごとに最短 33ms に 1 回にまとめる。相手が読まなくて
//!   送り待ちが 1MB を超えたら、その相手を切る（serve は待たない）
//! - 無通信（相手がいない・ジョブが無い）が [`ServeOptions::idle_exit`] 続いたら終わる。
//!   終わる前に SD の控え（[`crate::artifact::LastGood`]）を書く。SIGTERM / SIGINT でも同じ
//! - ビルド係への接続は、最後のジョブが終わってから [`ServeOptions::keep_warm`] だけ保ち、
//!   静かに閉じる（待機中に ping は打たない）
//! - **バックエンドの選択**（設定が `auto` のとき）: PC の設定があれば、まず PC へ 300ms だけ
//!   TCP の接続を試す。届けば PC、届かなければ Cloudflare。結果は 60 秒覚える
//! - ソケットは 0600、フォルダは 0700。相手の uid が自分と同じことを `SO_PEERCRED` で確かめる
//! - 二重起動: ソケットにつなげたら、もう動いているので何もせず終わる。つなげなければ
//!   古いソケットを消して作り直す（`bind` の前に `flock` で 1 つに絞る）
//!
//! # 決めたこと（実装）
//!
//! - **時間は `poll()` の時間切れで数える**（timerfd は使わない。期限の一番近いものまで待つ）。
//!   シグナルは signalfd で同じ `poll()` に載せる
//! - **PC の試し（`auto`）は、PC への本番の接続を短い時間切れ（名前解決 300ms・TCP 300ms）で
//!   始めるだけ**。つながればそのまま使う（試し用の接続を別に張らない）。名前解決か TCP で
//!   止まったら Cloudflare へ。前回つながった IP を覚えているので、ふつうは名前を引かない
//! - **最初の接続が名前解決・TCP で失敗したら、やり直さずに「オフライン」で返す**（F5 を
//!   待たせない）。ジョブの途中で切れたときだけ、状態機械のやり直し（0.5 秒から 3 回）に任せる
//! - 走査（[`crate::scan::HashCache::scan`]）は依頼を受けたその場で最後まで回す。再起動後の
//!   初回だけ全部ハッシュするので、その間はほかの相手を待たせる（上限 32MiB）
//! - 中身を送るのは 1 ファイルずつ（状態機械が 1 つずつ頼む）。送り待ちが 256KB を超えて
//!   いる間は、次のファイルを読まない
//! - 成果物は、同じ job id にまとめた依頼の先頭にだけ中身が届く（[`Event::ArtifactData`]）。
//!   ほかの依頼は、終わりの知らせのときに置き場（`.cur-<project>`）を見る
//! - 成果物が壊れていた（展開できない・SHA-256 が合わない）ら、同じ依頼を 1 回だけ出し直す
//!   （受付に結果が残っているので、作り直さずに送り直される）
//! - lock の書き戻しは `Cargo.lock` と `package-lock.json` だけ（根の中・SHA-256 が合うもの・
//!   中身が変わるときだけ）。1 ファイルだけ・C のプロジェクトには書かない
//! - IPC の 1 メッセージは [`zc_remote_ipc::MAX_FRAME`] を超えないように分ける（ログは 64KB、
//!   診断は約 96KB ごと。診断のメッセージは 1 件 2KB で切る）
//! - 相手が切れたら（EOF・ECONNRESET）、その相手の依頼は取り消す
//! - SD の控えを戻した実行ファイルには、控えの時刻（最後に成功した時刻）を付ける
//!   （CLI が「前の版（日時）」を出すため）
//! - **未保存の中身の重ね**（`Submit` の `overlay`。打鍵が止まったときの check）: 走査の
//!   あとで、目録の同じパスの大きさと SHA-256 を重ねの中身に差し替え、中身は RAM から送る
//!   （SD には書かない。ハッシュの覚えはディスクの中身のまま）。重ねの合計が
//!   [`zc_remote_ipc::MAX_OVERLAY_BYTES`] を超えたら「上限」で断る
//! - `SubmitFlags::AUTO` の依頼は、設定 `check_on_save` が `idle` のときだけ受ける（ほかは
//!   すぐ「対象ではない」。設定を読み直しても効く）
//! - 相手の版は [`zc_remote_ipc::MIN_IPC_VERSION`] から受ける（返事の `Hello` は相手の版。
//!   版 1 の zcode も、重ね無しでそのまま使える）

use crate::artifact::{self, ArtifactError, Dirs, LastGood, Meta, Placed, Receiver};
use crate::auth::{AccessServiceToken, AuthError, Credential, NoCredential, Retry};
use crate::config::{BackendChoice, CheckOnSave, Config};
use crate::net::{self, Conn, Dial, DialStats, DialStep, Incoming, NetError, Stage, Timeouts};
use crate::scan::{self, HashCache, Overlaid, Project, ProjectError, ScanError, Shape};
use crate::session::{Action, Event, JobRequest, Outcome, Session, Ticket};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::IpAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use zc_core::time::now_us;
use zc_remote_ipc as ipc;
use zc_wire::msg as wire;

/// serve の指定。
#[derive(Clone, Debug)]
pub struct ServeOptions {
    /// ソケットの場所（既定は [`zc_remote_ipc::socket_path`]）。
    pub socket: PathBuf,
    pub config: Config,
    /// 相手もジョブも無いまま、これだけ経ったら終わる。
    pub idle_exit: Duration,
    /// 最後のジョブのあと、ビルド係への接続を保つ時間。
    pub keep_warm: Duration,
    /// 段ごとの所要を標準エラーに出す（`--stats`）。
    pub stats: bool,
}

impl ServeOptions {
    /// 既定（無通信 180 秒で終了、接続は 75 秒保つ）。
    pub fn new(config: Config) -> ServeOptions {
        ServeOptions { socket: zc_remote_ipc::socket_path(), config, idle_exit: Duration::from_secs(180), keep_warm: Duration::from_secs(75), stats: false }
    }
}

/// 終わり方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeExit {
    /// 無通信で終わった。
    Idle,
    /// シグナル、または [`zc_remote_ipc::Request::Shutdown`]。
    Stopped,
    /// もう別の serve が動いていた。
    AlreadyRunning,
}

/// 進み具合とログをまとめる間隔。
const COALESCE_US: u64 = 33_000;
/// ログがこれだけ溜まったら、間隔を待たずに送る。
const LOG_FLUSH_BYTES: usize = 4096;
/// IPC のログ 1 メッセージに入れる量。
const LOG_FRAME_BYTES: usize = 64 << 10;
/// IPC の診断・試験 1 メッセージの目安。
const DIAG_FRAME_BYTES: usize = 96 << 10;
/// 診断・試験のメッセージ 1 件の上限。
const MESSAGE_MAX: usize = 2048;
/// 相手の送り待ちの上限（超えたら切る）。
const CLIENT_OUT_MAX: usize = 1 << 20;
/// 相手 1 人から 1 回の起床で読む量。
const CLIENT_READ: usize = 16 << 10;
/// 同時につなげる相手の数。
const MAX_CLIENTS: usize = 16;
/// ビルド係から 1 回の起床で読む量。
const NET_READ_BUDGET: usize = 64 << 10;
/// ビルド係への送り待ちがこれを超えている間は、次の中身を読まない。
const NET_HIGH_WATER: usize = 256 << 10;
/// `auto` の選んだ結果を覚えておく時間。
const CHOICE_KEEP_US: u64 = 60_000_000;
/// `auto` で PC を試すときの時間切れ。
const PROBE: Duration = Duration::from_millis(300);
/// 走査のあとで書き換わったファイルのために、目録を作り直す回数の上限。
const MAX_RESCANS: u8 = 3;

// ---- 相手（UNIX ソケット） ----

/// まとめて持っている、ジョブ 1 つぶんの進み具合とログ。
struct Held {
    tag: ipc::JobTag,
    req: ipc::ReqId,
    phase: Option<ipc::Response>,
    log: Vec<u8>,
}

struct Client {
    id: u32,
    stream: UnixStream,
    reader: ipc::FrameReader,
    out: Vec<u8>,
    out_pos: usize,
    /// `Hello` を受けるまで `None`。
    kind: Option<ipc::ClientKind>,
    /// 送り終えたら閉じる（版が違う相手）。
    closing: bool,
    dead: bool,
    held: Vec<Held>,
    /// まとめて持っているものを送る時刻。
    flush_at: Option<u64>,
    /// ジョブごとの、最後に送った段階。
    phases: Vec<(ipc::JobTag, ipc::Phase)>,
}

impl Client {
    fn send(&mut self, r: &ipc::Response) {
        ipc::encode_response(r, &mut self.out);
        if self.out.len() - self.out_pos > CLIENT_OUT_MAX {
            // 読まない相手を待たない。
            self.dead = true;
        }
    }

    fn held(&mut self, tag: ipc::JobTag, req: ipc::ReqId) -> &mut Held {
        let i = match self.held.iter().position(|h| h.tag == tag) {
            Some(i) => i,
            None => {
                self.held.push(Held { tag, req, phase: None, log: Vec::new() });
                self.held.len() - 1
            }
        };
        &mut self.held[i]
    }

    fn send_log(&mut self, req: ipc::ReqId, tag: ipc::JobTag, log: Vec<u8>) {
        for part in log.chunks(LOG_FRAME_BYTES) {
            self.send(&ipc::Response::Log { req, tag, bytes: part.to_vec() });
        }
    }

    /// そのジョブの、まとめて持っているログを先に送る（順番を保つ）。進み具合は捨てる
    /// （このあと新しい知らせが続く）。
    fn flush_tag(&mut self, tag: ipc::JobTag) {
        if let Some(i) = self.held.iter().position(|h| h.tag == tag) {
            let h = self.held.swap_remove(i);
            if !h.log.is_empty() {
                self.send_log(h.req, h.tag, h.log);
            }
        }
    }

    /// 進み具合。段階が変わったらすぐ、同じ段階の中の数字は最新だけをまとめて送る。
    fn phase(&mut self, now: u64, req: ipc::ReqId, tag: ipc::JobTag, phase: ipc::Phase, resp: ipc::Response) {
        match self.phases.iter_mut().find(|(t, _)| *t == tag) {
            Some((_, p)) if *p == phase => {
                self.held(tag, req).phase = Some(resp);
                self.flush_at.get_or_insert(now + COALESCE_US);
                return;
            }
            Some((_, p)) => *p = phase,
            None => self.phases.push((tag, phase)),
        }
        self.flush_tag(tag);
        self.send(&resp);
    }

    fn log(&mut self, now: u64, req: ipc::ReqId, tag: ipc::JobTag, bytes: &[u8]) {
        let h = self.held(tag, req);
        h.log.extend_from_slice(bytes);
        if h.log.len() >= LOG_FLUSH_BYTES {
            let log = std::mem::take(&mut h.log);
            self.send_log(req, tag, log);
        } else {
            self.flush_at.get_or_insert(now + COALESCE_US);
        }
    }

    /// 時間が来た: まとめて持っているものを全部送る。
    fn flush_held(&mut self) {
        self.flush_at = None;
        for h in std::mem::take(&mut self.held) {
            if !h.log.is_empty() {
                self.send_log(h.req, h.tag, h.log);
            }
            if let Some(p) = h.phase {
                self.send(&p);
            }
        }
    }

    fn forget(&mut self, tag: ipc::JobTag) {
        self.phases.retain(|(t, _)| *t != tag);
        self.held.retain(|h| h.tag != tag);
    }

    fn pending(&self) -> usize {
        self.out.len() - self.out_pos
    }

    /// 書けるだけ書く。
    fn write(&mut self) {
        while self.out_pos < self.out.len() {
            match self.stream.write(&self.out[self.out_pos..]) {
                Ok(0) => {
                    self.dead = true;
                    return;
                }
                Ok(n) => self.out_pos += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    // EPIPE・ECONNRESET: 相手が去った。
                    self.dead = true;
                    return;
                }
            }
        }
        self.out.clear();
        self.out_pos = 0;
        if self.closing {
            self.dead = true;
        }
    }
}

// ---- ジョブ ----

/// 知らせの届け先。
#[derive(Clone, Copy)]
struct Who {
    client: Option<u32>,
    req: ipc::ReqId,
    tag: ipc::JobTag,
}

/// 終わり方（状態・終了コード・人に見せる 1 行）。
struct End(ipc::DoneStatus, i32, String);

/// 依頼の中身（[`ipc::Request::Submit`]）。
struct Submit {
    req: ipc::ReqId,
    job: ipc::JobKind,
    lang: ipc::RemoteLang,
    flags: ipc::SubmitFlags,
    file: PathBuf,
    overlay: Vec<ipc::OverlayFile>,
}

/// 依頼 1 つ（[`Ticket`] 1 つ）ぶんの持ち物。
struct JobCtx {
    ticket: Ticket,
    who: Who,
    project: Project,
    request: JobRequest,
    flags: ipc::SubmitFlags,
    started_us: u64,
    /// `artifact` の中身（[`Event::ArtifactBegin`]）。
    art: Option<Meta>,
    rx: Option<Receiver>,
    placed: Option<Placed>,
    art_failed: Option<String>,
    art_retried: bool,
    rescans: u8,
    /// 未保存の中身（目録のこのパスは、ディスクではなくこれを送る）。
    overlay: Vec<Overlaid>,
}

fn ipc_backend(b: Option<Which>) -> ipc::Backend {
    match b {
        None => ipc::Backend::Unknown,
        Some(Which::Pc) => ipc::Backend::Pc,
        Some(Which::Cloudflare) => ipc::Backend::Cloudflare,
    }
}

fn clip(n: u64) -> u32 {
    n.min(u32::MAX as u64) as u32
}

/// 文字の境目で切る。
fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut n = max;
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        s.truncate(n);
    }
    s
}

fn convert_diag(d: wire::Diag) -> zc_core::Diagnostic {
    zc_core::Diagnostic {
        line: d.line,
        col: d.col,
        end: d.end,
        severity: match d.severity {
            wire::Severity::Error => zc_core::Severity::Error,
            wire::Severity::Warning => zc_core::Severity::Warning,
        },
        code: d.code.filter(|c| !c.is_empty()).map(|c| truncate(c, 64).into_boxed_str()),
        message: truncate(d.message, MESSAGE_MAX),
    }
}

/// 診断を、IPC の 1 メッセージに収まる束に分ける（空でも 1 束）。
fn diag_frames(files: Vec<wire::FileDiags>) -> Vec<Vec<ipc::FileDiags>> {
    let mut frames = vec![Vec::new()];
    let mut used = 0usize;
    for f in files {
        let path = truncate(f.path, 1024);
        let mut cur = ipc::FileDiags { path: path.clone(), diags: Vec::new() };
        used += 4 + path.len();
        for d in f.diags {
            let d = convert_diag(d);
            let size = 21 + d.code.as_deref().map_or(0, str::len) + d.message.len();
            if used + size > DIAG_FRAME_BYTES && (!cur.diags.is_empty() || !frames.last().unwrap().is_empty()) {
                // 束を閉じて、同じファイルの続きを次の束へ。
                if !cur.diags.is_empty() {
                    frames.last_mut().unwrap().push(std::mem::replace(&mut cur, ipc::FileDiags { path: path.clone(), diags: Vec::new() }));
                }
                frames.push(Vec::new());
                used = 4 + path.len();
            }
            used += size;
            cur.diags.push(d);
        }
        frames.last_mut().unwrap().push(cur);
    }
    frames
}

fn convert_tests(passed: u32, failed: u32, ignored: u32, cases: Vec<wire::TestCase>) -> ipc::TestReport {
    let mut out = Vec::new();
    let mut used = 0usize;
    // 落ちた試験を先に（入りきらないときに、落ちた方を残す）。
    let (fails, oks): (Vec<_>, Vec<_>) = cases.into_iter().partition(|c| !c.passed);
    for c in fails.into_iter().chain(oks) {
        let c = ipc::TestCase {
            name: truncate(c.name, 512),
            passed: c.passed,
            location: match (c.path, c.line) {
                (Some(p), Some(l)) => Some((truncate(p, 1024), l)),
                _ => None,
            },
            message: truncate(c.message, MESSAGE_MAX),
        };
        let size = 11 + c.name.len() + c.location.as_ref().map_or(0, |(p, _)| p.len()) + c.message.len();
        if used + size > DIAG_FRAME_BYTES || out.len() >= u16::MAX as usize {
            break;
        }
        used += size;
        out.push(c);
    }
    ipc::TestReport { passed, failed, ignored, cases: out }
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    zc_wire::tree::hex(&sha2::Sha256::digest(data))
}

/// ビルド係が作った lock を書き戻す。書いたら `Ok(Some(パス))`、同じ中身・対象でなければ
/// `Ok(None)`。
fn write_back(project: &Project, rel: &str, sha256: &str, text: &str) -> io::Result<Option<PathBuf>> {
    let bad = |why: &str| Err(io::Error::new(io::ErrorKind::InvalidInput, format!("書き戻せない（{why}）: {rel}")));
    if project.shape != Shape::Tree {
        return Ok(None);
    }
    if !zc_wire::tree::is_canonical_path(rel) {
        return bad("パスの形");
    }
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if name != "Cargo.lock" && name != "package-lock.json" {
        return bad("lock ファイルではない");
    }
    if text.len() > zc_wire::limits::MAX_WRITEBACK_BYTES {
        return bad("大きすぎる");
    }
    if sha256_hex(text.as_bytes()) != sha256 {
        return bad("SHA-256 が合わない");
    }
    let path = project.root.join(rel);
    let dir = path.parent().unwrap_or(&project.root);
    // フォルダがシンボリックリンクで根の外へ出ていないこと。
    if !std::fs::canonicalize(dir)?.starts_with(&project.root) {
        return bad("根の外");
    }
    match std::fs::symlink_metadata(&path) {
        Ok(m) if !m.is_file() => return bad("ふつうのファイルではない"),
        Ok(_) => {
            if std::fs::read(&path).is_ok_and(|old| old == text.as_bytes()) {
                return Ok(None);
            }
        }
        Err(_) => {}
    }
    // 一時ファイル → fsync → rename。
    let tmp = dir.join(format!(".{name}.zc-tmp"));
    let r = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map(|()| Some(path))
}

/// 人に見せる失敗の理由。
fn describe_net(e: &NetError) -> (ipc::DoneStatus, String) {
    use ipc::DoneStatus as S;
    match e {
        NetError::Offline => (S::Offline, "ネットワークが無い".into()),
        NetError::Timeout(stage) => (S::Offline, format!("ビルド係につながらない（時間切れ: {stage:?}）")),
        NetError::Unreachable(Stage::Dns, _) => (S::Offline, "ビルド係の名前を引けない".into()),
        NetError::Unreachable(_, errno) => (S::Offline, format!("ビルド係につながらない（{}）", io::Error::from_raw_os_error(*errno))),
        NetError::Tls(m) => (S::Error, format!("TLS の失敗: {m}")),
        NetError::Clock => (S::Clock, "時計が合っていない（NTP の同期待ち）".into()),
        NetError::Auth(0) => (S::Auth, "認証の設定が無い（~/.config/zcode/remote-token.json）".into()),
        NetError::Auth(status) => (S::Auth, format!("認証を断られた（HTTP {status}）。トークンを確かめる")),
        NetError::Busy(status, Some(s)) => (S::Limit, format!("上限に当たった（HTTP {status}）。あと {s} 秒")),
        NetError::Busy(status, None) => (S::Limit, format!("上限に当たった（HTTP {status}）")),
        NetError::Protocol(m) => (S::Error, format!("ビルド係の返事がおかしい: {m}")),
        NetError::Closed(code) => (S::Offline, format!("ビルド係との接続が切れた（{code}）")),
    }
}

fn describe_auth(e: &AuthError, path: &Path) -> String {
    match e {
        AuthError::Missing => format!("トークンが無い（{}）", path.display()),
        AuthError::Permissions(mode) => format!("{} の権限が広い（{mode:o}）。chmod 600 にする", path.display()),
        AuthError::Invalid(why) => format!("{} を読めない: {why}", path.display()),
        AuthError::Rejected(status) => format!("認証を断られた（HTTP {status}）。トークンを確かめる"),
    }
}

/// ビルド係との接続以外の全部（接続から届いたものを処理している間も触れるように分けてある）。
struct Core {
    opts: ServeOptions,
    session: Session,
    jobs: Vec<JobCtx>,
    clients: Vec<Client>,
    cache: HashCache,
    dirs: Option<Dirs>,
    next_tag: u32,
    next_client: u32,
    /// 状態機械の「やってほしいこと」のうち、接続に触るもの（あとで [`Serve`] が処理する）。
    deferred: VecDeque<Action>,
    /// SD の控えを書く予定（project id, 時刻）。
    persist: Vec<(String, u64)>,
    /// 今（最後に）選んだビルド係。
    backend: Option<Which>,
    connected: bool,
    /// 最後の通信の失敗（やり直しを使い切ったときの理由）。
    last_err: Option<NetError>,
    /// 資格を用意できなかった理由。
    auth_msg: Option<String>,
    /// 先につないでほしい（`Warm`）。
    want_warm: bool,
    /// 設定のファイルが変わったら読み直す（[`run_reloading`]）。
    reload: bool,
    /// 読んだときの設定のファイルの目印（大きさ・mtime）。無ければ `None`。
    cfg_stamp: Option<(u64, i64, i64)>,
    /// 設定を読めなかった理由（依頼に「設定」で返す）。
    cfg_err: Option<String>,
    stop: Option<ServeExit>,
}

fn config_stamp() -> Option<(u64, i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(crate::config::config_path()).ok()?;
    Some((m.len(), m.mtime(), m.mtime_nsec()))
}

impl Core {
    fn new_tag(&mut self) -> ipc::JobTag {
        let t = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1).max(1);
        ipc::JobTag(t)
    }

    fn stat(&self, f: impl FnOnce() -> String) {
        if self.opts.stats {
            eprintln!("zc-remote serve: {}", f());
        }
    }

    /// 頼んだ相手（`req` つき）と、`editors` なら zcode の相手（`req` = 0）に送る。
    fn deliver(&mut self, who: Who, editors: bool, make: impl Fn(ipc::ReqId) -> ipc::Response) {
        for c in &mut self.clients {
            let req = if Some(c.id) == who.client {
                who.req
            } else if editors && c.kind == Some(ipc::ClientKind::Editor) {
                ipc::ReqId::NONE
            } else {
                continue;
            };
            c.flush_tag(who.tag);
            c.send(&make(req));
        }
    }

    fn phase(&mut self, now: u64, who: Who, root: &Path, phase: ipc::Phase, done: u32, total: u32) {
        for c in &mut self.clients {
            let req = if Some(c.id) == who.client {
                who.req
            } else if c.kind == Some(ipc::ClientKind::Editor) {
                ipc::ReqId::NONE
            } else {
                continue;
            };
            c.phase(now, req, who.tag, phase, ipc::Response::Phase { req, tag: who.tag, root: root.to_owned(), phase, done, total });
        }
    }

    fn phase_all(&mut self, now: u64, phase: ipc::Phase) {
        for i in 0..self.jobs.len() {
            let (who, root) = (self.jobs[i].who, self.jobs[i].project.root.clone());
            self.phase(now, who, &root, phase, 0, 0);
        }
    }

    /// 終わりの知らせ。`editors` は、そのジョブの進み具合を zcode にも流していたとき。
    fn done(&mut self, now: u64, who: Who, editors: bool, started_us: u64, end: End) {
        let End(status, exit, message) = end;
        let backend = ipc_backend(self.backend);
        let elapsed_ms = clip(now.saturating_sub(started_us) / 1000);
        let message = truncate(message, MESSAGE_MAX);
        self.deliver(who, editors, |req| ipc::Response::Done { req, tag: who.tag, status, exit, backend, elapsed_ms, message: message.clone() });
        for c in &mut self.clients {
            c.forget(who.tag);
        }
    }

    /// 依頼を受ける。
    fn on_submit(&mut self, now: u64, cid: u32, kind: ipc::ClientKind, submit: Submit) {
        use ipc::DoneStatus as S;
        let Submit { req, job, lang, flags, file, overlay } = submit;
        let who = Who { client: Some(cid), req, tag: self.new_tag() };
        // 打鍵が止まったときの check は、設定が idle のときだけ（zcode の check だけ）。
        if flags.contains(ipc::SubmitFlags::AUTO) && !(kind == ipc::ClientKind::Editor && job == ipc::JobKind::Check && self.opts.config.check_on_save == CheckOnSave::Idle) {
            return self.done(now, who, false, now, End(S::NotApplicable, 0, "打鍵が止まったときの check は切ってある（check_on_save = idle で使う）".into()));
        }
        if overlay.iter().map(|o| o.bytes.len()).sum::<usize>() > ipc::MAX_OVERLAY_BYTES {
            return self.done(now, who, false, now, End(S::Limit, 0, format!("未保存の中身が {}MiB を超えている", ipc::MAX_OVERLAY_BYTES >> 20)));
        }
        let (wkind, wlang) = (
            match job {
                ipc::JobKind::Build => wire::Kind::Build,
                ipc::JobKind::Check => wire::Kind::Check,
                ipc::JobKind::Test => wire::Kind::Test,
            },
            crate::cli::wire_lang(lang),
        );
        // 根を先に探す（対象でないファイルは、設定が無くても「対象ではない」）。
        let project = match scan::find_project(&file, wlang) {
            Ok(p) => p,
            Err(ProjectError::NotApplicable) => return self.done(now, who, false, now, End(S::NotApplicable, 0, "ビルド係の対象ではない".into())),
            Err(ProjectError::Io(m)) => return self.done(now, who, false, now, End(S::Error, 0, m)),
        };
        if let Some(why) = self.cfg_err.clone().or_else(|| self.opts.config.unusable().map(str::to_owned)) {
            return self.done(now, who, false, now, End(S::Auth, 0, why));
        }
        // 保存時の check の設定（zcode からの check にだけ当てはめる）。
        if kind == ipc::ClientKind::Editor && job == ipc::JobKind::Check && self.opts.config.check_on_save == CheckOnSave::Manual {
            return self.done(now, who, false, now, End(S::NotApplicable, 0, "保存時の check は切ってある（check_on_save = manual）".into()));
        }
        let only_warm = flags.contains(ipc::SubmitFlags::ONLY_IF_WARM) || (kind == ipc::ClientKind::Editor && job == ipc::JobKind::Check && self.opts.config.check_on_save == CheckOnSave::Warm);
        if only_warm && !self.connected {
            return self.done(now, who, false, now, End(S::Offline, 0, "接続が冷えている".into()));
        }
        let overlay = scan::resolve_overlay(&project, overlay.into_iter().map(|o| (o.path, o.bytes)).collect());
        let manifest = match self.scan(&project).and_then(|m| scan::apply_overlay(m, &overlay)) {
            Ok(m) => m,
            Err(ScanError::TooLarge(m)) => return self.done(now, who, false, now, End(S::Limit, 0, m)),
            Err(ScanError::Io(m)) => return self.done(now, who, false, now, End(S::Error, 0, m)),
        };
        let request = JobRequest {
            project: project.id.clone(),
            manifest,
            kind: wkind,
            lang: wlang,
            opts: wire::JobOpts { release: flags.contains(ipc::SubmitFlags::RELEASE), entry: project.entry.clone(), filter: None },
            want_log: flags.contains(ipc::SubmitFlags::WANT_LOG),
        };
        self.phase(now, who, &project.root, ipc::Phase::Scanned, 0, 0);
        let ticket = self.session.submit(now, request.clone());
        self.jobs.push(JobCtx { ticket, who, project, request, flags, started_us: now, art: None, rx: None, placed: None, art_failed: None, art_retried: false, rescans: 0, overlay });
    }

    fn scan(&mut self, project: &Project) -> Result<Arc<zc_wire::tree::Manifest>, ScanError> {
        let now_ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
        let (m, st) = self.cache.scan(project, now_ns)?;
        self.stat(|| format!("走査 {} ファイル（読み直し {} / {} バイト）歩く {}µs ハッシュ {}µs", st.files, st.hashed_files, st.hashed_bytes, st.walk_us, st.hash_us));
        Ok(m)
    }

    /// 走査のあとでファイルが書き換わっていた依頼を、目録を作り直して出し直す。
    fn rescan(&mut self, now: u64, ticket: Ticket) {
        let Some(i) = self.jobs.iter().position(|j| j.ticket == ticket) else { return };
        let mut ctx = self.jobs.remove(i);
        ctx.rescans += 1;
        let fail = |core: &mut Core, ctx: JobCtx, status, m: String| core.done(now, ctx.who, true, ctx.started_us, End(status, 0, m));
        if ctx.rescans > MAX_RESCANS {
            return fail(self, ctx, ipc::DoneStatus::Error, "送っている間にファイルが書き換わり続けている".into());
        }
        match self.scan(&ctx.project).and_then(|m| scan::apply_overlay(m, &ctx.overlay)) {
            Ok(m) => {
                ctx.request.manifest = m;
                self.phase(now, ctx.who, &ctx.project.root.clone(), ipc::Phase::Scanned, 0, 0);
                ctx.ticket = self.session.submit(now, ctx.request.clone());
                self.jobs.push(ctx);
            }
            Err(ScanError::TooLarge(m)) => fail(self, ctx, ipc::DoneStatus::Limit, m),
            Err(ScanError::Io(m)) => fail(self, ctx, ipc::DoneStatus::Error, m),
        }
    }

    /// 状態機械の出力を取り出す。知らせはその場で処理し、接続に触るものは `deferred` へ。
    /// `frame` は、いま状態機械に渡したバイナリフレーム（成果物の中身はこの中の範囲で届く）。
    fn absorb(&mut self, now: u64, frame: Option<&[u8]>) {
        while let Some(a) = self.session.poll_action() {
            match a {
                Action::Emit(ev) => self.on_event(now, ev, frame),
                other => self.deferred.push_back(other),
            }
        }
    }

    fn on_event(&mut self, now: u64, ev: Event, frame: Option<&[u8]>) {
        let ticket = match &ev {
            Event::Uploading { ticket, .. }
            | Event::Queued { ticket, .. }
            | Event::Running { ticket }
            | Event::Log { ticket, .. }
            | Event::Diagnostics { ticket, .. }
            | Event::Tests { ticket, .. }
            | Event::WriteBack { ticket, .. }
            | Event::ArtifactBegin { ticket, .. }
            | Event::ArtifactData { ticket, .. }
            | Event::Done { ticket, .. } => *ticket,
        };
        let Some(i) = self.jobs.iter().position(|j| j.ticket == ticket) else { return };
        let who = self.jobs[i].who;
        match ev {
            Event::Uploading { done, total, .. } => {
                let root = self.jobs[i].project.root.clone();
                self.phase(now, who, &root, ipc::Phase::Uploading, clip(done), clip(total));
            }
            Event::Queued { ahead, .. } => {
                let root = self.jobs[i].project.root.clone();
                self.phase(now, who, &root, ipc::Phase::Queued, ahead, 0);
            }
            Event::Running { .. } => {
                let root = self.jobs[i].project.root.clone();
                self.phase(now, who, &root, ipc::Phase::Running, 0, 0);
            }
            Event::Log { text, .. } => {
                if let Some(c) = self.clients.iter_mut().find(|c| Some(c.id) == who.client) {
                    c.log(now, who.req, who.tag, text.as_bytes());
                }
            }
            Event::Diagnostics { complete, files, .. } => {
                let root = self.jobs[i].project.root.clone();
                let frames = diag_frames(files);
                let n = frames.len();
                for (k, files) in frames.into_iter().enumerate() {
                    let last = k + 1 == n;
                    self.deliver(who, true, |req| ipc::Response::Diagnostics { req, tag: who.tag, root: root.clone(), complete, last, files: files.clone() });
                }
            }
            Event::Tests { passed, failed, ignored, cases, .. } => {
                let root = self.jobs[i].project.root.clone();
                let report = convert_tests(passed, failed, ignored, cases);
                self.deliver(who, false, |req| ipc::Response::Tests { req, tag: who.tag, root: root.clone(), report: report.clone() });
            }
            Event::WriteBack { path, sha256, text, .. } => match write_back(&self.jobs[i].project, &path, &sha256, &text) {
                Ok(Some(p)) => self.deliver(who, false, |req| ipc::Response::FileWritten { req, tag: who.tag, path: p.clone() }),
                Ok(None) => {}
                Err(e) => self.stat(|| e.to_string()),
            },
            Event::ArtifactBegin { kind, name, sha256, bytes, zbytes, offset, .. } => {
                let j = &mut self.jobs[i];
                let meta = Meta { project: j.project.id.clone(), tree: j.request.manifest.tree_id(), kind, name, sha256, bytes, zbytes };
                // 続きの位置が合わない・別の成果物なら、受け取りを開き直す（途中のファイルは残す）。
                if j.art.as_ref() != Some(&meta) || j.rx.as_ref().is_some_and(|r| r.received() != offset) {
                    j.rx = None;
                }
                j.art = Some(meta);
            }
            Event::ArtifactData { offset, data, last, .. } => {
                let Some(frame) = frame else { return };
                let j = &mut self.jobs[i];
                if j.art_failed.is_some() {
                    return;
                }
                let Some(meta) = j.art.clone() else { return };
                let zbytes = meta.zbytes;
                let r = (|| {
                    if j.rx.is_none() {
                        let dirs = self.dirs.as_ref().ok_or_else(|| ArtifactError::Io("成果物の置き場が無い（XDG_RUNTIME_DIR）".into()))?;
                        j.rx = Some(Receiver::begin(dirs, meta, 0)?);
                    }
                    let rx = j.rx.as_mut().unwrap();
                    rx.write(offset, &frame[data.clone()])?;
                    if last {
                        j.placed = Some(j.rx.take().unwrap().finish()?);
                    }
                    Ok::<(), ArtifactError>(())
                })();
                match r {
                    Ok(()) => {
                        let root = j.project.root.clone();
                        self.phase(now, who, &root, ipc::Phase::Downloading, clip(offset + data.len() as u64), clip(zbytes));
                    }
                    Err(e) => {
                        if let Some(rx) = j.rx.take() {
                            rx.abort();
                        }
                        j.art_failed = Some(format!("{e:?}"));
                    }
                }
            }
            Event::Done { outcome, .. } => self.on_done(now, i, outcome),
        }
    }

    fn on_done(&mut self, now: u64, i: usize, outcome: Outcome) {
        use ipc::DoneStatus as S;
        let mut ctx = self.jobs.remove(i);
        let is_build = ctx.request.kind == wire::Kind::Build;
        let (status, exit, message) = match outcome {
            Outcome::Finished { status: wire::Status::Ok, exit, .. } if is_build => {
                // 先頭の依頼は自分で受け取っている。まとめられた依頼は、置き場を見る。
                let placed = ctx.placed.take().or_else(|| {
                    let meta = ctx.art.as_ref().filter(|_| ctx.art_failed.is_none())?;
                    let p = artifact::find_placed(self.dirs.as_ref()?, &ctx.project.id)?;
                    let want = format!("{}-{}", meta.name, &meta.tree[..8]);
                    (p.path.file_name().is_some_and(|n| n.to_string_lossy() == want)).then_some(p)
                });
                match placed {
                    Some(p) => {
                        let kind = match p.kind {
                            wire::ArtifactKind::Exe => ipc::ArtifactKind::Exe,
                            wire::ArtifactKind::Js => ipc::ArtifactKind::Js,
                        };
                        self.deliver(ctx.who, false, |req| ipc::Response::Artifact { req, tag: ctx.who.tag, kind, stale: false, path: p.path.clone() });
                        self.persist.retain(|(id, _)| *id != ctx.project.id);
                        self.persist.push((ctx.project.id.clone(), now + artifact::LASTGOOD_DELAY.as_micros() as u64));
                        (S::Ok, exit, String::new())
                    }
                    None if !ctx.art_retried => {
                        // 壊れていた。同じ依頼を 1 回だけ出し直す（受付に結果が残っている）。
                        self.stat(|| format!("成果物を受け取れなかった（{}）。出し直す", ctx.art_failed.as_deref().unwrap_or("届いていない")));
                        ctx.art_retried = true;
                        (ctx.art, ctx.art_failed) = (None, None);
                        if let Some(rx) = ctx.rx.take() {
                            rx.abort();
                        }
                        ctx.ticket = self.session.submit(now, ctx.request.clone());
                        self.jobs.push(ctx);
                        return;
                    }
                    None => (S::Error, exit, format!("成果物を受け取れなかった（{}）", ctx.art_failed.as_deref().unwrap_or("届いていない"))),
                }
            }
            Outcome::Finished { status, exit, .. } => match status {
                wire::Status::Ok => (S::Ok, exit, String::new()),
                wire::Status::Failed => (S::Failed, exit, String::new()),
                wire::Status::Cancelled => (S::Cancelled, exit, String::new()),
                wire::Status::Timeout => (S::Error, exit, "ビルド係で時間切れ".into()),
                wire::Status::Lost => (S::Error, exit, "ビルド係がジョブを失くした".into()),
                wire::Status::Error => (S::Error, exit, "ビルド係の中の失敗（ログを見る）".into()),
            },
            Outcome::Rejected { code, message } => match code {
                wire::ErrorCode::Limit | wire::ErrorCode::TooLarge => (S::Limit, 0, message),
                _ if unsupported_lang(code, &message, ctx.request.lang) => {
                    (S::Error, 0, format!("ビルド係が {} に対応していない（ビルド係のイメージが古い。deploy し直す）", ctx.request.lang.name()))
                }
                _ => (S::Error, 0, format!("ビルド係に断られた（{code:?}）: {message}")),
            },
            Outcome::GaveUp => match &self.last_err {
                Some(NetError::Auth(0)) => (S::Auth, 0, self.auth_msg.clone().unwrap_or_else(|| describe_net(&NetError::Auth(0)).1)),
                Some(e) => {
                    let (s, m) = describe_net(e);
                    (s, 0, m)
                }
                None => (S::Offline, 0, "ビルド係につながらない".into()),
            },
        };
        // 途中の受け取り: 取り消し・失敗なら捨てる。通信の失敗なら残す（次の依頼で続きから）。
        if let Some(rx) = ctx.rx.take() {
            if status != S::Offline {
                rx.abort();
            }
        }
        // ネットが無い build: 前に成功した版があれば、それを知らせる。
        if status == S::Offline && is_build && ctx.flags.contains(ipc::SubmitFlags::ALLOW_STALE) {
            if let Some(p) = self.stale(&ctx.project.id) {
                let kind = match p.kind {
                    wire::ArtifactKind::Exe => ipc::ArtifactKind::Exe,
                    wire::ArtifactKind::Js => ipc::ArtifactKind::Js,
                };
                self.deliver(ctx.who, false, |req| ipc::Response::Artifact { req, tag: ctx.who.tag, kind, stale: true, path: p.path.clone() });
            }
        }
        self.done(now, ctx.who, true, ctx.started_us, End(status, exit, message));
    }

    /// 前に成功した版（tmpfs にあればそれ、無ければ SD の控えを戻す）。
    fn stale(&mut self, project: &str) -> Option<Placed> {
        let dirs = self.dirs.as_ref()?;
        if let Some(p) = artifact::find_placed(dirs, project) {
            return Some(p);
        }
        let zcb = dirs.cache.join(format!("{project}.zcb"));
        let when = std::fs::metadata(&zcb).and_then(|m| m.modified()).ok();
        let p = LastGood::restore(dirs, project).ok().flatten()?;
        // 戻したファイルに、控えの時刻（最後に成功した時刻）を付ける。
        if let (Some(t), Ok(f)) = (when, std::fs::File::open(&p.path)) {
            let _ = f.set_modified(t);
        }
        Some(p)
    }

    fn persist_due(&mut self, now: u64, all: bool) {
        let Some(dirs) = self.dirs.clone() else { return };
        let mut i = 0;
        while i < self.persist.len() {
            if all || self.persist[i].1 <= now {
                let (project, _) = self.persist.swap_remove(i);
                let r = LastGood::persist(&dirs, &project);
                self.stat(|| format!("SD の控え {project}: {r:?}"));
            } else {
                i += 1;
            }
        }
    }
}

// ---- ビルド係との接続 ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Which {
    Pc = 0,
    Cloudflare = 1,
}

struct Dialing {
    dial: Dial,
    which: Which,
    /// `auto` の PC の試し（だめなら Cloudflare へ）。
    probing: bool,
}

enum Link {
    Down,
    Dialing(Box<Dialing>),
    Up(Box<Conn>),
}

struct Net {
    link: Link,
    /// この時刻になったらつなぎ始める。
    redial_at: Option<u64>,
    /// `auto` の選んだ結果と、選んだ時刻。
    choice: Option<(Which, u64)>,
    /// 前回つながった IP（PC・Cloudflare）。
    hints: [Option<IpAddr>; 2],
    tls: Option<Arc<rustls::ClientConfig>>,
    token: Option<AccessServiceToken>,
    auth_retried: bool,
    /// 今のジョブのために、1 回はつながった（途中で切れたのか、最初からつながらないのか）。
    was_up: bool,
    /// ジョブが無くなった接続を閉じる時刻。
    warm_until: Option<u64>,
    /// ジョブが 0 → 1 になった時刻（沈黙の判定の起点）。
    active_since: u64,
    was_active: bool,
    /// 送り待ちが減るまで待たせている中身。
    pending_blob: Option<Action>,
    /// 読み残しがある（`poll()` を待たずにもう一度読む）。
    more: bool,
    timeouts: Timeouts,
}

/// 何を待っているか（`poll()` の並びと同じ順）。
#[derive(Clone, Copy)]
enum Src {
    Signal,
    Listener,
    Client(u32),
    Net,
}

struct Serve {
    core: Core,
    net: Net,
    listener: UnixListener,
    sig: OwnedFd,
    /// 二重起動を防ぐ鍵（持っている間だけ有効）。
    _lock: std::fs::File,
    idle_since: Option<u64>,
    scratch: Box<[u8]>,
}

/// ビルド係がその言語を知らないという断りか。古い受付（2026-11-17 より前）は知らない `lang` を
/// 読めず、接続全体の `error {protocol, "読めないメッセージ: unknown variant `go`, …"}` を返して
/// 切る。新しい受付は、そのジョブだけに [`wire::UNSUPPORTED_LANG`] を含む `error {protocol}` を返す。
fn unsupported_lang(code: wire::ErrorCode, message: &str, lang: wire::Lang) -> bool {
    code == wire::ErrorCode::Protocol && (message.contains(&format!("unknown variant `{}`", lang.name())) || message.contains(wire::UNSUPPORTED_LANG))
}

fn io_err(what: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

/// SIGTERM・SIGINT・SIGHUP を止めて、signalfd で受ける。
fn signal_fd() -> io::Result<OwnedFd> {
    // SAFETY: set はこの場で初期化して使う。signalfd の戻り値は新しい fd。
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaddset(&mut set, s);
        }
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return Err(io_err("pthread_sigmask"));
        }
        let fd = libc::signalfd(-1, &set, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC);
        if fd < 0 {
            return Err(io_err("signalfd"));
        }
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

/// 相手の uid が自分と同じか。
fn same_user(s: &UnixStream) -> bool {
    let mut cred = libc::ucred { pid: 0, uid: u32::MAX, gid: u32::MAX };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred と len は有効な書き込み先。
    let r = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len) };
    // SAFETY: geteuid は失敗しない。
    r == 0 && cred.uid == unsafe { libc::geteuid() }
}

/// ソケットを作る。もう別の serve が動いていれば `None`。
fn bind(socket: &Path) -> io::Result<Option<(UnixListener, std::fs::File)>> {
    let dir = socket.parent().filter(|d| !d.as_os_str().is_empty()).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "ソケットの場所にフォルダが無い"))?;
    if !dir.is_dir() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    // 1 つに絞る: 鍵を取れた serve だけが、古いソケットを消して作り直す。
    let mut lock_path = socket.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(PathBuf::from(lock_path))?;
    // SAFETY: lock は開いている fd。
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(None);
    }
    if UnixStream::connect(socket).is_ok() {
        return Ok(None);
    }
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(Some((listener, lock)))
}

/// serve を動かす（終わるまで戻らない）。設定は `opts.config` のまま使う。
pub fn run(opts: ServeOptions) -> std::io::Result<ServeExit> {
    run_inner(opts, false, None)
}

/// [`run`] と同じ。ただし設定のファイル（[`crate::config::config_path`]）が書き換わったら、
/// 次の依頼のとき（ジョブが無ければ）読み直す。zcode が開いている間 serve は動き続けるので、
/// 人が設定を直したあと `zc-remote stop` しなくても効くようにする。`config_error` は、
/// 起動のときに設定を読めなかった理由（読み直せるまで、依頼に「設定」で返す）。
pub fn run_reloading(opts: ServeOptions, config_error: Option<String>) -> std::io::Result<ServeExit> {
    run_inner(opts, true, config_error)
}

fn run_inner(opts: ServeOptions, reload: bool, cfg_err: Option<String>) -> std::io::Result<ServeExit> {
    let Some((listener, lock)) = bind(&opts.socket)? else { return Ok(ServeExit::AlreadyRunning) };
    let sig = signal_fd()?;
    let socket = opts.socket.clone();
    let dirs = Dirs::from_env().ok();
    let mut s = Serve {
        core: Core {
            opts,
            session: Session::new(),
            jobs: Vec::new(),
            clients: Vec::new(),
            cache: HashCache::new(),
            dirs,
            next_tag: 1,
            next_client: 1,
            deferred: VecDeque::new(),
            persist: Vec::new(),
            backend: None,
            connected: false,
            last_err: None,
            auth_msg: None,
            want_warm: false,
            reload,
            cfg_stamp: if reload { config_stamp() } else { None },
            cfg_err,
            stop: None,
        },
        net: Net {
            link: Link::Down,
            redial_at: None,
            choice: None,
            hints: [None, None],
            tls: None,
            token: None,
            auth_retried: false,
            was_up: false,
            warm_until: None,
            active_since: 0,
            was_active: false,
            pending_blob: None,
            more: false,
            timeouts: Timeouts::default(),
        },
        listener,
        sig,
        _lock: lock,
        idle_since: None,
        scratch: vec![0u8; CLIENT_READ].into_boxed_slice(),
    };
    let exit = s.main_loop();
    // 終わる前に: SD の控えを書き、接続を静かに閉じ、ソケットを消す。
    s.core.persist_due(0, true);
    if let Link::Up(conn) = std::mem::replace(&mut s.net.link, Link::Down) {
        conn.close();
    }
    let _ = std::fs::remove_file(&socket);
    if s.core.opts.stats {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let pick = |k: &str| status.lines().find(|l| l.starts_with(k)).map(|l| l[k.len()..].trim().to_owned()).unwrap_or_default();
        eprintln!("zc-remote serve: 終了（{exit:?}） VmRSS {} VmHWM {}", pick("VmRSS:"), pick("VmHWM:"));
    }
    Ok(exit)
}

impl Serve {
    fn main_loop(&mut self) -> ServeExit {
        let mut fds: Vec<libc::pollfd> = Vec::new();
        let mut srcs: Vec<Src> = Vec::new();
        let mut interests = Vec::new();
        loop {
            let now = now_us();
            self.reap_clients(now);
            self.timers(now);
            self.settle(now);
            for c in &mut self.core.clients {
                if c.pending() > 0 && !c.dead {
                    c.write();
                }
            }
            if let Some(exit) = self.core.stop {
                return exit;
            }

            fds.clear();
            srcs.clear();
            let mut add = |fd: RawFd, read: bool, write: bool, src: Src| {
                fds.push(libc::pollfd { fd, events: (if read { libc::POLLIN } else { 0 }) | (if write { libc::POLLOUT } else { 0 }), revents: 0 });
                srcs.push(src);
            };
            add(self.sig.as_raw_fd(), true, false, Src::Signal);
            add(self.listener.as_raw_fd(), self.core.clients.len() < MAX_CLIENTS, false, Src::Listener);
            for c in &self.core.clients {
                add(c.stream.as_raw_fd(), true, c.pending() > 0, Src::Client(c.id));
            }
            interests.clear();
            match &self.net.link {
                Link::Down => {}
                Link::Dialing(d) => d.dial.interests(&mut interests),
                Link::Up(c) => interests.push(c.interest()),
            }
            for i in &interests {
                add(i.fd, i.read, i.write, Src::Net);
            }

            let timeout = match self.next_deadline(now) {
                Some(t) if t <= now => 0,
                // 切り上げ（早く起きて空回りしない）。
                Some(t) => ((t - now).div_ceil(1000)).min(60_000) as i32,
                None => -1,
            };
            // SAFETY: fds は有効な配列。
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if n < 0 {
                continue; // EINTR
            }
            let now = now_us();
            let mut net_ready = false;
            for (fd, src) in fds.iter().zip(&srcs) {
                if fd.revents == 0 {
                    continue;
                }
                match *src {
                    Src::Signal => {
                        let mut buf = [0u8; 128];
                        // SAFETY: buf は有効な書き込み先。
                        while unsafe { libc::read(fd.fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
                        self.core.stop = Some(ServeExit::Stopped);
                    }
                    Src::Listener => self.accept(),
                    Src::Client(id) => self.client_io(now, id, fd.revents),
                    Src::Net => net_ready = true,
                }
            }
            if net_ready || self.net.more {
                self.net_io(now);
            }
        }
    }

    /// 次に起きる時刻。
    fn next_deadline(&self, now: u64) -> Option<u64> {
        let mut d: Option<u64> = None;
        let mut at = |t: u64| d = Some(d.map_or(t, |x| x.min(t)));
        if self.net.more || !self.core.deferred.is_empty() || self.core.clients.iter().any(|c| c.dead) {
            at(now);
        }
        if let Some(t) = self.net.redial_at {
            at(t);
        }
        if let Some(t) = self.net.warm_until {
            at(t);
        }
        match &self.net.link {
            Link::Dialing(dl) => at(dl.dial.deadline_us()),
            Link::Up(c) if self.core.session.active() > 0 => at(c.last_rx_us().max(self.net.active_since) + self.net.timeouts.silence.as_micros() as u64),
            _ => {}
        }
        if let Some(t) = self.core.session.deadline_us() {
            at(t);
        }
        for c in &self.core.clients {
            if let Some(t) = c.flush_at {
                at(t);
            }
        }
        for (_, t) in &self.core.persist {
            at(*t);
        }
        if let Some(t) = self.idle_since {
            at(t + self.core.opts.idle_exit.as_micros() as u64);
        }
        d
    }

    fn timers(&mut self, now: u64) {
        if self.core.session.deadline_us().is_some_and(|t| t <= now) {
            self.core.session.on_tick(now);
        }
        for c in &mut self.core.clients {
            if c.flush_at.is_some_and(|t| t <= now) {
                c.flush_held();
            }
        }
        self.core.persist_due(now, false);

        let active = self.core.session.active() > 0;
        if active && !self.net.was_active {
            self.net.active_since = now;
        }
        self.net.was_active = active;
        let up = matches!(self.net.link, Link::Up(_));
        if active {
            self.net.warm_until = None;
            self.net.was_up |= up;
        } else {
            self.net.was_up = false;
            if up && self.net.warm_until.is_none() {
                self.net.warm_until = Some(now + self.core.opts.keep_warm.as_micros() as u64);
            }
        }
        // 用の無くなった接続を静かに閉じる（待機中に ping は打たない）。
        if !active && self.net.warm_until.is_some_and(|t| t <= now) {
            self.net.warm_until = None;
            if let Link::Up(conn) = std::mem::replace(&mut self.net.link, Link::Down) {
                conn.close();
                self.core.connected = false;
                self.core.session.on_closed(now, false);
            }
        }
        // ジョブの実行中の沈黙は、切れたものとして扱う。
        if active {
            if let Link::Up(c) = &self.net.link {
                if now.saturating_sub(c.last_rx_us().max(self.net.active_since)) >= self.net.timeouts.silence.as_micros() as u64 {
                    self.conn_failed(now, NetError::Timeout(Stage::Open));
                }
            }
        }
        if let Link::Dialing(d) = &self.net.link {
            if d.dial.deadline_us() <= now {
                self.advance_dial(now);
            }
        }
        // 先につないでおく（Warm）。
        if std::mem::take(&mut self.core.want_warm) && matches!(self.net.link, Link::Down) && self.net.redial_at.is_none() && self.core.opts.config.unusable().is_none() {
            self.net.redial_at = Some(now);
        }
        if self.net.redial_at.is_some_and(|t| t <= now) && matches!(self.net.link, Link::Down) {
            self.start_dial(now);
        }
        // 相手もジョブも無いまま、決めた時間が経ったら終わる。
        if self.core.clients.is_empty() && !active {
            let since = *self.idle_since.get_or_insert(now);
            if now.saturating_sub(since) >= self.core.opts.idle_exit.as_micros() as u64 && self.core.stop.is_none() {
                self.core.stop = Some(ServeExit::Idle);
            }
        } else {
            self.idle_since = None;
        }
    }

    /// 状態機械の出力を全部片づけ、溜まった分を送る。
    fn settle(&mut self, now: u64) {
        for _ in 0..8 {
            loop {
                self.core.absorb(now, None);
                let Some(a) = self.core.deferred.pop_front() else { break };
                self.do_action(now, a);
            }
            let Link::Up(conn) = &mut self.net.link else { return };
            if conn.queued_bytes() > 0 {
                if let Err(e) = conn.on_writable() {
                    self.conn_failed(now, e);
                    continue;
                }
            }
            let Link::Up(conn) = &self.net.link else { return };
            if self.net.pending_blob.is_some() && conn.queued_bytes() <= NET_HIGH_WATER {
                let a = self.net.pending_blob.take().unwrap();
                self.do_action(now, a);
                continue;
            }
            return;
        }
    }

    fn do_action(&mut self, now: u64, a: Action) {
        match a {
            Action::Emit(ev) => self.core.on_event(now, ev, None),
            Action::SendText(text) => {
                if let Link::Up(conn) = &mut self.net.link {
                    conn.send_text(&text);
                }
            }
            Action::SendBlob { id, manifest, path, sha256, size } => {
                let Link::Up(conn) = &mut self.net.link else { return };
                if conn.queued_bytes() > NET_HIGH_WATER {
                    self.net.pending_blob = Some(Action::SendBlob { id, manifest, path, sha256, size });
                    return;
                }
                let root = self.core.jobs.iter().find(|j| Arc::ptr_eq(&j.request.manifest, &manifest)).map(|j| j.project.root.clone());
                // 未保存の中身（どの依頼の重ねでも、同じパス・同じ中身なら使える）は RAM から送る。
                let mem = self.core.jobs.iter().flat_map(|j| j.overlay.iter()).find(|o| o.rel == path && o.sha256 == sha256 && o.bytes.len() as u64 == size).map(|o| o.bytes.clone());
                let mut sent_last = false;
                let ok = match (&mem, &root) {
                    (Some(bytes), _) => {
                        for (k, chunk) in bytes.chunks(zc_wire::limits::CHUNK_BYTES).enumerate() {
                            let offset = (k * zc_wire::limits::CHUNK_BYTES) as u64;
                            let last = offset + chunk.len() as u64 == size;
                            sent_last |= last;
                            conn.send_binary(&wire::BinHeader { kind: wire::BinKind::Blob, last, id, offset }.encode(), chunk);
                        }
                        true
                    }
                    (None, Some(root)) => scan::read_blob(root, &path, size, &sha256, &mut |offset, chunk| {
                        let last = offset + chunk.len() as u64 == size;
                        sent_last |= last;
                        conn.send_binary(&wire::BinHeader { kind: wire::BinKind::Blob, last, id, offset }.encode(), chunk);
                    })
                    .unwrap_or(false),
                    (None, None) => false,
                };
                if ok {
                    if !sent_last {
                        // 空のファイル。
                        conn.send_binary(&wire::BinHeader { kind: wire::BinKind::Blob, last: true, id, offset: 0 }.encode(), &[]);
                    }
                    self.core.session.blob_sent(now, id);
                } else {
                    // 走査のあとで書き換わった。目録を作り直す（接続は状態機械が作り直させる）。
                    for t in self.core.session.blob_failed(now, id) {
                        self.core.rescan(now, t);
                    }
                }
            }
            Action::Reconnect { after_ms } => {
                // 状態機械はもう「切れた」ものとして進んでいる。閉じてつなぎ直すだけ。
                self.net.link = Link::Down;
                self.net.pending_blob = None;
                self.net.more = false;
                self.core.connected = false;
                self.net.redial_at = Some(now + after_ms as u64 * 1000);
            }
            Action::Connect => {
                if matches!(self.net.link, Link::Down) && self.net.redial_at.is_none() {
                    self.net.redial_at = Some(now);
                }
            }
            // 温める時間は timers が数える。
            Action::Idle => {}
        }
        if self.net.redial_at.is_some_and(|t| t <= now) && matches!(self.net.link, Link::Down) {
            self.start_dial(now);
        }
    }

    /// つなぎ始める（どのビルド係かを決めるところから）。
    fn start_dial(&mut self, now: u64) {
        self.net.redial_at = None;
        let cfg = &self.core.opts.config;
        let (pc, cf) = (cfg.pc_endpoint(), cfg.cloudflare_endpoint());
        let (which, ep, probing) = match (pc, cf) {
            (Some(pc), Some(cf)) if cfg.backend == BackendChoice::Auto => match self.net.choice {
                Some((Which::Pc, at)) if now.saturating_sub(at) < CHOICE_KEEP_US => (Which::Pc, pc, false),
                Some((Which::Cloudflare, at)) if now.saturating_sub(at) < CHOICE_KEEP_US => (Which::Cloudflare, cf, false),
                _ => (Which::Pc, pc, true),
            },
            (Some(pc), _) => (Which::Pc, pc, false),
            (None, Some(cf)) => (Which::Cloudflare, cf, false),
            (None, None) => {
                self.core.auth_msg = cfg.unusable().map(str::to_owned);
                return self.dial_failed(now, NetError::Auth(0), Which::Pc, false);
            }
        };
        self.core.backend = Some(which);
        let mut none = NoCredential;
        let cred: &mut dyn Credential = match which {
            Which::Pc => &mut none,
            Which::Cloudflare => {
                if self.net.token.is_none() {
                    let path = crate::config::token_path();
                    match AccessServiceToken::load(&path) {
                        Ok(t) => self.net.token = Some(t),
                        // `ws://`（試験用）は、トークンが無ければ付けずにつなぐ。
                        Err(AuthError::Missing) if !ep.tls => {}
                        Err(e) => {
                            self.core.auth_msg = Some(describe_auth(&e, &path));
                            return self.dial_failed(now, NetError::Auth(0), which, probing);
                        }
                    }
                }
                match &mut self.net.token {
                    Some(t) => t,
                    None => &mut none,
                }
            }
        };
        let tls = ep.tls.then(|| self.net.tls.get_or_insert_with(|| net::tls_config(net::embedded_roots())).clone());
        let timeouts = if probing { Timeouts { dns: PROBE, connect: PROBE, ..self.net.timeouts } } else { self.net.timeouts };
        match Dial::start(&ep, cred, tls, timeouts, self.net.hints[which as usize], now) {
            Ok(dial) => {
                self.net.link = Link::Dialing(Box::new(Dialing { dial, which, probing }));
                self.core.phase_all(now, ipc::Phase::Connecting);
            }
            Err(e) => self.dial_failed(now, e, which, probing),
        }
    }

    fn dial_failed(&mut self, now: u64, e: NetError, which: Which, probing: bool) {
        self.net.link = Link::Down;
        self.core.connected = false;
        self.core.stat(|| format!("つなげない（{which:?}）: {e:?}"));
        // PC に届かない: Cloudflare へ（結果は 60 秒覚える）。
        if probing && matches!(e, NetError::Unreachable(..) | NetError::Timeout(_)) {
            self.net.choice = Some((Which::Cloudflare, now));
            return self.start_dial(now);
        }
        if let NetError::Auth(status) = e {
            if status != 0 && which == Which::Cloudflare {
                if let Some(t) = &mut self.net.token {
                    // 断られるまでの間に人がトークンを直していたら、もう 1 回だけ。
                    if t.rejected(status) == Retry::Once && !self.net.auth_retried {
                        self.net.auth_retried = true;
                        return self.start_dial(now);
                    }
                }
            }
        }
        if self.core.opts.config.backend == BackendChoice::Auto {
            self.net.choice = None;
        }
        // やり直しても同じ失敗と、最初からつながらないとき（オフライン）は、待たせずに返す。
        let fatal = match e {
            NetError::Offline | NetError::Auth(_) | NetError::Clock | NetError::Tls(_) | NetError::Busy(..) | NetError::Protocol(_) => true,
            NetError::Unreachable(..) | NetError::Timeout(_) | NetError::Closed(_) => !self.net.was_up,
        };
        if !matches!(e, NetError::Auth(0)) {
            self.core.auth_msg = None;
        }
        self.core.last_err = Some(e);
        self.core.session.on_closed(now, fatal);
    }

    fn advance_dial(&mut self, now: u64) {
        let Link::Dialing(d) = &mut self.net.link else { return };
        let (which, probing) = (d.which, d.probing);
        match d.dial.advance(now) {
            Ok(DialStep::Pending) => {}
            Ok(DialStep::Open(conn, stats)) => self.opened(now, conn, stats, which),
            Err(e) => self.dial_failed(now, e, which, probing),
        }
    }

    fn opened(&mut self, now: u64, conn: Conn, stats: DialStats, which: Which) {
        self.net.hints[which as usize] = Some(conn.peer_ip());
        // 選んだ結果を 60 秒覚える（同じ選択が続いている間は、時刻を延ばさない）。
        if !matches!(self.net.choice, Some((w, _)) if w == which) {
            self.net.choice = Some((which, now));
        }
        self.net.link = Link::Up(Box::new(conn));
        self.net.auth_retried = false;
        self.net.active_since = now;
        self.core.connected = true;
        self.core.last_err = None;
        self.core.stat(|| format!("つながった（{which:?}）: {stats:?}"));
        self.core.session.on_open(now);
    }

    /// つながっていた接続が切れた。
    fn conn_failed(&mut self, now: u64, e: NetError) {
        self.net.link = Link::Down;
        self.net.pending_blob = None;
        self.net.more = false;
        self.net.warm_until = None;
        self.core.connected = false;
        self.core.stat(|| format!("接続が切れた: {e:?}"));
        if self.core.session.active() > 0 {
            self.core.last_err = Some(e);
        }
        self.core.session.on_closed(now, false);
    }

    fn net_io(&mut self, now: u64) {
        match &self.net.link {
            Link::Down => {}
            Link::Dialing(_) => self.advance_dial(now),
            Link::Up(_) => {
                let Serve { core, net, .. } = self;
                let Link::Up(conn) = &mut net.link else { return };
                // 書けるようになっていれば、溜まっている分を先に出す。
                let r = conn.on_writable().and_then(|()| {
                    conn.on_readable(NET_READ_BUDGET, &mut |m| match m {
                        Incoming::Text(t) => {
                            core.session.on_text(now, t);
                            core.absorb(now, None);
                        }
                        Incoming::Binary(b) => {
                            core.session.on_binary(now, b);
                            core.absorb(now, Some(b));
                        }
                    })
                });
                match r {
                    // 読み残しがあれば、poll を待たずにもう一度読む（ほかの fd も 1 回見てから）。
                    Ok(more) => net.more = more,
                    Err(e) => self.conn_failed(now, e),
                }
            }
        }
    }

    fn accept(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if self.core.clients.len() >= MAX_CLIENTS || !same_user(&stream) || stream.set_nonblocking(true).is_err() {
                        continue; // 捨てる（閉じる）
                    }
                    let id = self.core.next_client;
                    self.core.next_client = self.core.next_client.wrapping_add(1).max(1);
                    self.core.clients.push(Client { id, stream, reader: ipc::FrameReader::for_requests(), out: Vec::new(), out_pos: 0, kind: None, closing: false, dead: false, held: Vec::new(), flush_at: None, phases: Vec::new() });
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }

    fn client_io(&mut self, now: u64, id: u32, revents: libc::c_short) {
        let Some(ci) = self.core.clients.iter().position(|c| c.id == id) else { return };
        let mut requests = Vec::new();
        {
            let c = &mut self.core.clients[ci];
            if revents & libc::POLLOUT != 0 {
                c.write();
            }
            if revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 && !c.closing {
                match c.stream.read(&mut self.scratch) {
                    // EOF。ECONNRESET（読んでいない知らせを残したまま相手が閉じた）も同じ。
                    Ok(0) => c.dead = true,
                    Ok(n) => {
                        c.reader.push(&self.scratch[..n]);
                        loop {
                            match c.reader.next_frame() {
                                Ok(Some(frame)) => match ipc::decode_request(frame) {
                                    Ok(r) => requests.push(r),
                                    Err(ipc::DecodeError::UnknownKind(_)) => {}
                                    Err(_) => {
                                        c.dead = true;
                                        break;
                                    }
                                },
                                Ok(None) => break,
                                Err(_) => {
                                    c.dead = true;
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                    Err(_) => c.dead = true,
                }
            }
        }
        for r in requests {
            self.on_request(now, id, r);
        }
    }

    fn on_request(&mut self, now: u64, id: u32, r: ipc::Request) {
        let Some(ci) = self.core.clients.iter().position(|c| c.id == id) else { return };
        let kind = self.core.clients[ci].kind;
        let status = |s: &Serve| ipc::Response::Status { backend: ipc_backend(s.core.backend.filter(|_| !matches!(s.net.link, Link::Down))), connected: s.core.connected, running_jobs: s.core.session.active().min(u16::MAX as usize) as u16 };
        match (r, kind) {
            (ipc::Request::Hello { version, client }, _) => {
                let c = &mut self.core.clients[ci];
                // 古い相手（版 1 の zcode）も受ける。返事は相手の版（相手は自分の版と比べる）。
                if (ipc::MIN_IPC_VERSION..=ipc::IPC_VERSION).contains(&version) {
                    c.send(&ipc::Response::Hello { version });
                    c.kind = Some(client);
                } else {
                    c.send(&ipc::Response::Hello { version: ipc::IPC_VERSION });
                    c.closing = true;
                }
            }
            // 最初は Hello。
            (_, None) => self.core.clients[ci].dead = true,
            (ipc::Request::Warm { .. }, _) => {
                self.maybe_reload(now);
                self.core.want_warm = true;
                let st = status(self);
                self.core.clients[ci].send(&st);
            }
            (ipc::Request::Status, _) => {
                let st = status(self);
                self.core.clients[ci].send(&st);
            }
            (ipc::Request::Submit { req, kind: job, lang, flags, file, overlay }, Some(kind)) => {
                self.maybe_reload(now);
                self.core.on_submit(now, id, kind, Submit { req, job, lang, flags, file, overlay })
            }
            (ipc::Request::Cancel { req }, _) => {
                if let Some(t) = self.core.jobs.iter().find(|j| j.who.client == Some(id) && j.who.req == req).map(|j| j.ticket) {
                    self.core.session.cancel(now, t);
                }
            }
            (ipc::Request::Shutdown, _) => self.core.stop = Some(ServeExit::Stopped),
        }
    }

    /// 設定のファイルが変わっていたら読み直す（ジョブが無いときだけ）。行き先が変わったら、
    /// 今の接続は閉じる。
    fn maybe_reload(&mut self, now: u64) {
        if !self.core.reload || self.core.session.active() > 0 {
            return;
        }
        let stamp = config_stamp();
        if stamp == self.core.cfg_stamp {
            return;
        }
        self.core.cfg_stamp = stamp;
        let (cfg, err) = match Config::load() {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(e.to_string())),
        };
        let changed = cfg != self.core.opts.config;
        self.core.opts.config = cfg;
        self.core.cfg_err = err;
        if changed {
            if let Link::Up(conn) = std::mem::replace(&mut self.net.link, Link::Down) {
                conn.close();
            }
            self.core.connected = false;
            self.core.backend = None;
            self.core.session.on_closed(now, false);
            self.net.redial_at = None;
            self.net.warm_until = None;
            self.net.more = false;
            self.net.choice = None;
            self.net.hints = [None, None];
            self.net.token = None;
        }
    }

    /// 去った相手を片づけ、その相手の依頼を取り消す。
    fn reap_clients(&mut self, now: u64) {
        let mut i = 0;
        while i < self.core.clients.len() {
            if !self.core.clients[i].dead {
                i += 1;
                continue;
            }
            let id = self.core.clients.swap_remove(i).id;
            let tickets: Vec<Ticket> = self.core.jobs.iter_mut().filter(|j| j.who.client == Some(id)).map(|j| {
                j.who.client = None;
                j.ticket
            }).collect();
            for t in tickets {
                self.core.session.cancel(now, t);
            }
        }
    }
}

/// serve が動いていなければ起動して、ソケットにつなげるようになるまで待つ（最大 `wait`）。
/// `detach` なら `setsid` で切り離す（CLI から。zcode からは切り離さない = 同じプロセス
/// グループなので、APPLaunch の Esc 長押しで一緒に終わる）。
///
/// 切り離すときは `serve --detach` を起動する（serve が自分で fork して親を終わらせ、
/// 子が `setsid` する。起動した側に子プロセスが残らない = `exec` で入れ替わったあとの
/// プログラムにゾンビを残さない）。
pub fn ensure_running(program: &std::path::Path, socket: &std::path::Path, detach: bool, wait: Duration) -> std::io::Result<std::os::unix::net::UnixStream> {
    use std::process::{Command, Stdio};
    if let Ok(s) = UnixStream::connect(socket) {
        return Ok(s);
    }
    let mut cmd = Command::new(program);
    cmd.arg("serve").arg("--socket").arg(socket).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if detach {
        cmd.arg("--detach");
    }
    let mut child = cmd.spawn().map_err(|e| io::Error::new(e.kind(), format!("{} を起動できない: {e}", program.display())))?;
    if detach {
        // 親はすぐ終わる（本体は孫）。
        let _ = child.wait();
    }
    let deadline = std::time::Instant::now() + wait;
    loop {
        match UnixStream::connect(socket) {
            Ok(s) => return Ok(s),
            Err(e) if std::time::Instant::now() >= deadline => return Err(io::Error::new(io::ErrorKind::TimedOut, format!("zc-remote serve につながらない: {e}"))),
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(msg: &str) -> wire::Diag {
        wire::Diag { line: 1, col: Some(2), end: None, severity: wire::Severity::Error, code: Some("E0308".into()), message: msg.into() }
    }

    #[test]
    fn diagnostics_are_split_under_the_ipc_frame_limit() {
        // 空でも 1 束（波線を消す知らせ）
        assert_eq!(diag_frames(Vec::new()), vec![Vec::<ipc::FileDiags>::new()]);
        // 小さければ 1 束
        let small = diag_frames(vec![wire::FileDiags { path: "src/main.rs".into(), diags: vec![diag("a"), diag("b")] }]);
        assert_eq!((small.len(), small[0].len(), small[0][0].diags.len()), (1, 1, 2));
        // 500 件 × 4KB（上限いっぱい）: どの束も符号化して MAX_FRAME に収まり、1 件も落とさない
        let long = "あ".repeat(1400);
        let files: Vec<wire::FileDiags> = (0..5).map(|f| wire::FileDiags { path: format!("src/f{f}.rs"), diags: (0..100).map(|_| diag(&long)).collect() }).collect();
        let frames = diag_frames(files);
        assert!(frames.len() > 1);
        let mut total = 0;
        for (k, files) in frames.iter().enumerate() {
            assert!(!files.is_empty());
            total += files.iter().map(|f| f.diags.len()).sum::<usize>();
            let mut out = Vec::new();
            ipc::encode_response(&ipc::Response::Diagnostics { req: ipc::ReqId(1), tag: ipc::JobTag(1), root: "/home/u/code/p".into(), complete: true, last: k + 1 == frames.len(), files: files.clone() }, &mut out);
            assert!(out.len() - 4 <= ipc::MAX_FRAME, "{}", out.len());
            let mut r = ipc::FrameReader::new();
            r.push(&out);
            assert!(ipc::decode_response(r.next_frame().unwrap().unwrap()).is_ok());
        }
        assert_eq!(total, 500);
        // メッセージは文字の境目で切る
        let d = convert_diag(diag(&long));
        assert!(d.message.len() <= MESSAGE_MAX && d.message.chars().all(|c| c == 'あ'));
    }

    #[test]
    fn old_builder_lang_error_is_recognised() {
        // 2026-11-17 より前の zc-buildd が "go" を読めなかったときの実物（serde_json の文）
        let old = "読めないメッセージ: unknown variant `go`, expected one of `rust`, `c`, `node` at line 1 column 103";
        assert!(unsupported_lang(wire::ErrorCode::Protocol, old, wire::Lang::Go));
        assert!(!unsupported_lang(wire::ErrorCode::Protocol, old, wire::Lang::Zig));
        assert!(!unsupported_lang(wire::ErrorCode::Internal, old, wire::Lang::Go));
        assert!(unsupported_lang(wire::ErrorCode::Protocol, &format!("{}: cobol", wire::UNSUPPORTED_LANG), wire::Lang::Moonbit));
        assert!(!unsupported_lang(wire::ErrorCode::Protocol, "最初のメッセージは hello", wire::Lang::Rust));
    }

    #[test]
    fn test_reports_fit_and_keep_failures_first() {
        let case = |name: String, passed| wire::TestCase { name, passed, path: Some("src/lib.rs".into()), line: Some(3), message: "x".repeat(5000) };
        let mut cases: Vec<wire::TestCase> = (0..300).map(|i| case(format!("ok{i}"), true)).collect();
        cases.push(case("bad".into(), false));
        let r = convert_tests(300, 1, 0, cases);
        assert_eq!((r.passed, r.failed), (300, 1));
        assert_eq!(r.cases[0].name, "bad");
        assert!(r.cases.len() < 301 && r.cases[0].message.len() == MESSAGE_MAX);
        let mut out = Vec::new();
        ipc::encode_response(&ipc::Response::Tests { req: ipc::ReqId(1), tag: ipc::JobTag(1), root: "/p".into(), report: r }, &mut out);
        assert!(out.len() - 4 <= ipc::MAX_FRAME);
    }

    #[test]
    fn write_back_only_touches_lock_files_inside_the_root() {
        let root = std::env::temp_dir().join(format!("zc-remote-writeback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let project = Project { root: root.clone(), id: "0123456789abcdef".into(), lang: wire::Lang::Rust, entry: None, shape: Shape::Tree };
        let text = "# lock\n";
        let sha = sha256_hex(text.as_bytes());
        // 書く → 同じ中身なら書かない
        assert_eq!(write_back(&project, "Cargo.lock", &sha, text).unwrap(), Some(root.join("Cargo.lock")));
        assert_eq!(std::fs::read_to_string(root.join("Cargo.lock")).unwrap(), text);
        assert_eq!(write_back(&project, "Cargo.lock", &sha, text).unwrap(), None);
        assert_eq!(write_back(&project, "sub/package-lock.json", &sha, text).unwrap(), Some(root.join("sub/package-lock.json")));
        // lock でない・根の外・SHA-256 が合わない
        assert!(write_back(&project, "src/main.rs", &sha, text).is_err());
        assert!(write_back(&project, "../Cargo.lock", &sha, text).is_err());
        assert!(write_back(&project, "/tmp/Cargo.lock", &sha, text).is_err());
        assert!(write_back(&project, "Cargo.lock", &sha256_hex(b"other"), text).is_err());
        // シンボリックリンクのフォルダで根の外へ出ない
        let outside = std::env::temp_dir().join(format!("zc-remote-writeback-out-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert!(write_back(&project, "link/Cargo.lock", &sha, text).is_err());
        assert!(!outside.join("Cargo.lock").exists());
        // 1 ファイルだけ・C のプロジェクトには書かない
        let single = Project { shape: Shape::SingleFile, ..project.clone() };
        assert_eq!(write_back(&single, "Cargo.lock", &sha256_hex(b"new"), "new").unwrap(), None);
        assert_eq!(std::fs::read_to_string(root.join("Cargo.lock")).unwrap(), text);
        // 一時ファイルを残さない
        assert!(std::fs::read_dir(&root).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains("zc-tmp")));
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }
}
