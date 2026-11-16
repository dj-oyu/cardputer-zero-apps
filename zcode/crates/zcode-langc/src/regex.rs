//! Oniguruma（Sublime Text）の正規表現 → zcode の規則で使える形（DFA にできる形）。
//!
//! 直せるものは直し、意味が変わるものは警告、直せないものはエラーにする。

/// 変換の結果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Converted {
    /// 規則の `match`。
    pub pattern: String,
    /// 末尾の先読み `(?=..)` から作った `followed_by`。
    pub followed_by: Option<String>,
    /// 意味が少し変わった点（近い形にしたもの）。
    pub warnings: Vec<String>,
}

/// `pat` を変換する。直せない書き方があれば `Err(理由)`。
pub fn convert(pat: &str) -> Result<Converted, String> {
    let mut warnings = Vec::new();
    let mut p = pat.to_string();
    // 末尾の先読みは followed_by にする（意味は同じ）
    let mut followed_by = None;
    if let Some((head, la)) = split_trailing_lookahead(&p) {
        let la = rewrite(&la, &mut warnings)?;
        followed_by = Some(la);
        p = head;
    }
    let p = rewrite(&p, &mut warnings)?;
    Ok(Converted { pattern: p, followed_by, warnings })
}

/// 括弧の対応を見ながら、末尾がちょうど最上位の `(?=..)` で終わっていれば分ける。
fn split_trailing_lookahead(p: &str) -> Option<(String, String)> {
    let b = p.as_bytes();
    let groups = top_level_groups(b)?;
    let &(s, e) = groups.last()?;
    if e + 1 != b.len() || !p[s..].starts_with("(?=") {
        return None;
    }
    // 最上位の | があれば、先読みは最後の選択肢にしか掛からないので分けない
    if has_top_level_alt(b) {
        return None;
    }
    Some((p[..s].to_string(), p[s + 3..e].to_string()))
}

/// 最上位の括弧の（開き, 閉じ）の位置。括弧が合わなければ `None`。
fn top_level_groups(b: &[u8]) -> Option<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut i = 0;
    let mut class = 0usize;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'[' => class += 1,
            b']' if class > 0 => class -= 1,
            b'(' if class == 0 => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            b')' if class == 0 => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    out.push((start, i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    (depth == 0).then_some(out)
}

fn has_top_level_alt(b: &[u8]) -> bool {
    let (mut depth, mut class, mut i) = (0i32, 0i32, 0);
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'[' => class += 1,
            b']' if class > 0 => class -= 1,
            b'(' if class == 0 => depth += 1,
            b')' if class == 0 => depth -= 1,
            b'|' if class == 0 && depth == 0 => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// 1 文字ずつ見て書き換える。
fn rewrite(p: &str, warnings: &mut Vec<String>) -> Result<String, String> {
    let b = p.as_bytes();
    let mut out = String::with_capacity(p.len());
    let mut i = 0;
    let mut class = 0usize;
    while i < b.len() {
        let c = b[i];
        if c == b'\\' && i + 1 < b.len() {
            let n = b[i + 1];
            match n {
                b'1'..=b'9' if class == 0 => return Err(format!("後方参照 \\{}", n as char)),
                b'k' if b.get(i + 2) == Some(&b'<') => return Err("名前つきの後方参照 \\k<..>".into()),
                b'G' => return Err("\\G（前の一致の終わり）".into()),
                b'h' => {
                    out.push_str(if class > 0 { "0-9a-fA-F" } else { "[0-9a-fA-F]" });
                    i += 2;
                    continue;
                }
                b'H' if class == 0 => {
                    out.push_str("[^0-9a-fA-F]");
                    i += 2;
                    continue;
                }
                b'Z' => {
                    out.push('$');
                    i += 2;
                    continue;
                }
                b'p' | b'P' => {
                    // \p{L} \pL \p{Alpha} などは「英字 + 非 ASCII」に寄せる
                    let mut j = i + 2;
                    if b.get(j) == Some(&b'{') {
                        while j < b.len() && b[j] != b'}' {
                            j += 1;
                        }
                    }
                    let name = &p[i + 2..(j + 1).min(b.len())];
                    if n == b'P' {
                        return Err(format!("Unicode の文字クラスの否定 \\P{name}"));
                    }
                    warnings.push(format!("\\p{name} を英字と非 ASCII（[A-Za-z\\x80-\\xFF]）にした"));
                    out.push_str(if class > 0 { r"A-Za-z\x80-\xFF" } else { r"[A-Za-z\x80-\xFF]" });
                    i = (j + 1).min(b.len());
                    continue;
                }
                _ => {}
            }
            // UTF-8 の途中で切らないように、次の文字ごと写す
            let len = utf8_len(n);
            out.push_str(&p[i..(i + 1 + len).min(b.len())]);
            i += 1 + len;
            continue;
        }
        if class == 0 && c == b'(' && b.get(i + 1) == Some(&b'?') {
            let rest = &p[i + 2..];
            let look = if rest.starts_with('=') || rest.starts_with('!') {
                Some(if rest.starts_with('=') { "先読み (?=..)" } else { "否定の先読み (?!..)" })
            } else if rest.starts_with("<=") || rest.starts_with("<!") {
                Some(if rest.starts_with("<=") { "後読み (?<=..)" } else { "否定の後読み (?<!..)" })
            } else {
                None
            };
            if let Some(what) = look {
                let end = group_end(b, i).ok_or("括弧が閉じていない")?;
                warnings.push(format!("{what} を外した: {}", &p[i..=end]));
                i = end + 1;
                continue;
            }
            if rest.starts_with('>') {
                warnings.push("アトミックグループ (?>..) を (?:..) にした".into());
                out.push_str("(?:");
                i += 3;
                continue;
            }
        }
        match c {
            // 文字クラスの入れ子（Oniguruma）と POSIX の [:alpha:] を数える
            b'[' => class += 1,
            b']' if class > 0 => class -= 1,
            b'+' if class == 0 && i > 0 && matches!(b[i - 1], b'*' | b'+' | b'?' | b'}') && !escaped(b, i - 1) => {
                warnings.push("強欲な繰り返し（*+ など）をふつうの繰り返しにした".into());
                i += 1;
                continue;
            }
            _ => {}
        }
        let len = utf8_len(c);
        out.push_str(&p[i..(i + len).min(b.len())]);
        i += len;
    }
    Ok(out)
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

fn escaped(b: &[u8], i: usize) -> bool {
    let mut n = 0;
    let mut j = i;
    while j > 0 && b[j - 1] == b'\\' {
        n += 1;
        j -= 1;
    }
    n % 2 == 1
}

/// `b[open]` の `(` に対応する `)` の位置。
fn group_end(b: &[u8], open: usize) -> Option<usize> {
    let (mut depth, mut class, mut i) = (0usize, 0usize, open);
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'[' => class += 1,
            b']' if class > 0 => class -= 1,
            b'(' if class == 0 => depth += 1,
            b')' if class == 0 => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_lookahead_becomes_followed_by() {
        let c = convert(r"\b[a-z_]+(?=\s*\()").unwrap();
        assert_eq!(c.pattern, r"\b[a-z_]+");
        assert_eq!(c.followed_by.as_deref(), Some(r"\s*\("));
        assert!(c.warnings.is_empty());
        // 先読みだけ → 空の match + followed_by
        let c = convert(r"(?=\S)").unwrap();
        assert_eq!(c.pattern, "");
        assert_eq!(c.followed_by.as_deref(), Some(r"\S"));
        // 最上位の | があるときは分けない（警告つきで外す）
        let c = convert(r"a|b(?=c)").unwrap();
        assert_eq!(c.pattern, "a|b");
        assert!(c.followed_by.is_none());
        assert_eq!(c.warnings.len(), 1);
    }

    #[test]
    fn rewrites_and_errors() {
        let c = convert(r"(?<!\.)\h+\b(?!x)").unwrap();
        assert_eq!(c.pattern, r"[0-9a-fA-F]+\b");
        assert_eq!(c.warnings.len(), 2);
        let c = convert(r"(?>a|b)c*+[\h_]").unwrap();
        assert_eq!(c.pattern, r"(?:a|b)c*[0-9a-fA-F_]");
        let c = convert(r"\p{L}[\p{L}\d]*").unwrap();
        assert_eq!(c.pattern, r"[A-Za-z\x80-\xFF][A-Za-z\x80-\xFF\d]*");
        assert!(convert(r"(a)\1").is_err());
        assert!(convert(r"\Gfoo").is_err());
        assert!(convert(r"\k<x>").is_err());
        assert_eq!(convert(r"a\Z").unwrap().pattern, "a$");
        // 文字クラスの中の ( や ? は括弧として数えない
        let c = convert(r"[(?=]x(?=y)").unwrap();
        assert_eq!((c.pattern.as_str(), c.followed_by.as_deref()), ("[(?=]x", Some("y")));
        // 非 ASCII はそのまま
        assert_eq!(convert("「.*」").unwrap().pattern, "「.*」");
    }
}
