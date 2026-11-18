const std = @import("std");
// 構文の誤り（セミコロンが無い）
pub fn main() void {
    const x: u32 = 1
    std.debug.print("{d}\n", .{x});
}
