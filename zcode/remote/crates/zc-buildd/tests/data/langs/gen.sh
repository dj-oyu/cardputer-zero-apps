#!/bin/bash
# 2026-11-17 に足した言語の診断・試験の出力の実物（このフォルダの fixture）の作り方。
# ビルド係のイメージ（cz-builder）の中で、root でなくてよい。版は Dockerfile の ARG のもの
# （pyright 1.1.414・ShellCheck 0.11.0・g++ 14.2・Go 1.27.4・ruby 3.3・Zig 0.17.1・MoonBit 0.10.20）。
#
#   wslc run --rm -v "<このフォルダ>:/out" --entrypoint bash cz-builder:latest /out/gen.sh
#
# fixture の中の絶対パス（/tmp/fxw/...）は、試験が木の根として渡す。ソースの中身は各試験に
# 写してある（桁を直すのに使う）。作り直したら、試験の期待値と合わせて見直す。
set -u
F=${OUT:-/out}
W=/tmp/fxw
rm -rf "$W" && mkdir -p "$W"

# ---- Python（pyright） ----
mkdir -p "$W/py" && cd "$W/py"
printf 'import requests\nimport os\n# 日本語\nx: int = "日本" + 1\ndef f() -> str:\n    return 1\nprint(undefined_name)\n' > a.py
pyright --outputjson --pythonversion 3.13 a.py > "$F/pyright.json"

# ---- Shell（shellcheck） ----
mkdir -p "$W/sh" && cd "$W/sh"
printf '#!/bin/bash\n# 日本語\necho "日本" $1\n\tif [ $x = 1 ] then\nfi\ncd foo\n' > a.sh
printf '#!/bin/bash\necho "日本語" $foo\n\techo $bar\n' > b.sh
shellcheck -f json1 -S info a.sh > "$F/shellcheck-syntax.json"
shellcheck -f json1 -S info b.sh > "$F/shellcheck.json"

# ---- C++（g++ の JSON） ----
mkdir -p "$W/cpp" && cd "$W/cpp"
printf '#include <string>\n// 日本語\nint main() { std::string s = /* 日本 */ 1.5; int unused; return s; }\n' > a.cpp
aarch64-linux-gnu-g++ -mcpu=cortex-a53 -fsyntax-only -Wall -Wextra -fdiagnostics-format=json a.cpp 2> "$F/gxx.json"

# ---- Go ----
mkdir -p "$W/go" && cd "$W/go"
printf 'module example.com/hello\n\ngo 1.24\n' > go.mod
printf 'package main\n\nimport "fmt"\n\n// 日本語\nfunc main() {\n\tvar s string = /* 日本 */ 1\n\tfmt.Println(s, undefinedX)\n}\n' > main.go
GOOS=linux GOARCH=arm64 go vet ./... > "$F/go-vet-typeerr.txt" 2>&1
GOOS=linux GOARCH=arm64 CGO_ENABLED=0 go build -o /tmp/gobin ./... > "$F/go-build-err.txt" 2>&1
printf 'package main\n\nimport "fmt"\n\n// 日本語\nfunc main() {\n\tx := 3\n\tfmt.Printf("%%s 日本 %%d\\n", x)\n}\n' > main.go
GOOS=linux GOARCH=arm64 go vet -json ./... > "$F/go-vet-json.txt" 2>&1
printf 'package main\n\nimport "fmt"\n\nfunc main() { fmt.Println(add(1, 2)) }\n\nfunc add(a, b int) int { return a + b }\n' > main.go
printf 'package main\n\nimport "testing"\n\nfunc TestAdd(t *testing.T) {\n\tif add(1, 2) != 4 {\n\t\tt.Errorf("日本 add = %%d", add(1, 2))\n\t}\n}\n\nfunc TestOk(t *testing.T) {}\n\nfunc TestSkip(t *testing.T) { t.Skip("later") }\n' > main_test.go
go test -json ./... > "$F/go-test.json" 2>&1
mkdir -p "$W/go2/sub" && cd "$W/go2"
printf 'module example.com/hello\n\ngo 1.24\n' > go.mod
printf 'package main\n\nimport "example.com/hello/sub"\n\nfunc main() { sub.F() }\n' > main.go
printf 'package sub\n\n// 日本語\nfunc F() {\n\tx := "日本" +\n}\n' > sub/sub.go
GOOS=linux GOARCH=arm64 go build -o /tmp/godir/ ./... > "$F/go-build-syntax.txt" 2>&1
printf 'package sub\n\nimport "fmt"\n\nfunc F() { fmt.Printf("%%d\\n", "x") }\n' > sub/sub.go
printf 'package sub\n\nimport "testing"\n\nfunc TestF(t *testing.T) { t.Fatal("日本 boom") }\n' > sub/sub_test.go
go test -json ./... > "$F/go-test-sub.json" 2>&1

# ---- Ruby（ruby -wc） ----
mkdir -p "$W/rb" && cd "$W/rb"
printf '# 日本語\nx = 1\ndef f\n  puts "日本" if (y = 2)\nend\n' > w.rb
printf '# 日本語\ndef f(\n  puts "日本"\nend\nfoo(1 2)\n' > e.rb
printf '# 日本語\nx = "日本" 1\ny = 2\n' > e1.rb
printf 'def a\n  if true\nend\n' > e2.rb
printf 'p(1,\np(2 3)\nq = [1 2]\n' > e3.rb
ruby -wc w.rb > "$F/ruby-warn.txt" 2>&1
ruby -wc e.rb > "$F/ruby-err.txt" 2>&1
ruby -wc e1.rb > "$F/ruby-err-cjk.txt" 2>&1
ruby -wc e2.rb > "$F/ruby-err-eof.txt" 2>&1
ruby -wc e3.rb > "$F/ruby-err-multi.txt" 2>&1

# ---- Zig ----
mkdir -p "$W/zig" && cd "$W/zig"
printf 'const std = @import("std");\n// 日本語\npub fn main() void {\n    const x: u32 = "日本"; const y = 1\n}\n' > a.zig
zig ast-check a.zig > "$F/zig-ast.txt" 2>&1
printf 'const std = @import("std");\npub fn main() void {\n    const s: []const u8 = "日本";\n    const n: u32 = s;\n    _ = n;\n}\n' > sema.zig
zig build-exe -fno-emit-bin -target aarch64-linux-gnu -mcpu=cortex_a53 sema.zig > "$F/zig-sema2.txt" 2>&1
printf 'const std = @import("std");\npub fn main() void {\n    std.debug.print("hi\\n", .{});\n}\nfn add(a: i32, b: i32) i32 { return a + b; }\ntest "add" {\n    try std.testing.expect(add(1, 2) == 4);\n}\ntest "ok" {\n    try std.testing.expect(add(1, 2) == 3);\n}\n' > c.zig
zig test c.zig > "$F/zig-test.txt" 2>&1
printf 'const std = @import("std");\nfn add(a: i32, b: i32) i32 { return a + b; }\ntest "add" {\n    try std.testing.expect(add(1, 2) == 3);\n}\ntest "two" {}\ntest "skip" { return error.SkipZigTest; }\n' > ok.zig
zig test ok.zig > "$F/zig-test-ok.txt" 2>&1
mkdir -p "$W/zig2/proj" && cd "$W/zig2/proj" && zig init > /dev/null 2>&1
printf '\ntest "zc fails" {\n    try std.testing.expect(1 + 1 == 3);\n}\n' >> src/root.zig
zig build test > "$F/zig-build-test.txt" 2>&1

# ---- MoonBit ----
mkdir -p "$W/mb" && cd "$W/mb" && moon new hello > /dev/null 2>&1 && cd hello
printf '// 日本語\nfn helper() -> Int {\n  let x : Int = "日本"\n  let unused = 1\n  x\n}\n' > cmd/main/err.mbt
moon check --target native --output-json > "$F/moon-check.json" 2> /dev/null
rm cmd/main/err.mbt
printf 'test "fails" {\n  assert_eq(1 + 1, 3)\n}\ntest "ok" {\n  assert_eq(1, 1)\n}\n' >> hello_test.mbt
mkdir -p sub && : > sub/moon.pkg && printf 'pub fn g() -> Int { 1 }\n' > sub/sub.mbt
printf 'test "sub fails" {\n  inspect(1, content="2")\n}\n' > sub/sub_test.mbt
moon test --target native --test-failure-json > "$F/moon-test-json.txt" 2>&1
