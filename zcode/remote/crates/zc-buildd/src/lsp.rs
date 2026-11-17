//! rust-analyzer の橋渡し（docs/lsp-design.md）。
//!
//! 実機では LSP を動かさない（design.md §10。メモリと CPU が足りない）。rust-analyzer は
//! ビルド係で動かし、実機とは zc-wire の `lsp_warm` / `lsp` / `lsp_state` / `lsp_result` で話す。
//! この層が LSP（JSON-RPC・UTF-16 の位置・didOpen / didChange）との間を訳す。
//!
//! ```text
//!   接続のスレッド ── warm / query ──▶ LspHub ──▶ Ra（rust-analyzer 1 つ）
//!                                                 ├ stdin  ← 依頼・知らせ（順番は st の錠で守る）
//!                                                 ├ stdout → 読むスレッド → 返事を訳して Reply へ
//!                                                 └ stderr → 末尾だけ覚える（落ちたときの理由）
//! ```
//!
//! 決めたこと:
//!
//! - **rust-analyzer は同時に 1 つ**（3GiB のコンテナで、ripgrep 級で約 1.2GB。
//!   docs/measurements.md §13）。別のプロジェクトが来たら前のを止める。コンテナが動いている間は
//!   止めない（費用はメモリの割り当てで決まり、動いていても変わらない）
//! - 木は**ジョブの作業の木とは別**（`proj/<id>/lsp`。[`Store::sync_lsp_tree`]）。走っている
//!   ビルドの木を書き換えない。build script・proc-macro の出力も別の target
//!   （`proj/<id>/lsp-target`。zc-buildd の cargo と `target/` のロックを取り合わない）
//! - 未保存の中身は、実機が目録に重ねて送ってくる（打鍵が止まったときの check と同じ）。
//!   こちらは目録のとおりに lsp の木へ写し、**rust-analyzer が起動してから変わった `.rs` は
//!   didOpen / didChange で中身ごと渡す**（ディスクの見張りの遅れに左右されない）。
//!   増えた・消えたファイルと `Cargo.toml` は `workspace/didChangeWatchedFiles` で知らせる
//! - 最初の索引が済む（`experimental/serverStatus` の `quiescent`）までは、問い合わせに
//!   「準備中」（`unavailable`）と返す（読み込み前の rust-analyzer は空の答えを返すため）
//! - 位置: 実機とは 0 始まりの行と UTF-8 のバイト位置、rust-analyzer とは UTF-16。どちらへも
//!   その行の中身を読んで数え直す（診断と同じ決まり。stage3-design.md §4.4）
//! - プロジェクトの外（std・crates.io の crate）の場所は、実機にファイルが無いので `ext` に
//!   見せ方（`core/src/option.rs`・`regex-1.11.1/src/lib.rs`）を入れる
//! - CPU: rust-analyzer とその子（proc-macro-srv・cargo・rustc）の CPU 時間を
//!   `/proc/<pid>/stat` で数え、受付の起動からの通算（減らない）を `cpu_ms` に載せる
//!   （DO が費用の計りに使う。worker/src/budget.ts）

use crate::runner::{find_in_path, tool_env};
use crate::store::Store;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zc_wire::limits::{MAX_LSP_ITEMS, MAX_LSP_TEXT};
use zc_wire::msg::{ErrorCode, LspItem, LspPhase, LspQuery, ServerMsg};
use zc_wire::tree::Manifest;

/// 返事の届け先（接続のスレッドが受けて、WebSocket で送る）。
pub type Reply = Sender<ServerMsg>;

/// 補完の候補の数の上限（実機の小窓に出す分。[`MAX_LSP_ITEMS`] より少ない）。
pub const MAX_COMPLETIONS: usize = 50;
/// 場所の添え書き（その行の中身）の長さ。
const DETAIL_MAX: usize = 120;
/// 落ちたときに覚えておく標準エラーの末尾。
const STDERR_TAIL: usize = 2048;
/// initialize の依頼の番号。
const INIT_ID: i64 = 0;

/// 問い合わせ 1 つ（[`zc_wire::msg::ClientMsg::Lsp`] の中身）。
#[derive(Clone, Debug)]
pub struct Query {
    pub id: u32,
    pub query: LspQuery,
    pub path: String,
    pub line: u32,
    pub col: u32,
    pub text: String,
}

/// rust-analyzer の設定（`initializationOptions` と `workspace/configuration` の返事）。
/// 理由は docs/measurements.md §13。
pub fn ra_config(target_dir: &Path, linked: Option<&Path>) -> Value {
    let mut v = json!({
        // zc-buildd の check があるので 2 重にしない
        "checkOnSave": false,
        // build script・proc-macro の出力は別の target（ロックを取り合わない）
        "cargo": { "targetDir": target_dir },
        // 変わったファイルはこちらから知らせる（inotify を使わない）
        "files": { "watcher": "client" },
        // 診断は zc-buildd の check（cargo）が出す
        "diagnostics": { "enable": false },
        // 実機の小窓は 1 行の文字列を入れるだけ（スニペット・別の行の編集は使わない）
        "completion": { "autoimport": { "enable": false }, "callable": { "snippets": "none" }, "postfix": { "enable": false }, "limit": 100 },
    });
    if let Some(p) = linked {
        v["linkedProjects"] = json!([p]);
    }
    v
}

// ───────────────────────── CPU の数え方 ─────────────────────────

/// rust-analyzer たちの CPU 時間の通算（ms）。受付に 1 つ。
#[derive(Default)]
struct Cpu {
    /// 止めた・落ちた rust-analyzer の分。
    dead_ms: AtomicU64,
    /// 今の rust-analyzer の分（最後に見た値）。
    live_ms: AtomicU64,
    /// 返したことのある最大（減らさない）。
    max_ms: AtomicU64,
}

impl Cpu {
    /// 今の rust-analyzer（プロセスグループ `pgid`）を数え直して、通算を返す。
    fn total(&self, pgid: Option<u32>) -> u64 {
        if let Some(p) = pgid {
            // 止めた直後に読むと 0 になる。減らさない（新しい rust-analyzer の分は retire で 0 から）
            self.live_ms.fetch_max(group_cpu_ms(p), Ordering::SeqCst);
        }
        let t = self.dead_ms.load(Ordering::SeqCst) + self.live_ms.load(Ordering::SeqCst);
        self.max_ms.fetch_max(t, Ordering::SeqCst).max(t)
    }

    /// 今の rust-analyzer が終わった（最後に見た値を通算へ移す）。
    fn retire(&self, pgid: u32) {
        let last = group_cpu_ms(pgid).max(self.live_ms.load(Ordering::SeqCst));
        self.dead_ms.fetch_add(last, Ordering::SeqCst);
        self.live_ms.store(0, Ordering::SeqCst);
    }
}

/// プロセスグループ `pgid` の全員の CPU 時間（自分の分 + 待ち終えた子の分）の合計（ms）。
/// 待ち終えた子の分は親の cutime に入るので、生きているものだけ足せば重ならない。
fn group_cpu_ms(pgid: u32) -> u64 {
    // SAFETY: sysconf は失敗しても -1 を返すだけ。
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let mut ticks = 0u64;
    let Ok(rd) = std::fs::read_dir("/proc") else { return 0 };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
        let Some(rest) = stat.rfind(')').map(|i| &stat[i + 2..]) else { continue };
        let f: Vec<&str> = rest.split(' ').collect();
        // f[0] = state（3 番目の欄）。pgrp = 5 番目、utime..cstime = 14〜17 番目
        if f.len() < 15 || f[2].parse::<u32>().ok() != Some(pgid) {
            continue;
        }
        for i in 11..=14 {
            ticks += f[i].parse::<u64>().unwrap_or(0);
        }
    }
    ticks * 1000 / hz
}

// ───────────────────────── 位置（UTF-8 ⇔ UTF-16） ─────────────────────────

/// 行の中の UTF-8 のバイト位置 → UTF-16 の単位の数。行の外なら行末。
pub fn utf8_to_utf16(line: &str, byte_col: u32) -> u32 {
    let mut n = 0u32;
    for (i, ch) in line.char_indices() {
        if i >= byte_col as usize {
            break;
        }
        n += ch.len_utf16() as u32;
    }
    n
}

/// 行の中の UTF-16 の単位の数 → UTF-8 のバイト位置。文字の途中・行の外は丸める。
pub fn utf16_to_utf8(line: &str, u16_col: u32) -> u32 {
    let mut n = 0u32;
    for (i, ch) in line.char_indices() {
        if n >= u16_col {
            return i as u32;
        }
        n += ch.len_utf16() as u32;
    }
    line.len() as u32
}

/// `n` 行目（0 始まり。改行・CR を除く）。無ければ空。
fn nth_line(text: &str, n: u32) -> &str {
    let l = text.split('\n').nth(n as usize).unwrap_or("");
    l.strip_suffix('\r').unwrap_or(l)
}

// ───────────────────────── URI ─────────────────────────

/// パス → `file://` の URI（`/` と英数字・`-._~` 以外は %XX）。
pub fn path_to_uri(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~') {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// `file://` の URI → パス。`file` でなければ `None`。
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    // file://host/path の host は localhost か空だけ
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = hex {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

/// プロジェクトの外の場所の見せ方: crates.io の crate は `<名前>-<版>/…`、std は
/// `core/src/…`、ほかは末尾の 3 つ。
pub fn external_label(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(i) = s.find("/registry/src/") {
        // registry/src/<index>-<hash>/<crate>-<版>/…
        let after = &s[i + "/registry/src/".len()..];
        if let Some(j) = after.find('/') {
            return after[j + 1..].to_string();
        }
    }
    if let Some(i) = s.find("/lib/rustlib/src/rust/library/") {
        return s[i + "/lib/rustlib/src/rust/library/".len()..].to_string();
    }
    if let Some(i) = s.find("/git/checkouts/") {
        // git/checkouts/<名前>-<hash>/<rev>/…
        let parts: Vec<&str> = s[i + "/git/checkouts/".len()..].splitn(3, '/').collect();
        if parts.len() == 3 {
            return format!("{}/{}", parts[0], parts[2]);
        }
    }
    let parts: Vec<&str> = s.rsplitn(4, '/').collect();
    let n = parts.len().min(3);
    parts[..n].iter().rev().copied().collect::<Vec<_>>().join("/")
}

/// 文字の境目で切る。
fn cut(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut n = max;
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        s.truncate(n);
    }
    s
}

// ───────────────────────── rust-analyzer 1 つ ─────────────────────────

/// 返事を待っている依頼。
struct Pending {
    wire_id: u32,
    query: LspQuery,
    reply: Reply,
    /// 問い合わせた位置（補完の置き換えの範囲を決めるのに使う）。
    path: String,
    line: u32,
    col: u32,
}

struct RaState {
    next_id: i64,
    pending: HashMap<i64, Pending>,
    phase: LspPhase,
    message: String,
    /// 最初の索引が済んだ。
    ready: bool,
    /// initialize の返事を受けた（それまでの送りは `outbox` にためる）。
    initialized: bool,
    outbox: Vec<Vec<u8>>,
    /// `lsp_warm` を送ってきた接続（接続の番号と届け先。状態が変わったら知らせる）。
    subs: Vec<(u64, Reply)>,
    /// 最後に写した目録（パス → SHA-256）。
    files: HashMap<String, String>,
    /// 開いている文書（パス → 版）。
    open: HashMap<String, i64>,
    alive: bool,
}

struct Ra {
    project: String,
    /// lsp の木（`proj/<id>/lsp`）。
    root: PathBuf,
    config: Value,
    pgid: u32,
    child: Mutex<Option<Child>>,
    stdin: Mutex<ChildStdin>,
    st: Mutex<RaState>,
    stderr_tail: Mutex<String>,
    cpu: Arc<Cpu>,
}

fn frame(v: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(v).expect("JSON にできる");
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

impl Ra {
    /// 書く（initialize の返事の前なら、ためる）。`st` の錠を持ったまま呼ぶ（順番を守る）。
    fn send(&self, st: &mut RaState, v: Value) {
        let bytes = frame(&v);
        if !st.initialized {
            st.outbox.push(bytes);
            return;
        }
        let mut w = self.stdin.lock().unwrap();
        if w.write_all(&bytes).and_then(|()| w.flush()).is_err() {
            st.alive = false;
        }
    }

    fn notify(&self, st: &mut RaState, method: &str, params: Value) {
        self.send(st, json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn uri(&self, rel: &str) -> String {
        path_to_uri(&self.root.join(rel))
    }

    fn read_rel(&self, rel: &str) -> String {
        std::fs::read(self.root.join(rel)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
    }

    /// 今の状態を `lsp_state` にする。
    fn state_msg(&self, st: &RaState) -> ServerMsg {
        ServerMsg::LspState { cpu_ms: self.cpu.total(Some(self.pgid)), project: self.project.clone(), state: st.phase, message: st.message.clone(), error: None }
    }

    fn broadcast(&self, st: &mut RaState) {
        let m = self.state_msg(st);
        st.subs.retain(|(_, s)| s.send(m.clone()).is_ok());
    }

    /// 文書を開いておく（開いていなければ中身ごと didOpen）。
    fn ensure_open(&self, st: &mut RaState, rel: &str) {
        if rel.is_empty() || !rel.ends_with(".rs") || st.open.contains_key(rel) || !st.files.contains_key(rel) {
            return;
        }
        let text = self.read_rel(rel);
        st.open.insert(rel.to_string(), 1);
        let uri = self.uri(rel);
        self.notify(st, "textDocument/didOpen", json!({ "textDocument": { "uri": uri, "languageId": "rust", "version": 1, "text": text } }));
    }

    /// 目録に合わせて lsp の木を写し、変わったところを rust-analyzer に知らせる。
    fn sync(&self, store: &Store, manifest: &Manifest) -> Result<(), String> {
        let mut st = self.st.lock().unwrap();
        store.sync_lsp_tree(&self.project, manifest).map_err(|e| format!("rust-analyzer 用の木を作れない: {e}"))?;
        let new: HashMap<String, String> = manifest.files().iter().map(|f| (f.path.clone(), f.sha256.clone())).collect();
        let mut watched = Vec::new();
        let mut changed_rs = Vec::new();
        for (p, h) in &new {
            match st.files.get(p) {
                Some(old) if old == h => {}
                Some(_) => {
                    watched.push(json!({ "uri": self.uri(p), "type": 2 }));
                    if p.ends_with(".rs") {
                        changed_rs.push(p.clone());
                    }
                }
                None => {
                    watched.push(json!({ "uri": self.uri(p), "type": 1 }));
                    if p.ends_with(".rs") {
                        changed_rs.push(p.clone());
                    }
                }
            }
        }
        let removed: Vec<String> = st.files.keys().filter(|p| !new.contains_key(*p)).cloned().collect();
        for p in &removed {
            watched.push(json!({ "uri": self.uri(p), "type": 3 }));
        }
        st.files = new;
        for p in removed {
            if st.open.remove(&p).is_some() {
                let uri = self.uri(&p);
                self.notify(&mut st, "textDocument/didClose", json!({ "textDocument": { "uri": uri } }));
            }
        }
        if !watched.is_empty() {
            self.notify(&mut st, "workspace/didChangeWatchedFiles", json!({ "changes": watched }));
        }
        changed_rs.sort();
        for p in changed_rs {
            match st.open.get(&p).copied() {
                Some(v) => {
                    let text = self.read_rel(&p);
                    st.open.insert(p.clone(), v + 1);
                    let uri = self.uri(&p);
                    self.notify(&mut st, "textDocument/didChange", json!({ "textDocument": { "uri": uri, "version": v + 1 }, "contentChanges": [{ "text": text }] }));
                }
                None => self.ensure_open(&mut st, &p),
            }
        }
        Ok(())
    }

    /// 問い合わせを送る（答えは読むスレッドが `reply` へ）。
    fn query(&self, q: Query, reply: &Reply) {
        let mut st = self.st.lock().unwrap();
        let fail = |code: ErrorCode, message: &str| {
            let _ = reply.send(ServerMsg::LspResult { cpu_ms: self.cpu.total(Some(self.pgid)), id: q.id, error: Some(code), message: message.into(), text: String::new(), items: vec![], retry_after_s: None });
        };
        if !st.alive {
            return fail(ErrorCode::Unavailable, "rust-analyzer が動いていない");
        }
        if !st.ready {
            return fail(ErrorCode::Unavailable, "rust-analyzer が準備中（プロジェクトを読んでいる）");
        }
        let (method, params) = if q.query == LspQuery::Symbols {
            ("workspace/symbol", json!({ "query": q.text }))
        } else {
            if !st.files.contains_key(&q.path) {
                return fail(ErrorCode::BadTree, "そのファイルは木に無い");
            }
            self.ensure_open(&mut st, &q.path);
            let text = self.read_rel(&q.path);
            let character = utf8_to_utf16(nth_line(&text, q.line), q.col);
            let doc = json!({ "textDocument": { "uri": self.uri(&q.path) }, "position": { "line": q.line, "character": character } });
            match q.query {
                LspQuery::Hover => ("textDocument/hover", doc),
                LspQuery::Definition => ("textDocument/definition", doc),
                LspQuery::References => {
                    let mut d = doc;
                    d["context"] = json!({ "includeDeclaration": true });
                    ("textDocument/references", d)
                }
                LspQuery::Completion => ("textDocument/completion", doc),
                LspQuery::Symbols => unreachable!(),
            }
        };
        let id = st.next_id;
        st.next_id += 1;
        st.pending.insert(id, Pending { wire_id: q.id, query: q.query, reply: reply.clone(), path: q.path.clone(), line: q.line, col: q.col });
        self.send(&mut st, json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    /// rust-analyzer から 1 メッセージ届いた（読むスレッド）。
    fn on_message(&self, v: Value) {
        let method = v.get("method").and_then(Value::as_str);
        let id = v.get("id").cloned();
        match (method, id) {
            (None, Some(id)) => self.on_response(id.as_i64().unwrap_or(-1), &v),
            (Some(m), Some(id)) => {
                // サーバからの依頼: 設定だけ答え、ほかは null（登録・進み具合の作成など）
                let result = if m == "workspace/configuration" {
                    let n = v["params"]["items"].as_array().map_or(1, Vec::len);
                    Value::Array(vec![self.config.clone(); n])
                } else {
                    Value::Null
                };
                let mut st = self.st.lock().unwrap();
                self.send(&mut st, json!({ "jsonrpc": "2.0", "id": id, "result": result }));
            }
            (Some("experimental/serverStatus"), None) => {
                let p = &v["params"];
                let quiescent = p["quiescent"].as_bool().unwrap_or(false);
                let health = p["health"].as_str().unwrap_or("ok");
                let message = p["message"].as_str().unwrap_or("").to_string();
                let mut st = self.st.lock().unwrap();
                let mut changed = false;
                if quiescent && !st.ready {
                    st.ready = true;
                    st.phase = LspPhase::Ready;
                    changed = true;
                }
                // 読み込みの失敗（Cargo.toml が読めない、など）は理由として残す
                let m = if health == "ok" { String::new() } else { cut(message, 400) };
                if quiescent && m != st.message {
                    st.message = m;
                    changed = true;
                }
                if changed {
                    self.broadcast(&mut st);
                }
            }
            _ => {}
        }
    }

    fn on_response(&self, id: i64, v: &Value) {
        let mut st = self.st.lock().unwrap();
        if id == INIT_ID {
            st.initialized = true;
            let init = frame(&json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
            let queued = std::mem::take(&mut st.outbox);
            let mut w = self.stdin.lock().unwrap();
            let mut ok = w.write_all(&init).is_ok();
            for b in queued {
                ok = ok && w.write_all(&b).is_ok();
            }
            if !(ok && w.flush().is_ok()) {
                st.alive = false;
            }
            return;
        }
        let Some(p) = st.pending.remove(&id) else { return };
        let open: HashSet<String> = st.files.keys().cloned().collect();
        drop(st);
        let cpu_ms = self.cpu.total(Some(self.pgid));
        let msg = match v.get("error") {
            Some(e) => {
                let code = e["code"].as_i64().unwrap_or(0);
                let (code, message) = if code == -32801 || code == -32800 {
                    (ErrorCode::Unavailable, "rust-analyzer が変わったところを読み直している。もう一度".to_string())
                } else {
                    (ErrorCode::Internal, cut(format!("rust-analyzer の失敗: {}", e["message"].as_str().unwrap_or("?")), 400))
                };
                ServerMsg::LspResult { cpu_ms, id: p.wire_id, error: Some(code), message, text: String::new(), items: vec![], retry_after_s: None }
            }
            None => {
                let result = v.get("result").cloned().unwrap_or(Value::Null);
                let (text, items) = convert(&self.root, &open, &p, &result);
                ServerMsg::LspResult { cpu_ms, id: p.wire_id, error: None, message: String::new(), text, items, retry_after_s: None }
            }
        };
        let _ = p.reply.send(msg);
    }

    /// rust-analyzer が終わった（読むスレッドが EOF を見た）。
    fn on_exit(&self) {
        let code = self.child.lock().unwrap().as_mut().and_then(|c| c.wait().ok()).and_then(|s| s.code());
        let tail = self.stderr_tail.lock().unwrap().clone();
        let mut st = self.st.lock().unwrap();
        if !st.alive && st.phase == LspPhase::Failed {
            return; // 止めた（stop。CPU の分はそちらで移した）
        }
        self.cpu.retire(self.pgid);
        st.alive = false;
        st.phase = LspPhase::Failed;
        let last = tail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
        st.message = cut(format!("rust-analyzer が終わった（終了コード {}）{}", code.map_or("?".into(), |c| c.to_string()), if last.is_empty() { String::new() } else { format!(": {last}") }), 400);
        eprintln!("zc-buildd: {}", st.message);
        let cpu_ms = self.cpu.total(None);
        for (_, p) in st.pending.drain() {
            let _ = p.reply.send(ServerMsg::LspResult { cpu_ms, id: p.wire_id, error: Some(ErrorCode::Unavailable), message: "rust-analyzer が終わった".into(), text: String::new(), items: vec![], retry_after_s: None });
        }
        self.broadcast(&mut st);
    }

    /// 止める（プロセスグループごと）。
    fn stop(&self) {
        {
            let mut st = self.st.lock().unwrap();
            if !st.alive && st.phase == LspPhase::Failed {
                return;
            }
            st.alive = false;
            st.phase = LspPhase::Failed;
            st.message = "止めた（ほかのプロジェクトに替えた）".into();
            let cpu_ms = self.cpu.total(Some(self.pgid));
            for (_, p) in st.pending.drain() {
                let _ = p.reply.send(ServerMsg::LspResult { cpu_ms, id: p.wire_id, error: Some(ErrorCode::Unavailable), message: "rust-analyzer を止めた".into(), text: String::new(), items: vec![], retry_after_s: None });
            }
        }
        self.cpu.retire(self.pgid);
        // SAFETY: 自分が作ったプロセスグループへのシグナル。
        unsafe {
            libc::kill(-(self.pgid as libc::pid_t), libc::SIGKILL);
        }
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.wait();
        }
    }
}

/// 答えを実機の形に訳す。`files` は木にあるパス（プロジェクトの中か外かを決める）。
/// 答えに出てくるファイルの中身（1 つの答えの中で 1 回だけ読む）。
struct Texts(HashMap<PathBuf, String>);

impl Texts {
    fn get(&mut self, path: &Path) -> &str {
        self.0.entry(path.to_path_buf()).or_insert_with(|| std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default())
    }

    /// `Location`（URI と範囲）を項目にする。
    fn loc(&mut self, root: &Path, files: &HashSet<String>, uri: &str, range: &Value) -> Option<LspItem> {
        let path = uri_to_path(uri)?;
        let text = self.get(&path);
        let (sl, sc) = (range["start"]["line"].as_u64()? as u32, range["start"]["character"].as_u64()? as u32);
        let (el, ec) = (range["end"]["line"].as_u64().unwrap_or(sl as u64) as u32, range["end"]["character"].as_u64().unwrap_or(sc as u64) as u32);
        let sline = nth_line(text, sl);
        let col = utf16_to_utf8(sline, sc);
        let end = (el, utf16_to_utf8(nth_line(text, el), ec));
        let detail = cut(sline.trim().to_string(), DETAIL_MAX);
        let rel = path.strip_prefix(root).ok().and_then(|r| r.to_str()).map(str::to_owned).filter(|r| files.contains(r));
        Some(match rel {
            Some(rel) => LspItem { path: rel, line: sl, col, end: Some(end), detail, ..Default::default() },
            None => LspItem { ext: external_label(&path), line: sl, col, end: Some(end), detail, ..Default::default() },
        })
    }
}

fn convert(root: &Path, files: &HashSet<String>, p: &Pending, result: &Value) -> (String, Vec<LspItem>) {
    let mut texts = Texts(HashMap::new());
    let mut loc = |uri: &str, range: &Value| texts.loc(root, files, uri, range);
    match p.query {
        LspQuery::Hover => {
            let c = &result["contents"];
            let text = match c {
                Value::String(s) => s.clone(),
                Value::Object(o) => o.get("value").and_then(Value::as_str).unwrap_or("").to_string(),
                Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_owned).unwrap_or_else(|| x["value"].as_str().unwrap_or("").to_string())).collect::<Vec<_>>().join("\n"),
                _ => String::new(),
            };
            (cut(text.trim().to_string(), MAX_LSP_TEXT), vec![])
        }
        LspQuery::Definition | LspQuery::References => {
            let arr = match result {
                Value::Array(a) => a.clone(),
                Value::Null => vec![],
                one => vec![one.clone()],
            };
            let mut items = Vec::new();
            for l in arr.iter().take(MAX_LSP_ITEMS) {
                // Location か LocationLink
                let it = match l.get("targetUri") {
                    Some(u) => loc(u.as_str().unwrap_or(""), &l["targetSelectionRange"]),
                    None => loc(l["uri"].as_str().unwrap_or(""), &l["range"]),
                };
                items.extend(it);
            }
            (String::new(), items)
        }
        LspQuery::Symbols => {
            let mut items = Vec::new();
            for s in result.as_array().map(Vec::as_slice).unwrap_or(&[]).iter().take(MAX_LSP_ITEMS) {
                let l = &s["location"];
                let range = if l["range"].is_object() { l["range"].clone() } else { json!({ "start": { "line": 0, "character": 0 } }) };
                if let Some(mut it) = loc(l["uri"].as_str().unwrap_or(""), &range) {
                    it.name = cut(s["name"].as_str().unwrap_or("").to_string(), 200);
                    it.kind = s["kind"].as_u64().unwrap_or(0) as u32;
                    it.detail = cut(s["containerName"].as_str().unwrap_or("").to_string(), DETAIL_MAX);
                    items.push(it);
                }
            }
            (String::new(), items)
        }
        LspQuery::Completion => {
            let list = match result {
                Value::Array(a) => a.clone(),
                Value::Object(o) => o.get("items").and_then(Value::as_array).cloned().unwrap_or_default(),
                _ => vec![],
            };
            let text = std::fs::read(root.join(&p.path)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            let line = nth_line(&text, p.line);
            // 単語の始まり（置き換えの範囲が無い候補のため）
            let word_start = line[..(p.col as usize).min(line.len())].char_indices().rev().take_while(|(_, c)| c.is_alphanumeric() || *c == '_').last().map_or(p.col, |(i, _)| i as u32);
            let mut all: Vec<(String, LspItem)> = Vec::new();
            for c in &list {
                if c["insertTextFormat"].as_u64() == Some(2) {
                    continue; // スニペット（実機では展開しない）
                }
                let label = c["label"].as_str().unwrap_or("").to_string();
                let edit = &c["textEdit"];
                let (col, insert) = if edit.is_object() {
                    let range = if edit["range"].is_object() { &edit["range"] } else { &edit["replace"] };
                    if range["start"]["line"].as_u64() != Some(p.line as u64) || range["end"]["line"].as_u64() != Some(p.line as u64) {
                        continue; // 別の行を書き換える候補は扱わない
                    }
                    (utf16_to_utf8(line, range["start"]["character"].as_u64().unwrap_or(0) as u32), edit["newText"].as_str().unwrap_or("").to_string())
                } else {
                    (word_start, c["insertText"].as_str().unwrap_or(&label).to_string())
                };
                if insert.is_empty() || insert.contains('\n') {
                    continue;
                }
                // rust-analyzer は打った分で絞らずに全部返す（絞るのは相手の役目）。置き換えの
                // 範囲の始まりからカーソルまでで始まるものだけ残す（大文字小文字は見ない）
                let typed = line.get(col as usize..(p.col as usize).max(col as usize)).unwrap_or("");
                let key = c["filterText"].as_str().unwrap_or(&label);
                if !key.to_lowercase().starts_with(&typed.to_lowercase()) {
                    continue;
                }
                let sort = c["sortText"].as_str().unwrap_or(&label).to_string();
                all.push((sort, LspItem { line: p.line, col, name: cut(label, 200), kind: c["kind"].as_u64().unwrap_or(0) as u32, detail: cut(c["detail"].as_str().unwrap_or("").to_string(), DETAIL_MAX), insert: cut(insert, 400), ..Default::default() }));
            }
            all.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
            all.truncate(MAX_COMPLETIONS);
            (String::new(), all.into_iter().map(|(_, it)| it).collect())
        }
    }
}

/// LSP のメッセージを 1 つ読む（`Content-Length` の頭 + 本体）。EOF・壊れていれば `None`。
fn read_message(r: &mut impl BufRead) -> Option<Value> {
    let mut len: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().ok();
            }
        }
    }
    let n = len?;
    if n > 256 << 20 {
        return None;
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

// ───────────────────────── 受付に 1 つ ─────────────────────────

/// rust-analyzer の持ち主（受付に 1 つ）。
#[derive(Default)]
pub struct LspHub {
    current: Mutex<Option<Arc<Ra>>>,
    cpu: Arc<Cpu>,
}

impl LspHub {
    pub fn new() -> LspHub {
        LspHub::default()
    }

    /// rust-analyzer たちが使った CPU の通算（ms。減らない）。
    pub fn cpu_ms(&self) -> u64 {
        let pgid = self.current.lock().unwrap().as_ref().filter(|r| r.st.lock().unwrap().alive).map(|r| r.pgid);
        self.cpu.total(pgid)
    }

    /// 今の rust-analyzer の状態（`/v1/status` 用）。
    pub fn state(&self) -> Option<(String, LspPhase)> {
        self.current.lock().unwrap().as_ref().map(|r| (r.project.clone(), r.st.lock().unwrap().phase))
    }

    /// 先に起こしておく。今の状態をすぐ `reply` へ送り、状態が変わったときにも送る。
    /// `conn` は接続の番号（同じ接続を 2 回数えない）。
    pub fn warm(&self, store: &Store, project: &str, manifest: &Manifest, path: &str, conn: u64, reply: &Reply) {
        match self.ensure(store, project, manifest) {
            Ok(ra) => {
                let mut st = ra.st.lock().unwrap();
                ra.ensure_open(&mut st, path);
                let m = ra.state_msg(&st);
                let _ = reply.send(m);
                st.subs.retain(|(c, _)| *c != conn);
                st.subs.push((conn, reply.clone()));
            }
            Err(message) => {
                let _ = reply.send(ServerMsg::LspState { cpu_ms: self.cpu_ms(), project: project.to_string(), state: LspPhase::Failed, message, error: None });
            }
        }
    }

    /// 問い合わせる（答えは必ず 1 つ `reply` へ届く）。
    pub fn query(&self, store: &Store, project: &str, manifest: &Manifest, q: Query, reply: &Reply) {
        match self.ensure(store, project, manifest) {
            Ok(ra) => ra.query(q, reply),
            Err(message) => {
                let _ = reply.send(ServerMsg::LspResult { cpu_ms: self.cpu_ms(), id: q.id, error: Some(ErrorCode::Unavailable), message, text: String::new(), items: vec![], retry_after_s: None });
            }
        }
    }

    /// 止める（受付が終わるとき）。
    pub fn shutdown(&self) {
        if let Some(ra) = self.current.lock().unwrap().take() {
            ra.stop();
        }
    }

    /// そのプロジェクトの rust-analyzer を用意し、目録に合わせる。
    fn ensure(&self, store: &Store, project: &str, manifest: &Manifest) -> Result<Arc<Ra>, String> {
        let mut cur = self.current.lock().unwrap();
        if let Some(ra) = cur.as_ref() {
            if ra.project == project && ra.st.lock().unwrap().alive {
                let ra = ra.clone();
                drop(cur);
                ra.sync(store, manifest)?;
                return Ok(ra);
            }
        }
        if let Some(old) = cur.take() {
            old.stop();
        }
        let ra = self.spawn(store, project, manifest)?;
        *cur = Some(ra.clone());
        Ok(ra)
    }

    fn spawn(&self, store: &Store, project: &str, manifest: &Manifest) -> Result<Arc<Ra>, String> {
        let program = find_in_path("rust-analyzer").ok_or("rust-analyzer が無い（ビルド係のイメージに入っていない）")?;
        store.sync_lsp_tree(project, manifest).map_err(|e| format!("rust-analyzer 用の木を作れない: {e}"))?;
        let root = store.lsp_dir(project).canonicalize().map_err(|e| format!("rust-analyzer 用の木が無い: {e}"))?;
        let pdir = root.parent().unwrap_or(&root).to_path_buf();
        let target = pdir.join("lsp-target");
        let _ = std::fs::create_dir_all(&target);
        // Cargo.toml の無い「1 ファイルだけ」: zc-buildd のジョブと同じ仮の crate を、木の外に作る
        let linked = if manifest.files().iter().any(|f| f.path == "Cargo.toml") {
            None
        } else {
            let entry = manifest.files().iter().find(|f| f.path.ends_with(".rs")).ok_or("Rust のファイルが無い")?;
            let dir = pdir.join("lsp-single");
            std::fs::create_dir_all(&dir).map_err(|e| format!("仮の crate を作れない: {e}"))?;
            let toml = format!(
                "# zc-buildd が rust-analyzer のために作った仮の crate\n[package]\nname = \"zc-single\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[[bin]]\nname = \"main\"\npath = {}\n\n[workspace]\n",
                serde_json::to_string(&root.join(&entry.path).to_string_lossy()).expect("JSON にできる")
            );
            let m = dir.join("Cargo.toml");
            std::fs::write(&m, toml).map_err(|e| format!("仮の crate を作れない: {e}"))?;
            Some(m)
        };
        let config = ra_config(&target, linked.as_deref());

        let mut cmd = Command::new(program);
        cmd.current_dir(&root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
        tool_env(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("rust-analyzer を起動できない: {e}"))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let pgid = child.id();
        let files = manifest.files().iter().map(|f| (f.path.clone(), f.sha256.clone())).collect();
        let st = RaState {
            next_id: 1,
            pending: HashMap::new(),
            phase: LspPhase::Loading,
            message: String::new(),
            ready: false,
            initialized: false,
            outbox: Vec::new(),
            subs: Vec::new(),
            files,
            open: HashMap::new(),
            alive: true,
        };
        let ra = Arc::new(Ra { project: project.to_string(), root: root.clone(), config: config.clone(), pgid, child: Mutex::new(Some(child)), stdin: Mutex::new(stdin), st: Mutex::new(st), stderr_tail: Mutex::new(String::new()), cpu: self.cpu.clone() });
        eprintln!("zc-buildd: rust-analyzer を起動した（プロジェクト {}）", &project[..project.len().min(8)]);

        // initialize はすぐ書く（ほかは返事が来るまで outbox にためる）
        let init = json!({
            "jsonrpc": "2.0",
            "id": INIT_ID,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": path_to_uri(&root),
                "workspaceFolders": [{ "uri": path_to_uri(&root), "name": "zc" }],
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "window": { "workDoneProgress": false },
                    "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true }, "symbol": {} },
                    "textDocument": {
                        "hover": { "contentFormat": ["plaintext"] },
                        "definition": { "linkSupport": false },
                        "completion": { "completionItem": { "snippetSupport": false } },
                    },
                    "experimental": { "serverStatusNotification": true },
                },
                "initializationOptions": config,
            }
        });
        {
            let mut w = ra.stdin.lock().unwrap();
            w.write_all(&frame(&init)).and_then(|()| w.flush()).map_err(|e| format!("rust-analyzer に書けない: {e}"))?;
        }
        let r2 = ra.clone();
        std::thread::Builder::new()
            .name("zc-ra-out".into())
            .spawn(move || {
                let mut r = BufReader::with_capacity(64 << 10, stdout);
                while let Some(v) = read_message(&mut r) {
                    r2.on_message(v);
                }
                r2.on_exit();
            })
            .map_err(|e| format!("スレッドを作れない: {e}"))?;
        let r3 = ra.clone();
        let _ = std::thread::Builder::new().name("zc-ra-err".into()).spawn(move || {
            let mut r = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let mut t = r3.stderr_tail.lock().unwrap();
                        t.push_str(&line);
                        if t.len() > STDERR_TAIL {
                            let mut cut_at = t.len() - STDERR_TAIL;
                            while !t.is_char_boundary(cut_at) {
                                cut_at += 1;
                            }
                            t.drain(..cut_at);
                        }
                    }
                }
            }
        });
        Ok(ra)
    }
}

impl Drop for LspHub {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 少し待つ（試験用の補助。届いたものを `pred` が受けるまで、最大 `limit`）。
pub fn recv_until(rx: &std::sync::mpsc::Receiver<ServerMsg>, limit: Duration, mut pred: impl FnMut(&ServerMsg) -> bool) -> Option<ServerMsg> {
    let deadline = std::time::Instant::now() + limit;
    loop {
        let left = deadline.checked_duration_since(std::time::Instant::now())?;
        match rx.recv_timeout(left) {
            Ok(m) if pred(&m) => return Some(m),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_and_utf8_columns() {
        let line = "let あ = \"😀x\"; // 日本";
        // 「あ」の後ろ = 7 バイト目 = UTF-16 で 5
        assert_eq!(utf8_to_utf16(line, 7), 5);
        assert_eq!(utf16_to_utf8(line, 5), 7);
        // 😀 は UTF-16 で 2 単位・UTF-8 で 4 バイト
        let x = line.find('x').unwrap() as u32;
        assert_eq!(utf8_to_utf16(line, x), 11);
        assert_eq!(utf16_to_utf8(line, 11), x);
        // 行の外は行末に丸める
        assert_eq!(utf16_to_utf8(line, 1000), line.len() as u32);
        assert_eq!(utf8_to_utf16(line, 1000), line.encode_utf16().count() as u32);
        // サロゲートの途中は次の文字の頭
        let e = line.find('😀').unwrap() as u32;
        assert_eq!(utf16_to_utf8(line, utf8_to_utf16(line, e) + 1), e + 4);
        assert_eq!(nth_line("a\r\nb\n", 0), "a");
        assert_eq!(nth_line("a\r\nb\n", 1), "b");
        assert_eq!(nth_line("a", 5), "");
    }

    #[test]
    fn uris_round_trip() {
        let p = Path::new("/var/lib/zc-buildd/proj/ab/lsp/src/日本 語%.rs");
        let u = path_to_uri(p);
        assert!(u.starts_with("file:///var/lib/zc-buildd/proj/ab/lsp/src/%E6%97%A5"), "{u}");
        assert!(!u.contains(' '));
        assert_eq!(uri_to_path(&u).as_deref(), Some(p));
        assert_eq!(uri_to_path("file:///a/b%20c"), Some(PathBuf::from("/a/b c")));
        assert_eq!(uri_to_path("file://localhost/a"), Some(PathBuf::from("/a")));
        assert_eq!(uri_to_path("https://x/a"), None);
        assert_eq!(uri_to_path("file:///a%2"), Some(PathBuf::from("/a%2")));
    }

    #[test]
    fn external_labels() {
        assert_eq!(external_label(Path::new("/var/lib/zc-buildd/cargo-home/registry/src/index.crates.io-1949cf8c6b5b557f/regex-1.11.1/src/regex/string.rs")), "regex-1.11.1/src/regex/string.rs");
        assert_eq!(external_label(Path::new("/opt/rustup/toolchains/1.100.0-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/option.rs")), "core/src/option.rs");
        assert_eq!(external_label(Path::new("/h/.cargo/git/checkouts/foo-0123/abcdef/src/lib.rs")), "foo-0123/src/lib.rs");
        assert_eq!(external_label(Path::new("/a/b/c/d/e.rs")), "c/d/e.rs");
        assert_eq!(external_label(Path::new("/e.rs")), "/e.rs");
    }

    #[test]
    fn reads_framed_messages() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let raw = format!("Content-Length: {}\r\nContent-Type: x\r\n\r\n{body}Content-Length: 2\r\n\r\n{{}}", body.len());
        let mut r = BufReader::new(raw.as_bytes());
        assert_eq!(read_message(&mut r).unwrap()["id"], 1);
        assert_eq!(read_message(&mut r).unwrap(), json!({}));
        assert!(read_message(&mut r).is_none());
        assert_eq!(frame(&json!({"a":1})), b"Content-Length: 7\r\n\r\n{\"a\":1}".to_vec());
    }

    fn pending(query: LspQuery, path: &str, line: u32, col: u32) -> Pending {
        let (tx, _rx) = std::sync::mpsc::channel();
        Pending { wire_id: 1, query, reply: tx, path: path.into(), line, col }
    }

    /// rust-analyzer の答えの形から実機の形へ（rust-analyzer なしで、答えを手で作って確かめる）。
    #[test]
    fn converts_answers() {
        let dir = std::env::temp_dir().join(format!("zc-lsp-conv-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "// 日本語\nfn main() { let s = String::new(); s.le }\n").unwrap();
        let files: HashSet<String> = ["src/main.rs".to_string()].into();
        let uri = path_to_uri(&dir.join("src/main.rs"));

        let (t, _) = convert(&dir, &files, &pending(LspQuery::Hover, "src/main.rs", 1, 20), &json!({ "contents": { "kind": "plaintext", "value": "  alloc::string::String\n\npub fn new() -> String  " } }));
        assert_eq!(t, "alloc::string::String\n\npub fn new() -> String");

        // 場所: 日本語の後ろの UTF-16 の桁が UTF-8 のバイトに直る。プロジェクトの外は ext
        let res = json!([
            { "uri": uri, "range": { "start": { "line": 1, "character": 16 }, "end": { "line": 1, "character": 17 } } },
            { "uri": "file:///opt/rustup/toolchains/x/lib/rustlib/src/rust/library/alloc/src/string.rs", "range": { "start": { "line": 3, "character": 0 }, "end": { "line": 3, "character": 4 } } },
        ]);
        let (_, items) = convert(&dir, &files, &pending(LspQuery::References, "src/main.rs", 1, 16), &res);
        assert_eq!(items.len(), 2);
        assert_eq!((items[0].path.as_str(), items[0].line, items[0].col, items[0].end), ("src/main.rs", 1, 16, Some((1, 17))));
        assert_eq!(items[0].detail, "fn main() { let s = String::new(); s.le }");
        assert_eq!((items[1].path.as_str(), items[1].ext.as_str(), items[1].line), ("", "alloc/src/string.rs", 3));
        // 行が日本語のとき
        let res = json!({ "uri": uri, "range": { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 5 } } });
        let (_, items) = convert(&dir, &files, &pending(LspQuery::Definition, "src/main.rs", 0, 0), &res);
        // 「// 」の 3 単位 + 「日」の 1 単位 = 4 → 3 + 3 バイト
        assert_eq!((items[0].col, items[0].end), (3 + 3, Some((0, 3 + 3 * 2))));
        // LocationLink
        let res = json!([{ "targetUri": uri, "targetRange": {}, "targetSelectionRange": { "start": { "line": 1, "character": 3 }, "end": { "line": 1, "character": 7 } } }]);
        let (_, items) = convert(&dir, &files, &pending(LspQuery::Definition, "src/main.rs", 0, 0), &res);
        assert_eq!((items[0].line, items[0].col), (1, 3));
        assert!(convert(&dir, &files, &pending(LspQuery::Definition, "src/main.rs", 0, 0), &Value::Null).1.is_empty());

        // symbols
        let res = json!([{ "name": "main", "kind": 12, "containerName": "", "location": { "uri": uri, "range": { "start": { "line": 1, "character": 3 }, "end": { "line": 1, "character": 7 } } } }]);
        let (_, items) = convert(&dir, &files, &pending(LspQuery::Symbols, "", 0, 0), &res);
        assert_eq!((items[0].name.as_str(), items[0].kind, items[0].path.as_str()), ("main", 12, "src/main.rs"));

        // completion: textEdit の範囲、スニペットと別の行の編集は捨てる、sortText の順
        let line1 = "fn main() { let s = String::new(); s.le }";
        let c16 = line1.find("le }").unwrap() as u64;
        let res = json!({ "isIncomplete": true, "items": [
            { "label": "len()", "kind": 2, "sortText": "b", "detail": "fn(&self) -> usize", "textEdit": { "range": { "start": { "line": 1, "character": c16 }, "end": { "line": 1, "character": c16 + 2 } }, "newText": "len()" } },
            { "label": "leak", "kind": 2, "sortText": "a", "textEdit": { "insert": { "start": { "line": 1, "character": c16 }, "end": { "line": 1, "character": c16 + 2 } }, "replace": { "start": { "line": 1, "character": c16 }, "end": { "line": 1, "character": c16 + 2 } }, "newText": "leak" } },
            { "label": "snip", "insertTextFormat": 2, "insertText": "snip($0)" },
            { "label": "other", "textEdit": { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 1, "character": 0 } }, "newText": "x" } },
            { "label": "lex" },
            { "label": "plain" },
            { "label": "zzz", "textEdit": { "range": { "start": { "line": 1, "character": c16 }, "end": { "line": 1, "character": c16 + 2 } }, "newText": "zzz" } },
        ]});
        let col = (line1.find("le }").unwrap() + 2) as u32;
        let (_, items) = convert(&dir, &files, &pending(LspQuery::Completion, "src/main.rs", 1, col), &res);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["leak", "len()", "lex"]);
        assert_eq!((items[1].col, items[1].insert.as_str(), items[1].kind), (c16 as u32 + 0, "len()", 2));
        // 置き換えの範囲が無い候補は、単語の始まりから
        assert_eq!((items[2].col, items[2].insert.as_str()), (line1.find("le }").unwrap() as u32, "lex"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_shape() {
        let c = ra_config(Path::new("/d/lsp-target"), None);
        assert_eq!(c["checkOnSave"], false);
        assert_eq!(c["cargo"]["targetDir"], "/d/lsp-target");
        assert_eq!(c["files"]["watcher"], "client");
        assert!(c.get("linkedProjects").is_none());
        let c = ra_config(Path::new("/d/t"), Some(Path::new("/d/lsp-single/Cargo.toml")));
        assert_eq!(c["linkedProjects"][0], "/d/lsp-single/Cargo.toml");
    }
}
