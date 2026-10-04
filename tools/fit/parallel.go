package main

import "sync"

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
