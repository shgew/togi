package carry

import (
	"bytes"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestPrepareEvidenceEpochTransition(t *testing.T) {
	for _, tc := range []struct {
		name                                         string
		evidence                                     int
		deferred, transition, unknown, refuseUnknown bool
	}{
		{name: "older epoch", evidence: 1, transition: true},
		{name: "unstamped ruleset six epoch", transition: true},
		{name: "deferred older epoch", evidence: 1, transition: true, deferred: true},
		{name: "same epoch", evidence: 2},
		{name: "newer epoch", evidence: 3},
		{name: "older epoch with unknown kind", evidence: 1, transition: true, unknown: true},
		{name: "same epoch with unknown kind", evidence: 2, unknown: true, refuseUnknown: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			binary := journal.Build{Schema: journal.Schema, Ruleset: 6, EvidenceEpoch: 2}
			old, err := journal.Open(dir, opts())
			if err != nil {
				t.Fatal(err)
			}
			w := &writer{t: t, j: old, session: "20261001T000000Z"}
			w.add(&journal.SessionStart{Build: journal.Build{Schema: journal.Schema, Ruleset: 6}, Session: w.session, Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}, Evidence: tc.evidence})
			w.add(&journal.SessionContext{BIOSContext: context})
			factTrial(w, 0, journal.OutcomePass)
			failure, mark := factTrial(w, 1, journal.OutcomeFailure)
			originalFacts := facts.FromEvents(old.Events()).Facts
			w.close()
			path := filepath.Join(dir, "events.jsonl")
			if tc.unknown {
				f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0)
				if err != nil {
					t.Fatal(err)
				}
				_, writeErr := f.WriteString("{\"seq\":8,\"kind\":\"future.unknown\",\"msg\":\"future\"}\n")
				closeErr := f.Close()
				if err := errors.Join(writeErr, closeErr); err != nil {
					t.Fatal(err)
				}
			}
			before, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			j, err := journal.Lock(dir, opts())
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			current := &context
			if tc.deferred {
				current = nil
			}
			c, err := Prepare(j, binary, []defect.Entry{}, current)
			if tc.refuseUnknown {
				var unknown *journal.UnknownKindError
				if !errors.As(err, &unknown) {
					t.Fatalf("Prepare = %v, want unknown-kind refusal", err)
				}
			} else if err != nil {
				t.Fatal(err)
			}
			if !tc.transition {
				if c != nil {
					t.Fatalf("same/newer epoch must not transition: %+v", c)
				}
				after, err := os.ReadFile(path)
				if err != nil || !bytes.Equal(before, after) {
					t.Fatalf("non-transition changed live journal: %v", err)
				}
				if pending, err := journal.PendingCarry(dir); err != nil || pending != "" {
					t.Fatalf("non-transition created pending carry %q: %v", pending, err)
				}
				if _, err := os.Stat(filepath.Join(dir, "archive", w.session+".jsonl")); !errors.Is(err, fs.ErrNotExist) {
					t.Fatalf("non-transition archived source: %v", err)
				}
				return
			}
			if c == nil {
				t.Fatal("epoch-only upgrade must prepare a transition")
			}
			if tc.deferred {
				if err := c.ResolveFacts(&context); err != nil {
					t.Fatal(err)
				}
			}
			want := []facts.Fact{originalFacts[1]}
			if diff := cmp.Diff(want, c.Facts); diff != "" {
				t.Fatalf("upgrade must retain failure provenance and discard old-epoch pass (-want +got):\n%s", diff)
			}
			if c.Facts[0].Session != failure.session || c.Facts[0].Seq != failure.seq || len(c.Cores) != 2 || c.Cores[1].FailedMark == nil || *c.Cores[1].FailedMark != -30 || c.Cores[1].MarkSeq != mark {
				t.Fatalf("failure and failed mark not preserved: %+v", c)
			}
			archived, err := os.ReadFile(filepath.Join(dir, "archive", w.session+".jsonl"))
			if err != nil || !bytes.Equal(before, archived) {
				t.Fatalf("epoch transition changed archived source: %v", err)
			}
			if _, err := os.Stat(path); !errors.Is(err, fs.ErrNotExist) {
				t.Fatalf("epoch transition retained live journal: %v", err)
			}
			if pending, err := journal.PendingCarry(dir); err != nil || pending != w.session {
				t.Fatalf("epoch transition pending carry = %q, error %v", pending, err)
			}
		})
	}
}
