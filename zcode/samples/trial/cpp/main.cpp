// 標準ライブラリを使う小さなプログラム（F5 でビルド係が aarch64 向けに作る）。
#include <algorithm>
#include <iostream>
#include <string>
#include <vector>

struct Fruit {
    std::string name;
    int price;
};

int main() {
    std::vector<Fruit> fruits{{"りんご", 120}, {"みかん", 80}, {"ぶどう", 300}};
    std::sort(fruits.begin(), fruits.end(), [](const Fruit &a, const Fruit &b) { return a.price < b.price; });
    for (const auto &f : fruits) {
        std::cout << f.name << " " << f.price << "円\n";
    }
    return 0;
}
