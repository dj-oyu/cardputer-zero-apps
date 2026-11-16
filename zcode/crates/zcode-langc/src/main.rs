//! zcode-langc — zcode の言語定義の道具（PC 用）。
//!
//! ```text
//! zcode-langc import <file.sublime-syntax>      language.toml の下書きを標準出力へ（報告は標準エラー）
//! zcode-langc check  <language.toml>...         変換できるか確かめ、文脈ごとの DFA の大きさを出す
//! zcode-langc show   <language.toml> <file>     ファイルに色を付けて出す（端末の 24 ビット色）
//! ```

use std::process::ExitCode;
use zc_core::TokenKind;
use zc_highlight::compile::{compile_language, parse_language_toml, CompiledLanguage};

const USAGE: &str = "使い方:
  zcode-langc import <file.sublime-syntax>     language.toml の下書きを標準出力へ
  zcode-langc check  <language.toml>...        変換できるか確かめて大きさを出す
  zcode-langc show   <language.toml> <file>    ファイルに色を付けて表示する";

fn load(path: &str) -> Result<CompiledLanguage, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let file = parse_language_toml(&text).map_err(|e| format!("{path}: {e}"))?;
    let key = std::path::Path::new(path)
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "lang".into());
    compile_language(&key, &file, path).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(|s| s.as_str()) {
        Some("import") if args.len() == 2 => import(&args[1]),
        Some("check") if args.len() >= 2 => check(&args[1..]),
        Some("show") if args.len() == 3 => show(&args[1], &args[2]),
        _ => Err(USAGE.to_string()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn import(path: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let r = zcode_langc::import_sublime_syntax(&text)?;
    print!("{}", r.to_toml());
    eprintln!(
        "{path}: 文脈 {} 個、規則 {} 個。近い形にした {} 件、外した {} 件",
        r.file.contexts.len(),
        r.file.contexts.values().map(|v| v.len()).sum::<usize>(),
        r.report.warnings.len(),
        r.report.errors.len()
    );
    for e in &r.report.errors {
        eprintln!("  外した: {e}");
    }
    for w in &r.report.warnings {
        eprintln!("  近似: {w}");
    }
    Ok(())
}

fn check(paths: &[String]) -> Result<(), String> {
    let mut failed = false;
    for p in paths {
        match load(p) {
            Ok(c) => {
                println!("{p}: {}（表 {} バイト、followed_by {} バイト）", c.file.name, c.region.len(), c.head_bytes);
                for s in &c.contexts {
                    if s.dfa_bytes > 0 {
                        println!("  {:<20} 規則 {:>3}  DFA {:>7} バイト  文字の種類 {:>3}（幅 {}）", s.name, s.rules, s.dfa_bytes, s.alphabet, s.stride);
                    }
                }
                if c.region.len() > 100 * 1024 {
                    println!("  注意: 1 言語 100KB の目安を超えている（languages/README.md「表を小さく保つ」）");
                }
            }
            Err(e) => {
                eprintln!("{e}");
                failed = true;
            }
        }
    }
    if failed {
        Err("変換できないファイルがある".into())
    } else {
        Ok(())
    }
}

fn show(lang: &str, file: &str) -> Result<(), String> {
    let c = load(lang)?;
    let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    let lines: Vec<&str> = text.lines().collect();
    let spans = zc_highlight::tokenize_with_region(&c.region, &lines).ok_or("変換した表が読めない")?;
    let theme = zc_highlight::theme();
    for (l, sp) in lines.iter().zip(&spans) {
        let mut s = String::new();
        for x in sp {
            let st = theme.token(x.kind);
            let [r, g, b] = st.fg.to_rgb8();
            let piece = &l[x.start as usize..x.end as usize];
            if x.kind == TokenKind::Text {
                s.push_str(piece);
            } else {
                s.push_str(&format!("\x1b[38;2;{r};{g};{b}m{piece}\x1b[0m"));
            }
        }
        println!("{s}");
    }
    Ok(())
}
