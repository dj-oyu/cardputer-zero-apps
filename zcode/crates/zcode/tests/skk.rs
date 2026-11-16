//! SKK をエディタ・ターミナルに組み込んだ動き（段階 4、モック 7〜9）。システム辞書は
//! 試験の中で作る小さな EUC-JP の辞書（`ZCODE_SKK_DICT`）。個人辞書には書かない
//! （学習は 5 分ためてから書くので、試験の間には書かれない）。

use std::fs;
use std::path::PathBuf;
use std::sync::Once;
use zc_core::rt::{main_channel, MainReceiver, MainSender};
use zc_core::time::now_us;
use zc_core::{Key, KeyEvent, Mods};
use zc_skk::InputMode;
use zcode::app::{AppEvent, LoopCx, Screen, Timers, Workers};
use zcode::doc::{DocBackend, SimpleDoc};
use zcode::editor::{EditorConfig, EditorScreen};

/// 試験用の辞書（本物と同じ並び: 送りありは降順、送りなしは昇順）。
const DICT: &str = ";; -*- coding: euc-jp -*-
;; okuri-ari entries.
かk /書/欠/
;; okuri-nasi entries.
きかい /機械/機会/
てんめつ /点滅/明滅;めいめつ/天滅/
にほん /日本/二本/
";

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let path = std::env::temp_dir().join(format!("zcode-skk-dict-{}", std::process::id()));
        let (bytes, _, bad) = encoding_rs::EUC_JP.encode(DICT);
        assert!(!bad);
        fs::write(&path, &bytes).unwrap();
        std::env::set_var("ZCODE_SKK_DICT", &path);
        // 個人辞書を読まない・書かない（HOME を空のフォルダにする）
        let home = std::env::temp_dir().join(format!("zcode-skk-home-{}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
    });
}

struct Loop {
    timers: Timers,
    workers: Workers,
    tx: MainSender<AppEvent>,
    rx: MainReceiver<AppEvent>,
}

impl Loop {
    fn new() -> Loop {
        let (tx, rx) = main_channel().unwrap();
        Loop { timers: Timers::new().unwrap(), workers: Workers::new(), tx, rx }
    }
    fn with<R>(&mut self, f: impl FnOnce(&mut LoopCx<'_>) -> R) -> R {
        let mut cx = LoopCx { timers: &mut self.timers, workers: &self.workers, results: &self.tx, now_us: now_us(), quit: false, force_present: false };
        f(&mut cx)
    }
    /// 1 打鍵ずつ流して、そのたびに辞書引きの結果を届ける。
    fn keys(&mut self, ed: &mut EditorScreen, keys: &[KeyEvent]) {
        for k in keys {
            self.with(|cx| ed.keys(std::slice::from_ref(k), Mods::NONE, cx));
            self.pump(ed);
        }
    }
    /// まとめて 1 回の起床で流す（辞書引きの最中に次の打鍵が来る場合）。
    fn burst(&mut self, ed: &mut EditorScreen, keys: &[KeyEvent]) {
        self.with(|cx| ed.keys(keys, Mods::NONE, cx));
        self.pump(ed);
    }
    fn type_str(&mut self, ed: &mut EditorScreen, s: &str) {
        let keys: Vec<KeyEvent> = s.chars().map(ch).collect();
        self.keys(ed, &keys);
    }
    fn pump(&mut self, ed: &mut EditorScreen) {
        for _ in 0..3 {
            self.workers.assist.wait_idle();
            self.workers.batch.wait_idle();
            self.workers.storage.wait_idle();
            let evs: Vec<AppEvent> = self.rx.drain().collect();
            for ev in evs {
                self.with(|cx| ed.on_event(ev, cx));
            }
        }
    }
}

/// 文字の打鍵（英大文字は Shift つき。実機では Shift を 1 回タップして打つ）。
fn ch(c: char) -> KeyEvent {
    let mut k = KeyEvent::char(c);
    if c.is_ascii_uppercase() {
        k.key = Key::Char(c.to_ascii_lowercase());
        k.mods = Mods::SHIFT;
    }
    k
}
fn key(k: Key) -> KeyEvent {
    KeyEvent::new(k, Mods::NONE)
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(Key::Char(c), Mods::CTRL)
}

fn editor(text: &str) -> EditorScreen {
    setup();
    // ファイルを渡して起動したことにする（入力がエディタに向く。読み込みはしない）
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Simple, root: PathBuf::from("/nonexistent"), files: vec![PathBuf::from("a.py")] });
    ed.open_doc(Some(PathBuf::from("/nonexistent/a.py")), "a.py", Box::new(SimpleDoc::from_text(text)), None);
    ed
}

fn text(ed: &EditorScreen) -> String {
    zcode::edbench::doc_text(ed.active_doc().unwrap())
}

#[test]
fn ctrl_j_types_hiragana_and_l_returns_to_ascii() {
    let mut lp = Loop::new();
    let mut ed = editor("");
    lp.keys(&mut ed, &[ctrl('j')]);
    assert_eq!(ed.skk_state().0, InputMode::Hiragana);
    lp.type_str(&mut ed, "nihongo");
    assert_eq!(text(&ed), "にほんご");
    // ローマ字の途中は未確定（文書には入らない）
    lp.type_str(&mut ed, "k");
    assert_eq!(text(&ed), "にほんご");
    assert_eq!(ed.skk_state().1, "k");
    lp.type_str(&mut ed, "a");
    lp.keys(&mut ed, &[ch('l')]);
    assert_eq!(ed.skk_state().0, InputMode::Ascii);
    lp.type_str(&mut ed, " ok");
    assert_eq!(text(&ed), "にほんごか ok");
}

#[test]
fn convert_choose_with_asdfjkl_and_learn() {
    let mut lp = Loop::new();
    let mut ed = editor("");
    lp.keys(&mut ed, &[ctrl('j')]);
    lp.type_str(&mut ed, "Tenmetu");
    assert_eq!(ed.skk_state().1, "てんめつ");
    assert_eq!(text(&ed), "", "▽ の間は文書に入らない");
    lp.keys(&mut ed, &[ch(' ')]);
    let (_, pre, cands, _) = ed.skk_state();
    assert_eq!(pre, "点滅");
    assert_eq!(cands, ["点滅", "明滅", "天滅"]);
    // s で 2 番目を選んで確定。学習がたまる（まだ書かない）
    lp.type_str(&mut ed, "s");
    assert_eq!(text(&ed), "明滅");
    assert!(ed.skk_state().2.is_empty());
    assert_eq!(ed.skk_pending_learned(), 1);
    // 次の変換では学習した語が先頭に来る
    lp.type_str(&mut ed, "Tenmetu ");
    assert_eq!(ed.skk_state().1, "明滅");
    lp.keys(&mut ed, &[key(Key::Enter)]);
    assert_eq!(text(&ed), "明滅明滅");
}

#[test]
fn keys_typed_during_a_lookup_are_held_and_replayed() {
    let mut lp = Loop::new();
    let mut ed = editor("");
    lp.keys(&mut ed, &[ctrl('j')]);
    lp.type_str(&mut ed, "Nihon");
    // Space と a が同じ起床で届く: a は候補の結果の後に「1 番目を選ぶ」として効く
    lp.burst(&mut ed, &[ch(' '), ch('a')]);
    assert_eq!(text(&ed), "日本");
}

#[test]
fn okuri_ari_conversion() {
    let mut lp = Loop::new();
    let mut ed = editor("");
    lp.keys(&mut ed, &[ctrl('j')]);
    lp.type_str(&mut ed, "KaKu");
    // 一覧は語幹だけ（本物の SKK と同じ）、本文の ▼ は送り仮名つき
    assert_eq!(ed.skk_state().2, ["書", "欠"]);
    assert_eq!(ed.skk_state().1, "書く");
    lp.keys(&mut ed, &[key(Key::Enter)]);
    assert_eq!(text(&ed), "書く");
}

#[test]
fn no_candidates_opens_registration_and_registers() {
    let mut lp = Loop::new();
    let mut ed = editor("");
    lp.keys(&mut ed, &[ctrl('j')]);
    lp.type_str(&mut ed, "Kaado ");
    assert_eq!(ed.skk_state().3.as_deref(), Some("かあど"));
    // 登録欄の中でも変換できる（入れ子）: 機械 + カタカナ
    lp.type_str(&mut ed, "Kikai ");
    lp.keys(&mut ed, &[key(Key::Enter)]);
    lp.type_str(&mut ed, "q");
    lp.type_str(&mut ed, "do");
    lp.keys(&mut ed, &[key(Key::Enter)]);
    assert_eq!(ed.skk_state().3, None);
    assert_eq!(text(&ed), "機械ド");
    assert_eq!(ed.skk_pending_learned(), 2, "機械の学習と、かあど の登録");
    // Ctrl+G で登録をやめると、▽ に戻る
    lp.type_str(&mut ed, "Zzz ");
    assert!(ed.skk_state().3.is_some());
    lp.keys(&mut ed, &[ctrl('g')]);
    assert_eq!(ed.skk_state().3, None);
}

#[test]
fn ctrl_shortcuts_still_work_in_kana_mode() {
    let mut lp = Loop::new();
    let mut ed = editor("x\n");
    lp.keys(&mut ed, &[ctrl('j')]);
    lp.type_str(&mut ed, "Tenmetu");
    // Ctrl+A（すべて選択）は SKK を通り抜ける。▽ は確定してから
    lp.keys(&mut ed, &[ctrl('a')]);
    assert_eq!(text(&ed), "てんめつx");
    assert_eq!(ed.skk_state().0, InputMode::Hiragana);
}

// --- 画面（モック 7〜9）: 正解画像と比べる ---

fn check(ed: &mut EditorScreen, name: &str) {
    let fonts = zc_font::FontConfig::default();
    if !fonts.available() {
        return;
    }
    let canvas = zcode::render_screen(ed, &fonts).unwrap();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden").join(name);
    if let Err(e) = zc_display::png::check_golden(&canvas, path) {
        panic!("{e}");
    }
}

/// モックと同じ見本（5 行目の行末にカーソル）。
fn mock_editor() -> EditorScreen {
    setup();
    let mut ed = EditorScreen::new(EditorConfig { backend: DocBackend::Simple, root: PathBuf::from("/nonexistent"), files: vec![PathBuf::from("main.py")] });
    let src = "import time
from utils import led

def main():
    # 0.5 秒ごとに
    for i in range(10):
        led(i % 2)
        time.sleep(0.5)
    print(\"done\")
";
    let mut doc = SimpleDoc::from_text(src);
    use zcode::doc::TextDoc;
    let end = zc_core::Pos::new(4, doc.line_len(4));
    doc.set_selection(end, end);
    ed.open_doc(Some(PathBuf::from("/nonexistent/main.py")), "main.py", Box::new(doc), None);
    ed
}

#[test]
fn mock7_reading_screen() {
    let mut lp = Loop::new();
    let mut ed = mock_editor();
    lp.keys(&mut ed, &[ctrl('j')]);
    lp.type_str(&mut ed, "Tenmetu");
    check(&mut ed, "skk-reading.png");
}

#[test]
fn mock8_converting_screen() {
    let mut lp = Loop::new();
    let mut ed = mock_editor();
    lp.keys(&mut ed, &[ctrl('j')]);
    lp.type_str(&mut ed, "Tenmetu ");
    check(&mut ed, "skk-converting.png");
}

#[test]
fn mock9_registration_screen() {
    let mut lp = Loop::new();
    let mut ed = mock_editor();
    lp.keys(&mut ed, &[ctrl('j')]);
    lp.type_str(&mut ed, "Ka-dopyu-ta- ");
    assert_eq!(ed.skk_state().3.as_deref(), Some("かーどぴゅーたー"));
    lp.type_str(&mut ed, "qka-doqPyu-ta");
    check(&mut ed, "skk-registration.png");
}
