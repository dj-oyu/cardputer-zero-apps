//! 統合ターミナルと F5 実行（段階 3、モック 3・4）。本物の pty と bash / python3 を使う
//! （開発コンテナにはある）。実機もイベントループも要らない（pty の fd を自分で待つ）。

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zc_core::rt::{main_channel, MainReceiver, MainSender};
use zc_core::time::now_us;
use zc_core::{Key, KeyEvent, Mods};
use zcode::app::{AppEvent, LoopCx, Screen, Timers, Workers};
use zcode::doc::DocBackend;
use zcode::editor::{EditorConfig, EditorScreen};

struct Loop {
    timers: Timers,
    workers: Workers,
    tx: MainSender<AppEvent>,
    rx: MainReceiver<AppEvent>,
}

impl Loop {
    fn new() -> Loop {
        let (tx, rx) = main_channel().unwrap();
        Loop { timers: Timers::new().unwrap(), workers: Workers::new(), tx, rx }
    }
    fn with<R>(&mut self, f: impl FnOnce(&mut LoopCx<'_>) -> R) -> R {
        let mut cx = LoopCx { timers: &mut self.timers, workers: &self.workers, results: &self.tx, now_us: now_us(), quit: false, force_present: false };
        f(&mut cx)
    }
    fn keys(&mut self, ed: &mut EditorScreen, keys: &[KeyEvent]) {
        self.with(|cx| ed.keys(keys, Mods::NONE, cx));
    }
    fn type_str(&mut self, ed: &mut EditorScreen, s: &str) {
        for c in s.chars() {
            let ev = if c == '\n' { KeyEvent::new(Key::Enter, Mods::NONE) } else { KeyEvent::char(c) };
            self.keys(ed, &[ev]);
        }
    }
    /// ワーカーの結果を画面に届ける。
    fn pump(&mut self, ed: &mut EditorScreen) {
        self.workers.batch.wait_idle();
        self.workers.storage.wait_idle();
        let evs: Vec<AppEvent> = self.rx.drain().collect();
        for ev in evs {
            self.with(|cx| ed.on_event(ev, cx));
        }
    }
    /// 端末の出力を読みながら、`done` が真になるまで待つ（最大 10 秒）。
    fn until(&mut self, ed: &mut EditorScreen, what: &str, mut done: impl FnMut(&EditorScreen) -> bool) {
        let t0 = Instant::now();
        while !done(ed) {
            assert!(t0.elapsed() < Duration::from_secs(10), "待ちきれなかった: {what}\n{:#?}", ed.terminal_text());
            self.pump(ed);
            if let Some(fd) = ed.poll_fd() {
                let mut ready = [false];
                zc_core::rt::poll_fds(&[fd], &mut ready, 50).unwrap();
                if ready[0] {
                    self.with(|cx| ed.on_poll_fd(cx));
                }
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(Key::Char(c), Mods::CTRL)
}
fn f5() -> KeyEvent {
    KeyEvent::new(Key::F(5), Mods::NONE)
}

fn tmp_dir(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("zcode-term-{tag}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn has_tools() -> bool {
    let ok = |p: &str, a: &str| std::process::Command::new(p).arg("-c").arg(a).status().is_ok_and(|s| s.success());
    ok("bash", "true") && ok("python3", "pass")
}

fn screen_has(ed: &EditorScreen, needle: &str) -> bool {
    ed.terminal_text().is_some_and(|rows| rows.iter().any(|r| r.contains(needle)))
}

fn open(lp: &mut Loop, dir: &std::path::Path, file: &str) -> EditorScreen {
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Buffer, root: dir.to_path_buf(), files: vec![dir.join(file)] });
    lp.with(|cx| ed.start(cx));
    lp.pump(&mut ed);
    ed
}

#[test]
fn f5_runs_the_file_and_reports_exit_code() {
    if !has_tools() {
        return;
    }
    let dir = tmp_dir("run");
    fs::write(dir.join("ok.py"), "print('hello from zcode')\n").unwrap();
    fs::write(dir.join("bad.py"), "import sys\nprint('bye')\nsys.exit(3)\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = open(&mut lp, &dir, "ok.py");
    lp.keys(&mut ed, &[f5()]);
    // パネルが開いて、入力が端末へ向く
    assert_eq!(ed.terminal_state(), (true, false, true));
    lp.until(&mut ed, "ok.py の終了", |ed| ed.run_state().1.is_some());
    assert!(screen_has(&ed, "hello from zcode"));
    assert!(screen_has(&ed, "[終了コード 0]"));
    assert_eq!(ed.run_state(), (false, Some("✓ 終了コード 0  ok.py")));
    // シェルは残っていて、続けてコマンドを打てる
    lp.type_str(&mut ed, "echo still-$((40+2))\n");
    lp.until(&mut ed, "シェルの応答", |ed| screen_has(ed, "still-42"));
    // Ctrl+` で閉じてエディタへ。別のファイルを F5 すると、新しい端末で実行する
    lp.keys(&mut ed, &[ctrl('`')]);
    assert_eq!(ed.terminal_state(), (false, false, false));
    lp.with(|cx| ed.open_path(&dir.join("bad.py"), cx));
    lp.pump(&mut ed);
    lp.keys(&mut ed, &[f5()]);
    lp.until(&mut ed, "bad.py の終了", |ed| ed.run_state().1.is_some_and(|s| s.contains("bad.py")));
    assert_eq!(ed.run_state().1, Some("✕ 終了コード 3  bad.py"));
    assert!(screen_has(&ed, "bye"));
    assert!(!screen_has(&ed, "still-42"), "前の端末の画面が残っている");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn f5_saves_first_then_runs_the_saved_text() {
    if !has_tools() {
        return;
    }
    let dir = tmp_dir("save");
    fs::write(dir.join("main.py"), "print('old')\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = open(&mut lp, &dir, "main.py");
    // 書き換えて、保存せずに F5
    lp.keys(&mut ed, &[KeyEvent::new(Key::Right, Mods::NONE); 7]);
    lp.keys(&mut ed, &[KeyEvent::new(Key::Delete, Mods::NONE); 3]);
    lp.type_str(&mut ed, "new");
    assert!(ed.active_doc().unwrap().is_modified());
    lp.keys(&mut ed, &[f5()]);
    lp.until(&mut ed, "保存して実行", |ed| ed.run_state().1.is_some());
    assert!(screen_has(&ed, "new"), "{:#?}", ed.terminal_text());
    assert!(!screen_has(&ed, "old"));
    assert_eq!(fs::read_to_string(dir.join("main.py")).unwrap(), "print('new')\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ctrl_c_stops_a_running_program() {
    if !has_tools() {
        return;
    }
    let dir = tmp_dir("stop");
    fs::write(dir.join("loop.py"), "import time\nprint('started', flush=True)\nwhile True:\n    time.sleep(0.1)\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = open(&mut lp, &dir, "loop.py");
    lp.keys(&mut ed, &[f5()]);
    lp.until(&mut ed, "起動", |ed| screen_has(ed, "started"));
    assert!(ed.run_state().0);
    // 端末に入力があるので Ctrl+C は端末へ（コピーではない）。Python は KeyboardInterrupt で止まる
    lp.keys(&mut ed, &[ctrl('c')]);
    lp.until(&mut ed, "停止", |ed| ed.run_state().1.is_some());
    assert!(ed.run_state().1.unwrap().starts_with("✕ 終了コード"), "{:?}", ed.run_state());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ctrl_backtick_opens_a_shell_and_toggles_focus() {
    if !has_tools() {
        return;
    }
    let dir = tmp_dir("shell");
    fs::write(dir.join("a.py"), "x = 1\n").unwrap();
    let mut lp = Loop::new();
    let mut ed = open(&mut lp, &dir, "a.py");
    lp.keys(&mut ed, &[ctrl('`')]);
    assert_eq!(ed.terminal_state(), (true, false, true));
    lp.type_str(&mut ed, "echo shell-$((6*7))\n");
    lp.until(&mut ed, "シェル", |ed| screen_has(ed, "shell-42"));
    // 端末にいるとき: Ctrl+` で閉じてエディタへ。もう一度で同じシェルに戻る
    lp.keys(&mut ed, &[ctrl('`')]);
    assert_eq!(ed.terminal_state(), (false, false, false));
    lp.keys(&mut ed, &[ctrl('`')]);
    assert_eq!(ed.terminal_state(), (true, false, true));
    assert!(screen_has(&ed, "shell-42"));
    // Ctrl+Shift+` で全画面
    lp.keys(&mut ed, &[KeyEvent::new(Key::Char('`'), Mods(Mods::CTRL.0 | Mods::SHIFT.0))]);
    assert_eq!(ed.terminal_state(), (true, true, true));
    // exit でシェルが終わると、パネルが閉じてエディタへ戻る
    lp.type_str(&mut ed, "exit\n");
    lp.until(&mut ed, "シェルの終了", |ed| !ed.terminal_state().0);
    assert!(ed.terminal_text().is_none());
    let _ = fs::remove_dir_all(&dir);
}

/// 決まった名前のフォルダ（正解画像に出るパスを、どの環境でも同じにする）。
fn fixed_dir(name: &str) -> PathBuf {
    let d = PathBuf::from("/tmp").join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn render_to(ed: &mut EditorScreen) -> Option<zc_core::Canvas> {
    let fonts = zc_font::FontConfig::default();
    if !fonts.available() {
        return None;
    }
    Some(zcode::render_screen(ed, &fonts).unwrap())
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden").join(name)
}

/// モック 3: F5 で実行中（下のパネル、ステータスバーが橙）。正解画像と比べる。
#[test]
fn terminal_running_screen() {
    if !has_tools() {
        return;
    }
    let dir = fixed_dir("zcode-golden-run");
    fs::write(dir.join("main.py"), "import time
for i in range(5):
    print('led on' if i % 2 == 0 else 'led off', flush=True)
time.sleep(30)
").unwrap();
    let mut lp = Loop::new();
    let mut ed = open(&mut lp, &dir, "main.py");
    lp.keys(&mut ed, &[f5()]);
    lp.until(&mut ed, "5 行の出力", |ed| ed.terminal_text().is_some_and(|rows| rows.iter().filter(|r| r.starts_with("led o")).count() == 5));
    assert!(ed.run_state().0);
    if let Some(canvas) = render_to(&mut ed) {
        if let Err(e) = zc_display::png::check_golden(&canvas, golden("terminal-run.png")) {
            panic!("{e}");
        }
    }
    let _ = fs::remove_dir_all(&dir);
}

/// モック 4: 全画面・エラー終了。最後にシェルのプロンプトが出る（環境で変わる）ので正解画像には
/// しない。`cargo test -p zcode --test terminal -- --ignored` で `out/terminal-error.png` に描く。
#[test]
#[ignore]
fn terminal_error_fullscreen_picture() {
    if !has_tools() {
        return;
    }
    let dir = fixed_dir("zcode-golden-err");
    fs::write(dir.join("main.py"), "def main():
    for i in range(10):
        led(i % 2)

if __name__ == '__main__':
    main()
").unwrap();
    let mut lp = Loop::new();
    let mut ed = open(&mut lp, &dir, "main.py");
    lp.keys(&mut ed, &[f5()]);
    lp.keys(&mut ed, &[KeyEvent::new(Key::Char('`'), Mods(Mods::CTRL.0 | Mods::SHIFT.0))]);
    lp.until(&mut ed, "終了", |ed| ed.run_state().1.is_some());
    std::thread::sleep(Duration::from_millis(300));
    lp.until(&mut ed, "プロンプト", |ed| ed.terminal_text().is_some_and(|r| r.iter().any(|l| l.ends_with('$') || l.ends_with('#'))));
    if let Some(canvas) = render_to(&mut ed) {
        let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../out/terminal-error.png");
        zc_display::png::write_png(&canvas, &out).unwrap();
    }
    let _ = fs::remove_dir_all(&dir);
}
