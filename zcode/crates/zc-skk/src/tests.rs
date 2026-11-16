//! 状態機械と辞書の試験。辞書は試験の中で EUC-JP の小さなファイルを作る
//! （SKK-JISYO.L と同じ形・同じ並び: 送りありは降順、送りなしは昇順）。

use super::*;
use crate::dict::{decode_concat, quote_word, to_euc};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use zc_core::{Key, Mods};

// --- 辞書を作る -------------------------------------------------------------

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn temp_file(bytes: &[u8]) -> TempFile {
    static N: AtomicU32 = AtomicU32::new(0);
    let p = std::env::temp_dir().join(format!("zc-skk-test-{}-{}.jisyo", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&p, bytes).unwrap();
    TempFile(p)
}

/// 送りありは降順、送りなしは昇順に並べた辞書（EUC-JP）。`comment` なら送りなしの区画の
/// 途中にコメント行を挟む。
fn make_dict(ari: &[(&str, &str)], nasi: &[(&str, &str)], comment: bool) -> Vec<u8> {
    let enc = |v: &[(&str, &str)]| -> Vec<(Vec<u8>, Vec<u8>)> {
        v.iter().map(|(r, b)| (to_euc(r).unwrap(), to_euc(&format!("{r} {b}")).unwrap())).collect()
    };
    let mut a = enc(ari);
    let mut n = enc(nasi);
    a.sort_by(|x, y| y.0.cmp(&x.0));
    n.sort_by(|x, y| x.0.cmp(&y.0));
    let mut out = to_euc(";; -*- mode: fundamental; coding: euc-jp -*-\n;; 試験用の辞書\n;; okuri-ari entries.\n").unwrap();
    for (_, l) in &a {
        out.extend_from_slice(l);
        out.push(b'\n');
    }
    out.extend_from_slice(b";; okuri-nasi entries.\n");
    for (i, (_, l)) in n.iter().enumerate() {
        if comment && i == n.len() / 2 {
            out.extend_from_slice(b";; a comment in the middle\n\n");
        }
        out.extend_from_slice(l);
        out.push(b'\n');
    }
    out
}

const ARI: &[(&str, &str)] = &[
    ("かk", "/書/描/掻/[く/書/描/]/[け/書/]/"),
    ("かt", "/勝/買/"),
    ("つかu", "/使/遣/"),
    ("しn", "/死/"),
    ("おくr", "/送/贈/"),
    ("あいs", "/愛/"),
    ("わらu", "/笑/嗤/"),
];

const NASI: &[(&str, &str)] = &[
    ("1", "/１/一/"),
    ("cpu", "/CPU/"),
    ("あ", "/亜/阿/"),
    ("かーど", "/カード/"),
    ("かんじ", "/漢字/感じ/幹事/監事/莞爾/完治/寛治/閑事/換字/"),
    ("すらっしゅ", "/(concat \"a\\057b\")/スラッシュ/"),
    ("てんめつ", "/点滅/明滅;めいめつ/天滅/点めつ/"),
    ("ぷろぐらむ", "/(skk-gadget \"x\")/プログラム;program/"),
    ("にほん", "/日本/二本/"),
    ("をんな", "/女/"),
];

fn sys_dict() -> (TempFile, SystemDict) {
    let f = temp_file(&make_dict(ARI, NASI, true));
    let d = SystemDict::open(&f.0).unwrap();
    (f, d)
}

fn texts(c: &[Candidate]) -> Vec<&str> {
    c.iter().map(|c| c.text.as_str()).collect()
}

// --- 打鍵 -----------------------------------------------------------------

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(Key::Char(c), Mods::CTRL)
}

fn key(k: Key) -> KeyEvent {
    KeyEvent::new(k, Mods::NONE)
}

/// 試験の場: 辞書を引きながら打鍵し、確定した文字列を貯める。
struct T {
    skk: Skk,
    dict: SystemDict,
    _file: TempFile,
    text: String,
    /// 最後の打鍵の結果。
    last: SkkOutput,
}

impl T {
    fn new() -> T {
        T::with_user(UserDict::new())
    }
    fn with_user(user: UserDict) -> T {
        let (f, d) = sys_dict();
        let mut t = T { skk: Skk::new(user), dict: d, _file: f, text: String::new(), last: SkkOutput::default() };
        t.press(ctrl('j'));
        assert_eq!(t.skk.mode(), InputMode::Hiragana);
        t
    }
    fn press(&mut self, ev: KeyEvent) -> SkkOutput {
        let o = self.skk.handle_key_with(&ev, Some(&self.dict));
        self.text.push_str(&o.commit);
        self.last = o.clone();
        o
    }
    fn typ(&mut self, s: &str) {
        for c in s.chars() {
            self.press(KeyEvent::char(c));
        }
    }
    fn pre(&self) -> (PreeditState, &str) {
        let p = self.skk.preedit();
        (p.state, p.text.as_str())
    }
    fn cands(&self) -> Vec<&str> {
        self.skk.candidates().map(|l| texts(&l.items)).unwrap_or_default()
    }
}

// --- システム辞書 -------------------------------------------------------------

#[test]
fn system_dict_finds_every_entry_and_misses() {
    let (_f, d) = sys_dict();
    for (r, _) in ARI {
        assert!(!d.lookup(r, true).is_empty(), "送りあり {r}");
        assert!(d.lookup(r, false).is_empty(), "区画が違う {r}");
    }
    for (r, _) in NASI {
        assert!(!d.lookup(r, false).is_empty(), "送りなし {r}");
    }
    // 最初と最後の項目（送りありは降順なので「わらu」が先頭、「あいs」が最後）
    assert_eq!(texts(&d.lookup("わらu", true)), ["笑", "嗤"]);
    assert_eq!(texts(&d.lookup("あいs", true)), ["愛"]);
    assert_eq!(texts(&d.lookup("1", false)), ["１", "一"]);
    assert_eq!(texts(&d.lookup("をんな", false)), ["女"]);
    // 無い読み: 先頭より前・最後より後・間
    for r in ["", "0", "ああああ", "ん", "かんじい", "かん", "zzz"] {
        assert!(d.lookup(r, false).is_empty(), "{r}");
    }
    for r in ["あs", "んk", "かs", "ぁk"] {
        assert!(d.lookup(r, true).is_empty(), "{r}");
    }
    // EUC-JP にできない読み
    assert!(d.lookup("😀", false).is_empty());
}

#[test]
fn system_dict_candidates_annotations_and_lisp() {
    let (_f, d) = sys_dict();
    let c = d.lookup("てんめつ", false);
    assert_eq!(texts(&c), ["点滅", "明滅", "天滅", "点めつ"]);
    assert_eq!(c[1].annotation.as_deref(), Some("めいめつ"));
    assert_eq!(c[0].annotation, None);
    // [送り仮名/…/] の塊は飛ばす
    assert_eq!(texts(&d.lookup("かk", true)), ["書", "描", "掻"]);
    // concat は展開、ほかの式は出さない
    assert_eq!(texts(&d.lookup("すらっしゅ", false)), ["a/b", "スラッシュ"]);
    let p = d.lookup("ぷろぐらむ", false);
    assert_eq!(texts(&p), ["プログラム"]);
    assert_eq!(p[0].annotation.as_deref(), Some("program"));
    d.prefetch();
}

#[test]
fn system_dict_edge_files() {
    // 見出しの無いファイルは全部を送りなしとみなす
    let f = temp_file(&to_euc("あ /亜/\nい /伊/\nう /宇/\n").unwrap());
    let d = SystemDict::open(&f.0).unwrap();
    assert_eq!(texts(&d.lookup("い", false)), ["伊"]);
    assert_eq!(texts(&d.lookup("う", false)), ["宇"]);
    assert!(d.lookup("い", true).is_empty());
    // 最後の行に改行が無い・CRLF
    let f2 = temp_file(&to_euc(";; okuri-ari entries.\r\nかk /書/\r\n;; okuri-nasi entries.\r\nあ /亜/\r\nい /伊/").unwrap());
    let d2 = SystemDict::open(&f2.0).unwrap();
    assert_eq!(texts(&d2.lookup("い", false)), ["伊"]);
    assert_eq!(texts(&d2.lookup("かk", true)), ["書"]);
    // 空・無いファイル
    let f3 = temp_file(b"");
    assert!(SystemDict::open(&f3.0).is_err());
    assert!(SystemDict::open(std::path::Path::new("/nonexistent/skk")).is_err());
    // 大きめの辞書: 全部の項目が二分探索で見つかる
    let mut ari = Vec::new();
    let mut nasi = Vec::new();
    let kana: Vec<char> = "あいうえおかきくけこさしすせそたちつてとなにぬねのはひふへほまみむめもやゆよらりるれろわをん".chars().collect();
    for (i, a) in kana.iter().enumerate() {
        for b in kana.iter().skip(i % 3).step_by(3) {
            nasi.push((format!("{a}{b}"), format!("/{a}{b}候補/")));
            ari.push((format!("{a}{b}k"), format!("/{a}{b}送/")));
        }
    }
    fn r(v: &[(String, String)]) -> Vec<(&str, &str)> {
        v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
    }
    let f4 = temp_file(&make_dict(&r(&ari), &r(&nasi), true));
    let d4 = SystemDict::open(&f4.0).unwrap();
    for (k, _) in &nasi {
        assert_eq!(texts(&d4.lookup(k, false)), [format!("{k}候補").as_str()]);
    }
    for (k, _) in &ari {
        assert_eq!(d4.lookup(k, true).len(), 1, "{k}");
    }
    fn is_send_sync<T: Send + Sync>() {}
    is_send_sync::<SystemDict>();
}

#[test]
fn concat_and_quote() {
    assert_eq!(decode_concat("(concat \"a\\057b\")").as_deref(), Some("a/b"));
    assert_eq!(decode_concat("(concat \"x\" \"y\\073z\")").as_deref(), Some("xy;z"));
    assert_eq!(decode_concat("(concat \"\\\"q\\\"\")").as_deref(), Some("\"q\""));
    assert_eq!(decode_concat("(concat \"a\" foo)"), None);
    assert_eq!(decode_concat("(concat \"open"), None);
    for w in ["a/b", "x;y", "\"q\"", "c:\\dir", "(a b)", "[x"] {
        let q = quote_word(w);
        assert!(!q.contains('/') && !q.contains(';'), "{q}");
        assert_eq!(decode_concat(&q).as_deref(), Some(w), "{q}");
    }
    assert_eq!(quote_word("漢字"), "漢字");
    assert_eq!(quote_word("(株)"), "(株)");
}

// --- 個人辞書 ---------------------------------------------------------------

fn uim_sample() -> Vec<u8> {
    to_euc(concat!(
        ";; okuri-ari entries.\n",
        "かえs /返/[す/返/]/帰/\n",
        "おくr /贈/\n",
        ";; okuri-nasi entries.\n",
        "てんめつ /天滅/点滅;てんめつ/\n",
        "にほん /二本/\n",
        "こわれ\n",
    ))
    .unwrap()
}

#[test]
fn user_dict_parse_lookup_learn() {
    let mut u = UserDict::parse(&uim_sample());
    assert_eq!(texts(&u.lookup("かえs", true)), ["返", "帰"]);
    assert_eq!(texts(&u.lookup("てんめつ", false)), ["天滅", "点滅"]);
    assert_eq!(u.lookup("てんめつ", false)[1].annotation.as_deref(), Some("てんめつ"));
    assert!(u.lookup("こわれ", false).is_empty());
    assert!(!u.is_dirty());
    // 既にある語は先頭へ、無い語は足す
    u.learn("てんめつ", false, "点滅");
    assert_eq!(texts(&u.lookup("てんめつ", false)), ["点滅", "天滅"]);
    u.learn("かえs", true, "変");
    assert_eq!(texts(&u.lookup("かえs", true)), ["変", "返", "帰"]);
    assert!(u.is_dirty());
    // 書き出して読み直しても同じ。新しく学習した項目が区画の先頭
    let b = u.to_bytes();
    assert!(!u.is_dirty());
    let s = encoding_rs::EUC_JP.decode(&b).0.into_owned();
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(
        lines,
        [
            ";; okuri-ari entries.",
            "かえs /変/返/[す/返/]/帰/",
            "おくr /贈/",
            ";; okuri-nasi entries.",
            "てんめつ /点滅;てんめつ/天滅/",
            "にほん /二本/",
        ]
    );
    let u2 = UserDict::parse(&b);
    assert_eq!(texts(&u2.lookup("かえs", true)), ["変", "返", "帰"]);
    // 見出しの無いファイル
    let u3 = UserDict::parse(&to_euc("かくs /隠/\nあい /愛/\n").unwrap());
    assert_eq!(texts(&u3.lookup("かくs", true)), ["隠"]);
    assert_eq!(texts(&u3.lookup("あい", false)), ["愛"]);
    // ごみでも panic しない
    for junk in [&b"\xff\xfe\n/ /\n["[..], b"a", b"\n\n", b"x /[/", b";; okuri-nasi entries."] {
        let _ = UserDict::parse(junk).to_bytes();
    }
}

// --- 状態機械 ---------------------------------------------------------------

#[test]
fn ascii_mode_passes_everything_but_ctrl_j() {
    let mut s = Skk::new(UserDict::new());
    assert_eq!(s.mode(), InputMode::Ascii);
    for ev in [KeyEvent::char('a'), KeyEvent::char('A'), key(Key::Enter), ctrl('s'), ctrl('g'), key(Key::Backspace)] {
        let o = s.handle_key(&ev);
        assert!(!o.consumed && o.commit.is_empty(), "{ev:?}");
    }
    let o = s.handle_key(&ctrl('j'));
    assert!(o.consumed);
    assert_eq!(s.mode(), InputMode::Hiragana);
}

#[test]
fn direct_input_romaji_and_passthrough() {
    let mut t = T::new();
    t.typ("k");
    assert_eq!(t.pre(), (PreeditState::Direct, "k"));
    assert!(t.last.consumed);
    t.typ("ana");
    assert_eq!(t.text, "かな");
    // 数字・`/` はエディタに通す
    let o = t.press(KeyEvent::char('1'));
    assert!(!o.consumed);
    // 記号は全角
    t.typ("!.,");
    assert_eq!(t.text, "かな！。、");
    // 未確定の n は Enter で「ん」になり、Enter はエディタへ
    t.typ("hon");
    let o = t.press(key(Key::Enter));
    assert_eq!((o.consumed, o.commit.as_str()), (false, "ん"));
    // Ctrl / Alt つきは使わない
    assert!(!t.press(ctrl('s')).consumed);
    assert!(!t.press(KeyEvent::new(Key::Char('b'), Mods::ALT)).consumed);
    // Ctrl+G: ローマ字の途中があれば捨てる、無ければエディタへ（行へ移動）
    t.typ("k");
    assert!(t.press(ctrl('g')).consumed);
    assert_eq!(t.pre(), (PreeditState::Direct, ""));
    assert!(!t.press(ctrl('g')).consumed);
    // Backspace も同じ
    t.typ("ky");
    assert!(t.press(key(Key::Backspace)).consumed);
    assert_eq!(t.pre().1, "k");
    assert!(t.press(key(Key::Backspace)).consumed);
    assert!(!t.press(key(Key::Backspace)).consumed);
    // text の無い打鍵（Shift つきの小文字キー）も大文字として扱う
    t.press(KeyEvent::new(Key::Char('a'), Mods::SHIFT));
    assert_eq!(t.pre(), (PreeditState::Reading, "あ"));
}

#[test]
fn q_and_l_switch_modes() {
    let mut t = T::new();
    t.typ("q");
    assert_eq!(t.skk.mode(), InputMode::Katakana);
    t.typ("kata");
    assert_eq!(t.text, "カタ");
    // カタカナの ▽ は表示がカタカナ、探索はひらがな
    let o = t.skk.handle_key(&KeyEvent::char('K'));
    assert!(o.lookup.is_none());
    t.skk.handle_key(&KeyEvent::char('a'));
    t.skk.handle_key(&KeyEvent::char('n'));
    t.skk.handle_key(&KeyEvent::char('j'));
    t.skk.handle_key(&KeyEvent::char('i'));
    assert_eq!(t.skk.preedit().text, "カンジ");
    let o = t.skk.handle_key(&KeyEvent::char(' '));
    assert_eq!(o.lookup.as_ref().map(|l| l.reading.as_str()), Some("かんじ"));
    t.skk.reset();
    // カタカナモードの ▽ で q はひらがなで確定
    t.text.clear();
    t.typ("Hiraq");
    assert_eq!(t.text, "ひら");
    t.typ("q");
    assert_eq!(t.skk.mode(), InputMode::Hiragana);
    // ひらがなの ▽ で q はカタカナで確定（モードはそのまま）
    t.text.clear();
    t.typ("Katakanaq");
    assert_eq!(t.text, "カタカナ");
    assert_eq!(t.skk.mode(), InputMode::Hiragana);
    // l で英数へ（▽ は確定）。英数では何も使わない
    t.text.clear();
    t.typ("Eil");
    assert_eq!(t.text, "えい");
    assert_eq!(t.skk.mode(), InputMode::Ascii);
    assert!(!t.press(KeyEvent::char('x')).consumed);
    t.press(ctrl('j'));
    assert_eq!(t.skk.mode(), InputMode::Hiragana);
    // set_mode(Ascii) は未確定を捨てる
    t.typ("Kan");
    t.skk.set_mode(InputMode::Ascii);
    assert_eq!(t.pre(), (PreeditState::Direct, ""));
}

#[test]
fn reading_then_enter_commits_kana() {
    let mut t = T::new();
    t.typ("Tenmetu");
    assert_eq!(t.pre(), (PreeditState::Reading, "てんめつ"));
    let o = t.press(key(Key::Enter));
    assert!(o.consumed, "▽ の Enter は確定だけ（改行しない）");
    assert_eq!(t.text, "てんめつ");
    assert_eq!(t.pre(), (PreeditState::Direct, ""));
    // 未確定の n も入る
    t.text.clear();
    t.typ("Hon");
    assert_eq!(t.pre().1, "ほn");
    t.press(ctrl('j'));
    assert_eq!(t.text, "ほん");
    assert_eq!(t.skk.mode(), InputMode::Hiragana);
}

#[test]
fn convert_and_commit_learns() {
    let mut t = T::new();
    t.typ("Tenmetu");
    let o = t.skk.handle_key(&KeyEvent::char(' '));
    let req = o.lookup.clone().unwrap();
    assert_eq!((req.reading.as_str(), req.okuri), ("てんめつ", false));
    // 結果が来るまでは読みのまま ▼
    assert_eq!(t.pre(), (PreeditState::Converting, "てんめつ"));
    assert!(t.skk.candidates().is_none());
    let sys = t.dict.lookup(&req.reading, req.okuri);
    assert!(t.skk.candidates_ready(Versioned::new(req.version, sys)));
    assert_eq!(t.pre(), (PreeditState::Converting, "点滅"));
    let l = t.skk.candidates().unwrap();
    assert_eq!((l.selected, l.page_start, l.items.len()), (0, 0, 4));
    assert_eq!(l.items[1].annotation.as_deref(), Some("めいめつ"));
    // Space で次
    t.press(KeyEvent::char(' '));
    assert_eq!(t.pre().1, "明滅");
    let o = t.press(key(Key::Enter));
    assert!(o.consumed && o.learned);
    assert_eq!(t.text, "明滅");
    assert_eq!(t.skk.take_changes(), [LearnedWord { reading: "てんめつ".into(), word: "明滅".into() }]);
    assert!(t.skk.take_changes().is_empty());
    // 学習した語が先頭に来る
    t.typ("Tenmetu ");
    assert_eq!(t.cands(), ["明滅", "点滅", "天滅", "点めつ"]);
    assert_eq!(t.skk.user_dict().lookup("てんめつ", false)[0].text, "明滅");
}

#[test]
fn okuri_lookup_and_commit() {
    let mut t = T::new();
    // KaKu: 送り仮名がそろった時点で「かk」を引く
    t.skk.handle_key(&KeyEvent::char('K'));
    t.skk.handle_key(&KeyEvent::char('a'));
    let o = t.skk.handle_key(&KeyEvent::char('K'));
    assert!(o.lookup.is_none());
    assert_eq!(t.skk.preedit().text, "かk");
    assert_eq!(t.skk.preedit().okuri_at, Some("か".len()));
    let o = t.skk.handle_key(&KeyEvent::char('u'));
    let req = o.lookup.unwrap();
    assert_eq!((req.reading.as_str(), req.okuri), ("かk", true));
    t.skk.candidates_ready(Versioned::new(req.version, t.dict.lookup("かk", true)));
    let p = t.skk.preedit().clone();
    assert_eq!((p.state, p.text.as_str(), p.okuri_at), (PreeditState::Converting, "書く", Some("書".len())));
    t.press(key(Key::Enter));
    assert_eq!(t.text, "書く");
    assert_eq!(t.skk.take_changes(), [LearnedWord { reading: "かk".into(), word: "書".into() }]);
    // 促音・撥音の送り仮名
    t.text.clear();
    t.typ("KaTta");
    assert_eq!(t.pre(), (PreeditState::Converting, "勝った"));
    t.press(key(Key::Enter));
    t.typ("ShiNda");
    assert_eq!(t.pre().1, "死んだ");
    t.press(key(Key::Enter));
    // 母音の送り仮名はその場で引く
    t.typ("TukaU");
    assert_eq!(t.pre().1, "使う");
    t.press(key(Key::Enter));
    assert_eq!(t.text, "勝った死んだ使う");
    // 送り仮名の途中の Backspace は ▽ に戻る、Ctrl+G は捨てる
    t.typ("KaK");
    t.press(key(Key::Backspace));
    assert_eq!(t.pre(), (PreeditState::Reading, "か"));
    assert_eq!(t.skk.preedit().okuri_at, None);
    t.typ("K");
    t.press(ctrl('g'));
    assert_eq!(t.pre(), (PreeditState::Direct, ""));
}

#[test]
fn paging_and_select_keys() {
    let mut t = T::new();
    t.typ("Kanji ");
    let all = ["漢字", "感じ", "幹事", "監事", "莞爾", "完治", "寛治", "閑事", "換字"];
    assert_eq!(t.cands(), all);
    // 7 つ進むと 2 ページ目
    for _ in 0..7 {
        t.press(KeyEvent::char(' '));
    }
    let l = t.skk.candidates().unwrap();
    assert_eq!((l.selected, l.page_start), (7, 7));
    assert_eq!(t.pre().1, "閑事");
    // 2 ページ目は 2 件だけ: d は無効
    assert!(t.press(KeyEvent::char('d')).consumed);
    assert_eq!(t.pre(), (PreeditState::Converting, "閑事"));
    // x で前へ（ページも戻る）
    t.press(KeyEvent::char('x'));
    let l = t.skk.candidates().unwrap();
    assert_eq!((l.selected, l.page_start), (6, 0));
    // asdfjkl でページの中から直接選んで確定
    t.press(KeyEvent::char('f'));
    assert_eq!(t.text, "監事");
    t.typ("Kanji ");
    // 学習した「監事」が先頭
    assert_eq!(t.cands()[0], "監事");
    t.press(KeyEvent::char('l'));
    assert_eq!(t.text, "監事寛治");
    assert_eq!(t.skk.mode(), InputMode::Hiragana, "選ぶキーの l はモードを変えない");
    // 最初の候補で x は ▽ に戻る
    t.typ("Nihon ");
    t.press(KeyEvent::char('x'));
    assert_eq!(t.pre(), (PreeditState::Reading, "にほん"));
}

#[test]
fn next_key_commits_current_candidate() {
    let mut t = T::new();
    t.typ("Tenmetu ");
    // k は選ぶキー（asdfjkl）なので、ページに無い番号なら何もしない
    assert!(t.press(KeyEvent::char('k')).consumed);
    assert_eq!(t.pre(), (PreeditState::Converting, "点滅"));
    let o = t.press(KeyEvent::char('t'));
    assert_eq!(o.commit, "点滅");
    assert!(o.consumed && o.learned);
    assert_eq!(t.pre(), (PreeditState::Direct, "t"));
    t.typ("a");
    assert_eq!(t.text, "点滅た");
    // 大文字は確定して ▽ を始める
    t.typ("Nihon Kanji");
    assert_eq!(t.text, "点滅た日本");
    assert_eq!(t.pre(), (PreeditState::Reading, "かんじ"));
    // 記号も確定してから
    t.typ(" .");
    assert_eq!(t.text, "点滅た日本漢字。");
    // ほかのキー（矢印・Ctrl+S）は確定してエディタへ
    t.typ("Nihon ");
    let o = t.press(key(Key::Right));
    assert_eq!((o.consumed, o.commit.as_str()), (false, "日本"));
    t.typ("Nihon ");
    let o = t.press(ctrl('s'));
    assert_eq!((o.consumed, o.commit.as_str()), (false, "日本"));
    // ▽ で矢印は読みを確定して通す
    t.typ("Nihon");
    let o = t.press(key(Key::Left));
    assert_eq!((o.consumed, o.commit.as_str()), (false, "にほん"));
}

#[test]
fn cancel_and_backspace_at_each_state() {
    let mut t = T::new();
    // ▽: Ctrl+G で捨てる
    t.typ("Kanj");
    t.press(ctrl('g'));
    assert_eq!(t.pre(), (PreeditState::Direct, ""));
    // ▽: Backspace はローマ字 → 読み → ▽ の順に消す
    t.typ("Kanj");
    t.press(key(Key::Backspace));
    assert_eq!(t.pre(), (PreeditState::Reading, "かん"));
    t.press(key(Key::Backspace));
    t.press(key(Key::Backspace));
    assert_eq!(t.pre(), (PreeditState::Reading, ""));
    assert!(t.press(key(Key::Backspace)).consumed);
    assert_eq!(t.pre(), (PreeditState::Direct, ""));
    // ▼: Ctrl+G は ▽ に戻る（もう一度で捨てる）。Esc・Backspace も同じ
    t.typ("Kanji ");
    t.press(ctrl('g'));
    assert_eq!(t.pre(), (PreeditState::Reading, "かんじ"));
    t.press(KeyEvent::char(' '));
    t.press(key(Key::Esc));
    assert_eq!(t.pre(), (PreeditState::Reading, "かんじ"));
    t.press(KeyEvent::char(' '));
    t.press(key(Key::Backspace));
    assert_eq!(t.pre(), (PreeditState::Reading, "かんじ"));
    t.press(ctrl('g'));
    assert_eq!(t.pre(), (PreeditState::Direct, ""));
    // 送りありの ▼ から戻ると、送り仮名は読みに入る
    t.typ("KaKu");
    t.press(ctrl('g'));
    assert_eq!(t.pre(), (PreeditState::Reading, "かく"));
    t.press(ctrl('g'));
    assert_eq!(t.text, "");
    assert!(t.skk.take_changes().is_empty(), "取り消しは学習しない");
}

#[test]
fn registration_with_nested_conversion() {
    let mut t = T::new();
    t.typ("Ka-dopyu-ta- ");
    // 候補が無いので登録欄が開く（本文は ▼ 読み）
    let r = t.skk.registration().unwrap().clone();
    assert_eq!((r.reading.as_str(), r.text.as_str()), ("かーどぴゅーたー", ""));
    assert_eq!(t.pre(), (PreeditState::Converting, "かーどぴゅーたー"));
    assert!(t.skk.candidates().is_none());
    assert_eq!(t.skk.mode(), InputMode::Hiragana);
    // 欄の中で変換（入れ子）: ▽かーど → ▼カード
    t.typ("Ka-do ");
    assert_eq!(t.cands(), ["カード"], "候補一覧は欄の中のもの");
    let rp = t.skk.registration_preedit().unwrap();
    assert_eq!((rp.state, rp.text.as_str()), (PreeditState::Converting, "カード"));
    // 次の大文字で確定して ▽ぴゅーた
    t.typ("Pyu-ta");
    assert_eq!(t.skk.registration().unwrap().text, "カード");
    assert_eq!(t.skk.registration_preedit().unwrap().text, "ぴゅーた");
    // q でカタカナにして確定、続けて「ー」
    t.typ("q-");
    assert_eq!(t.skk.registration().unwrap().text, "カードピューター");
    // Backspace は欄の字を消す
    t.typ("-");
    t.press(key(Key::Backspace));
    assert_eq!(t.skk.registration().unwrap().text, "カードピューター");
    assert_eq!(t.text, "", "登録が終わるまで本文には何も入らない");
    // Enter で登録して本文に確定
    let o = t.press(key(Key::Enter));
    assert!(o.consumed && o.learned);
    assert_eq!(t.text, "カードピューター");
    assert!(t.skk.registration().is_none());
    assert_eq!(t.pre(), (PreeditState::Direct, ""));
    let ch = t.skk.take_changes();
    assert_eq!(
        ch,
        [
            LearnedWord { reading: "かーど".into(), word: "カード".into() },
            LearnedWord { reading: "かーどぴゅーたー".into(), word: "カードピューター".into() },
        ]
    );
    // 次からは候補に出る
    t.typ("Ka-dopyu-ta- ");
    assert_eq!(t.cands(), ["カードピューター"]);
}

#[test]
fn registration_cancel_and_edge_cases() {
    let mut t = T::new();
    // 候補が無いとき: 欄の中の ▽ を Ctrl+G で消し、もう一度で登録をやめて ▽ へ
    t.typ("Hogera ");
    assert!(t.skk.registration().is_some());
    t.typ("Fu");
    t.press(ctrl('g'));
    assert!(t.skk.registration().is_some());
    assert_eq!(t.skk.registration_preedit().unwrap().text, "");
    t.press(ctrl('g'));
    assert!(t.skk.registration().is_none());
    assert_eq!(t.pre(), (PreeditState::Reading, "ほげら"));
    // 候補を最後まで送ると登録欄、やめると最後の候補の ▼
    t.press(ctrl('g'));
    t.typ("Nihon   ");
    assert!(t.skk.registration().is_some());
    t.press(key(Key::Esc));
    assert!(t.skk.registration().is_none());
    assert_eq!(t.pre(), (PreeditState::Converting, "二本"));
    let l = t.skk.candidates().unwrap();
    assert_eq!(l.selected, 1);
    // 空のまま Enter はやめるのと同じ
    t.press(KeyEvent::char(' '));
    assert!(t.skk.registration().is_some());
    t.press(key(Key::Enter));
    assert!(t.skk.registration().is_none());
    assert_eq!(t.pre().1, "二本");
    t.press(ctrl('g'));
    t.press(ctrl('g'));
    // 送りありの登録: 欄では英数（l）で打てる。Ctrl+S は欄を開いたまま通す
    t.text.clear();
    t.typ("HogeRu");
    let r = t.skk.registration().unwrap();
    assert_eq!(r.reading, "ほげ*る");
    t.typ("l");
    assert_eq!(t.skk.mode(), InputMode::Ascii, "欄の中のモード");
    t.typ("Hoge/1");
    assert!(!t.press(ctrl('s')).consumed);
    assert!(t.press(key(Key::Tab)).consumed);
    assert_eq!(t.skk.registration().unwrap().text, "Hoge/1");
    t.press(key(Key::Enter));
    assert_eq!(t.text, "Hoge/1る");
    assert_eq!(t.skk.mode(), InputMode::Hiragana, "本文のモードに戻る");
    assert_eq!(t.skk.take_changes(), [LearnedWord { reading: "ほげr".into(), word: "(concat \"Hoge\\0571\")".into() }]);
    t.typ("HogeRu");
    assert_eq!(t.pre(), (PreeditState::Converting, "Hoge/1る"));
}

#[test]
fn nested_registration_inside_registration() {
    let mut t = T::new();
    t.typ("Hogera ");
    t.typ("Fuga ");
    // 欄の中でも候補が無い → 2 段目の登録欄（表示はいちばん内側）
    assert_eq!(t.skk.registration().unwrap().reading, "ふが");
    t.typ("lfuga");
    t.press(key(Key::Enter));
    assert_eq!(t.skk.registration().unwrap().reading, "ほげら");
    assert_eq!(t.skk.registration().unwrap().text, "fuga");
    t.press(key(Key::Enter));
    assert_eq!(t.text, "fuga");
    let ch = t.skk.take_changes();
    assert_eq!(ch.len(), 2);
    assert_eq!(ch[1], LearnedWord { reading: "ほげら".into(), word: "fuga".into() });
}

#[test]
fn user_dict_first_and_dedup() {
    let mut t = T::with_user(UserDict::parse(&uim_sample()));
    t.typ("Tenmetu ");
    assert_eq!(t.cands(), ["天滅", "点滅", "明滅", "点めつ"]);
    let l = t.skk.candidates().unwrap();
    assert_eq!(l.items[1].annotation.as_deref(), Some("てんめつ"), "個人辞書の注釈が先");
    t.press(ctrl('g'));
    t.press(ctrl('g'));
    // 個人辞書にしか無い送りあり
    t.typ("KaeSu");
    assert_eq!(t.cands(), ["返", "帰"]);
}

#[test]
fn stale_results_are_dropped() {
    let mut s = Skk::new(UserDict::new());
    s.set_mode(InputMode::Hiragana);
    for c in "Kanji ".chars() {
        s.handle_key(&KeyEvent::char(c));
    }
    // 1 回目の依頼を出してから取り消し、もう一度 Space
    let o1 = {
        s.reset();
        for c in "Kanji".chars() {
            s.handle_key(&KeyEvent::char(c));
        }
        s.handle_key(&KeyEvent::char(' '))
    };
    let v1 = o1.lookup.unwrap().version;
    s.handle_key(&ctrl('g'));
    assert_eq!(s.preedit().state, PreeditState::Reading);
    let v2 = s.handle_key(&KeyEvent::char(' ')).lookup.unwrap().version;
    assert_ne!(v1, v2);
    let c = vec![Candidate { text: "漢字".into(), annotation: None }];
    assert!(!s.candidates_ready(Versioned::new(v1, c.clone())), "古い版は捨てる");
    assert!(s.candidates().is_none());
    assert!(s.candidates_ready(Versioned::new(v2, c.clone())));
    assert_eq!(s.preedit().text, "漢字");
    // 2 度目は受け取らない
    assert!(!s.candidates_ready(Versioned::new(v2, c.clone())));
    // reset の後に届いた結果も捨てる
    s.handle_key(&KeyEvent::char(' '));
    s.reset();
    for c in "Kanji".chars() {
        s.handle_key(&KeyEvent::char(c));
    }
    let v3 = s.handle_key(&KeyEvent::char(' ')).lookup.unwrap().version;
    s.reset();
    assert!(!s.candidates_ready(Versioned::new(v3, c)));
    assert_eq!(s.preedit(), &Preedit::default());
}

#[test]
fn keys_during_lookup_are_queued() {
    let (_f, d) = sys_dict();
    let mut s = Skk::new(UserDict::new());
    s.set_mode(InputMode::Hiragana);
    for c in "Tenmetu".chars() {
        s.handle_key(&KeyEvent::char(c));
    }
    let req = s.handle_key(&KeyEvent::char(' ')).lookup.unwrap();
    // 結果が来る前に s（2 番目の候補を選ぶ）と k
    let o = s.handle_key(&KeyEvent::char('s'));
    assert!(o.consumed && o.commit.is_empty());
    s.handle_key(&KeyEvent::char('k'));
    // Ctrl+S は取っておかずにエディタへ
    assert!(!s.handle_key(&ctrl('s')).consumed);
    assert!(s.candidates_ready(Versioned::new(req.version, d.lookup("てんめつ", false))));
    assert_eq!(s.preedit().text, "k");
    // 取っておいた打鍵で確定した分は次の打鍵で出る
    let o = s.handle_key(&KeyEvent::char('a'));
    assert_eq!(o.commit, "明滅か");
    assert!(o.learned);
}

#[test]
fn q_starts_empty_reading_and_symbols_in_reading() {
    let mut t = T::new();
    t.typ("Q");
    assert_eq!(t.pre(), (PreeditState::Reading, ""));
    t.typ("ka-do ");
    assert_eq!(t.pre(), (PreeditState::Converting, "カード"));
    t.press(key(Key::Enter));
    assert_eq!(t.text, "カード");
}
