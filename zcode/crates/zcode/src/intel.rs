//! エディタ画面と `zc-intel` のつなぎ（design.md §10.2 の最初の版。モック 12〜14）。
//!
//! - 言語の設定（`zc-highlight` の `LanguageInfo`）から `zc-intel` の形を作る
//!   （横の crate 同士は依存しないので、写すのはここ）
//! - 診断を編集に合わせてずらす、波線に直す
//! - アウトラインの絞り込み（一致した項目の親を薄く残す）

use zc_core::{palette, LineChange, Span, TokenKind};
use zc_highlight::{LanguageInfo, OutlineSpec};
use zc_intel::{Diagnostic, OutlineItem, OutlineKind, OutlineRule, OutlineRules, Severity};
use zc_ui::{LineMark, PickRow};

/// 自動で補完を出すのに要る文字数（モック 12）。
pub const AUTO_MIN_CHARS: usize = 2;
/// 補完の候補の最大数。
pub const MAX_ITEMS: usize = 5;

/// `LanguageInfo` の `[outline]` を `zc-intel` の規則に写して `f` に渡す。
pub fn with_outline_rules<R>(spec: &OutlineSpec, f: impl FnOnce(&OutlineRules<'_>) -> R) -> R {
    let items: Vec<OutlineRule<'_>> = spec
        .items
        .iter()
        .map(|(kw, k)| OutlineRule {
            keyword: kw,
            kind: match k {
                zc_highlight::OutlineKind::Function => OutlineKind::Function,
                zc_highlight::OutlineKind::Class => OutlineKind::Class,
                zc_highlight::OutlineKind::Heading => OutlineKind::Heading,
            },
        })
        .collect();
    let rules = OutlineRules { items: &items, modifiers: spec.modifiers, heading_char: spec.heading_char, indent_width: spec.indent_width.max(1) };
    f(&rules)
}

/// 構文チェックのコマンドと、エラー行を拾う正規表現（両方あるときだけ）。
pub fn check_command(info: &LanguageInfo) -> Option<(&'static str, &'static str)> {
    Some((info.check?, info.check_pattern?))
}

/// 行の中のバイト位置 `byte` の直前の文字が、文字列かコメントの中か（補完を出さない所）。
pub fn in_string_or_comment(spans: &[Span], byte: usize) -> bool {
    if byte == 0 {
        return false;
    }
    let b = (byte - 1) as u32;
    spans.iter().any(|s| s.start <= b && b < s.end && matches!(s.kind, TokenKind::String | TokenKind::Comment | TokenKind::Escape))
}

/// 編集に合わせて診断の行をずらす。変わった範囲の中の診断は、範囲の中に残す。
pub fn shift_diagnostics(diags: &mut [Diagnostic], change: LineChange) {
    let delta = change.delta();
    for d in diags {
        if d.line >= change.old.end {
            d.line = (d.line as i64 + delta).max(0) as u32;
        } else if d.line >= change.old.start && d.line >= change.new.end {
            d.line = change.new.end.saturating_sub(1).max(change.new.start);
        }
    }
}

/// 診断 → 波線（行の文字全体。エラーは赤、警告は黄）。
pub fn marks_of(diags: &[Diagnostic]) -> Vec<LineMark> {
    diags
        .iter()
        .map(|d| LineMark { line: d.line, cols: None, color: if d.severity == Severity::Error { palette::ERROR } else { palette::WARN } })
        .collect()
}

/// アウトラインの項目の印と色（モック 14）。
fn icon_of(kind: OutlineKind) -> (&'static str, zc_core::Color) {
    match kind {
        OutlineKind::Function => ("ƒ", palette::FUNCTION),
        OutlineKind::Class => ("C", palette::CLASS),
        OutlineKind::Heading => ("#", palette::KEYWORD),
    }
}

/// 絞り込んだアウトラインの 1 行と、選べる行ならその行番号（0 始まり）。
pub struct OutlineRow {
    pub row: PickRow,
    pub line: Option<u32>,
}

/// `query` で絞り込む（ASCII の大文字小文字を無視した部分一致）。一致した項目の親は
/// 薄い行（選べない）として残す。`query` が空なら全部。
pub fn filter_outline(items: &[OutlineItem], query: &str) -> Vec<OutlineRow> {
    let q = query.to_ascii_lowercase();
    let hit = |name: &str| -> Option<core::ops::Range<usize>> {
        if q.is_empty() {
            return Some(0..0);
        }
        name.to_ascii_lowercase().find(&q).map(|s| s..s + q.len())
    };
    let mut keep: Vec<Option<bool>> = vec![None; items.len()]; // Some(true) = 一致、Some(false) = 親として残す
    for (i, it) in items.iter().enumerate() {
        if hit(&it.name).is_some() {
            keep[i] = Some(true);
            // 親（前にあって、深さが浅いもの）をたどる
            let mut depth = it.depth;
            for j in (0..i).rev() {
                if depth == 0 {
                    break;
                }
                if items[j].depth < depth {
                    depth = items[j].depth;
                    if keep[j].is_none() {
                        keep[j] = Some(false);
                    }
                }
            }
        }
    }
    items
        .iter()
        .zip(keep)
        .filter_map(|(it, k)| {
            let matched = k?;
            let (icon, icon_color) = icon_of(it.kind);
            let hits = if matched { hit(&it.name).filter(|r| !r.is_empty()).into_iter().collect() } else { Vec::new() };
            let row = PickRow { depth: it.depth, icon, icon_color, name: it.name.clone(), hits, right: (it.line + 1).to_string(), context: !matched };
            Some(OutlineRow { row, line: matched.then_some(it.line) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::LineRange;

    fn item(line: u32, depth: u8, kind: OutlineKind, name: &str) -> OutlineItem {
        OutlineItem { line, depth, kind, name: name.into() }
    }

    #[test]
    fn filter_keeps_parents_as_context() {
        let items = vec![
            item(2, 0, OutlineKind::Class, "Blinker"),
            item(3, 1, OutlineKind::Function, "__init__"),
            item(8, 1, OutlineKind::Function, "make_pattern"),
            item(17, 0, OutlineKind::Function, "main"),
            item(30, 0, OutlineKind::Function, "run_formatted"),
        ];
        let rows = filter_outline(&items, "MA");
        let names: Vec<(&str, bool)> = rows.iter().map(|r| (r.row.name.as_str(), r.row.context)).collect();
        assert_eq!(names, [("Blinker", true), ("make_pattern", false), ("main", false), ("run_formatted", false)]);
        assert_eq!(rows[0].line, None);
        assert_eq!(rows[1].line, Some(8));
        assert_eq!(rows[3].row.hits, vec![7..9]);
        assert_eq!(rows[1].row.right, "9");
        assert_eq!(filter_outline(&items, "").len(), 5);
        assert!(filter_outline(&items, "zzz").is_empty());
    }

    #[test]
    fn diagnostics_follow_edits() {
        let d = |line| Diagnostic { line, col: None, severity: Severity::Error, message: String::new() };
        let mut ds = vec![d(1), d(5), d(9)];
        // 3 行目（0 始まり 2）に 2 行足した
        shift_diagnostics(&mut ds, LineChange { old: LineRange::new(2, 3), new: LineRange::new(2, 5) });
        assert_eq!(ds.iter().map(|d| d.line).collect::<Vec<_>>(), [1, 7, 11]);
        // 5〜8 行目を 1 行にまとめた: 中の診断は残った行へ
        shift_diagnostics(&mut ds, LineChange { old: LineRange::new(5, 9), new: LineRange::new(5, 6) });
        assert_eq!(ds.iter().map(|d| d.line).collect::<Vec<_>>(), [1, 5, 8]);
    }

    #[test]
    fn string_and_comment_spans_block_completion() {
        let spans = [Span { start: 0, end: 3, kind: TokenKind::Text }, Span { start: 4, end: 10, kind: TokenKind::String }];
        assert!(!in_string_or_comment(&spans, 2));
        assert!(in_string_or_comment(&spans, 6));
        assert!(!in_string_or_comment(&spans, 0));
    }
}
