/* 2 つのファイルとヘッダをビルド係で作る（F5）。 */
#include <stdio.h>
#include "util.h"

int main(void) {
    int xs[] = {3, 1, 4, 1, 5, 9, 2, 6};
    int n = (int)(sizeof xs / sizeof xs[0]);
    printf("合計 %d、最大 %d\n", sum(xs, n), max_of(xs, n));
    return 0;
}
