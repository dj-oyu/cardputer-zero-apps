package main

// factorial は n の階乗（n が 0 以下なら 1）。
func factorial(n int) int {
	if n <= 1 {
		return 1
	}
	return n * factorial(n-1)
}
