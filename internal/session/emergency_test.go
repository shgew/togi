package session

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"strings"
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

type failAppendJournal struct {
	Journal
	kind   journal.Kind
	failed bool
}

func (j *failAppendJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	if !j.failed && p.Kind() == j.kind {
		j.failed = true
		return journal.Event{}, io.ErrClosedPipe
	}
	return j.Journal.Append(p, cause...)
}

func TestJournalFailureZerosMachineAndStopsAppending(t *testing.T) {
	for _, kind := range []journal.Kind{journal.KindTrialStart, journal.KindTrialProgress} {
		t.Run(string(kind), func(t *testing.T) {
			cfg := small()
			cfg.Script = map[string]sim.Outcome{"0001": {Signal: machine.ComputationError, AtS: 1, Core: 0}}
			m := newSim(t, cfg)
			in := simInput(t.TempDir(), m)
			var stderr bytes.Buffer
			in.Stderr = &stderr
			var faulty *failAppendJournal
			_, err := simulateBoot(context.Background(), in, func(j *journal.Journal) Journal {
				faulty = &failAppendJournal{Journal: j, kind: kind}
				return faulty
			})
			if !faulty.failed || !errors.Is(err, io.ErrClosedPipe) || !strings.HasPrefix(err.Error(), "journal write: ") {
				t.Fatalf("journal failure %+v, err %v", faulty, err)
			}
			want := "togi: journal write failed: io: read/write on closed pipe; every core set to CO 0 without an intent (readback all 0)\n"
			if got := stderr.String(); got != want {
				t.Fatalf("stderr mismatch (-want +got):\n%s", fmt.Sprintf("%q != %q", got, want))
			}
			for core := range cfg.BIOS {
				got, err := m.Seams().SMU.Offset(core)
				if err != nil || got != 0 {
					t.Errorf("core %d reads %d (%v), want 0", core, got, err)
				}
			}
			events := readEvents(t, in.Dir)
			for _, e := range events {
				if e.Kind == kind || e.Kind == journal.KindTrialEnd {
					t.Errorf("event after failed append: %s", e.Kind)
				}
			}
		})
	}
}
