//! ローマ字 → かな（SKK の標準の表。ddskk の `skk-rom-kana-base-rule-list` と
//! `skk-input-vector` に合わせる）。
//!
//! - 表に完全に一致したらかなを出す。表のどれかの頭に一致している間は待つ（未確定のローマ字）
//! - どれにも一致しなくなったら: 先頭が `n` なら「ん」、同じ子音が 2 つ続いたら「っ」を出して
//!   残りで続ける。それ以外は先頭の字を捨てる（ddskk と同じ）。最後に打った字だけが残って
//!   それも表に無ければ、その字は「ローマ字ではない」として呼ぶ側に返す
//! - `l`（英数へ）と `q`（ひらがな ⇔ カタカナ）は表に無い。状態機械が先に見る
//! - 小書きは `x` だけ（`la` などの `l` は英数モードへの切り替えなので使えない。SKK と同じ）

/// 表の 1 項目: ローマ字, ひらがな, カタカナ（ひらがなを機械的に直した形と違うときだけ）。
type Rule = (&'static str, &'static str, Option<&'static str>);

const fn r(rom: &'static str, hira: &'static str) -> Rule {
    (rom, hira, None)
}

#[rustfmt::skip]
static RULES: &[Rule] = &[
    r("a", "あ"), r("i", "い"), r("u", "う"), r("e", "え"), r("o", "お"),
    r("ka", "か"), r("ki", "き"), r("ku", "く"), r("ke", "け"), r("ko", "こ"),
    r("kya", "きゃ"), r("kyi", "きぃ"), r("kyu", "きゅ"), r("kye", "きぇ"), r("kyo", "きょ"),
    r("ga", "が"), r("gi", "ぎ"), r("gu", "ぐ"), r("ge", "げ"), r("go", "ご"),
    r("gya", "ぎゃ"), r("gyi", "ぎぃ"), r("gyu", "ぎゅ"), r("gye", "ぎぇ"), r("gyo", "ぎょ"),
    r("sa", "さ"), r("si", "し"), r("su", "す"), r("se", "せ"), r("so", "そ"),
    r("sya", "しゃ"), r("syi", "しぃ"), r("syu", "しゅ"), r("sye", "しぇ"), r("syo", "しょ"),
    r("sha", "しゃ"), r("shi", "し"), r("shu", "しゅ"), r("she", "しぇ"), r("sho", "しょ"),
    r("za", "ざ"), r("zi", "じ"), r("zu", "ず"), r("ze", "ぜ"), r("zo", "ぞ"),
    r("zya", "じゃ"), r("zyi", "じぃ"), r("zyu", "じゅ"), r("zye", "じぇ"), r("zyo", "じょ"),
    r("ja", "じゃ"), r("ji", "じ"), r("ju", "じゅ"), r("je", "じぇ"), r("jo", "じょ"),
    r("jya", "じゃ"), r("jyi", "じぃ"), r("jyu", "じゅ"), r("jye", "じぇ"), r("jyo", "じょ"),
    r("ta", "た"), r("ti", "ち"), r("tu", "つ"), r("te", "て"), r("to", "と"), r("tsu", "つ"),
    r("tya", "ちゃ"), r("tyi", "ちぃ"), r("tyu", "ちゅ"), r("tye", "ちぇ"), r("tyo", "ちょ"),
    r("tha", "てぁ"), r("thi", "てぃ"), r("thu", "てゅ"), r("the", "てぇ"), r("tho", "てょ"),
    r("cha", "ちゃ"), r("chi", "ち"), r("chu", "ちゅ"), r("che", "ちぇ"), r("cho", "ちょ"),
    r("da", "だ"), r("di", "ぢ"), r("du", "づ"), r("de", "で"), r("do", "ど"),
    r("dya", "ぢゃ"), r("dyi", "ぢぃ"), r("dyu", "ぢゅ"), r("dye", "ぢぇ"), r("dyo", "ぢょ"),
    r("dha", "でゃ"), r("dhi", "でぃ"), r("dhu", "でゅ"), r("dhe", "でぇ"), r("dho", "でょ"),
    r("na", "な"), r("ni", "に"), r("nu", "ぬ"), r("ne", "ね"), r("no", "の"),
    r("nya", "にゃ"), r("nyi", "にぃ"), r("nyu", "にゅ"), r("nye", "にぇ"), r("nyo", "にょ"),
    r("nn", "ん"), r("n'", "ん"),
    r("ha", "は"), r("hi", "ひ"), r("hu", "ふ"), r("he", "へ"), r("ho", "ほ"),
    r("hya", "ひゃ"), r("hyi", "ひぃ"), r("hyu", "ひゅ"), r("hye", "ひぇ"), r("hyo", "ひょ"),
    r("fa", "ふぁ"), r("fi", "ふぃ"), r("fu", "ふ"), r("fe", "ふぇ"), r("fo", "ふぉ"),
    r("fya", "ふゃ"), r("fyi", "ふぃ"), r("fyu", "ふゅ"), r("fye", "ふぇ"), r("fyo", "ふょ"),
    r("ba", "ば"), r("bi", "び"), r("bu", "ぶ"), r("be", "べ"), r("bo", "ぼ"),
    r("bya", "びゃ"), r("byi", "びぃ"), r("byu", "びゅ"), r("bye", "びぇ"), r("byo", "びょ"),
    r("pa", "ぱ"), r("pi", "ぴ"), r("pu", "ぷ"), r("pe", "ぺ"), r("po", "ぽ"),
    r("pya", "ぴゃ"), r("pyi", "ぴぃ"), r("pyu", "ぴゅ"), r("pye", "ぴぇ"), r("pyo", "ぴょ"),
    r("ma", "ま"), r("mi", "み"), r("mu", "む"), r("me", "め"), r("mo", "も"),
    r("mya", "みゃ"), r("myi", "みぃ"), r("myu", "みゅ"), r("mye", "みぇ"), r("myo", "みょ"),
    r("ya", "や"), r("yi", "い"), r("yu", "ゆ"), r("ye", "いぇ"), r("yo", "よ"),
    r("ra", "ら"), r("ri", "り"), r("ru", "る"), r("re", "れ"), r("ro", "ろ"),
    r("rya", "りゃ"), r("ryi", "りぃ"), r("ryu", "りゅ"), r("rye", "りぇ"), r("ryo", "りょ"),
    r("wa", "わ"), r("wi", "うぃ"), r("wu", "う"), r("we", "うぇ"), r("wo", "を"),
    r("va", "ゔぁ"), r("vi", "ゔぃ"), r("vu", "ゔ"), r("ve", "ゔぇ"), r("vo", "ゔぉ"),
    r("xa", "ぁ"), r("xi", "ぃ"), r("xu", "ぅ"), r("xe", "ぇ"), r("xo", "ぉ"),
    ("xka", "か", Some("ヵ")), ("xke", "け", Some("ヶ")),
    r("xtu", "っ"), r("xtsu", "っ"), r("xwa", "ゎ"), r("xwi", "ゐ"), r("xwe", "ゑ"),
    r("xya", "ゃ"), r("xyu", "ゅ"), r("xyo", "ょ"),
    // z で始まる記号
    r("zh", "←"), r("zj", "↓"), r("zk", "↑"), r("zl", "→"), r("z-", "〜"), r("z.", "…"),
    r("z,", "‥"), r("z/", "・"), r("z[", "『"), r("z]", "』"), r("z ", "　"),
    // 記号（skk-input-vector）
    r("-", "ー"), r(",", "、"), r(".", "。"), r("[", "「"), r("]", "」"),
    r("!", "！"), r("?", "？"), r("(", "（"), r(")", "）"), r(":", "："), r(";", "；"),
    r("~", "〜"), r("{", "｛"), r("}", "｝"),
];

/// ひらがなをカタカナに（ぁ〜ゖ・ゝゞ だけ。ほかはそのまま）。
pub fn to_katakana(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{3041}'..='\u{3096}' | '\u{309D}' | '\u{309E}' => char::from_u32(c as u32 + 0x60).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// 出てきたかな（ひらがなとカタカナの両方の形）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kana {
    pub hira: String,
    pub kata: String,
}

impl Kana {
    fn push_rule(&mut self, rule: &Rule) {
        self.hira.push_str(rule.1);
        match rule.2 {
            Some(k) => self.kata.push_str(k),
            None => self.kata.push_str(&to_katakana(rule.1)),
        }
    }
    fn push(&mut self, hira: &str) {
        self.hira.push_str(hira);
        self.kata.push_str(&to_katakana(hira));
    }
}

fn exact(buf: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.0 == buf)
}

fn is_prefix(buf: &str) -> bool {
    RULES.iter().any(|r| r.0.len() > buf.len() && r.0.starts_with(buf))
}

/// 未確定のローマ字。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Romaji {
    buf: String,
}

impl Romaji {
    /// 未確定のローマ字（表示用）。
    pub fn pending(&self) -> &str {
        &self.buf
    }
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
    pub fn clear(&mut self) {
        self.buf.clear();
    }
    /// 最後の 1 字を消す。
    pub fn pop(&mut self) -> Option<char> {
        self.buf.pop()
    }

    /// 1 字を足す。出てきたかなは `out` に足す。ローマ字として使えなかった字（最後に
    /// 打った字）は `Some` で返す（その前の未確定分は「ん」「っ」にするか捨て済み）。
    pub fn feed(&mut self, c: char, out: &mut Kana) -> Option<char> {
        self.buf.push(c);
        loop {
            if let Some(rule) = exact(&self.buf) {
                out.push_rule(rule);
                self.buf.clear();
                return None;
            }
            if is_prefix(&self.buf) {
                return None;
            }
            let mut it = self.buf.chars();
            let first = it.next().unwrap_or(c);
            let Some(second) = it.next() else {
                // 新しい字だけが残り、それも表に無い
                self.buf.clear();
                return Some(c);
            };
            if first == 'n' {
                out.push("ん");
            } else if first == second && first.is_ascii_lowercase() && !"aiueo".contains(first) {
                out.push("っ");
            }
            self.buf.remove(0);
        }
    }

    /// 未確定分を片づける（`n` は「ん」に、それ以外は捨てる）。
    pub fn flush(&mut self, out: &mut Kana) {
        if self.buf == "n" {
            out.push("ん");
        }
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 打った列をひらがなにする（ローマ字でない字はそのまま）。最後に flush する。
    fn conv(s: &str) -> (String, String) {
        let mut r = Romaji::default();
        let mut out = Kana::default();
        for c in s.chars() {
            if let Some(p) = r.feed(c, &mut out) {
                out.hira.push(p);
                out.kata.push(p);
            }
        }
        r.flush(&mut out);
        (out.hira, out.kata)
    }

    fn h(s: &str) -> String {
        conv(s).0
    }

    #[test]
    fn basic_rows() {
        assert_eq!(h("aiueo"), "あいうえお");
        assert_eq!(h("kakikukeko"), "かきくけこ");
        assert_eq!(h("shashishushesho"), "しゃししゅしぇしょ");
        assert_eq!(h("tsutuchichatya"), "つつちちゃちゃ");
        assert_eq!(h("fafifufefo"), "ふぁふぃふふぇふぉ");
        assert_eq!(h("jajijujejo"), "じゃじじゅじぇじょ");
        assert_eq!(h("thidhi"), "てぃでぃ");
        assert_eq!(h("wawo"), "わを");
        assert_eq!(conv("vu").1, "ヴ");
    }

    #[test]
    fn n_handling() {
        assert_eq!(h("kanji"), "かんじ");
        assert_eq!(h("tenmetu"), "てんめつ");
        assert_eq!(h("nn"), "ん");
        assert_eq!(h("kan'i"), "かんい");
        assert_eq!(h("kanni"), "かんい");
        assert_eq!(h("nyuu"), "にゅう");
        assert_eq!(h("hon"), "ほん"); // 最後の n は flush で「ん」
        assert_eq!(h("n1"), "ん1");
    }

    #[test]
    fn sokuon_and_dropping() {
        assert_eq!(h("kitte"), "きって");
        assert_eq!(h("matcha"), "まちゃ"); // t は捨てる（tch の規則は無い）
        assert_eq!(h("haccha"), "はっちゃ");
        assert_eq!(h("zasshi"), "ざっし");
        assert_eq!(h("kta"), "た"); // 続かない k は捨てる
        assert_eq!(h("k"), ""); // 未確定の k は flush で捨てる
    }

    #[test]
    fn small_and_symbols() {
        assert_eq!(h("xaxixuxexo"), "ぁぃぅぇぉ");
        assert_eq!(h("xtuxtsuxyaxyuxyoxwa"), "っっゃゅょゎ");
        assert_eq!(conv("xkaxke"), ("かけ".into(), "ヵヶ".into()));
        assert_eq!(h("a-,.[]"), "あー、。「」");
        assert_eq!(h("!?():;~{}"), "！？（）：；〜｛｝");
        assert_eq!(h("zhzjzkzlz-z.z,z/z[z]z "), "←↓↑→〜…‥・『』　");
        assert_eq!(h("1/@"), "1/@");
    }

    #[test]
    fn pending_and_pop() {
        let mut r = Romaji::default();
        let mut out = Kana::default();
        assert_eq!(r.feed('k', &mut out), None);
        assert_eq!(r.feed('y', &mut out), None);
        assert_eq!(r.pending(), "ky");
        assert_eq!(r.pop(), Some('y'));
        assert_eq!(r.feed('a', &mut out), None);
        assert_eq!(out.hira, "か");
        assert!(r.is_empty());
    }

    #[test]
    fn kana_conversion() {
        assert_eq!(to_katakana("きゃっぷゔー"), "キャップヴー");
        assert_eq!(to_katakana("ゝabc"), "ヽabc");
    }
}
