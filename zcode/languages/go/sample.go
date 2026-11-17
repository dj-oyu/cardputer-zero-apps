// 見本: Go の宣言・文字列・ルーン・goroutine
package main

import (
	"errors"
	"fmt"
	"os"
)

const (
	MaxItems = 64
	Version  = "1.0"
	mask     = 0xFF_FF
)

type Stack[T any] struct {
	items []T
}

type Shape interface {
	Area() float64
}

type Circle struct {
	R    float64 `json:"r"`
	Name string  `json:"name,omitempty"`
}

var ErrEmpty = errors.New("stack is empty")

/* ブロック
   コメント */
func (c Circle) Area() float64 {
	return 3.14159 * c.R * c.R
}

func (s *Stack[T]) Pop() (T, error) {
	var zero T
	if len(s.items) == 0 {
		return zero, ErrEmpty
	}
	v := s.items[len(s.items)-1]
	s.items = s.items[:len(s.items)-1]
	return v, nil
}

func worker(id int, jobs <-chan int, done chan<- bool) {
	for j := range jobs {
		fmt.Printf("worker %d: job %-4d %5.2f%%\n", id, j, 1e-3)
	}
	done <- true
}

func main() {
	query := `SELECT *
FROM t WHERE id = %d`
	sep, tab, jp := '\n', '\t', '日'
	jobs := make(chan int, MaxItems)
	done := make(chan bool)
	go worker(1, jobs, done)
	defer close(jobs)
	switch {
	case sep == tab:
		fmt.Println("same\t\"quoted\"", query, jp)
	default:
		fmt.Fprintln(os.Stderr, "bad", 0b101, 0o17, 1.5i)
	}
	if err := run(); err != nil {
		os.Exit(1)
	}
}
