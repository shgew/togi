package session

import (
	"context"
	"fmt"
	"testing"
	"time"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type frozenSessionClock struct {
	machine.Clock
	now time.Time
}

func (c frozenSessionClock) Now() time.Time { return c.now }

func TestFrozenClockSessionsArchiveWithoutCollision(t *testing.T) {
	dir := t.TempDir()
	m := newSim(t, small())
	now := time.Date(2026, 10, 2, 0, 0, 0, 0, time.UTC)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	for n := 1; n <= 3; n++ {
		seams := m.Seams()
		seams.Clock = frozenSessionClock{Clock: seams.Clock, now: now}
		j, err := journal.Open(dir, journal.Options{Boot: fmt.Sprintf("boot-%d", n), Now: func() time.Time { return now }, Build: Build()})
		if err != nil {
			t.Fatal(err)
		}
		_, err = Run(ctx, Input{Config: config.Default(), Boot: fmt.Sprintf("boot-%d", n), Journal: j, Machine: seams, SessionID: j.SessionID})
		if err != nil {
			_ = j.Close()
			t.Fatal(err)
		}
		id := j.Events()[0].Data.(*journal.SessionStart).Session
		want := "20261002T000000Z"
		if n > 1 {
			want += fmt.Sprintf("-%d", n)
		}
		if id != want {
			_ = j.Close()
			t.Fatalf("session %d identity %q, want %q", n, id, want)
		}
		_, archiveErr := j.Archive(id)
		closeErr := j.Close()
		if archiveErr != nil || closeErr != nil {
			t.Fatalf("archive session %d: %v, %v", n, archiveErr, closeErr)
		}
	}
}
