package main

import (
	"context"
	"fmt"
	"os"
	"sync"

	"github.com/shgew/togi/internal/watch"
)

// dashboard drops event lines while the journal-driven watch is visible; hidden, it forwards them to out.
// watch updates the clock once a second outside idle trials and holds still until an idle trial's planned end.
type dashboard struct {
	dir     string
	out     *os.File
	run     func(ctx context.Context, dir string, out *os.File) error // watch.Run when nil
	mu      sync.Mutex
	showing bool
	cancel  context.CancelFunc
	done    chan struct{}
}

func (d *dashboard) Write(p []byte) (int, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.showing {
		return len(p), nil
	}
	return d.out.Write(p)
}

func (d *dashboard) show() {
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	d.mu.Lock()
	d.showing, d.cancel, d.done = true, cancel, done
	d.mu.Unlock()
	run := d.run
	if run == nil {
		run = func(ctx context.Context, dir string, out *os.File) error { return watch.Run(ctx, dir, out, nil) }
	}
	go func() {
		defer close(done)
		err := contained(func() error { return run(ctx, d.dir, d.out) })
		d.mu.Lock()
		defer d.mu.Unlock()
		d.showing = false
		if err != nil {
			fmt.Fprintf(d.out, "togi run: dashboard: %v; printing events instead\n", err)
		}
	}()
}

// contained turns a panic into an error, so a dashboard bug hides the dashboard instead of stopping tuning.
func contained(f func() error) (err error) {
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("panic: %v", r)
		}
	}()
	return f()
}

func (d *dashboard) hide() {
	d.cancel()
	<-d.done
}
