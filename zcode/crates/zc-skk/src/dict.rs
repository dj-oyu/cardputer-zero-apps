//! 辞書: システム辞書（mmap・二分探索）、個人辞書（メモリ上）、候補の行の読み方。
//!
//! 形式は SKK-JISYO と同じ EUC-JP のテキスト。`;; okuri-ari entries.` の区画は読みの
//! **降順**、`;; okuri-nasi entries.` の区画は**昇順**（どちらも EUC-JP のバイト列で比べる）。
//! 1 行は `よみ /候補1/候補2;注釈/[送り仮名/候補/…/]/`。
//!
//! - 注釈は `;` の後ろ（[`Candidate::annotation`]）
//! - `(concat "…")` は文字列だけのもの（`\057` のような 8 進の書き方を含む）を展開する。
//!   それ以外の Lisp の式（`(skk-current-date)` など）の候補は捨てる
//! - `[送り仮名/候補/…/]` の塊（送り仮名ごとの候補）は使わない（読み飛ばす。個人辞書では
//!   書き戻しのために残す）

use crate::Candidate;
use std::fs::File;
use std::io;
use std::ops::Range;
use std::os::fd::AsRawFd;
use std::path::Path;

pub(crate) const OKURI_ARI: &[u8] = b";; okuri-ari entries.";
pub(crate) const OKURI_NASI: &[u8] = b";; okuri-nasi entries.";

/// UTF-8 → EUC-JP（表せない文字があれば `None`）。
pub(crate) fn to_euc(s: &str) -> Option<Vec<u8>> {
    let (b, _, bad) = encoding_rs::EUC_JP.encode(s);
    (!bad).then(|| b.into_owned())
}

/// EUC-JP → UTF-8（壊れたバイト列なら `None`）。
fn from_euc(b: &[u8]) -> Option<String> {
    let (s, bad) = encoding_rs::EUC_JP.decode_without_bom_handling(b);
    (!bad).then(|| s.into_owned())
}

/// 行の本体（`/候補/…/`）を項目に分ける。`[…/…/]` の塊は 1 項目にまとめ、`true` を付ける。
pub(crate) fn split_items(body: &str) -> Vec<(bool, &str)> {
    let body = body.strip_prefix('/').unwrap_or(body);
    let mut parts: Vec<(usize, usize)> = Vec::new();
    let mut s = 0;
    for (i, b) in body.bytes().enumerate() {
        if b == b'/' {
            parts.push((s, i));
            s = i + 1;
        }
    }
    if s < body.len() {
        parts.push((s, body.len()));
    }
    let mut out = Vec::new();
    let mut k = 0;
    while k < parts.len() {
        let (a, b) = parts[k];
        let p = &body[a..b];
        k += 1;
        if p.starts_with('[') && !(p.len() > 1 && p.ends_with(']')) {
            // 送り仮名の塊: `]` で終わる部分まで
            let mut e = b;
            while k < parts.len() {
                let (c, d) = parts[k];
                e = d;
                k += 1;
                if body[c..d].ends_with(']') {
                    break;
                }
            }
            out.push((true, &body[a..e]));
        } else if !p.is_empty() {
            out.push((false, p));
        }
    }
    out
}

/// Lisp の式に見えるか（ddskk の `skk-lisp-prog-p` と同じく、`(` で始まり `)` で終わり、空白を含む）。
fn is_lisp(s: &str) -> bool {
    s.starts_with('(') && s.ends_with(')') && s.contains(' ')
}

/// `(concat "…" "…")` を展開する。文字列以外が入っていれば `None`。
pub(crate) fn decode_concat(s: &str) -> Option<String> {
    let inner = s.strip_prefix("(concat")?.strip_suffix(')')?;
    let mut out = String::new();
    let mut it = inner.chars().peekable();
    loop {
        while it.peek().is_some_and(|c| c.is_whitespace()) {
            it.next();
        }
        match it.next() {
            None => return Some(out),
            Some('"') => {}
            Some(_) => return None,
        }
        loop {
            match it.next()? {
                '"' => break,
                '\\' => {
                    let c = it.next()?;
                    if let Some(mut v) = c.to_digit(8) {
                        for _ in 0..2 {
                            match it.peek().and_then(|d| d.to_digit(8)) {
                                Some(d) => {
                                    v = v * 8 + d;
                                    it.next();
                                }
                                None => break,
                            }
                        }
                        out.push(char::from_u32(v)?);
                    } else {
                        out.push(match c {
                            'n' => '\n',
                            't' => '\t',
                            o => o,
                        });
                    }
                }
                c => out.push(c),
            }
        }
    }
}

/// 候補・注釈の文字列を表示する形にする（`concat` は展開、ほかの式は `None`）。
fn unlisp(s: &str) -> Option<String> {
    if is_lisp(s) {
        decode_concat(s)
    } else {
        Some(s.to_string())
    }
}

/// 項目 1 つ（`候補;注釈`）を候補にする。使えない候補（Lisp の式、空）は `None`。
pub(crate) fn candidate_from_item(item: &str) -> Option<Candidate> {
    let (t, a) = match item.find(';') {
        Some(i) => (&item[..i], Some(&item[i + 1..])),
        None => (item, None),
    };
    let text = unlisp(t)?;
    if text.is_empty() {
        return None;
    }
    let annotation = a.and_then(unlisp).filter(|a| !a.is_empty());
    Some(Candidate { text, annotation })
}

/// 候補の行の本体から候補を取り出す（送り仮名の塊は飛ばす）。
pub(crate) fn parse_candidates(body: &str) -> Vec<Candidate> {
    split_items(body).into_iter().filter(|(block, _)| !block).filter_map(|(_, it)| candidate_from_item(it)).collect()
}

/// 語を辞書に書く形にする。`/` `;` などの区切りを含む語は `(concat "…")` に包む
/// （SKK・uim-skk と同じ書き方。`/` は `\057`、`;` は `\073`）。
pub(crate) fn quote_word(w: &str) -> String {
    let needs = w.contains(['/', ';', '"', '\\', '\n', '\r']) || w.starts_with('[') || is_lisp(w);
    if !needs {
        return w.to_string();
    }
    let mut s = String::from("(concat \"");
    for c in w.chars() {
        match c {
            '/' => s.push_str("\\057"),
            ';' => s.push_str("\\073"),
            '"' => s.push_str("\\042"),
            '\\' => s.push_str("\\134"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\015"),
            c => s.push(c),
        }
    }
    s.push_str("\")");
    s
}

/// 送りありの読みか（`おくr` のように、非 ASCII で始まり英小文字 1 つで終わる）。
pub(crate) fn is_okuri_key(reading: &str) -> bool {
    let first = reading.chars().next();
    let last = reading.chars().last();
    reading.chars().count() >= 2 && first.is_some_and(|c| !c.is_ascii()) && last.is_some_and(|c| c.is_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// システム辞書

/// システム辞書（mmap、読むだけ）。`Send + Sync`。**メインスレッドで探索しない。**
///
/// 開くときに区画の見出し（`;; okuri-ari entries.` と `;; okuri-nasi entries.`）を探すため、
/// 送りありの区画（SKK-JISYO.L で前の約 1/4）を 1 度だけ読む。探索は二分探索で、
/// 1 回に触るのは数十行（数ページ）だけ。行の中身を UTF-8 に直すのは見つかった行だけ。
pub struct SystemDict {
    ptr: *const u8,
    len: usize,
    ari: Range<usize>,
    nasi: Range<usize>,
}

// SAFETY: 読み取り専用のマッピングを指すだけで、書き換えない。
unsafe impl Send for SystemDict {}
// SAFETY: 同上（共有して読むだけ）。
unsafe impl Sync for SystemDict {}

impl Drop for SystemDict {
    fn drop(&mut self) {
        // SAFETY: open で作ったマッピングを 1 度だけ外す。
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
        }
    }
}

impl SystemDict {
    /// 辞書を mmap する。**assist スレッドで呼ぶ。**
    pub fn open(path: &Path) -> io::Result<SystemDict> {
        let f = File::open(path)?;
        let len = f.metadata()?.len() as usize;
        if len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{}: 空のファイル", path.display())));
        }
        // SAFETY: 読み取り専用の私有マッピング。Drop で外す。
        let p = unsafe { libc::mmap(core::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0) };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let mut d = SystemDict { ptr: p as *const u8, len, ari: 0..0, nasi: 0..len };
        let (ari, nasi) = find_blocks(d.data());
        d.ari = ari;
        d.nasi = nasi;
        Ok(d)
    }

    fn data(&self) -> &[u8] {
        // SAFETY: ptr は len バイトの読める領域で、self が生きている間は外さない。
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// 辞書全体に `madvise(WILLNEED)` をかける（かなモードに入ったときの先読み）。
    /// 読み込みはカーネルが裏で行い、この呼び出しは待たない。
    pub fn prefetch(&self) {
        // SAFETY: 自分のマッピング全体への助言だけ。
        unsafe {
            libc::madvise(self.ptr as *mut libc::c_void, self.len, libc::MADV_WILLNEED);
        }
    }

    /// 読みを探す。**assist スレッドで呼ぶ。** 送りありなら `reading` は `かk` の形
    /// （最後が英小文字）。見つからなければ空。
    pub fn lookup(&self, reading: &str, okuri: bool) -> Vec<Candidate> {
        let Some(key) = to_euc(reading) else { return Vec::new() };
        if key.is_empty() || key.contains(&b' ') || key.contains(&b'\n') {
            return Vec::new();
        }
        let (r, desc) = if okuri { (self.ari.clone(), true) } else { (self.nasi.clone(), false) };
        match search(self.data(), r, &key, desc).and_then(from_euc) {
            Some(body) => parse_candidates(body.trim_start().trim_end_matches('\r')),
            None => Vec::new(),
        }
    }
}

/// 区画の範囲（送りあり, 送りなし）を探す。見出しが無ければ全体を送りなしとみなす。
fn find_blocks(d: &[u8]) -> (Range<usize>, Range<usize>) {
    let mut ari_start = None;
    let mut pos = 0;
    while pos < d.len() {
        let end = d[pos..].iter().position(|&b| b == b'\n').map_or(d.len(), |i| pos + i);
        if d[pos] == b';' {
            let line = d[pos..end].strip_suffix(b"\r").unwrap_or(&d[pos..end]);
            if line == OKURI_ARI {
                ari_start = Some(end + 1);
            } else if line == OKURI_NASI {
                let ari = ari_start.map_or(0..0, |s| s.min(pos)..pos);
                return (ari, (end + 1).min(d.len())..d.len());
            }
        }
        pos = end + 1;
    }
    match ari_start {
        // 送りなしの見出しが無い: 送りありの区画だけ
        Some(s) => (s.min(d.len())..d.len(), 0..0),
        None => (0..0, 0..d.len()),
    }
}

/// 区画の中を二分探索し、見つかった行の読みより後ろ（` /候補/…/`）を返す。
/// コメント行・空行は飛ばす。`desc` は降順の区画。
fn search<'a>(d: &'a [u8], r: Range<usize>, key: &[u8], desc: bool) -> Option<&'a [u8]> {
    let (mut lo, mut hi) = (r.start, r.end.min(d.len()));
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let start = d[lo..mid].iter().rposition(|&b| b == b'\n').map_or(lo, |i| lo + i + 1);
        // start から先で、最初の項目の行
        let mut s = start;
        let mut line = None;
        while s < hi {
            let e = d[s..hi].iter().position(|&b| b == b'\n').map_or(hi, |i| s + i);
            if e > s && d[s] != b';' {
                line = Some((s, e));
                break;
            }
            s = e + 1;
        }
        let Some((s, e)) = line else {
            hi = start;
            continue;
        };
        let l = &d[s..e];
        let sp = l.iter().position(|&b| b == b' ').unwrap_or(l.len());
        let ord = key.cmp(&l[..sp]);
        let ord = if desc { ord.reverse() } else { ord };
        match ord {
            core::cmp::Ordering::Less => hi = start,
            core::cmp::Ordering::Greater => lo = e + 1,
            core::cmp::Ordering::Equal => return Some(&l[sp..]),
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 個人辞書

/// 個人辞書の項目（候補は辞書に書く形のまま。注釈・`concat`・送り仮名の塊も残す）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct UEntry {
    reading: String,
    items: Vec<String>,
}

/// 個人辞書（メモリ上に持つ。uim-skk と同じ形式）。
///
/// 区画ごとに、新しく学習したものが先頭に来る（uim-skk・ddskk と同じ並べ方）。
/// ファイルへの書き戻しは zc-store の `SkkUserDict::apply` が行う（ロックを取って読み直し、
/// 変更を重ねて丸ごと書く）。エディタは [`crate::Skk::take_changes`] で変更を受け取って
/// そちらに渡す。[`UserDict::to_bytes`] / [`UserDict::is_dirty`] は、ほかのアプリと
/// 共有しない辞書を丸ごと書くとき・試験用（エディタは使わない）。
#[derive(Clone, Debug, Default)]
pub struct UserDict {
    /// 最初の見出しより前のコメント行。
    header: Vec<String>,
    ari: Vec<UEntry>,
    nasi: Vec<UEntry>,
    dirty: bool,
}

impl UserDict {
    /// 空の辞書（ファイルが無いとき）。
    pub fn new() -> UserDict {
        UserDict::default()
    }

    /// EUC-JP のバイト列から読む。壊れた行（EUC-JP として読めない、形が違う）は読み飛ばす。
    /// 区画の見出しが無い項目は、読みの形（最後が英小文字か）で区画を決める。
    pub fn parse(euc_jp: &[u8]) -> UserDict {
        let mut d = UserDict::default();
        // 0: 見出しの前、1: 送りあり、2: 送りなし
        let mut sec = 0;
        for line in euc_jp.split(|&b| b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line == OKURI_ARI {
                sec = 1;
                continue;
            }
            if line == OKURI_NASI {
                sec = 2;
                continue;
            }
            if line.is_empty() {
                continue;
            }
            let Some(s) = from_euc(line) else { continue };
            if s.starts_with(';') {
                if sec == 0 {
                    d.header.push(s);
                }
                continue;
            }
            let Some(sp) = s.find(' ') else { continue };
            let (reading, body) = (&s[..sp], &s[sp + 1..]);
            if reading.is_empty() || !body.starts_with('/') {
                continue;
            }
            let items: Vec<String> = split_items(body).into_iter().map(|(_, it)| it.to_string()).collect();
            if items.is_empty() {
                continue;
            }
            let ari = match sec {
                1 => true,
                2 => false,
                _ => is_okuri_key(reading),
            };
            let list = if ari { &mut d.ari } else { &mut d.nasi };
            if list.iter().any(|e| e.reading == reading) {
                continue; // 同じ読みが 2 回: 先のもの（新しい方）を使う
            }
            list.push(UEntry { reading: reading.to_string(), items });
        }
        d
    }

    /// 読みの候補（新しく学習したものが先）。
    pub fn lookup(&self, reading: &str, okuri: bool) -> Vec<Candidate> {
        let list = if okuri { &self.ari } else { &self.nasi };
        match list.iter().find(|e| e.reading == reading) {
            Some(e) => e.items.iter().filter(|it| !is_block(it)).filter_map(|it| candidate_from_item(it)).collect(),
            None => Vec::new(),
        }
    }

    /// 確定した語を先頭に上げる（無ければ足す）。項目も区画の先頭へ。
    pub fn learn(&mut self, reading: &str, okuri: bool, word: &str) {
        self.learn_form(reading, okuri, word);
    }

    /// [`UserDict::learn`] と同じ。辞書に書いた候補の形（注釈を除く。`concat` で
    /// 包んだときはその形）を返す。
    pub(crate) fn learn_form(&mut self, reading: &str, okuri: bool, word: &str) -> String {
        let list = if okuri { &mut self.ari } else { &mut self.nasi };
        let mut entry = match list.iter().position(|e| e.reading == reading) {
            Some(i) => list.remove(i),
            None => UEntry { reading: reading.to_string(), items: Vec::new() },
        };
        let existing = entry
            .items
            .iter()
            .position(|it| !is_block(it) && candidate_from_item(it).is_some_and(|c| c.text == word));
        let item = match existing {
            Some(i) => entry.items.remove(i),
            None => quote_word(word),
        };
        let form = item_base(&item).to_string();
        entry.items.insert(0, item);
        list.insert(0, entry);
        self.dirty = true;
        form
    }

    /// 前回の [`UserDict::to_bytes`] から変わったか（学習・登録で `true`）。
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 書き戻す内容（EUC-JP、uim-skk と同じ並び）。EUC-JP にできない項目は書かない。
    /// エディタはこれを使わず、[`crate::Skk::take_changes`] を zc-store に渡す
    /// （uim-skk が同時に学習した分を消さないため）。
    pub fn to_bytes(&mut self) -> Vec<u8> {
        self.dirty = false;
        let mut out = Vec::new();
        let mut line = |s: &str| {
            if let Some(b) = to_euc(s) {
                out.extend_from_slice(&b);
                out.push(b'\n');
            }
        };
        for h in &self.header {
            line(h);
        }
        line(";; okuri-ari entries.");
        for e in &self.ari {
            line(&entry_line(e));
        }
        line(";; okuri-nasi entries.");
        for e in &self.nasi {
            line(&entry_line(e));
        }
        out
    }
}

fn entry_line(e: &UEntry) -> String {
    let mut s = format!("{} /", e.reading);
    for it in &e.items {
        s.push_str(it);
        s.push('/');
    }
    s
}

fn is_block(item: &str) -> bool {
    item.starts_with('[') && !(item.len() > 1 && item.ends_with(']') && !item.contains('/'))
}

/// 候補の注釈を除いた部分（zc-store が同じ候補かを比べる部分）。
fn item_base(item: &str) -> &str {
    item.find(';').map_or(item, |i| &item[..i])
}
