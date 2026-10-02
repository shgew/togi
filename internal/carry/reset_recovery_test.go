package carry

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestInterruptedResetArchivesDiscardedLiveSessionBeforeResume(t *testing.T) {
	for _, recordedReset := range []bool{false, true} {
		t.Run(fmt.Sprintf("reset event recorded %v", recordedReset), func(t *testing.T) {
			dir := t.TempDir()
			oldID := "20261002T000000Z"
			cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
			a := newJournal(t, dir, oldID, 6, &context, cores...)
			factTrial(a, 0, journal.OutcomePass)
			factTrial(a, 1, journal.OutcomeFailure)
			archive := filepath.Join(dir, "archive")
			if err := os.MkdirAll(archive, 0o755); err != nil {
				t.Fatal(err)
			}
			pending := filepath.Join(archive, "20261001T000000Z-carry-pending")
			if err := os.WriteFile(pending, nil, 0o644); err != nil {
				t.Fatal(err)
			}
			if err := a.j.MarkResetAll(); err != nil {
				t.Fatal(err)
			}
			if recordedReset {
				a.add(&journal.CommandReset{All: true})
			}
			a.close()
			before, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
			if err != nil {
				t.Fatal(err)
			}
			build := journal.Build{Schema: journal.Schema, Ruleset: 6}
			j, err := journal.Lock(dir, opts())
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			c, err := Prepare(j, build, nil, &context)
			if err != nil || c != nil {
				t.Fatalf("interrupted reset must archive rather than resume/carry: carry %+v, error %v", c, err)
			}
			after, err := os.ReadFile(filepath.Join(archive, oldID+".jsonl"))
			if err != nil || !bytes.Equal(before, after) {
				t.Fatalf("discarded live journal not archived unchanged: %v", err)
			}
			if _, err := os.Stat(pending); !os.IsNotExist(err) {
				t.Fatalf("reset pending source survived recovery: %v", err)
			}
			if err := j.Open(); err != nil {
				t.Fatal(err)
			}
			if len(j.Events()) != 0 {
				t.Fatalf("discarded session resumed: %+v", j.Events())
			}
			freshID, err := j.SessionID(time.Date(2026, 10, 2, 0, 0, 0, 0, time.UTC))
			if err != nil || journal.CompareSessionIDs(freshID, oldID) <= 0 {
				t.Fatalf("fresh ID must follow reset boundary: id %q, error %v", freshID, err)
			}
			fresh := &writer{t: t, j: j, session: freshID}
			fresh.add(&journal.SessionStart{Build: build, Session: freshID, Evidence: 1, Cores: cores})
			fresh.add(&journal.SessionContext{BIOSContext: context})
			ownPass, _ := factTrial(fresh, 0, journal.OutcomePass)
			ownFailure, _ := factTrial(fresh, 1, journal.OutcomeFailure)
			fresh.close()
			j, err = journal.Lock(dir, opts())
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			build.Ruleset++
			c, err = Prepare(j, build, nil, &context)
			if err != nil || c == nil {
				t.Fatalf("later transition lost fresh evidence: carry %+v, error %v", c, err)
			}
			if diff := cmp.Diff([]factID{ownPass, ownFailure}, factKeys(c.Facts), cmp.AllowUnexported(factID{})); diff != "" {
				t.Fatalf("later transition must preserve only post-reset evidence (-want +got):\n%s", diff)
			}
		})
	}
}
