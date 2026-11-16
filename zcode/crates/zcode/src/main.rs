//! zcode の入口。段階 1 では、キー確認・フォント比較・計測の 3 つの画面だけが動く。

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use zc_core::time::now_us;
use zc_font::FontConfig;
use zc_input::Keymap;
use zcode::app::{self, Options, Screen};
use zcode::bench::{Bench, BenchKind};
use zcode::fonttest::FontTest;
use zcode::keytest::KeyTest;
use zcode::{keylog, render_screen};

const HELP: &str = "\
zcode — CardputerZero 用コードエディタ（段階 1: 土台。エディタ本体はまだ無い）

使い方:
  zcode --keytest [--guide] [--log <パス>]   キー確認モード
  zcode --fonttest [--bdf <パス>] [--auto <ms>] [--page <n>]   フォント比較
  zcode --bench <line|line-status|scroll|full> [--frames <n>]  描画の計測
                （panic / hang は異常終了の後始末の試験）
  zcode --keymap-from-log <ログ>             キー確認のログからキー配列の行を作る

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
                a.mode = Some(Mode::Bench(BenchKind::parse(&v).ok_or_else(|| format!("--bench: 種類が分からない: {v}"))?));
            }
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
            other => return Err(format!("分からない引数: {other}")),
        }
    }
    Ok(a)
}

fn real_main(t_start_us: u64) -> Result<(), String> {
    let a = parse_args()?;
    if let Some(path) = &a.keymap_from_log {
        let log = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        print!("{}", keylog::keymap_lines(&keylog::parse_guide(&log), &Keymap::builtin()));
        return Ok(());
    }
    let Some(mode) = a.mode else {
        return Err("モードの指定が要る（--keytest / --fonttest / --bench。--help を参照）".into());
    };
    let fonts: &FontConfig = &a.opts.fonts;
    let rendering = a.render_test.is_some();
    let mut screen: Box<dyn Screen> = match mode {
        Mode::KeyTest => {
            let log = (!rendering).then_some(a.log.as_path());
            let mut kt = KeyTest::new(Keymap::builtin(), log, a.guide, a.idle);
            if rendering {
                kt.demo();
            }
            Box::new(kt)
        }
        Mode::FontTest => {
            let has_bitmap = fonts.bdf.as_ref().is_some_and(|p| p.exists());
            let mut ft = FontTest::new(a.auto, a.idle, has_bitmap);
            ft.set_page(a.page - 1);
            Box::new(ft)
        }
        Mode::Bench(kind) => Box::new(Bench::new(kind, a.frames)),
    };
    if let Some(out) = &a.render_test {
        let canvas = render_screen(screen.as_mut(), fonts).map_err(|e| e.to_string())?;
        zc_display::png::write_png(&canvas, out).map_err(|e| format!("{}: {e}", out.display()))?;
        println!("{}", out.display());
        return Ok(());
    }
    let stats = app::run(&a.opts, screen.as_mut(), t_start_us).map_err(|e| e.to_string())?;
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
