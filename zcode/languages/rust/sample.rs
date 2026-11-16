//! 見本: ライフタイム・文字・生文字列・マクロ・属性
use std::collections::HashMap;

/* ブロックコメント /* 入れ子 */ もここまで */
#[derive(Debug, Clone)]
pub struct Config<'a> {
    name: &'a str,
    limit: u32,
}

pub(crate) enum Mode {
    Fast,
    Slow(u8),
}

trait Shape {
    fn area(&self) -> f64;
}

impl<'a> Config<'a> {
    pub const MAX_LEN: usize = 0xFF_u8 as usize;

    pub async fn load(path: &'static str) -> Option<Self> {
        let mut map: HashMap<String, Vec<i32>> = HashMap::new();
        let c = 'x';
        let nl = '\n';
        let jp = 'あ';
        let raw = r#"raw "quoted" \n text"#;
        let bytes = b"bytes\x00";
        let msg = format!("{name}: {:>5} {{literal}}\t{}", 1.5e-3, 42u64);
        'outer: for i in 0..10 {
            if i % 2 == 0 && c != 'y' {
                continue 'outer;
            }
            println!("{} {}", i, msg);
        }
        unsafe { map.insert(path.to_string(), vec![1, 2, 3]); }
        match map.get("k") {
            Some(v) if v.len() > 1 => Some(Self { name: "a", limit: 3 }),
            _ => None,
        }
    }
}

mod util {
    pub fn max<T: PartialOrd>(a: T, b: T) -> T {
        if a > b { a } else { b }
    }
}
