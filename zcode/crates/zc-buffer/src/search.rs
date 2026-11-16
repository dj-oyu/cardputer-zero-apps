//! 検索（単純な部分文字列。大文字小文字の区別あり / なし）。
//!
//! rope を直接読むので、[`crate::Buffer`] でも [`crate::Snapshot`] でも使える
//! （batch に渡した写しの上で検索できる）。

use crate::{line_len_of, Selection};
use ropey::Rope;
use zc_core::Pos;

fn fold(c: char, case_sensitive: bool) -> char {
    if case_sensitive {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// 行の文字を（必要なら小文字にして）`out` に入れる。行末の改行は含めない。
fn fill_line(rope: &Rope, line: usize, case_sensitive: bool, out: &mut Vec<char>) {
    out.clear();
    let n = line_len_of(rope, line);
    let s = rope.line(line);
    out.extend(s.chars().take(n).map(|c| fold(c, case_sensitive)));
}

fn matches_at(hay: &[char], s: usize, needle: &[char]) -> bool {
    hay[s..s + needle.len()] == *needle
}

/// `from` より後ろ（`backward` なら前）で `needle` が最初に現れる範囲。
/// 前方: 開始が `from` 以降。後方: 終了が `from` 以前。見つからなければ反対側から
/// 巻き戻って探す。空の `needle` は `None`。
pub(crate) fn find(rope: &Rope, needle: &str, from: Pos, backward: bool, case_sensitive: bool) -> Option<Selection> {
    if needle.is_empty() {
        return None;
    }
    let nd: Vec<char> = needle.chars().map(|c| fold(c, case_sensitive)).collect();
    if nd.contains(&'\n') {
        return find_multiline(rope, &nd, from, backward, case_sensitive);
    }
    let m = nd.len();
    let n_lines = rope.len_lines();
    let from_line = (from.line as usize).min(n_lines - 1);
    let mut buf: Vec<char> = Vec::new();
    let sel = |line: usize, s: usize| Selection {
        anchor: Pos::new(line as u32, s as u32),
        head: Pos::new(line as u32, (s + m) as u32),
    };
    if !backward {
        // 順番: from 行（from.col 以降）→ 次の行以降 → 先頭から from 行まで
        let order = (from_line..n_lines).chain(0..=from_line);
        for (i, line) in order.enumerate() {
            fill_line(rope, line, case_sensitive, &mut buf);
            if buf.len() < m {
                continue;
            }
            let min_start = if i == 0 { from.col as usize } else { 0 };
            for s in min_start..=buf.len() - m {
                if matches_at(&buf, s, &nd) {
                    return Some(sel(line, s));
                }
            }
        }
    } else {
        let order = (0..=from_line).rev().chain((from_line..n_lines).rev());
        for (i, line) in order.enumerate() {
            fill_line(rope, line, case_sensitive, &mut buf);
            if buf.len() < m {
                continue;
            }
            let max_end = if i == 0 { (from.col as usize).min(buf.len()) } else { buf.len() };
            if max_end < m {
                continue;
            }
            for s in (0..=max_end - m).rev() {
                if matches_at(&buf, s, &nd) {
                    return Some(sel(line, s));
                }
            }
        }
    }
    None
}

/// 改行を含む検索語。全体を 1 回だけ文字の列に写して探す（まれなので割り切る）。
fn find_multiline(rope: &Rope, nd: &[char], from: Pos, backward: bool, case_sensitive: bool) -> Option<Selection> {
    let hay: Vec<char> = rope.chars().map(|c| fold(c, case_sensitive)).collect();
    let m = nd.len();
    if hay.len() < m {
        return None;
    }
    let from_idx = crate::idx_of(rope, from);
    let (mut first, mut first_ge, mut last, mut last_le) = (None, None, None, None);
    for s in 0..=hay.len() - m {
        if matches_at(&hay, s, nd) {
            if first.is_none() {
                first = Some(s);
            }
            if first_ge.is_none() && s >= from_idx {
                first_ge = Some(s);
            }
            last = Some(s);
            if s + m <= from_idx {
                last_le = Some(s);
            }
        }
    }
    let s = if backward { last_le.or(last) } else { first_ge.or(first) }?;
    Some(Selection { anchor: crate::pos_of(rope, s), head: crate::pos_of(rope, s + m) })
}
