///|
/// 見本: MoonBit の構造体・列挙型・トレイト・式展開
pub(all) struct Point {
  x : Double
  y : Double
} derive(Show, Eq)

///|
pub enum Shape {
  Circle(Point, Double)
  Rect(Point, Point)
  Empty
} derive(Show)

///|
pub trait Area {
  area(Self) -> Double
}

///|
impl Area for Shape with area(self) {
  match self {
    Circle(_, r) => 3.14159 * r * r
    Rect(a, b) => (b.x - a.x).abs() * (b.y - a.y).abs()
    Empty => 0.0
  }
}

///|
#deprecated("use Point::origin")
pub fn Point::zero() -> Point {
  { x: 0.0, y: 1e-3 }
}

///|
fn[T : Show] describe(items : Array[T]) -> String {
  let mut out = ""
  for i, item in items {
    out += "\{i}: \{item} (\{items.length() - i} left)\n"
  }
  out
}

///|
suberror ParseError String

///|
fn parse_digit(c : Char) -> Int raise ParseError {
  if c >= '0' && c <= '9' {
    c.to_int() - '0'.to_int()
  } else {
    raise ParseError("bad digit: \{c}")
  }
}

///|
let banner : String =
  #|zcode "demo"
  #|  raw line \n stays
  $|interpolated \{1 + 2}

///|
test "parse" {
  inspect(parse_digit('7'), content="7")
  let big = 1_000_000L
  let mask = 0xFF_FFU
  let bytes = b"abc\x00"
  println("\{big} \{mask} \{bytes} \u{1F600}")
}

///|
fn main {
  let shapes = [Circle({ x: 0.0, y: 0.0 }, 2.0), Empty]
  shapes |> describe |> println
}
