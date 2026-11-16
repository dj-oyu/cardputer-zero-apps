//! `zcode/languages/*/language.toml` と `languages/theme.toml` を、ビルド時に実行できる形へ
//! 変換する（design.md §7.2・§8.2・§12 #10）。
//!
//! - 各言語の文脈を多パターンの dense DFA にし、1 つのバイト列 `languages.bin` にまとめる
//!   （言語ごとに 4096 境界の連続した領域）。実行時は `from_bytes` で読むだけ
//! - 規則以外の設定（名前・拡張子・実行コマンドなど）と配色は Rust の表 `languages.rs` にする
//! - 実機の実行ファイルに TOML のパーサは入らない（ここで使うだけ）
//!
//! 規則が DFA にできないときは、ファイル名と規則の位置を出してビルドを止める。

use std::fmt::Write as _;
use std::path::PathBuf;

#[allow(dead_code)]
#[path = "src/format.rs"]
mod format;
#[allow(dead_code)]
#[path = "src/schema.rs"]
mod schema;
#[allow(dead_code)]
#[path = "src/compile.rs"]
mod compile;

/// 1 言語あたりの DFA の目安（design.md §7.2）。超えたら警告する。
const SIZE_BUDGET: usize = 100 * 1024;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let lang_dir = manifest.join("../../languages");
    println!("cargo:rerun-if-changed={}", lang_dir.display());
    for f in ["build.rs", "src/format.rs", "src/schema.rs", "src/compile.rs"] {
        println!("cargo:rerun-if-changed={f}");
    }

    let mut build_id = format::layout::fnv64(0, &format::layout::FORMAT_VERSION.to_le_bytes());
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&lang_dir)
        .unwrap_or_else(|e| panic!("{}: {e}", lang_dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("language.toml").is_file())
        .collect();
    dirs.sort();

    let mut langs = Vec::new();
    for d in &dirs {
        let key = d.file_name().unwrap().to_string_lossy().to_string();
        let path = d.join("language.toml");
        let origin = format!("languages/{key}/language.toml");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{origin}: {e}"));
        build_id = format::layout::fnv64(build_id, key.as_bytes());
        build_id = format::layout::fnv64(build_id, text.as_bytes());
        let file = compile::parse_language_toml(&text).unwrap_or_else(|e| fail(&format!("{origin}: {e}")));
        let c = compile::compile_language(&key, &file, &origin).unwrap_or_else(|e| fail(&e.to_string()));
        if c.region.len() > SIZE_BUDGET || std::env::var_os("ZC_HIGHLIGHT_SIZES").is_some() {
            let per: Vec<String> =
                c.contexts.iter().filter(|s| s.dfa_bytes > 0).map(|s| format!("{} {} ({}/{})", s.name, s.dfa_bytes, s.alphabet, s.stride)).collect();
            println!(
                "cargo:warning={origin}: 表 {} バイト（目安 {} KB）。文脈ごと: {}、followed_by {}",
                c.region.len(),
                SIZE_BUDGET / 1024,
                per.join(", "),
                c.head_bytes
            );
        }
        langs.push(c);
    }

    let theme_path = lang_dir.join("theme.toml");
    let theme_text = std::fs::read_to_string(&theme_path).unwrap_or_else(|e| panic!("{}: {e}", theme_path.display()));
    build_id = format::layout::fnv64(build_id, theme_text.as_bytes());
    let theme: schema::ThemeFile =
        toml::from_str(&theme_text).unwrap_or_else(|e| fail(&format!("languages/theme.toml: {e}")));

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let blob = compile::build_blob(&langs, build_id);
    std::fs::write(out.join("languages.bin"), &blob).unwrap();
    std::fs::write(out.join("languages.rs"), gen_rust(&langs, &theme, build_id, blob.len())).unwrap();
}

fn fail(msg: &str) -> ! {
    // panic の文言は長いバックトレースに埋もれるので、先に分かりやすく出す
    eprintln!("\nerror: {msg}\n");
    println!("cargo:warning={msg}");
    std::process::exit(1);
}

fn opt(s: &Option<String>) -> String {
    match s {
        Some(v) => format!("Some({v:?})"),
        None => "None".into(),
    }
}

fn gen_rust(langs: &[compile::CompiledLanguage], theme: &schema::ThemeFile, build_id: u64, blob_len: usize) -> String {
    let mut o = String::new();
    writeln!(o, "// build.rs が languages/ から作った。手で直さない。").unwrap();
    writeln!(o, "pub(crate) const BUILD_ID: u64 = {build_id:#018x};").unwrap();
    writeln!(o, "pub(crate) const BLOB_LEN: usize = {blob_len};").unwrap();
    writeln!(o, "pub(crate) static INFOS: [LanguageInfo; {}] = [", langs.len()).unwrap();
    for l in langs {
        let f = &l.file;
        let exts: Vec<String> = f.extensions.iter().map(|e| e.trim_start_matches('.').to_ascii_lowercase()).collect();
        writeln!(o, "    LanguageInfo {{").unwrap();
        writeln!(o, "        key: {:?},", l.key).unwrap();
        writeln!(o, "        name: {:?},", f.name).unwrap();
        writeln!(o, "        extensions: &{:?},", exts).unwrap();
        writeln!(o, "        filenames: &{:?},", f.filenames).unwrap();
        writeln!(o, "        line_comment: {},", opt(&f.line_comment)).unwrap();
        writeln!(o, "        indent: {:?},", f.indent.clone().unwrap_or_else(|| "    ".into())).unwrap();
        writeln!(o, "        run: {},", opt(&f.run)).unwrap();
        writeln!(o, "        check: {},", opt(&f.check)).unwrap();
        writeln!(o, "        check_pattern: {},", opt(&f.check_pattern)).unwrap();
        // 補完のキーワード: keywords と [words] の語を合わせたもの（重複なし、書いた順 → 語の表の順）
        let mut kw: Vec<&str> = Vec::new();
        for w in f.keywords.iter().chain(f.words.values().flatten()) {
            if !kw.contains(&w.as_str()) {
                kw.push(w);
            }
        }
        writeln!(o, "        keywords: &{kw:?},").unwrap();
        let indent = f.indent.clone().unwrap_or_else(|| "    ".into());
        let indent_w = if indent.starts_with('\t') { 1 } else { indent.chars().count().max(1) as u32 };
        match &f.outline {
            Some(ol) => {
                let items: Vec<String> = ol
                    .items
                    .iter()
                    .map(|(k, kind)| {
                        let v = match kind.as_str() {
                            "function" => "Function",
                            "class" => "Class",
                            _ => "Heading",
                        };
                        format!("({k:?}, OutlineKind::{v})")
                    })
                    .collect();
                let hc = match ol.heading_char.as_deref().and_then(|h| h.chars().next()) {
                    Some(c) => format!("Some({c:?})"),
                    None => "None".into(),
                };
                writeln!(
                    o,
                    "        outline: Some(OutlineSpec {{ items: &[{}], modifiers: &{:?}, heading_char: {hc}, indent_width: {} }}),",
                    items.join(", "),
                    ol.modifiers,
                    ol.indent_width.unwrap_or(indent_w)
                )
                .unwrap();
            }
            None => writeln!(o, "        outline: None,").unwrap(),
        }
        writeln!(o, "    }},").unwrap();
    }
    writeln!(o, "];").unwrap();
    // 文脈の名前と大きさ（報告・デバッグ用）
    writeln!(o, "pub(crate) static CONTEXT_NAMES: [&[&str]; {}] = [", langs.len()).unwrap();
    for l in langs {
        let names: Vec<&str> = l.contexts.iter().map(|c| c.name.as_str()).collect();
        writeln!(o, "    &{names:?},").unwrap();
    }
    writeln!(o, "];").unwrap();

    // 配色: (種類, 文字色, 背景色, 装飾)
    writeln!(o, "pub(crate) static THEME_ENTRIES: &[(u8, u32, Option<u32>, u8)] = &[").unwrap();
    for (k, e) in &theme.tokens {
        let kind = zc_core::TokenKind::from_name(k)
            .unwrap_or_else(|| fail(&format!("languages/theme.toml: 知らないトークンの種類 \"{k}\"")));
        let st = match e {
            schema::ThemeEntry::Color(c) => schema::ThemeStyle { fg: c.clone(), ..Default::default() },
            schema::ThemeEntry::Style(s) => s.clone(),
        };
        let col = |s: &str| {
            schema::parse_hex_color(s)
                .unwrap_or_else(|| fail(&format!("languages/theme.toml: {k}: 色は \"#rrggbb\" で書く（{s}）")))
        };
        let fg = col(&st.fg);
        let bg = st.bg.as_deref().map(col);
        let attrs = (st.bold as u8) | (st.italic as u8) << 1 | (st.underline as u8) << 2;
        writeln!(o, "    ({}, {fg:#08x}, {bg:?}, {attrs}),", kind as u8).unwrap();
    }
    writeln!(o, "];").unwrap();
    writeln!(o, "pub(crate) static BLOB: &AlignedBlob<[u8]> = &AlignedBlob(*include_bytes!(concat!(env!(\"OUT_DIR\"), \"/languages.bin\")));").unwrap();
    o
}
