// 見本: C++ のクラス・名前空間・テンプレート・生文字列
#include <iostream>
#include <string>
#include <vector>
#include "util.hpp"

#define LOG(x) std::cerr << (x) << '\n'

namespace geo {

constexpr double kPi = 3.14159265358979;
constexpr int MAX_POINTS = 1'000;

template <typename T>
class Stack {
public:
    void push(const T &v) { items_.push_back(v); }
    T pop() {
        T v = std::move(items_.back());
        items_.pop_back();
        return v;
    }
    [[nodiscard]] bool empty() const noexcept { return items_.empty(); }

private:
    std::vector<T> items_;
};

enum class Color : uint8_t { Red, Green = 0x10, Blue };

struct Point {
    double x = 0.0, y = 1e-3;
    virtual ~Point() = default;
    auto len() const -> double { return std::sqrt(x * x + y * y); }
};

}  // namespace geo

/* ブロック
   コメント */
static const char *kHelp = R"(使い方:
  zcode "file" --help)";
static const char *kTagged = R"sql(SELECT * FROM t)sql";

int main(int argc, char **argv) {
    using namespace geo;
    Stack<int> st;
    std::string name = "wide\t\"q\" %d\n";
    auto f = [&](int a) -> bool { return a > 0x1F && a != 'x'; };
    for (int i = 0; i < argc; ++i) {
        if (!f(i)) { continue; }
        st.push(i);
    }
    try {
        throw std::runtime_error("bad");
    } catch (const std::exception &e) {
        std::cout << e.what() << std::endl;
    }
    return st.empty() ? EXIT_FAILURE : 0;
}
