#include <stdio.h>
/* 型の誤り: 文字列を int に入れる */
int main(void) {
    int n = "日本";
    printf("%d\n", n)
    return 0;
}
