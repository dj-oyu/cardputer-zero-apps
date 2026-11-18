package main

import "testing"

func TestFactorial(t *testing.T) {
	cases := map[int]int{0: 1, 1: 1, 5: 120, 10: 3628800}
	for n, want := range cases {
		if got := factorial(n); got != want {
			t.Errorf("factorial(%d) = %d、期待は %d", n, got, want)
		}
	}
}
