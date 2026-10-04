package main

import (
	"errors"
	"fmt"
	"slices"
	"testing"
	"testing/synctest"

	"github.com/google/go-cmp/cmp"
)

func TestFitParallelKeepsIndexOrder(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		release := []chan struct{}{make(chan struct{}), make(chan struct{}), make(chan struct{})}
		completed := make(chan int, len(release))
		var results []int
		var err error
		go func() {
			results, err = fitParallel(len(release), len(release), func(index int) (int, error) {
				<-release[index]
				completed <- index
				return index, nil
			})
		}()
		for index, ready := range slices.Backward(release) {
			close(ready)
			if got := <-completed; got != index {
				t.Fatalf("completed fit %d, want %d", got, index)
			}
			synctest.Wait()
		}
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff([]int{0, 1, 2}, results); diff != "" {
			t.Fatalf("fits reported in completion order (-want +got):\n%s", diff)
		}
	})
}

func TestFitParallelBoundsWorkersAndReturnsFirstFailure(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		release := [2]chan struct{}{make(chan struct{}), make(chan struct{})}
		started := make(chan int, 5)
		want := errors.New("first failure")
		var err error
		go func() {
			_, err = fitParallel(5, 2, func(index int) (int, error) {
				started <- index
				if index < len(release) {
					<-release[index]
				}
				if index == 0 {
					return 0, fmt.Errorf("fit %d: %w", index, want)
				}
				return index, errors.New("later failure")
			})
		}()
		synctest.Wait()
		if diff := cmp.Diff(2, len(started)); diff != "" {
			t.Fatalf("worker limit (-want +got):\n%s", diff)
		}
		close(release[0])
		synctest.Wait()
		if diff := cmp.Diff(2, len(started)); diff != "" {
			t.Fatalf("started queued fits after failure (-want +got):\n%s", diff)
		}
		close(release[1])
		synctest.Wait()
		if !errors.Is(err, want) {
			t.Fatalf("fit failure lost: %v", err)
		}
	})
}
