//! キー確認モードのログ（JSON Lines）から、キー配列（`keymap/*.toml`）に入れる行を作る
//! （`zcode --keymap-from-log <ログ>`）。
//!
//! 使うのは `--guide` の結果の行だけ:
//!
//! ```text
//! {"type":"guide","symbol":"&","code":71,"scan":19,"mods":["sym"],"layer":"sym","pos":[1,3]}
//! {"type":"guide","symbol":"~","code":null}          ← その刻印は無い
//! {"type":"guide_undo","symbol":"&"}                 ← 直前の答えを取り消した
//! ```
//!
//! ログは自分で書いた決まった形なので、JSON の読み取りは必要な分だけの簡単なもの。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use zc_input::Keymap;

/// `"key":` の後ろの値（文字列なら中身、それ以外は生の表記）を取り出す。
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let rest = &line[line.find(&pat)? + pat.len()..];
    if let Some(body) = rest.strip_prefix('"') {
        let mut esc = false;
        for (i, c) in body.char_indices() {
            match c {
                '\\' if !esc => esc = true,
                '"' if !esc => return Some(&body[..i]),
                _ => esc = false,
            }
        }
        None
    } else {
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(&rest[..end])
    }
}

fn unescape(s: &str) -> String {
    let mut o = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            if let Some(n) = it.next() {
                o.push(n);
            }
        } else {
            o.push(c);
        }
    }
    o
}

/// `--guide` の 1 つの答え。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuideAnswer {
    pub symbol: char,
    /// 押されたキーコード（その刻印が無ければ `None`）。
    pub code: Option<u16>,
    /// Shift を押していたか。
    pub shift: bool,
}

/// ログから `--guide` の答えを取り出す（取り消しを反映する。同じ刻印に 2 回答えて
/// いれば後の方を採る）。
pub fn parse_guide(log: &str) -> Vec<GuideAnswer> {
    let mut out: Vec<GuideAnswer> = Vec::new();
    for line in log.lines() {
        match field(line, "type") {
            Some("guide") => {
                let Some(symbol) = field(line, "symbol").map(unescape).and_then(|s| s.chars().next()) else { continue };
                let code = field(line, "code").and_then(|c| c.parse().ok());
                let shift = line.find("\"mods\":").is_some_and(|i| line[i..].split(']').next().is_some_and(|m| m.contains("\"shift\"")));
                out.retain(|a| a.symbol != symbol);
                out.push(GuideAnswer { symbol, code, shift });
            }
            Some("guide_undo") => {
                if let Some(symbol) = field(line, "symbol").map(unescape).and_then(|s| s.chars().next()) {
                    out.retain(|a| a.symbol != symbol);
                }
            }
            _ => {}
        }
    }
    out
}

fn toml_str(c: char) -> String {
    match c {
        '"' => "\"\\\"\"".into(),
        '\\' => "\"\\\\\"".into(),
        c => format!("\"{c}\""),
    }
}

/// 答えから、キー配列に入れる行と、気づいた点の報告を作る。
pub fn keymap_lines(answers: &[GuideAnswer], keymap: &Keymap) -> String {
    let mut o = String::new();
    // キーコード → (Shift なしの刻印, Shift ありの刻印)
    let mut by_code: BTreeMap<u16, (Option<char>, Option<char>)> = BTreeMap::new();
    let mut missing = vec![];
    let mut notes = vec![];
    for a in answers {
        let Some(code) = a.code else {
            missing.push(a.symbol);
            continue;
        };
        let slot = by_code.entry(code).or_default();
        let dst = if a.shift { &mut slot.1 } else { &mut slot.0 };
        if let Some(prev) = dst.replace(a.symbol) {
            notes.push(format!("code {code}: 刻印 {prev:?} と {:?} が同じキーになった（押し間違い？）", a.symbol));
        }
    }
    let _ = writeln!(o, "# キー確認モードの結果から作った行。keymap/cardputer-zero.toml の該当行と置き換える。");
    for (code, (plain, shifted)) in &by_code {
        let def = keymap.get(*code);
        let layer = def.map_or("sym", |d| d.layer.as_str());
        let mut line = format!("  {{ code = {code}");
        match (plain, shifted, def) {
            // 通常レイヤーのキーに Shift で記号が出る場合は、元のキーに shift を足す
            (None, Some(s), Some(d)) if !d.provisional => {
                let _ = write!(line, ", key = {}, shift = {}", toml_str(d.text.unwrap_or('?')), toml_str(*s));
            }
            (p, s, _) => {
                if let Some(p) = p.or(*s) {
                    let _ = write!(line, ", key = {}", toml_str(p));
                }
                if let (Some(_), Some(s)) = (p, s) {
                    let _ = write!(line, ", shift = {}", toml_str(*s));
                }
            }
        }
        if layer != "normal" {
            let _ = write!(line, ", layer = \"{layer}\"");
        }
        if let Some((r, c)) = def.and_then(|d| d.pos) {
            let _ = write!(line, ", pos = [{r}, {c}]");
        }
        line.push_str(" },");
        if def.is_none() {
            line.push_str("  # 表に無かったキーコード");
        } else if let Some(d) = def.filter(|d| !d.provisional && d.layer.as_str() != "normal") {
            let _ = write!(line, "  # 既存の定義: {}", d.key);
        }
        let _ = writeln!(o, "{line}");
    }
    if !missing.is_empty() {
        let s: String = missing.iter().collect();
        let _ = writeln!(o, "# 刻印が無いと答えた記号: {s}");
    }
    // まだ未確定のまま残るキー
    let mut left: Vec<u16> = keymap.defs().filter(|d| d.provisional && !by_code.contains_key(&d.code)).map(|d| d.code).collect();
    left.sort_unstable();
    if !left.is_empty() {
        let _ = writeln!(o, "# 答えに出てこなかった未確定のキーコード: {left:?}");
    }
    for n in notes {
        let _ = writeln!(o, "# 注意: {n}");
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = r##"{"type":"start","version":1,"guide":true,"t_us":1}
{"type":"guide","symbol":"!","code":71,"scan":19,"mods":["sym"],"layer":"sym","pos":[1,3]}
{"type":"ev","seq":3,"ev_type":1,"code":71,"value":1,"name":"KEY_KP7","t_kernel_us":5,"t_read_us":6,"t_fsync_us":null,"scan":19,"mods":["sym"],"down":[71],"map":{"key":"Key#71","text":null,"layer":"sym","pos":[1,3],"physical":"r","provisional":true,"modifier":false},"repeats":0}
{"type":"guide","symbol":"\"","code":72,"scan":20,"mods":["sym"],"layer":"sym","pos":[1,4]}
{"type":"guide","symbol":"#","code":73,"scan":21,"mods":[],"layer":"sym","pos":[1,5]}
{"type":"guide_undo","symbol":"#"}
{"type":"guide","symbol":"#","code":74,"scan":22,"mods":[],"layer":"sym","pos":[1,6]}
{"type":"guide","symbol":"$","code":null}
{"type":"guide","symbol":"\\","code":43,"scan":4,"mods":[],"layer":"sym","pos":[0,4]}
{"type":"guide","symbol":"_","code":3,"scan":0,"mods":["shift"],"layer":"normal","pos":[0,0]}
{"type":"guide_end"}
"##;

    #[test]
    fn guide_answers_with_undo_and_escapes() {
        let a = parse_guide(LOG);
        let got: Vec<(char, Option<u16>, bool)> = a.iter().map(|a| (a.symbol, a.code, a.shift)).collect();
        assert_eq!(
            got,
            vec![('!', Some(71), false), ('"', Some(72), false), ('#', Some(74), false), ('$', None, false), ('\\', Some(43), false), ('_', Some(3), true)]
        );
    }

    #[test]
    fn output_is_valid_keymap_entries() {
        let km = Keymap::builtin();
        let out = keymap_lines(&parse_guide(LOG), &km);
        assert!(out.contains(r#"{ code = 71, key = "!", layer = "sym", pos = [1, 3] },"#), "{out}");
        assert!(out.contains(r#"{ code = 72, key = "\"", layer = "sym", pos = [1, 4] },"#), "{out}");
        assert!(out.contains(r#"{ code = 43, key = "\\", layer = "sym", pos = [0, 4] },"#), "{out}");
        assert!(out.contains(r#"{ code = 3, key = "2", shift = "_", pos = [0, 0] },"#), "{out}");
        assert!(out.contains("刻印が無いと答えた記号: $"));
        assert!(out.contains("未確定のキーコード"));
        // 出力の行は、そのままキー配列として読める
        let entries: String = out.lines().filter(|l| l.starts_with("  {")).collect::<Vec<_>>().join("\n");
        let src = format!("version = 1\nmodifiers = {{ shift = [42] }}\nkeys = [\n{entries}\n]\n");
        let parsed = Keymap::parse(&src).unwrap();
        assert_eq!(parsed.translate(71, zc_core::Mods::NONE).text, Some('!'));
        assert_eq!(parsed.translate(3, zc_core::Mods::SHIFT).text, Some('_'));
    }
}
