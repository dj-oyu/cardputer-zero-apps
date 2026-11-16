/* 見本: プリプロセッサ・型・文字列・数 */
#include <stdio.h>
#include "util.h"
#define MAX_ITEMS 64
#define SQUARE(x) ((x) * (x)) /* 継続行 */ \
    + 0
#ifdef DEBUG
#  define LOG(fmt, ...) fprintf(stderr, fmt, __VA_ARGS__)
#endif

typedef struct Node {
    int value;
    struct Node *next;
} Node;

typedef unsigned long long u64_t;

static const char *names[] = { "alpha", "beta\n", L"wide" };

// 行コメント
int sum(const int *items, size_t n)
{
    int total = 0;
    for (size_t i = 0; i < n; i++) {
        total += items[i];
    }
    return total;
}

int main(int argc, char **argv)
{
    unsigned flags = 0xFFu | 0b101 | 017;
    double ratio = 1.5e-3f + .25;
    long big = 1000000UL;
    char c = 'x', nl = '\n', q = '\'';
    Node head = { .value = 42, .next = NULL };
    u64_t id = 7;
    if (argc > 1 && argv[1][0] != '-') {
        printf("%s has %d items, %5.2f%% done, %lu\n", argv[1], MAX_ITEMS, ratio * 100, big);
    } else {
        puts("usage: prog \"file\" \
more text");
    }
    switch (c) {
    case 'x': break;
    default:  goto done;
    }
done:
    return sizeof(Node) > 8 ? 0 : (int)id;
}
