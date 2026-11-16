// 埋め込みのハイライト表（`languages.bin`）の形（design.md §8.2）。
//
// このファイルは `build.rs`（書く側）とライブラリ（読む側）の両方から使う。
// 依存は std だけにしておくこと。
//
// 全体はリトルエンディアン固定、各表は 4 バイト境界、DFA は 8 バイト境界。
// 言語ごとの領域（region）は 4096 バイト境界から始まり、連続している。開いた言語の
// 領域だけが SD から読まれ、`madvise(WILLNEED)` も領域単位でかける。
//
// ```text
// 先頭（HEADER_LEN バイト）
//   0  magic        b"ZCHL"
//   4  format       u32 = FORMAT_VERSION
//   8  build_id     u64（build.rs が入力から作る。生成コードの BUILD_ID と一致すること）
//  16  n_langs      u32
//  20  dir_off      u32（言語の目録の位置）
//  24  reserved     u32 × 2
// 言語の目録（n_langs × DIR_REC_LEN）
//   0  region_off   u32（4096 境界）
//   4  region_len   u32
//   8  first_off    u32（1 行目の判定用 DFA。無ければ 0）
//  12  first_len    u32
// 1 行目の判定用 DFA（小さいので先頭にまとめる。言語の判定で各言語の領域に触らないため）
// 言語の領域（4096 境界）
//   0  magic        b"ZCLR"
//   4  n_contexts   u32
//   8  n_rules      u32
//  12  n_push       u32
//  16  ctx_off      u32（領域の先頭から）
//  20  rule_off     u32
//  24  push_off     u32
//  28  words_off    u32
//  32  n_words      u32
//  36  reserved     u32
//   文脈の表（n_contexts × CTX_REC_LEN）
//     0 dfa_off u32（DFA が無い文脈は 0）, 4 dfa_len u32, 8 rule_base u32, 12 rule_count u16,
//    14 default_token u8, 15 reserved u8, 16 fallback u16（NO_FALLBACK = 無し）, 18 reserved u16
//     fallback: 自分の規則がどれも当たらなかったら、この文脈の DFA で探す（末尾の
//     include を DFA の複製でなく共有にしたもの。結果は include と同じ）
//   規則の表（n_rules × RULE_REC_LEN）
//     0 token u8（TOKEN_INHERIT = 文脈の地のトークン）, 1 pop u8, 2 n_push u8, 3 flags u8,
//     4 push_idx u32（push の表の位置）, 8 head_off u32, 12 head_len u32（followed_by 用）
//   push の表（n_push × u16、文脈の番号。並べた順に積む＝最後が一番上）
//   語の表（n_words × WORD_REC_LEN、バイト列の昇順）
//     0 str_off u32（領域の先頭から）, 4 len u16, 6 token u8, 7 reserved u8,
//     8 push u16（その語で積む文脈。NO_FALLBACK = 無し）, 10 reserved u16
//   語の文字列
//   DFA（regex-automata の dense DFA、リトルエンディアン、8 バイト境界）
// ```

#[allow(dead_code)]
pub mod layout {
    pub const MAGIC: [u8; 4] = *b"ZCHL";
    pub const REGION_MAGIC: [u8; 4] = *b"ZCLR";
    /// 形式の版。形を変えたら増やす。
    pub const FORMAT_VERSION: u32 = 1;
    pub const HEADER_LEN: usize = 32;
    pub const DIR_REC_LEN: usize = 16;
    pub const REGION_HEADER_LEN: usize = 40;
    pub const CTX_REC_LEN: usize = 20;
    pub const RULE_REC_LEN: usize = 16;
    pub const WORD_REC_LEN: usize = 12;
    /// 文脈の fallback が無い。
    pub const NO_FALLBACK: u16 = 0xFFFF;
    /// 規則の flags: 一致した文字列を語の表で引く。
    pub const RULE_WORDS: u8 = 1;
    /// 言語の領域の境界（ページ）。
    pub const REGION_ALIGN: usize = 4096;
    /// DFA の境界（regex-automata は 4 バイト境界を要求する。余裕を見て 8）。
    pub const DFA_ALIGN: usize = 8;
    /// 規則の token がこの値なら、文脈の地のトークンを使う。
    pub const TOKEN_INHERIT: u8 = 0xFF;
    /// 文脈の数の上限（push の表が u16）。
    pub const MAX_CONTEXTS: usize = 4096;

    pub fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
        let s = b.get(off..off.checked_add(4)?)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    pub fn rd_u16(b: &[u8], off: usize) -> Option<u16> {
        let s = b.get(off..off.checked_add(2)?)?;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    pub fn rd_u64(b: &[u8], off: usize) -> Option<u64> {
        Some(rd_u32(b, off)? as u64 | (rd_u32(b, off + 4)? as u64) << 32)
    }
    pub fn wr_u32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    pub fn wr_u16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    pub fn pad_to(v: &mut Vec<u8>, align: usize) {
        while !v.len().is_multiple_of(align) {
            v.push(0);
        }
    }
    /// ビルド ID などに使う 64 ビットの FNV-1a。
    pub fn fnv64(mut h: u64, data: &[u8]) -> u64 {
        if h == 0 {
            h = 0xcbf2_9ce4_8422_2325;
        }
        for &b in data {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    }
}
