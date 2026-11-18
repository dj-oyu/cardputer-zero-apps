#include <string>
// 型の誤り: double を std::string に入れる
int main() {
    std::string s = 1.5;
    return 0;
}
