//! 1 つの言語の実行時の形（埋め込みの領域から作る）とトークン化。
//!
//! DFA は埋め込みのバイト列をそのまま使う（ゼロコピー）。表（文脈・規則）だけは
//! 小さいので `Vec` に読み出す。

use crate::format::layout::*;
use regex_automata::dfa::{dense, Automaton};
use regex_automata::util::start;
use regex_automata::Anchored;
use std::collections::HashMap;
use std::sync::Mutex;

/// 文脈スタックの深さの上限。超えた push は無視する（壊れた規則で膨らみ続けないように）。
pub(crate) const MAX_DEPTH: usize = 32;
/// 同じ位置で空の一致が続いたら 1 文字進める（push / pop を繰り返して止まらないのを防ぐ）。
const MAX_ZERO_MOVES: u32 = 8;

type Dfa = dense::DFA<&'static [u32]>;

pub(crate) struct Ctx {
    /// include で取り込まれるだけの文脈（スタックに載らない）は持たない。
    dfa: Option<Dfa>,
    rule_base: u32,
    default_token: u8,
    /// 自分の規則が当たらなかったときに探す文脈（末尾の include）。
    fallback: u16,
}

pub(crate) struct RuleRt {
    token: u8,
    pop: u8,
    flags: u8,
    push_idx: u32,
    n_push: u8,
    head: Option<Dfa>,
}

/// fallback をたどる回数の上限（循環しても止まるように）。
const MAX_FALLBACK_HOPS: u32 = 8;

/// 文脈スタック ⇔ 番号（[`crate::LineState`]）の対応。言語ごとに 1 つで、メインと
/// batch の両方から使う（行末でスタックが変わったときだけ触る）。
#[derive(Default)]
pub(crate) struct Interner {
    ids: HashMap<Vec<u16>, u32>,
    stacks: Vec<Vec<u16>>,
}

pub(crate) struct LangRt {
    ctxs: Vec<Ctx>,
    rules: Vec<RuleRt>,
    push: Vec<u16>,
    /// 語の表（バイト列の昇順）。
    words: Vec<(&'static [u8], u8, u16)>,
    interner: Mutex<Interner>,
}

/// DFA をバイト列から作る。埋め込みの表は build.rs が作ったもので、ビルド ID も
/// 確かめてあるので、検査を省いた読み込みを使う（全ページを読む検査を避ける）。
fn load_dfa(region: &'static [u8], off: u32, len: u32, checked: bool) -> Option<Dfa> {
    let b = region.get(off as usize..(off as usize).checked_add(len as usize)?)?;
    if !(b.as_ptr() as usize).is_multiple_of(4) {
        return None;
    }
    let r = if checked {
        dense::DFA::from_bytes(b)
    } else {
        // SAFETY: build.rs が regex-automata の to_bytes_little_endian で書いたバイト列で、
        // 先頭のビルド ID が生成コードの BUILD_ID と一致することを Registry が確かめてある。
        unsafe { dense::DFA::from_bytes_unchecked(b) }
    };
    r.ok().map(|(d, _)| d)
}

impl LangRt {
    /// 領域から作る。形が合わなければ `None`（ハイライトなしで続ける）。
    pub(crate) fn load(region: &'static [u8], checked: bool) -> Option<LangRt> {
        if region.get(0..4)? != REGION_MAGIC {
            return None;
        }
        let n_ctx = rd_u32(region, 4)? as usize;
        let n_rules = rd_u32(region, 8)? as usize;
        let n_push = rd_u32(region, 12)? as usize;
        let ctx_off = rd_u32(region, 16)? as usize;
        let rule_off = rd_u32(region, 20)? as usize;
        let push_off = rd_u32(region, 24)? as usize;
        if n_ctx == 0 || n_ctx > MAX_CONTEXTS {
            return None;
        }
        let mut push = Vec::with_capacity(n_push);
        for i in 0..n_push {
            let c = rd_u16(region, push_off + i * 2)?;
            if c as usize >= n_ctx {
                return None;
            }
            push.push(c);
        }
        let mut rules = Vec::with_capacity(n_rules);
        for i in 0..n_rules {
            let at = rule_off + i * RULE_REC_LEN;
            let rec = region.get(at..at + RULE_REC_LEN)?;
            let push_idx = rd_u32(rec, 4)?;
            let n_push_r = rec[2];
            if push_idx as usize + n_push_r as usize > push.len() {
                return None;
            }
            let (ho, hl) = (rd_u32(rec, 8)?, rd_u32(rec, 12)?);
            let head = if hl > 0 { Some(load_dfa(region, ho, hl, checked)?) } else { None };
            rules.push(RuleRt { token: rec[0], pop: rec[1], flags: rec[3], push_idx, n_push: n_push_r, head });
        }
        let words_off = rd_u32(region, 28)? as usize;
        let n_words = rd_u32(region, 32)? as usize;
        let mut words = Vec::with_capacity(n_words);
        for i in 0..n_words {
            let at = words_off + i * WORD_REC_LEN;
            let so = rd_u32(region, at)? as usize;
            let sl = rd_u16(region, at + 4)? as usize;
            let push = rd_u16(region, at + 8)?;
            if push != NO_FALLBACK && push as usize >= n_ctx {
                return None;
            }
            words.push((region.get(so..so + sl)?, *region.get(at + 6)?, push));
        }
        if words.windows(2).any(|w| w[0].0 >= w[1].0) {
            return None;
        }
        let mut ctxs = Vec::with_capacity(n_ctx);
        for i in 0..n_ctx {
            let at = ctx_off + i * CTX_REC_LEN;
            let rec = region.get(at..at + CTX_REC_LEN)?;
            let (dfa_off, dfa_len) = (rd_u32(rec, 0)?, rd_u32(rec, 4)?);
            let dfa = if dfa_len > 0 { Some(load_dfa(region, dfa_off, dfa_len, checked)?) } else { None };
            let rule_base = rd_u32(rec, 8)?;
            let rule_count = rd_u16(rec, 12)? as u32;
            let patterns = dfa.as_ref().map_or(0, |d| d.pattern_len());
            if (rule_base + rule_count) as usize > rules.len() || patterns != rule_count as usize {
                return None;
            }
            let fallback = rd_u16(rec, 16)?;
            if fallback != NO_FALLBACK && fallback as usize >= n_ctx {
                return None;
            }
            ctxs.push(Ctx { dfa, rule_base, default_token: rec[14], fallback });
        }
        let mut interner = Interner::default();
        interner.ids.insert(vec![0], 0);
        interner.stacks.push(vec![0]);
        Some(LangRt { ctxs, rules, push, words, interner: Mutex::new(interner) })
    }

    /// 文脈の数。
    pub(crate) fn context_count(&self) -> usize {
        self.ctxs.len()
    }

    /// スタックの番号を得る（無ければ足す）。
    pub(crate) fn intern(&self, stack: &[u16]) -> u32 {
        if stack == [0] {
            return 0;
        }
        let mut g = self.interner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&id) = g.ids.get(stack) {
            return id;
        }
        let id = g.stacks.len() as u32;
        g.stacks.push(stack.to_vec());
        g.ids.insert(stack.to_vec(), id);
        id
    }

    /// 番号 → スタック。知らない番号なら最下段だけ。
    pub(crate) fn stack_of(&self, id: u32, out: &mut Vec<u16>) {
        out.clear();
        if id != 0 {
            let g = self.interner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = g.stacks.get(id as usize) {
                out.extend_from_slice(s);
                return;
            }
        }
        out.push(0);
    }

    /// これまでに見た文脈スタックの数（テスト・計測用）。
    pub(crate) fn interned(&self) -> usize {
        self.interner.lock().unwrap_or_else(|e| e.into_inner()).stacks.len()
    }

    /// 1 行をトークン化する。`stack` は行頭の文脈（終わると行末の文脈になる）。
    /// トークンは `emit(start, end, kind)` で、行の頭から隙間なく順に渡す。
    /// スタックを触ったら `true`。
    #[inline]
    pub(crate) fn tokenize(&self, stack: &mut Vec<u16>, text: &[u8], emit: &mut impl FnMut(usize, usize, u8)) -> bool {
        let n = text.len();
        let mut at = 0usize;
        let mut zero_moves = 0u32;
        let mut touched = false;
        if stack.is_empty() {
            stack.push(0);
        }
        while at <= n {
            let top = *stack.last().unwrap() as usize;
            let ctx = &self.ctxs[top.min(self.ctxs.len() - 1)];
            // 自分の規則 → fallback の文脈の規則、の順に探す
            let mut found = None;
            let mut c = ctx;
            let mut hops = 0;
            loop {
                if let Some(d) = &c.dfa {
                    if let Some((pid, end)) = find(d, text, at, n + 1) {
                        found = Some((c.rule_base as usize + pid, end));
                        break;
                    }
                }
                if c.fallback == NO_FALLBACK || hops >= MAX_FALLBACK_HOPS {
                    break;
                }
                c = &self.ctxs[c.fallback as usize];
                hops += 1;
            }
            match found {
                Some((ri, end)) => {
                    let rule = &self.rules[ri];
                    let mut tok_end = end;
                    if let Some(h) = &rule.head {
                        if let Some((_, he)) = find(h, text, at, end) {
                            tok_end = he;
                        }
                    }
                    // 文字の途中で切らない（バイト単位の規則が UTF-8 の途中で終わることがある）
                    while tok_end < n && (text[tok_end] & 0xC0) == 0x80 {
                        tok_end += 1;
                    }
                    let mut word_push = NO_FALLBACK;
                    if tok_end > at {
                        zero_moves = 0;
                        if at < n {
                            let end = tok_end.min(n);
                            let mut kind = if rule.token == TOKEN_INHERIT { ctx.default_token } else { rule.token };
                            if rule.flags & RULE_WORDS != 0 {
                                let w = &text[at..end];
                                if let Ok(i) = self.words.binary_search_by(|x| x.0.cmp(w)) {
                                    kind = self.words[i].1;
                                    word_push = self.words[i].2;
                                }
                            }
                            emit(at, end, kind);
                        }
                    } else {
                        zero_moves += 1;
                        if zero_moves > MAX_ZERO_MOVES {
                            if at >= n {
                                break;
                            }
                            let next = next_char(text, at);
                            emit(at, next, ctx.default_token);
                            at = next;
                            zero_moves = 0;
                            continue;
                        }
                    }
                    if rule.pop > 0 || rule.n_push > 0 {
                        touched = true;
                        for _ in 0..rule.pop {
                            if stack.len() > 1 {
                                stack.pop();
                            }
                        }
                        let p = rule.push_idx as usize;
                        for &c in &self.push[p..p + rule.n_push as usize] {
                            if stack.len() < MAX_DEPTH {
                                stack.push(c);
                            }
                        }
                    }
                    if word_push != NO_FALLBACK && stack.len() < MAX_DEPTH {
                        touched = true;
                        stack.push(word_push);
                    }
                    at = tok_end;
                }
                None => {
                    if at >= n {
                        break;
                    }
                    let next = next_char(text, at);
                    emit(at, next, ctx.default_token);
                    at = next;
                }
            }
        }
        touched
    }
}

#[inline]
fn next_char(t: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < t.len() && (t[j] & 0xC0) == 0x80 {
        j += 1;
    }
    j
}

/// `text` の `at` から先頭一致で探す（優先順位は規則の順）。行末には `\n` があるものとして
/// 照合する（位置 `text.len()` が `\n`）。`limit` までしか読まない（その先の 1 バイトは
/// 境界の判定にだけ使う）。戻り値は（規則の番号, 終わりの位置）。
#[inline]
fn find(dfa: &Dfa, text: &[u8], at: usize, limit: usize) -> Option<(usize, usize)> {
    let n = text.len();
    let byte_at = |i: usize| if i < n { text[i] } else { b'\n' };
    let cfg = start::Config::new().anchored(Anchored::Yes).look_behind(if at > 0 { Some(byte_at(at - 1)) } else { None });
    let mut sid = dfa.start_state(&cfg).ok()?;
    let mut last = None;
    let mut i = at;
    // DFA の一致は 1 バイト遅れて分かる: i のバイトを読んで一致状態になったら、一致は i で終わる
    while i < limit {
        sid = dfa.next_state(sid, byte_at(i));
        if dfa.is_special_state(sid) {
            if dfa.is_match_state(sid) {
                last = Some((dfa.match_pattern(sid, 0).as_usize(), i));
            } else if dfa.is_dead_state(sid) || dfa.is_quit_state(sid) {
                return last;
            }
        }
        i += 1;
    }
    sid = if i > n { dfa.next_eoi_state(sid) } else { dfa.next_state(sid, byte_at(i)) };
    if dfa.is_match_state(sid) {
        last = Some((dfa.match_pattern(sid, 0).as_usize(), i));
    }
    last
}

/// 1 行目の判定（先頭一致でなく、行のどこかに一致すればよい）。
pub(crate) fn first_line_matches(dfa: &Dfa, line: &str) -> bool {
    let input = regex_automata::Input::new(line.as_bytes());
    matches!(dfa.try_search_fwd(&input), Ok(Some(_)))
}

pub(crate) fn load_first_line(blob: &'static [u8], off: u32, len: u32) -> Option<Dfa> {
    if len == 0 {
        return None;
    }
    load_dfa(blob, off, len, true)
}
