//! 課題 D5 の試験の道具（`tests/serve.rs` と `tests/e2e.rs` が使う）。
//!
//! 本物の実行ファイル `zc-remote` を、試験ごとの一時フォルダに向けて動かす
//! （`XDG_RUNTIME_DIR`・`XDG_CACHE_HOME`・`XDG_CONFIG_HOME`・`HOME`。docs/stage3-design.md §14）。
//! serve は別プロセスなので、シグナル・無通信での終了・二重起動もそのまま試せる。
#![allow(dead_code)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use zc_remote_ipc as ipc;

pub const ZC_REMOTE: &str = env!("CARGO_BIN_EXE_zc-remote");

/// 試験 1 つぶんの置き場。捨てるときに serve を止めて消す。
pub struct Sandbox {
    pub root: PathBuf,
}

impl Sandbox {
    pub fn new(tag: &str) -> Sandbox {
        // ソケットのパスは 108 バイトまで。短い名前にする。
        let root = std::env::temp_dir().join(format!("zc-d5-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["run", "cache", "config/zcode", "home/code"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        Sandbox { root: std::fs::canonicalize(&root).unwrap() }
    }

    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    pub fn run_bin(&self) -> PathBuf {
        self.root.join("run/zcode/bin")
    }

    pub fn cache_bin(&self) -> PathBuf {
        self.root.join("cache/zcode/bin")
    }

    pub fn socket(&self) -> PathBuf {
        self.root.join("run/zcode/remote.sock")
    }

    /// `home/` の下にファイルを書く。絶対パスを返す。
    pub fn write(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.home().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        p
    }

    /// `zc-remote` を、この置き場に向けて動かすコマンド。`env` は `ZC_REMOTE_PC` など。
    pub fn cmd(&self, env: &[(&str, String)]) -> Command {
        let mut c = Command::new(ZC_REMOTE);
        for k in ["ZC_REMOTE_PC", "ZC_REMOTE_URL", "ZC_REMOTE_BACKEND"] {
            c.env_remove(k);
        }
        c.env("XDG_RUNTIME_DIR", self.root.join("run")).env("XDG_CACHE_HOME", self.root.join("cache")).env("XDG_CONFIG_HOME", self.root.join("config")).env("HOME", self.home());
        for (k, v) in env {
            c.env(k, v);
        }
        c.stdin(Stdio::null());
        c
    }

    /// serve を子プロセスで起動して、ソケットができるまで待つ。
    pub fn serve(&self, env: &[(&str, String)], args: &[&str]) -> Serve {
        let mut c = self.cmd(env);
        c.arg("serve").arg("--stats").args(args).stdout(Stdio::null()).stderr(Stdio::piped());
        let child = c.spawn().expect("zc-remote serve を起動できない");
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(self.socket()).is_err() {
            assert!(Instant::now() < deadline, "serve のソケットができない");
            std::thread::sleep(Duration::from_millis(10));
        }
        Serve { child: Some(child) }
    }

    pub fn client(&self, kind: ipc::ClientKind) -> Client {
        Client::connect(&self.socket(), kind)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // CLI が切り離して起動した serve が残っていれば止める。
        if UnixStream::connect(self.socket()).is_ok() {
            let _ = self.cmd(&[]).arg("stop").stdout(Stdio::null()).stderr(Stdio::null()).status();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 子プロセスの serve。捨てると止める。
pub struct Serve {
    child: Option<Child>,
}

impl Serve {
    pub fn pid(&self) -> i32 {
        self.child.as_ref().unwrap().id() as i32
    }

    /// 終わるのを待つ（最大 `wait`）。終了コードと標準エラー。
    pub fn wait(mut self, wait: Duration) -> (Option<i32>, String) {
        let mut child = self.child.take().unwrap();
        let deadline = Instant::now() + wait;
        loop {
            match child.try_wait().unwrap() {
                Some(st) => {
                    let mut err = String::new();
                    let _ = child.stderr.take().unwrap().read_to_string(&mut err);
                    return (st.code(), err);
                }
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("serve が {wait:?} で終わらない");
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// 時間切れつきで最後まで動かす。
pub fn run(mut c: Command, wait: Duration) -> Output {
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().expect("起動できない");
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let to = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let te = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let deadline = Instant::now() + wait;
    let status = loop {
        match child.try_wait().unwrap() {
            Some(st) => break st,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let st = child.wait().unwrap();
                let (o, e) = (to.join().unwrap(), te.join().unwrap());
                panic!("{wait:?} で終わらない（{st:?}）\n--- stdout\n{}\n--- stderr\n{}", String::from_utf8_lossy(&o), String::from_utf8_lossy(&e));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    Output { status, stdout: to.join().unwrap(), stderr: te.join().unwrap() }
}

pub fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// UNIX ソケットの相手（zcode の専用スレッドや CLI の役）。`zc_remote_ipc` で話す。
pub struct Client {
    s: UnixStream,
    r: ipc::FrameReader,
}

impl Client {
    pub fn connect(socket: &Path, kind: ipc::ClientKind) -> Client {
        let s = UnixStream::connect(socket).expect("serve につなげない");
        let mut c = Client { s, r: ipc::FrameReader::new() };
        c.send(&ipc::Request::Hello { version: ipc::IPC_VERSION, client: kind });
        assert_eq!(c.recv(Duration::from_secs(5)), Some(ipc::Response::Hello { version: ipc::IPC_VERSION }));
        c
    }

    pub fn send(&mut self, r: &ipc::Request) {
        let mut out = Vec::new();
        ipc::encode_request(r, &mut out);
        self.s.write_all(&out).unwrap();
    }

    pub fn submit(&mut self, req: u32, kind: ipc::JobKind, lang: ipc::RemoteLang, flags: ipc::SubmitFlags, file: &Path) {
        self.submit_overlay(req, kind, lang, flags, file, Vec::new());
    }

    /// 未保存の中身を重ねて頼む（打鍵が止まったときの check の形）。
    pub fn submit_overlay(&mut self, req: u32, kind: ipc::JobKind, lang: ipc::RemoteLang, flags: ipc::SubmitFlags, file: &Path, overlay: Vec<ipc::OverlayFile>) {
        self.send(&ipc::Request::Submit { req: ipc::ReqId(req), kind, lang, flags, file: file.to_owned(), overlay });
    }

    /// 次の知らせ（最大 `wait`）。切れた・時間切れは `None`。
    pub fn recv(&mut self, wait: Duration) -> Option<ipc::Response> {
        let deadline = Instant::now() + wait;
        let mut buf = [0u8; 16 << 10];
        loop {
            if let Some(frame) = self.r.next_frame().expect("serve の返事は切り出せる") {
                // 1 メッセージは上限の中
                assert!(frame.len() <= ipc::MAX_FRAME);
                return Some(ipc::decode_response(frame).expect("serve の返事は読める"));
            }
            let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
            self.s.set_read_timeout(Some(left)).unwrap();
            match self.s.read(&mut buf) {
                Ok(0) => return None,
                Ok(n) => self.r.push(&buf[..n]),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
                Err(_) => return None,
            }
        }
    }

    /// `Done` まで集める（`Done` を含む）。
    pub fn until_done(&mut self, wait: Duration) -> Vec<ipc::Response> {
        let deadline = Instant::now() + wait;
        let mut got = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.recv(left) {
                Some(r) => {
                    let done = matches!(r, ipc::Response::Done { .. });
                    got.push(r);
                    if done {
                        return got;
                    }
                }
                None => panic!("Done が届かない。届いたもの: {got:#?}"),
            }
        }
    }
}

pub fn done_of(got: &[ipc::Response]) -> (ipc::DoneStatus, i32, ipc::Backend, String) {
    match got.last() {
        Some(ipc::Response::Done { status, exit, backend, message, .. }) => (*status, *exit, *backend, message.clone()),
        other => panic!("Done で終わっていない: {other:?}"),
    }
}

/// 使われていないポート（開けてすぐ閉じる）。つなぐと断られる。
pub fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}
