//! 小さな正規表現（バックトラック）。`check_pattern` 用。
//!
//! 外部の `regex` crate を実機の実行ファイルに入れないための自前実装（出力 1 行ずつに
//! かけるだけなので、速さより小ささを取る）。対応する構文:
//! リテラル、`.`、`[...]`（範囲・否定・`\d\w\s`）、`\d \D \w \W \s \S \b`、
//! `\` でのエスケープ、`^` `$`、グループ `(...)`（捕獲）と `(?:...)`、`|`、
//! `* + ? {n} {n,} {n,m}` と、その最短一致（末尾に `?`）。
//! 後方参照・先読み・Unicode クラスは無い（zcode のほかの規則と同じ決まり）。

#[derive(Debug, Clone)]
enum Item {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

impl Item {
    fn matches(&self, c: char) -> bool {
        match *self {
            Item::Range(a, b) => a <= c && c <= b,
            Item::Digit(neg) => c.is_ascii_digit() != neg,
            Item::Word(neg) => (c.is_alphanumeric() || c == '_') != neg,
            Item::Space(neg) => c.is_whitespace() != neg,
        }
    }
}

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class(bool, Vec<Item>),
    Start,
    End,
    WordBoundary,
    /// 捕獲番号（1 から）。`None` は捕獲しないグループ。
    Group(Box<Node>, Option<usize>),
    Alt(Vec<Node>),
    Cat(Vec<Node>),
    Repeat { node: Box<Node>, min: u32, max: u32, greedy: bool },
}

/// 一致の結果。位置は文字数（バイトではない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// 一致した範囲の終わり（文字数）。
    pub end: usize,
    /// 捕獲グループ（添字 0 は全体）。
    pub groups: Vec<Option<String>>,
    /// 一致より後ろの文字列。
    pub rest: String,
}

#[derive(Debug, Clone)]
pub struct Regex {
    root: Node,
    n_groups: usize,
}

type Caps = Vec<Option<(usize, usize)>>;

struct Parser<'a> {
    s: &'a [char],
    i: usize,
    groups: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn alt(&mut self) -> Result<Node, String> {
        let mut alts = vec![self.cat()?];
        while self.eat('|') {
            alts.push(self.cat()?);
        }
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn cat(&mut self) -> Result<Node, String> {
        let mut v = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            v.push(self.quantified(atom)?);
        }
        Ok(Node::Cat(v))
    }

    fn quantified(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some('*') => (0, u32::MAX),
            Some('+') => (1, u32::MAX),
            Some('?') => (0, 1),
            Some('{') => {
                let save = self.i;
                self.i += 1;
                match self.braces() {
                    Some(r) => {
                        let greedy = !self.eat('?');
                        return Ok(Node::Repeat { node: Box::new(atom), min: r.0, max: r.1, greedy });
                    }
                    None => {
                        // 量指定でなければ、ただの '{'
                        self.i = save;
                        return Ok(atom);
                    }
                }
            }
            _ => return Ok(atom),
        };
        self.i += 1;
        let greedy = !self.eat('?');
        Ok(Node::Repeat { node: Box::new(atom), min, max, greedy })
    }

    /// `{` の後ろ（`n}` / `n,}` / `n,m}`）。
    fn braces(&mut self) -> Option<(u32, u32)> {
        let num = |p: &mut Self| -> Option<u32> {
            let st = p.i;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.i += 1;
            }
            if p.i == st {
                return None;
            }
            p.s[st..p.i].iter().collect::<String>().parse().ok()
        };
        let min = num(self)?;
        let max = if self.eat(',') {
            if self.peek() == Some('}') {
                u32::MAX
            } else {
                num(self)?
            }
        } else {
            min
        };
        if !self.eat('}') || max < min {
            return None;
        }
        Some((min, max))
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or("式が途中で終わっている")?;
        self.i += 1;
        Ok(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                let idx = if self.peek() == Some('?') {
                    if self.s.get(self.i + 1) == Some(&':') {
                        self.i += 2;
                        None
                    } else {
                        return Err("(? は (?: だけ使える".into());
                    }
                } else {
                    self.groups += 1;
                    Some(self.groups)
                };
                let inner = self.alt()?;
                if !self.eat(')') {
                    return Err("( が閉じていない".into());
                }
                Node::Group(Box::new(inner), idx)
            }
            '[' => self.class()?,
            '\\' => {
                let e = self.peek().ok_or("\\ で終わっている")?;
                self.i += 1;
                match e {
                    'd' => Node::Class(false, vec![Item::Digit(false)]),
                    'D' => Node::Class(false, vec![Item::Digit(true)]),
                    'w' => Node::Class(false, vec![Item::Word(false)]),
                    'W' => Node::Class(false, vec![Item::Word(true)]),
                    's' => Node::Class(false, vec![Item::Space(false)]),
                    'S' => Node::Class(false, vec![Item::Space(true)]),
                    'b' => Node::WordBoundary,
                    'n' => Node::Char('\n'),
                    't' => Node::Char('\t'),
                    'r' => Node::Char('\r'),
                    c => Node::Char(c),
                }
            }
            '*' | '+' | '?' => return Err(format!("'{c}' の前に対象が無い")),
            c => Node::Char(c),
        })
    }

    fn class(&mut self) -> Result<Node, String> {
        let neg = self.eat('^');
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek().ok_or("[ が閉じていない")?;
            self.i += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = if c == '\\' {
                let e = self.peek().ok_or("[ が閉じていない")?;
                self.i += 1;
                match e {
                    'd' => {
                        items.push(Item::Digit(false));
                        continue;
                    }
                    'D' => {
                        items.push(Item::Digit(true));
                        continue;
                    }
                    'w' => {
                        items.push(Item::Word(false));
                        continue;
                    }
                    'W' => {
                        items.push(Item::Word(true));
                        continue;
                    }
                    's' => {
                        items.push(Item::Space(false));
                        continue;
                    }
                    'S' => {
                        items.push(Item::Space(true));
                        continue;
                    }
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    e => e,
                }
            } else {
                c
            };
            if self.peek() == Some('-') && self.s.get(self.i + 1).is_some_and(|c| *c != ']') {
                self.i += 1;
                let mut hi = self.peek().unwrap();
                self.i += 1;
                if hi == '\\' {
                    hi = self.peek().ok_or("[ が閉じていない")?;
                    self.i += 1;
                }
                if hi < lo {
                    return Err("範囲の向きが逆".into());
                }
                items.push(Item::Range(lo, hi));
            } else {
                items.push(Item::Range(lo, lo));
            }
        }
        Ok(Node::Class(neg, items))
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex, String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut p = Parser { s: &chars, i: 0, groups: 0 };
        let root = p.alt()?;
        if p.i < chars.len() {
            return Err("対応しない ) がある".into());
        }
        Ok(Regex { root, n_groups: p.groups })
    }

    /// 捕獲グループの数（全体は含めない）。
    pub fn group_count(&self) -> usize {
        self.n_groups
    }

    /// `text` の中で最初に一致するところ（検索。先頭固定ではない）。
    pub fn find(&self, text: &str) -> Option<Match> {
        let s: Vec<char> = text.chars().collect();
        for start in 0..=s.len() {
            let mut caps: Caps = vec![None; self.n_groups + 1];
            let mut end = None;
            let ok = self.m(&self.root, &s, start, &mut caps, &mut |j, _| {
                end = Some(j);
                true
            });
            if ok {
                let end = end.unwrap();
                let mut groups: Vec<Option<String>> =
                    caps.iter().map(|c| c.map(|(a, b)| s[a..b].iter().collect())).collect();
                groups[0] = Some(s[start..end].iter().collect());
                return Some(Match { end, groups, rest: s[end..].iter().collect() });
            }
        }
        None
    }

    fn m(&self, n: &Node, s: &[char], i: usize, caps: &mut Caps, k: &mut dyn FnMut(usize, &mut Caps) -> bool) -> bool {
        match n {
            Node::Char(c) => i < s.len() && s[i] == *c && k(i + 1, caps),
            Node::Any => i < s.len() && k(i + 1, caps),
            Node::Class(neg, items) => i < s.len() && (items.iter().any(|it| it.matches(s[i])) != *neg) && k(i + 1, caps),
            Node::Start => i == 0 && k(i, caps),
            Node::End => i == s.len() && k(i, caps),
            Node::WordBoundary => {
                let before = i > 0 && is_word(s[i - 1]);
                let after = i < s.len() && is_word(s[i]);
                before != after && k(i, caps)
            }
            Node::Group(inner, None) => self.m(inner, s, i, caps, k),
            Node::Group(inner, Some(g)) => {
                let g = *g;
                let old = caps[g];
                let r = self.m(inner, s, i, caps, &mut |j, caps| {
                    let prev = caps[g];
                    caps[g] = Some((i, j));
                    if k(j, caps) {
                        true
                    } else {
                        caps[g] = prev;
                        false
                    }
                });
                if !r {
                    caps[g] = old;
                }
                r
            }
            Node::Alt(v) => {
                for a in v {
                    if self.m(a, s, i, caps, k) {
                        return true;
                    }
                }
                false
            }
            Node::Cat(v) => self.cat(v, s, i, caps, k),
            Node::Repeat { node, min, max, greedy } => self.rep(node, *min, *max, *greedy, 0, s, i, caps, k),
        }
    }

    fn cat(&self, v: &[Node], s: &[char], i: usize, caps: &mut Caps, k: &mut dyn FnMut(usize, &mut Caps) -> bool) -> bool {
        match v.split_first() {
            None => k(i, caps),
            Some((h, t)) => self.m(h, s, i, caps, &mut |j, caps| self.cat(t, s, j, caps, k)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rep(
        &self,
        n: &Node,
        min: u32,
        max: u32,
        greedy: bool,
        count: u32,
        s: &[char],
        i: usize,
        caps: &mut Caps,
        k: &mut dyn FnMut(usize, &mut Caps) -> bool,
    ) -> bool {
        if count < min {
            return self.m(n, s, i, caps, &mut |j, caps| self.rep(n, min, max, greedy, count + 1, s, j, caps, k));
        }
        if greedy {
            if count < max {
                // 空の繰り返し（j == i）は無限ループになるので進めない
                let more = self.m(n, s, i, caps, &mut |j, caps| j != i && self.rep(n, min, max, greedy, count + 1, s, j, caps, k));
                if more {
                    return true;
                }
            }
            k(i, caps)
        } else {
            if k(i, caps) {
                return true;
            }
            count < max && self.m(n, s, i, caps, &mut |j, caps| j != i && self.rep(n, min, max, greedy, count + 1, s, j, caps, k))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(p: &str, t: &str) -> Option<Vec<Option<String>>> {
        Regex::new(p).unwrap().find(t).map(|m| m.groups)
    }
    fn s(x: &str) -> Option<String> {
        Some(x.to_string())
    }

    #[test]
    fn basics() {
        assert_eq!(g(r"line (\d+)", r#"  File "x.py", line 42"#), Some(vec![s("line 42"), s("42")]));
        assert_eq!(g(r"line (\d+)", "nothing"), None);
        assert_eq!(g("a.c", "xxabcxx").unwrap()[0], s("abc"));
        assert_eq!(g("^a", "ba"), None);
        assert_eq!(g("a$", "ba").unwrap()[0], s("a"));
        assert_eq!(g("colou?r", "color").unwrap()[0], s("color"));
        assert_eq!(g("(foo|bar)+", "xfoobarbaz").unwrap()[0], s("foobar"));
        assert_eq!(g("a{2,3}", "aaaa").unwrap()[0], s("aaa"));
        assert_eq!(g("a{2}", "a"), None);
        assert_eq!(g("a+?", "aaa").unwrap()[0], s("a"));
        assert_eq!(g("a*", "").unwrap()[0], s(""));
        assert_eq!(g("(?:ab)+c", "ababc").unwrap()[0], s("ababc"));
        assert_eq!(g(r"\bfoo\b", "a foo b").unwrap()[0], s("foo"));
        assert_eq!(g(r"\bfoo\b", "afoob"), None);
    }

    #[test]
    fn classes_and_japanese() {
        assert_eq!(g("[a-c]+", "xxabcabd").unwrap()[0], s("abcab"));
        assert_eq!(g("[^0-9]+", "12ab3").unwrap()[0], s("ab"));
        assert_eq!(g(r"[\d.]+", "v1.25x").unwrap()[0], s("1.25"));
        assert_eq!(g("[]a]+", "]a").unwrap()[0], s("]a"));
        assert_eq!(g(r"(\d+)行目", "エラー: 12行目です").unwrap()[1], s("12"));
        assert_eq!(g("[あ-ん]+", "xxひらがなyy").unwrap()[0], s("ひらがな"));
    }

    #[test]
    fn captures_and_rest() {
        let m = Regex::new(r"^(.+?):(\d+):(?:(\d+):)? (.*)$").unwrap().find("a/b.rs:3:7: oops").unwrap();
        assert_eq!(m.groups, vec![s("a/b.rs:3:7: oops"), s("a/b.rs"), s("3"), s("7"), s("oops")]);
        let m = Regex::new(r"^(.+?):(\d+):(?:(\d+):)? (.*)$").unwrap().find("a.rs:3: oops").unwrap();
        assert_eq!(m.groups[3], None);
        assert_eq!(m.groups[4], s("oops"));
        let m = Regex::new(r"line (\d+)").unwrap().find("line 5: bad").unwrap();
        assert_eq!(m.rest, ": bad");
    }

    #[test]
    fn errors_and_edge_cases() {
        assert!(Regex::new("(a").is_err());
        assert!(Regex::new("a)").is_err());
        assert!(Regex::new("[a").is_err());
        assert!(Regex::new("*a").is_err());
        assert!(Regex::new("[z-a]").is_err());
        assert!(Regex::new("(?=a)").is_err());
        // 空の繰り返しで止まる
        assert!(Regex::new("(a*)*b").unwrap().find("aaaa").is_none());
        assert_eq!(g("a{x", "a{x").unwrap()[0], s("a{x"));
        assert_eq!(Regex::new(r"(a)(b)").unwrap().group_count(), 2);
    }
}
