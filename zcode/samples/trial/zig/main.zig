//! Zig のサンプル（F5 でビルド係が aarch64 向けに作る。F6 で zig test）。
const std = @import("std");

/// フィボナッチ数（n 番目）。
fn fib(n: u32) u64 {
    var a: u64 = 0;
    var b: u64 = 1;
    var i: u32 = 0;
    while (i < n) : (i += 1) {
        const t = a + b;
        a = b;
        b = t;
    }
    return a;
}

pub fn main() void {
    var i: u32 = 0;
    while (i < 10) : (i += 1) {
        std.debug.print("{d} ", .{fib(i)});
    }
    std.debug.print("\n", .{});
}

test "fib" {
    try std.testing.expectEqual(@as(u64, 55), fib(10));
    try std.testing.expectEqual(@as(u64, 0), fib(0));
}
