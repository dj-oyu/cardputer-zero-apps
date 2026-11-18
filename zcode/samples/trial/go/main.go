// Go のサンプル（F5 で実行、F6 で go test）。
package main

import "fmt"

func main() {
	for _, n := range []int{1, 2, 3, 10} {
		fmt.Printf("%d の階乗 = %d\n", n, factorial(n))
	}
}
