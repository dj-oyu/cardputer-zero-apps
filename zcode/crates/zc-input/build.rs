//! キー配列（`zcode/keymap/cardputer-zero.toml`）を、ビルド時に Rust の表に変換する。
//!
//! 実機の実行ファイルには TOML のパーサを入れない（design.md §8.1: 読むだけのデータは
//! 実行できる形にして埋め込む）。ここでは形の変換だけを行い、意味の検査は
//! `Keymap::from_table` に任せる（`cargo test` が埋め込みの表を検査する）。

use std::fmt::Write as _;
use std::path::PathBuf;

include!("src/schema.rs");

fn main() {
    let src_path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../keymap/cardputer-zero.toml");
    println!("cargo:rerun-if-changed={}", src_path.display());
    println!("cargo:rerun-if-changed=src/schema.rs");
    println!("cargo:rerun-if-changed=build.rs");
    let src = std::fs::read_to_string(&src_path).unwrap_or_else(|e| panic!("{}: {e}", src_path.display()));
    let raw: RawFile = toml::from_str(&src).unwrap_or_else(|e| panic!("{}: {e}", src_path.display()));

    let mut o = String::new();
    writeln!(o, "// build.rs が {} から作った。手で直さない。", src_path.file_name().unwrap().to_string_lossy()).unwrap();
    writeln!(o, "pub(crate) static BUILTIN: Table<'static> = Table {{").unwrap();
    writeln!(o, "    version: {},", raw.version).unwrap();
    writeln!(o, "    device: {:?},", raw.device).unwrap();
    writeln!(o, "    shift: &{:?},", raw.modifiers.shift).unwrap();
    writeln!(o, "    ctrl: &{:?},", raw.modifiers.ctrl).unwrap();
    writeln!(o, "    alt: &{:?},", raw.modifiers.alt).unwrap();
    writeln!(o, "    sym: &{:?},", raw.modifiers.sym).unwrap();
    writeln!(o, "    fn_: &{:?},", raw.modifiers.fn_).unwrap();
    writeln!(o, "    keys: &[").unwrap();
    for k in &raw.keys {
        writeln!(
            o,
            "        Entry {{ code: {}, key: {:?}, text: {:?}, shift: {:?}, layer: {:?}, pos: {:?}, provisional: {} }},",
            k.code,
            k.key,
            k.text,
            k.shift,
            k.layer,
            k.pos.map(|p| (p[0], p[1])),
            k.provisional
        )
        .unwrap();
    }
    writeln!(o, "    ],").unwrap();
    writeln!(o, "}};").unwrap();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("keymap_builtin.rs");
    std::fs::write(out, o).unwrap();
}
