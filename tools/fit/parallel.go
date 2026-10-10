package main

import (
	"runtime"
	"sync"
	"sync/atomic"
)

// parallelFor calls do(k) for every k in [0, n) on at most GOMAXPROCS goroutines, the caller's included, and waits
// for all of them. Each call must write only its own results.
func parallelFor(n int, do func(k int)) {
	var next atomic.Int64
	work := func() {
		for {
			k := int(next.Add(1)) - 1
			if k >= n {
				return
			}
			do(k)
		}
	}
	var wg sync.WaitGroup
	for range min(n, runtime.GOMAXPROCS(0)) - 1 {
		wg.Go(work)
	}
	work()
	wg.Wait()
}

func fitParallel[T any](count, jobs int, fit func(int) (T, error)) ([]T, error) {
	results := make([]T, count)
	queue := make(chan int)
	failed := make(chan struct{})
	var first sync.Once
	var failure error
	var wg sync.WaitGroup
	for range min(jobs, count) {
		wg.Go(func() {
			for index := range queue {
				select {
				case <-failed:
					return
				default:
				}
				result, err := fit(index)
				if err != nil {
					first.Do(func() {
						failure = err
						close(failed)
					})
					return
				}
				results[index] = result
			}
		})
	}
dispatch:
	for index := range count {
		select {
		case <-failed:
			break dispatch
		case queue <- index:
		}
	}
	close(queue)
	wg.Wait()
	if failure != nil {
		return nil, failure
	}
	return results, nil
}
