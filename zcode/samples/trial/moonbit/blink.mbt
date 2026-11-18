///|
/// n 回の点滅で点灯する回数（1 つおきに点く）。
pub fn lit_count(n : Int) -> Int {
  (n + 1) / 2
}

///|
/// 点灯なら "●"、消灯なら "○"。
pub fn mark(i : Int) -> String {
  if i % 2 == 0 {
    "●"
  } else {
    "○"
  }
}

///|
test "lit_count" {
  assert_eq(lit_count(4), 2)
  assert_eq(lit_count(5), 3)
}

///|
test "mark" {
  assert_eq(mark(0), "●")
  assert_eq(mark(1), "○")
}
