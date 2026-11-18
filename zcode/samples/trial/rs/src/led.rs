/// 点滅の型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// 1 つおき。
    Blink,
    /// 2 回点いて 2 回休む。
    Heartbeat,
}

/// 一歩ずつ点滅を進める。
pub struct Blinker {
    pattern: Pattern,
    tick: u32,
    lit: u32,
}

impl Blinker {
    pub fn new(pattern: Pattern) -> Blinker {
        Blinker { pattern, tick: 0, lit: 0 }
    }

    /// 次の一歩。点灯なら `true`。
    pub fn step(&mut self) -> bool {
        let on = match self.pattern {
            Pattern::Blink => self.tick % 2 == 0,
            Pattern::Heartbeat => self.tick % 4 < 2,
        };
        self.tick += 1;
        self.lit += on as u32;
        on
    }

    /// これまでに点灯した回数。
    pub fn lit(&self) -> u32 {
        self.lit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blink_alternates() {
        let mut b = Blinker::new(Pattern::Blink);
        let seq: Vec<bool> = (0..4).map(|_| b.step()).collect();
        assert_eq!(seq, [true, false, true, false]);
    }

    #[test]
    fn heartbeat_counts() {
        let mut b = Blinker::new(Pattern::Heartbeat);
        for _ in 0..8 {
            b.step();
        }
        assert_eq!(b.lit(), 4);
    }
}
