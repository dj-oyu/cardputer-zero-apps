//! 状態機械の中身（1 つの入力欄ぶん）。辞書登録欄は、この [`Engine`] をもう 1 つ入れ子に持つ。
//!
//! 読み（▽ の中身・送り仮名）はいつもひらがなで持ち、カタカナモードでは表示と確定のときに
//! カタカナに直す（辞書はひらがなの読みで引く）。

use crate::dict::to_euc;
use crate::romaji::{to_katakana, Kana, Romaji};
use crate::{Candidate, CandidateList, InputMode, LearnedWord, LookupRequest, Preedit, PreeditState, Registration, SkkOutput, UserDict};
use zc_core::{Key, KeyEvent, Version};

/// 候補一覧の 1 ページの件数と、選ぶキー。
pub(crate) const SELECT_KEYS: &str = "asdfjkl";
pub(crate) const PAGE: usize = SELECT_KEYS.len();

/// 打鍵の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum K {
    /// 文字（Shift を反映済み。Ctrl / Alt なし）。
    Char(char),
    CtrlJ,
    CtrlG,
    Enter,
    Backspace,
    Esc,
    /// そのほか。`chord` は Ctrl / Alt つき（SKK は使わず、エディタに渡す）。
    Other { chord: bool },
}

pub(crate) fn classify(ev: &KeyEvent) -> K {
    let m = ev.mods;
    if m.ctrl() || m.alt() {
        if m.ctrl() && !m.alt() {
            match ev.key {
                Key::Char('j') => return K::CtrlJ,
                Key::Char('g') => return K::CtrlG,
                _ => {}
            }
        }
        return K::Other { chord: true };
    }
    match ev.key {
        Key::Enter => K::Enter,
        Key::Backspace => K::Backspace,
        Key::Esc => K::Esc,
        Key::Char(c) => K::Char(ev.text.unwrap_or(if m.shift() { c.to_ascii_uppercase() } else { c })),
        _ => K::Other { chord: false },
    }
}

/// 状態機械が触るもの（個人辞書・版の数え・学習の記録）。
pub(crate) struct Ctx<'a> {
    pub user: &'a mut UserDict,
    pub next_version: &'a mut u64,
    pub changes: &'a mut Vec<LearnedWord>,
    pub learned: bool,
}

impl Ctx<'_> {
    fn version(&mut self) -> Version {
        *self.next_version += 1;
        Version(*self.next_version)
    }
    /// 確定・登録した語を個人辞書に入れ、書き戻す変更として記録する。
    fn learn(&mut self, key: &str, okuri: bool, word: &str) {
        let form = self.user.learn_form(key, okuri, word);
        // EUC-JP にできないもの（絵文字など）は、この起動の間だけ覚える
        if to_euc(key).is_some() && to_euc(&form).is_some() {
            self.changes.push(LearnedWord { reading: key.to_string(), word: form });
            self.learned = true;
        }
    }
}

fn eat() -> SkkOutput {
    SkkOutput { consumed: true, ..Default::default() }
}

fn eat_commit(commit: String) -> SkkOutput {
    SkkOutput { consumed: true, commit, ..Default::default() }
}

fn pass(commit: String) -> SkkOutput {
    SkkOutput { consumed: false, commit, ..Default::default() }
}

/// 送り仮名（▽ の中で大文字を打ってから）。
#[derive(Clone, Debug)]
pub(crate) struct Okuri {
    /// 辞書を引くときの英小文字（`かk` の `k`）。
    ch: char,
    /// 送り仮名（ひらがな）。
    kana: String,
}

/// ▼ 候補選択（`pending` が `Some` の間は辞書の結果待ち）。
#[derive(Clone, Debug)]
pub(crate) struct Conv {
    base: String,
    okuri: Option<Okuri>,
    list: CandidateList,
    pending: Option<Version>,
}

impl Conv {
    fn key(&self) -> String {
        match &self.okuri {
            Some(o) => format!("{}{}", self.base, o.ch),
            None => self.base.clone(),
        }
    }
    fn okuri_kana(&self) -> &str {
        self.okuri.as_ref().map_or("", |o| o.kana.as_str())
    }
    fn select(&mut self, i: usize) {
        self.list.selected = i;
        self.list.page_start = i / PAGE * PAGE;
    }
}

/// 辞書登録欄。
pub(crate) struct Reg {
    /// 登録をやめたときに戻る ▼（候補が無ければ ▽ へ戻る）。
    conv: Conv,
    /// 登録欄の中の入力（入れ子の SKK）。
    pub(crate) inner: Engine,
    pub(crate) view: Registration,
}

enum St {
    Direct,
    /// ▽ 読み入力中。`okuri` が `Some` なら送り仮名を入力中。
    Reading { text: String, okuri: Option<Okuri> },
    Conv(Conv),
    Reg(Box<Reg>),
}

pub(crate) struct Engine {
    pub(crate) mode: InputMode,
    romaji: Romaji,
    st: St,
    pub(crate) preedit: Preedit,
}

impl Engine {
    pub(crate) fn new(mode: InputMode) -> Engine {
        Engine { mode, romaji: Romaji::default(), st: St::Direct, preedit: Preedit::default() }
    }

    /// ひらがなを今のモードの形にする。
    fn disp(&self, hira: &str) -> String {
        if self.mode == InputMode::Katakana {
            to_katakana(hira)
        } else {
            hira.to_string()
        }
    }

    fn kana_str(&self, k: &Kana) -> String {
        if self.mode == InputMode::Katakana {
            k.kata.clone()
        } else {
            k.hira.clone()
        }
    }

    /// 未確定のローマ字を片づけて、確定する文字列（`n` だけ「ん」になる）。
    fn flush_commit(&mut self) -> String {
        let mut k = Kana::default();
        self.romaji.flush(&mut k);
        self.kana_str(&k)
    }

    /// 未確定のローマ字を片づけて読みに足す。
    fn flush_into(&mut self, text: &mut String) {
        let mut k = Kana::default();
        self.romaji.flush(&mut k);
        text.push_str(&k.hira);
    }

    /// 打鍵を 1 つ処理する。
    pub(crate) fn handle(&mut self, ev: &KeyEvent, k: K, cx: &mut Ctx) -> SkkOutput {
        match std::mem::replace(&mut self.st, St::Direct) {
            St::Direct => self.on_direct(k, cx),
            St::Reading { text, okuri: None } => self.on_reading(k, text, cx),
            St::Reading { text, okuri: Some(o) } => self.on_okuri(k, text, o, cx),
            St::Conv(c) => self.on_conv(ev, k, c, cx),
            St::Reg(r) => self.on_reg(ev, k, r, cx),
        }
    }

    fn on_direct(&mut self, k: K, cx: &mut Ctx) -> SkkOutput {
        if self.mode == InputMode::Ascii {
            if k == K::CtrlJ {
                self.mode = InputMode::Hiragana;
                return eat();
            }
            return pass(String::new());
        }
        match k {
            K::CtrlJ => eat_commit(self.flush_commit()),
            K::CtrlG | K::Esc => {
                if self.romaji.is_empty() {
                    pass(String::new())
                } else {
                    self.romaji.clear();
                    eat()
                }
            }
            K::Backspace => match self.romaji.pop() {
                Some(_) => eat(),
                None => pass(String::new()),
            },
            K::Enter | K::Other { .. } => pass(self.flush_commit()),
            K::Char('l') => {
                let c = self.flush_commit();
                self.mode = InputMode::Ascii;
                eat_commit(c)
            }
            K::Char('q') => {
                let c = self.flush_commit();
                self.mode = if self.mode == InputMode::Katakana { InputMode::Hiragana } else { InputMode::Katakana };
                eat_commit(c)
            }
            // 全角英数モードは無い
            K::Char('L') => eat(),
            K::Char('Q') => {
                let c = self.flush_commit();
                self.st = St::Reading { text: String::new(), okuri: None };
                eat_commit(c)
            }
            // 大文字: ▽ を始める（未確定のローマ字はそのまま続ける。`kA` → ▽か）
            K::Char(c) if c.is_ascii_uppercase() => self.on_reading(k, String::new(), cx),
            K::Char(c) => {
                let mut kana = Kana::default();
                let rest = self.romaji.feed(c, &mut kana);
                let commit = self.kana_str(&kana);
                match rest {
                    None => eat_commit(commit),
                    // ローマ字でない字（数字・`/` など）はエディタにそのまま渡す
                    Some(_) => pass(commit),
                }
            }
        }
    }

    fn on_reading(&mut self, k: K, mut text: String, cx: &mut Ctx) -> SkkOutput {
        match k {
            K::CtrlJ | K::Enter => {
                self.flush_into(&mut text);
                eat_commit(self.disp(&text))
            }
            K::CtrlG | K::Esc => {
                self.romaji.clear();
                eat()
            }
            K::Backspace => {
                if self.romaji.pop().is_none() && text.pop().is_none() {
                    return eat(); // 空の ▽ を消す
                }
                self.st = St::Reading { text, okuri: None };
                eat()
            }
            K::Other { .. } => {
                self.flush_into(&mut text);
                pass(self.disp(&text))
            }
            K::Char(' ') => {
                self.flush_into(&mut text);
                if text.is_empty() {
                    self.st = St::Reading { text, okuri: None };
                    return eat();
                }
                self.start_conv(text, None, cx)
            }
            K::Char('l') => {
                self.flush_into(&mut text);
                let c = self.disp(&text);
                self.mode = InputMode::Ascii;
                eat_commit(c)
            }
            K::Char('q') => {
                self.flush_into(&mut text);
                let c = if self.mode == InputMode::Katakana { text } else { to_katakana(&text) };
                eat_commit(c)
            }
            K::Char('L') | K::Char('Q') => {
                self.st = St::Reading { text, okuri: None };
                eat()
            }
            // ▽ の中の大文字: 送り仮名の始まり
            K::Char(c) if c.is_ascii_uppercase() && !text.is_empty() => {
                self.flush_into(&mut text);
                let ch = c.to_ascii_lowercase();
                self.okuri_feed(text, Okuri { ch, kana: String::new() }, ch, cx)
            }
            K::Char(c) => {
                let mut kana = Kana::default();
                let rest = self.romaji.feed(c.to_ascii_lowercase(), &mut kana);
                text.push_str(&kana.hira);
                if let Some(p) = rest {
                    text.push(p);
                }
                self.st = St::Reading { text, okuri: None };
                eat()
            }
        }
    }

    fn okuri_feed(&mut self, text: String, mut o: Okuri, c: char, cx: &mut Ctx) -> SkkOutput {
        let mut kana = Kana::default();
        // ローマ字にならない字は無視する
        let _ = self.romaji.feed(c, &mut kana);
        o.kana.push_str(&kana.hira);
        if self.romaji.is_empty() && !o.kana.is_empty() {
            return self.start_conv(text, Some(o), cx);
        }
        self.st = St::Reading { text, okuri: Some(o) };
        eat()
    }

    fn on_okuri(&mut self, k: K, text: String, mut o: Okuri, cx: &mut Ctx) -> SkkOutput {
        match k {
            K::CtrlG | K::Esc => {
                self.romaji.clear();
                eat()
            }
            K::Backspace => {
                // 送り仮名をやめて ▽ に戻る
                self.romaji.clear();
                self.st = St::Reading { text, okuri: None };
                eat()
            }
            K::CtrlJ | K::Enter | K::Other { .. } => {
                self.flush_into(&mut o.kana);
                let c = self.disp(&format!("{text}{}", o.kana));
                if matches!(k, K::Other { .. }) {
                    pass(c)
                } else {
                    eat_commit(c)
                }
            }
            K::Char(c) => self.okuri_feed(text, o, c.to_ascii_lowercase(), cx),
        }
    }

    fn start_conv(&mut self, base: String, okuri: Option<Okuri>, cx: &mut Ctx) -> SkkOutput {
        let version = cx.version();
        let c = Conv { base, okuri, list: CandidateList::default(), pending: Some(version) };
        let req = LookupRequest { reading: c.key(), okuri: c.okuri.is_some(), version };
        self.st = St::Conv(c);
        SkkOutput { consumed: true, lookup: Some(req), ..Default::default() }
    }

    fn back_to_reading(&mut self, c: Conv) {
        let text = format!("{}{}", c.base, c.okuri_kana());
        self.st = St::Reading { text, okuri: None };
    }

    /// ▼ の今の候補を確定する（学習する）。確定する文字列を返す。状態は直接入力へ。
    fn commit_conv(&mut self, c: Conv, cx: &mut Ctx) -> String {
        let word = c.list.items[c.list.selected].text.clone();
        cx.learn(&c.key(), c.okuri.is_some(), &word);
        format!("{word}{}", self.disp(c.okuri_kana()))
    }

    fn open_reg(&mut self, c: Conv) {
        let view = Registration { reading: reg_reading(&c), text: String::new() };
        self.st = St::Reg(Box::new(Reg { conv: c, inner: Engine::new(InputMode::Hiragana), view }));
    }

    fn on_conv(&mut self, ev: &KeyEvent, k: K, mut c: Conv, cx: &mut Ctx) -> SkkOutput {
        if c.pending.is_some() {
            // 結果待ち（ふつうは Skk が打鍵を取っておくので、ここには来ない）
            match k {
                K::CtrlG | K::Esc => self.back_to_reading(c),
                _ => self.st = St::Conv(c),
            }
            return eat();
        }
        let n = c.list.items.len();
        match k {
            K::Char(' ') => {
                if c.list.selected + 1 >= n {
                    self.open_reg(c); // 最後まで送った: 辞書登録
                } else {
                    c.select(c.list.selected + 1);
                    self.st = St::Conv(c);
                }
                eat()
            }
            K::Char('x') => {
                if c.list.selected == 0 {
                    self.back_to_reading(c);
                } else {
                    c.select(c.list.selected - 1);
                    self.st = St::Conv(c);
                }
                eat()
            }
            K::Char(ch) if SELECT_KEYS.contains(ch) => {
                let i = c.list.page_start + SELECT_KEYS.find(ch).unwrap_or(0);
                if i < n {
                    c.select(i);
                    eat_commit(self.commit_conv(c, cx))
                } else {
                    self.st = St::Conv(c);
                    eat()
                }
            }
            K::CtrlJ | K::Enter => eat_commit(self.commit_conv(c, cx)),
            K::CtrlG | K::Esc | K::Backspace => {
                self.back_to_reading(c);
                eat()
            }
            K::Other { .. } => pass(self.commit_conv(c, cx)),
            // ほかの文字: 確定してから、その文字を直接入力として処理する
            K::Char(_) => {
                let s = self.commit_conv(c, cx);
                let mut o = self.handle(ev, k, cx);
                o.commit = s + &o.commit;
                o
            }
        }
    }

    fn on_reg(&mut self, ev: &KeyEvent, k: K, mut r: Box<Reg>, cx: &mut Ctx) -> SkkOutput {
        let o = r.inner.handle(ev, k, cx);
        r.view.text.push_str(&o.commit);
        let mut out = SkkOutput { consumed: true, lookup: o.lookup, ..Default::default() };
        if o.consumed {
            self.st = St::Reg(r);
            return out;
        }
        match k {
            K::Enter => {
                let word = std::mem::take(&mut r.view.text);
                let c = r.conv;
                if word.is_empty() {
                    self.abort_reg(c);
                } else {
                    cx.learn(&c.key(), c.okuri.is_some(), &word);
                    out.commit = format!("{word}{}", self.disp(c.okuri_kana()));
                }
            }
            K::CtrlG | K::Esc => self.abort_reg(r.conv),
            K::Backspace => {
                r.view.text.pop();
                self.st = St::Reg(r);
            }
            K::Char(ch) => {
                r.view.text.push(ch);
                self.st = St::Reg(r);
            }
            K::CtrlJ => self.st = St::Reg(r),
            K::Other { chord } => {
                out.consumed = !chord;
                self.st = St::Reg(r);
            }
        }
        out
    }

    /// 登録をやめる: 候補があれば最後の候補の ▼ へ、無ければ ▽ へ。
    fn abort_reg(&mut self, mut c: Conv) {
        let n = c.list.items.len();
        if n == 0 {
            self.back_to_reading(c);
        } else {
            c.select(n - 1);
            self.st = St::Conv(c);
        }
    }

    // --- 辞書の結果・問い合わせ（入れ子をたどる） ---

    /// いちばん内側の入力欄。
    pub(crate) fn active(&self) -> &Engine {
        match &self.st {
            St::Reg(r) => r.inner.active(),
            _ => self,
        }
    }

    pub(crate) fn active_mut(&mut self) -> &mut Engine {
        if !matches!(self.st, St::Reg(_)) {
            return self;
        }
        match &mut self.st {
            St::Reg(r) => r.inner.active_mut(),
            _ => unreachable!(),
        }
    }

    /// いちばん内側の辞書登録欄。
    pub(crate) fn innermost_reg(&self) -> Option<&Reg> {
        match &self.st {
            St::Reg(r) => r.inner.innermost_reg().or(Some(r)),
            _ => None,
        }
    }

    /// 辞書の結果を待っているか（いちばん内側）。
    pub(crate) fn waiting(&self) -> bool {
        match &self.active().st {
            St::Conv(c) => c.pending.is_some(),
            _ => false,
        }
    }

    /// 結果待ちをやめて ▽ に戻る。
    pub(crate) fn cancel_wait(&mut self) {
        let e = self.active_mut();
        if let St::Conv(c) = std::mem::replace(&mut e.st, St::Direct) {
            e.back_to_reading(c);
        }
    }

    /// 候補一覧（いちばん内側の ▼）。
    pub(crate) fn candidates(&self) -> Option<&CandidateList> {
        match &self.active().st {
            St::Conv(c) if c.pending.is_none() && !c.list.items.is_empty() => Some(&c.list),
            _ => None,
        }
    }

    /// 辞書の結果を渡す。待っている版でなければ `false`。個人辞書の候補を先に、
    /// 同じ候補は 1 つにまとめる。候補が無ければ辞書登録を開く。
    pub(crate) fn deliver(&mut self, v: Version, sys: Vec<Candidate>, user: &UserDict) -> bool {
        let e = self.active_mut();
        let St::Conv(c) = &mut e.st else { return false };
        if c.pending != Some(v) {
            return false;
        }
        let mut items = user.lookup(&c.key(), c.okuri.is_some());
        merge(&mut items, sys);
        c.pending = None;
        c.list = CandidateList { items, selected: 0, page_start: 0 };
        if c.list.items.is_empty() {
            if let St::Conv(c) = std::mem::replace(&mut e.st, St::Direct) {
                e.open_reg(c);
            }
        }
        true
    }

    /// 表示用の情報（未確定文字列・登録欄）を作り直す。
    pub(crate) fn refresh(&mut self) {
        let pending = self.romaji.pending().to_string();
        let (state, text, okuri_at) = match &self.st {
            St::Direct => (PreeditState::Direct, pending, None),
            St::Reading { text, okuri: None } => (PreeditState::Reading, self.disp(text) + &pending, None),
            St::Reading { text, okuri: Some(o) } => {
                let t = self.disp(text);
                let at = t.len();
                (PreeditState::Reading, t + &self.disp(&o.kana) + &pending, Some(at))
            }
            St::Conv(c) => {
                let head = if c.pending.is_some() || c.list.items.is_empty() {
                    self.disp(&c.base)
                } else {
                    c.list.items[c.list.selected].text.clone()
                };
                let at = head.len();
                (PreeditState::Converting, head + &self.disp(c.okuri_kana()), c.okuri.as_ref().map(|_| at))
            }
            St::Reg(r) => {
                let head = self.disp(&r.conv.base);
                let at = head.len();
                (PreeditState::Converting, head + &self.disp(r.conv.okuri_kana()), r.conv.okuri.as_ref().map(|_| at))
            }
        };
        self.preedit = Preedit { state, text, okuri_at };
        if let St::Reg(r) = &mut self.st {
            r.inner.refresh();
        }
    }
}

/// 登録欄に出す読み（送りありは `か*く` の形）。
fn reg_reading(c: &Conv) -> String {
    match &c.okuri {
        Some(o) => format!("{}*{}", c.base, o.kana),
        None => c.base.clone(),
    }
}

/// 候補を後ろに足す（同じ文字列は足さない。注釈は先のものに無ければ写す）。
pub(crate) fn merge(items: &mut Vec<Candidate>, more: Vec<Candidate>) {
    for c in more {
        match items.iter_mut().find(|e| e.text == c.text) {
            Some(e) => {
                if e.annotation.is_none() {
                    e.annotation = c.annotation;
                }
            }
            None => items.push(c),
        }
    }
}
