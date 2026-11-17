//! 見本: Zig の組み込み関数・複数行文字列・エラー処理
const std = @import("std");
const Allocator = std.mem.Allocator;

/// ドキュメントコメント
pub const MAX_ITEMS: usize = 64;

const Color = enum(u8) { red, green = 0x10, blue };

const Point = struct {
    x: f32 = 0.0,
    y: f32 = 1e-3,

    pub fn len(self: Point) f32 {
        return @sqrt(self.x * self.x + self.y * self.y);
    }
};

const ParseError = error{ Empty, BadDigit };

const banner =
    \\zcode "demo"
    \\  line two {not a format}
;

fn parse(text: []const u8) ParseError!u32 {
    if (text.len == 0) return error.Empty;
    var total: u32 = 0;
    for (text) |c| {
        if (c < '0' or c > '9') return error.BadDigit;
        total = total * 10 + (c - '0');
    }
    return total;
}

pub fn Stack(comptime T: type) type {
    return struct {
        items: std.ArrayList(T),

        pub fn init(allocator: Allocator) @This() {
            return .{ .items = std.ArrayList(T).init(allocator) };
        }
    };
}

test "parse digits" {
    try std.testing.expectEqual(@as(u32, 1_024), try parse("1024"));
    try std.testing.expectError(error.BadDigit, parse("1x"));
}

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const n: u7 = 0b101_0101;
    const msg = "tab\t \"q\" \x41 \u{1F600}";
    std.debug.print("{s} {d} {x:0>4}\n", .{ msg, n, 0xBEEF });
    const value = parse("42") catch |err| {
        std.debug.print("error: {}\n", .{err});
        return err;
    };
    _ = value;
    _ = banner;
}
