#include "util.h"

int sum(const int *xs, int n) {
    int s = 0;
    for (int i = 0; i < n; i++) {
        s += xs[i];
    }
    return s;
}

int max_of(const int *xs, int n) {
    int m = xs[0];
    for (int i = 1; i < n; i++) {
        if (xs[i] > m) {
            m = xs[i];
        }
    }
    return m;
}
