package watch

import (
	"context"
	"testing"
	"testing/synctest"
)

func TestJournalChangeWaitsForItsReload(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		w := &journalWatch{changes: make(chan error), reloaded: make(chan struct{})}
		sent := make(chan bool, 1)
		go func() { sent <- w.send(context.Background(), nil) }()
		if err := <-w.changes; err != nil {
			t.Fatal(err)
		}
		synctest.Wait()
		select {
		case <-sent:
			t.Fatal("the notifier went on before the consumer reloaded for its change")
		default:
		}
		w.reloaded <- struct{}{}
		if !<-sent {
			t.Fatal("an acknowledged change stopped the notifier")
		}
	})
}
