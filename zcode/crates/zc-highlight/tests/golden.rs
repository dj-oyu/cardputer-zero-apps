//! 各言語の見本（`languages/<言語>/sample.*`）をハイライトして、正解
//! （`tests/golden/highlight/<言語>.txt`）と比べる。言語のフォルダは全部自動で見つけるので、
//! 言語を足すと見本も自動でテストされる。
//!
//! - 正解が無ければ作って失敗する（中身を見てからコミットする）
//! - 違えば隣に `<言語>.actual.txt` を出して失敗する
//! - `ZCODE_UPDATE_GOLDEN=1` なら正解を書き直す
//!
//! 正解の形: 見本の各行の下に、色の付いたトークンを「種類 "文字列"」で並べる（地の文は省く）。

use std::path::{Path, PathBuf};
use zc_core::{LineSource, TokenKind};
use zc_highlight::{Highlighter, Registry};

struct Lines(Vec<String>);
impl LineSource for Lines {
    fn len_lines(&self) -> u32 {
        self.0.len().max(1) as u32
    }
    fn line(&self, line: u32, out: &mut String) {
        out.clear();
        if let Some(s) = self.0.get(line as usize) {
            out.push_str(s);
        }
    }
}

fn zcode_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// 見本をハイライトして、正解の形の文字列にする。
fn render(key: &str, sample: &Path) -> String {
    let text = std::fs::read_to_string(sample).unwrap();
    let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let reg = Registry::builtin();
    let id = reg.detect(sample, lines.first().map(|s| s.as_str()).unwrap_or(""));
    assert_eq!(id, reg.by_key(key), "{}: 見本のファイル名から言語 {key} を判定できない", sample.display());
    let src = Lines(lines);
    let mut h = Highlighter::new(id);
    assert!(h.update(&src, u32::MAX).complete);
    let mut out = format!("# {} ({}) — zc-highlight の見本の正解。ZCODE_UPDATE_GOLDEN=1 で作り直す\n", reg.info(id.unwrap()).name, key);
    let mut spans = Vec::new();
    for (i, l) in src.0.iter().enumerate() {
        h.spans(i as u32, l, &mut spans);
        out.push_str(&format!("{:4}| {l}\n", i + 1));
        let toks: Vec<String> = spans
            .iter()
            .filter(|s| s.kind != TokenKind::Text && !l[s.start as usize..s.end as usize].trim().is_empty())
            .map(|s| format!("{} {:?}", s.kind.name(), &l[s.start as usize..s.end as usize]))
            .collect();
        if !toks.is_empty() {
            out.push_str(&format!("    : {}\n", toks.join(" · ")));
        }
    }
    out
}

#[test]
fn every_language_sample_matches_golden() {
    let lang_dir = zcode_dir().join("languages");
    let golden_dir = zcode_dir().join("tests/golden/highlight");
    let update = std::env::var_os("ZCODE_UPDATE_GOLDEN").is_some();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&lang_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("language.toml").is_file())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty());
    let mut failures = Vec::new();
    for d in &dirs {
        let key = d.file_name().unwrap().to_string_lossy().to_string();
        let sample = std::fs::read_dir(d)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.file_stem().is_some_and(|s| s == "sample"));
        let Some(sample) = sample else {
            failures.push(format!("languages/{key}: 見本（sample.*）が無い"));
            continue;
        };
        let got = render(&key, &sample);
        let golden = golden_dir.join(format!("{key}.txt"));
        let actual = golden_dir.join(format!("{key}.actual.txt"));
        match std::fs::read_to_string(&golden) {
            Ok(want) if want == got => {
                let _ = std::fs::remove_file(&actual);
            }
            _ if update => {
                std::fs::create_dir_all(&golden_dir).unwrap();
                std::fs::write(&golden, &got).unwrap();
                let _ = std::fs::remove_file(&actual);
            }
            Ok(want) => {
                std::fs::write(&actual, &got).unwrap();
                let first = want.lines().zip(got.lines()).position(|(a, b)| a != b).unwrap_or(0);
                failures.push(format!(
                    "{key}: 正解と違う（{} 行目から）。{} を見て、正しければ ZCODE_UPDATE_GOLDEN=1 で更新\n  正解: {}\n  今回: {}",
                    first + 1,
                    actual.display(),
                    want.lines().nth(first).unwrap_or(""),
                    got.lines().nth(first).unwrap_or("")
                ));
            }
            Err(_) => {
                std::fs::create_dir_all(&golden_dir).unwrap();
                std::fs::write(&golden, &got).unwrap();
                failures.push(format!("{key}: 正解が無かったので作った（{}）。中身を見てからコミットする", golden.display()));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// 全部の言語が埋め込まれていること（languages/ のフォルダと Registry が一致）。
#[test]
fn registry_has_every_language_dir() {
    let lang_dir = zcode_dir().join("languages");
    let reg = Registry::builtin();
    for e in std::fs::read_dir(&lang_dir).unwrap().flatten() {
        if e.path().join("language.toml").is_file() {
            let key = e.file_name().to_string_lossy().to_string();
            assert!(reg.by_key(&key).is_some(), "{key} が埋め込まれていない");
        }
    }
}
