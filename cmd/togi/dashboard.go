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
	go func() {
		defer close(done)
		err := watch.Run(ctx, d.dir, d.out, nil)
		d.mu.Lock()
		defer d.mu.Unlock()
		d.showing = false
		if err != nil {
			fmt.Fprintf(d.out, "togi run: dashboard: %v; printing events instead\n", err)
		}
	}()
}

func (d *dashboard) hide() {
	d.cancel()
	<-d.done
}
