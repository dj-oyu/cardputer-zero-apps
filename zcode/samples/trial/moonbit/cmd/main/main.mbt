///|
/// MoonBit のサンプル（F5 でビルド係が native で作る。F6 で moon test）。
fn main {
  let mut line = ""
  for i = 0; i < 8; i = i + 1 {
    line = line + @blink.mark(i)
  }
  println(line)
  println("点灯 \{@blink.lit_count(8)} 回")
}
