//! zcode の入口。引数にモードが無ければエディタを開く（`zcode [ファイル...]`）。
//! ほかに、キー確認・フォント比較・計測の画面がある。

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use zc_core::time::now_us;
use zc_font::FontConfig;
use zc_input::Keymap;
use zcode::app::{self, Options, Screen};
use zc_core::CancelToken;
use zcode::bench::{Bench, BenchKind};
use zcode::doc::DocBackend;
use zcode::edbench::{EdBenchKind, EditorBench};
use zcode::editor::{EditorConfig, EditorScreen};
use zcode::fonttest::FontTest;
use zcode::keytest::KeyTest;
use zcode::{keylog, render_screen};

const HELP: &str = "\
zcode — CardputerZero 用コードエディタ

使い方:
  zcode [ファイル...] [--root <フォルダ>]    エディタ（ファイルが無ければツリーから開く）
  zcode --keytest [--guide] [--log <パス>]   キー確認モード
  zcode --fonttest [--bdf <パス>] [--auto <ms>] [--page <n>]   フォント比較
  zcode --bench <line|line-status|scroll|full> [--frames <n>]  描画の計測
                （panic / hang は異常終了の後始末の試験）
  zcode --bench <editor-type|editor-scroll> [--frames <n>]     エディタ画面の計測
  zcode --keymap-from-log <ログ>             キー確認のログからキー配列の行を作る

エディタ:
  終了は Ctrl+Q。Ctrl+1 ツリー・Ctrl+2 エディタ・Ctrl+3 ターミナルへ移る。Ctrl+B / F12 で
  ツリー / ターミナルを開閉、F11 でいまの区画を最大化（ツリーではフォルダを開く画面）。
  --root <フォルダ>        ツリーの根（既定は ~/code。無ければ今いるフォルダ）
  --simple-buffer          文書を zc-buffer でなく、行の配列の小さな実装で持つ（開発用）
  --font-dir <フォルダ>    コードのフォント（cozette.bdf・PixelMplus12-Regular.ttf）の場所。
                           既定は実行ファイルの隣の fonts/。無ければ DejaVu Sans Mono
  --no-bundled-font        同梱のフォントを使わず DejaVu Sans Mono で描く

キー確認モード:
  終了は Esc を続けて 5 回。無操作が続いても終わる（--idle <秒>、既定 120）。
  ログは JSON Lines で /tmp/zcode-keytest.jsonl に追記する（--log で変更）。
  --guide を付けると、刻印を 1 つずつ指定して押してもらい、Sym の対応表を作る。

共通:
  --render-test <out.png>  実機を使わず、画面を PNG に描いて終わる
  --stats                  終了時に計測値を標準エラーに出す
  --watch-fb <ms>          fb0 を定期的に読み戻し、ほかのプロセスの上書きを数える
  --idle <秒>              無操作で終了するまで（既定 120）
  --fb <パス>              既定 /dev/fb0
  --kbd <パス>             既定はデバイス名 tca8418c で探す
  --inline-raster          字形を assist スレッドでなく、その場で作る
  --grab                   キーボードを独占する（開発用。APPLaunch の横で SSH から
                           動かすとき。APPLaunch から起動したときは付けない）
  --backlight <値>         起こすときの明るさ（既定はランチャーの設定 brightness）
  --no-backlight           バックライトに触らない
  --sleep-after <秒>       無操作でこの秒数たったら画面を消す（0 で消さない）。既定は
                           ランチャーの設定 dark_time（無ければ 30 秒）。消えている間の
                           打鍵は画面を起こすだけで、アプリには渡さない
  --version / --help
";

enum Mode {
    KeyTest,
    FontTest,
    Bench(BenchKind),
    EdBench(EdBenchKind),
}

struct Args {
    mode: Option<Mode>,
    opts: Options,
    render_test: Option<PathBuf>,
    keymap_from_log: Option<PathBuf>,
    log: PathBuf,
    guide: bool,
    auto: Option<Duration>,
    page: usize,
    frames: u32,
    idle: Duration,
    files: Vec<PathBuf>,
    root: Option<PathBuf>,
    backend: DocBackend,
    /// コードのフォントの場所（`None` なら実行ファイルの隣。`Some(None)` なら使わない）。
    font_dir: Option<Option<PathBuf>>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        mode: None,
        opts: Options::default(),
        render_test: None,
        keymap_from_log: None,
        log: "/tmp/zcode-keytest.jsonl".into(),
        guide: false,
        auto: None,
        page: 1,
        frames: 150,
        idle: Duration::from_secs(120),
        files: Vec::new(),
        root: None,
        backend: DocBackend::Buffer,
        font_dir: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} に値が要る"));
        let num = |name: &str, v: String| v.parse::<u64>().map_err(|_| format!("{name}: 数でない: {v}"));
        match arg.as_str() {
            "--keytest" => a.mode = Some(Mode::KeyTest),
            "--fonttest" => a.mode = Some(Mode::FontTest),
            "--bench" => {
                let v = val("--bench")?;
                a.mode = Some(match EdBenchKind::parse(&v) {
                    Some(k) => Mode::EdBench(k),
                    None => Mode::Bench(BenchKind::parse(&v).ok_or_else(|| format!("--bench: 種類が分からない: {v}"))?),
                });
            }
            "--root" => a.root = Some(val("--root")?.into()),
            "--simple-buffer" => a.backend = DocBackend::Simple,
            "--font-dir" => a.font_dir = Some(Some(val("--font-dir")?.into())),
            "--no-bundled-font" => a.font_dir = Some(None),
            "--guide" => a.guide = true,
            "--log" => a.log = val("--log")?.into(),
            "--bdf" => a.opts.fonts.bdf = Some(val("--bdf")?.into()),
            "--auto" => a.auto = Some(Duration::from_millis(num("--auto", val("--auto")?)?)),
            "--page" => a.page = num("--page", val("--page")?)?.max(1) as usize,
            "--frames" => a.frames = num("--frames", val("--frames")?)? as u32,
            "--idle" => a.idle = Duration::from_secs(num("--idle", val("--idle")?)?.max(1)),
            "--render-test" => a.render_test = Some(val("--render-test")?.into()),
            "--keymap-from-log" => a.keymap_from_log = Some(val("--keymap-from-log")?.into()),
            "--stats" => a.opts.print_stats = true,
            "--watch-fb" => a.opts.watch_fb = Some(Duration::from_millis(num("--watch-fb", val("--watch-fb")?)?.max(10))),
            "--fb" => a.opts.fb = val("--fb")?.into(),
            "--kbd" => a.opts.kbd = Some(val("--kbd")?.into()),
            "--inline-raster" => a.opts.inline_raster = true,
            "--grab" => a.opts.grab = true,
            "--backlight" => a.opts.backlight = Some(num("--backlight", val("--backlight")?)? as u32),
            "--no-backlight" => a.opts.no_backlight = true,
            "--sleep-after" => {
                let s = num("--sleep-after", val("--sleep-after")?)?;
                a.opts.sleep_after = Some((s > 0).then(|| Duration::from_secs(s)));
            }
            "--version" => {
                println!("zcode {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            other if other.starts_with("--") => return Err(format!("分からない引数: {other}")),
            file => a.files.push(file.into()),
        }
    }
    Ok(a)
}

/// `PATH` の先頭に `<home>/.local/bin` を足した値。もう入っていれば `None`（変えない）。
///
/// 実行環境（`llrt`・`tsc`）と `zc-remote` は `~/.local/bin` に入れる
/// （`tools/install-runtimes.sh`。docs/stage3-design.md §10.3）。APPLaunch から起動したときの
/// `PATH` にはそこが入っていないので、F5 の `run`・保存時の `check`・`zc-remote serve` の
/// 起動が「コマンドが無い」にならないように、起動時に足す。
fn path_with_local_bin(path: Option<&OsStr>, home: Option<&OsStr>) -> Option<OsString> {
    let home = home.filter(|h| !h.is_empty())?;
    let bin = Path::new(home).join(".local/bin");
    let old: Vec<PathBuf> = path.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    if old.contains(&bin) {
        return None;
    }
    std::env::join_paths(std::iter::once(bin).chain(old)).ok()
}

fn real_main(t_start_us: u64) -> Result<(), String> {
    let mut stats = zcode::stats::Stats::new(t_start_us);
    stats.mark("main");
    // スレッドを作る前に変える（環境変数の書き換えは、ほかのスレッドが読んでいると危ない）
    if let Some(path) = path_with_local_bin(std::env::var_os("PATH").as_deref(), std::env::var_os("HOME").as_deref()) {
        std::env::set_var("PATH", path);
    }
    let mut a = parse_args()?;
    stats.mark("args parsed");
    // エディタは同梱のフォントがあればそれで描く（zcode::fonts）
    let code_line_h = match (&a.mode, a.font_dir.clone().unwrap_or_else(zcode::fonts::default_dir)) {
        (None, Some(dir)) => zcode::fonts::apply(&mut a.opts.fonts, &dir),
        _ => None,
    };
    if let Some(path) = &a.keymap_from_log {
        let log = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        print!("{}", keylog::keymap_lines(&keylog::parse_guide(&log), &Keymap::builtin()));
        return Ok(());
    }
    let fonts: &FontConfig = &a.opts.fonts;
    let rendering = a.render_test.is_some();
    let mut screen: Box<dyn Screen> = match a.mode {
        None => {
            let root = a.root.clone().unwrap_or_else(EditorConfig::default_root);
            let cfg = EditorConfig { backend: a.backend, root, files: a.files.clone() };
            let mut ed = EditorScreen::new(cfg);
            if let Some(h) = code_line_h {
                ed.set_code_font(zc_ui::layout::CODE_PX, h);
            }
            if rendering {
                // 画像に描くだけのときは、その場で読む（ふつうは batch スレッドが読む）
                for f in &a.files {
                    let doc = a.backend.load(f, &CancelToken::never()).map_err(|e| format!("{}: {e}", f.display()))?;
                    let title = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ed.open_doc(Some(f.clone()), &title, doc, None);
                }
            }
            Box::new(ed)
        }
        Some(Mode::EdBench(kind)) => Box::new(EditorBench::new(kind, a.frames)),
        Some(Mode::KeyTest) => {
            let log = (!rendering).then_some(a.log.as_path());
            let mut kt = KeyTest::new(Keymap::builtin(), log, a.guide, a.idle);
            if rendering {
                kt.demo();
            }
            Box::new(kt)
        }
        Some(Mode::FontTest) => {
            let has_bitmap = fonts.bdf.as_ref().is_some_and(|p| p.exists());
            let mut ft = FontTest::new(a.auto, a.idle, has_bitmap);
            ft.set_page(a.page - 1);
            Box::new(ft)
        }
        Some(Mode::Bench(kind)) => Box::new(Bench::new(kind, a.frames)),
    };
    stats.mark("screen built");
    if let Some(out) = &a.render_test {
        let canvas = render_screen(screen.as_mut(), fonts).map_err(|e| e.to_string())?;
        zc_display::png::write_png(&canvas, out).map_err(|e| format!("{}: {e}", out.display()))?;
        println!("{}", out.display());
        return Ok(());
    }
    let stats = app::run(&a.opts, screen.as_mut(), stats).map_err(|e| e.to_string())?;
    if a.opts.print_stats {
        eprint!("{}", stats.report());
    }
    Ok(())
}

fn main() -> ExitCode {
    let t_start_us = now_us();
    match real_main(t_start_us) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zcode: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_bin_goes_first_once() {
        let f = |path: Option<&str>, home: Option<&str>| path_with_local_bin(path.map(OsStr::new), home.map(OsStr::new)).map(|p| p.into_string().unwrap());
        assert_eq!(f(Some("/usr/bin:/bin"), Some("/home/u")).as_deref(), Some("/home/u/.local/bin:/usr/bin:/bin"));
        // もう入っていれば変えない（どの位置でも）
        assert_eq!(f(Some("/usr/bin:/home/u/.local/bin"), Some("/home/u")), None);
        // PATH が無い・HOME が無い
        assert_eq!(f(None, Some("/home/u")).as_deref(), Some("/home/u/.local/bin"));
        assert_eq!(f(Some("/usr/bin"), None), None);
        assert_eq!(f(Some("/usr/bin"), Some("")), None);
    }
}
