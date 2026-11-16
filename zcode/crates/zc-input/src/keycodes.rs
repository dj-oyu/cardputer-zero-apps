//! evdev の定数と、キーコードの名前（表示・ログ用）。

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_MSC: u16 = 4;
pub const SYN_REPORT: u16 = 0;
pub const SYN_DROPPED: u16 = 3;
pub const MSC_SCAN: u16 = 4;
/// キーコードの上限（`KEY_MAX` = 0x2ff）+ 1。
pub const KEY_CNT: usize = 0x300;

pub const KEY_ESC: u16 = 1;
pub const KEY_ENTER: u16 = 28;
pub const KEY_BACKSPACE: u16 = 14;

const NAMES: &[(u16, &str)] = &[
    (1, "ESC"),
    (2, "1"),
    (3, "2"),
    (4, "3"),
    (5, "4"),
    (6, "5"),
    (7, "6"),
    (8, "7"),
    (9, "8"),
    (10, "9"),
    (11, "0"),
    (12, "MINUS"),
    (13, "EQUAL"),
    (14, "BACKSPACE"),
    (15, "TAB"),
    (16, "Q"),
    (17, "W"),
    (18, "E"),
    (19, "R"),
    (20, "T"),
    (21, "Y"),
    (22, "U"),
    (23, "I"),
    (24, "O"),
    (25, "P"),
    (26, "LEFTBRACE"),
    (27, "RIGHTBRACE"),
    (28, "ENTER"),
    (29, "LEFTCTRL"),
    (30, "A"),
    (31, "S"),
    (32, "D"),
    (33, "F"),
    (34, "G"),
    (35, "H"),
    (36, "J"),
    (37, "K"),
    (38, "L"),
    (39, "SEMICOLON"),
    (40, "APOSTROPHE"),
    (41, "GRAVE"),
    (42, "LEFTSHIFT"),
    (43, "BACKSLASH"),
    (44, "Z"),
    (45, "X"),
    (46, "C"),
    (47, "V"),
    (48, "B"),
    (49, "N"),
    (50, "M"),
    (51, "COMMA"),
    (52, "DOT"),
    (53, "SLASH"),
    (54, "RIGHTSHIFT"),
    (55, "KPASTERISK"),
    (56, "LEFTALT"),
    (57, "SPACE"),
    (58, "CAPSLOCK"),
    (59, "F1"),
    (60, "F2"),
    (61, "F3"),
    (62, "F4"),
    (63, "F5"),
    (64, "F6"),
    (65, "F7"),
    (66, "F8"),
    (67, "F9"),
    (68, "F10"),
    (69, "NUMLOCK"),
    (70, "SCROLLLOCK"),
    (71, "KP7"),
    (72, "KP8"),
    (73, "KP9"),
    (74, "KPMINUS"),
    (75, "KP4"),
    (76, "KP5"),
    (77, "KP6"),
    (78, "KPPLUS"),
    (79, "KP1"),
    (80, "KP2"),
    (81, "KP3"),
    (82, "KP0"),
    (83, "KPDOT"),
    (85, "ZENKAKUHANKAKU"),
    (86, "102ND"),
    (87, "F11"),
    (88, "F12"),
    (89, "RO"),
    (90, "KATAKANA"),
    (91, "HIRAGANA"),
    (92, "HENKAN"),
    (93, "KATAKANAHIRAGANA"),
    (94, "MUHENKAN"),
    (96, "KPENTER"),
    (99, "SYSRQ"),
    (102, "HOME"),
    (103, "UP"),
    (104, "PAGEUP"),
    (105, "LEFT"),
    (106, "RIGHT"),
    (107, "END"),
    (108, "DOWN"),
    (109, "PAGEDOWN"),
    (110, "INSERT"),
    (111, "DELETE"),
    (113, "MUTE"),
    (114, "VOLUMEDOWN"),
    (115, "VOLUMEUP"),
    (138, "HELP"),
    (163, "NEXTSONG"),
    (164, "PLAYPAUSE"),
    (165, "PREVIOUSSONG"),
    (224, "BRIGHTNESSDOWN"),
    (225, "BRIGHTNESSUP"),
    (383, "CD"),
    (389, "DVD"),
];

/// キーコードの名前（`KEY_` を除いた部分）。表に無ければ `None`。
pub fn key_name(code: u16) -> Option<&'static str> {
    NAMES.binary_search_by_key(&code, |&(c, _)| c).ok().map(|i| NAMES[i].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sorted_and_lookup() {
        assert!(NAMES.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(key_name(30), Some("A"));
        assert_eq!(key_name(71), Some("KP7"));
        assert_eq!(key_name(383), Some("CD"));
        assert_eq!(key_name(84), None);
    }
}
