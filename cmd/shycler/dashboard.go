package main

import (
	"context"
	"fmt"
	"os"
	"sync"

	"code.marleb.org/shgew/shycler/internal/watch"
)

// dashboard draws the session on out while it shows and drops the event lines written to it meanwhile; they stay in
// the journal. Hidden, it forwards them to out.
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
		err := watch.Run(ctx, d.dir, d.out)
		d.mu.Lock()
		defer d.mu.Unlock()
		d.showing = false
		if err != nil {
			fmt.Fprintf(d.out, "shycler run: dashboard: %v; printing events instead\n", err)
		}
	}()
}

func (d *dashboard) hide() {
	d.cancel()
	<-d.done
}
