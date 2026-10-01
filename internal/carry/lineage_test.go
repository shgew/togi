package carry

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestInterruptedTransitionRetainsOriginalPendingLineage(t *testing.T) {
	for _, loaded := range []bool{false, true} {
		t.Run(map[bool]string{false: "after start", true: "after config"}[loaded], func(t *testing.T) {
			dir := t.TempDir()
			old := newJournal(t, dir, "A", 2, &context)
			old.pass(0, -30, machine.Isolated)
			old.fail(1, -25, machine.Isolated, journal.Attributed)
			old.close()
			first := prepare(t, dir, []defect.Entry{})
			original, err := os.ReadFile(filepath.Join(dir, "archive", "A.jsonl"))
			if err != nil {
				t.Fatal(err)
			}
			interrupted := newJournal(t, dir, "B", 3, nil)
			if loaded {
				interrupted.add(&journal.ConfigLoaded{Schema: 2, Ruleset: 3})
			}
			interrupted.close()
			got := prepare(t, dir, []defect.Entry{})
			if diff := cmp.Diff(first, got); diff != "" {
				t.Fatalf("carry after second upgrade (-want +got):\n%s", diff)
			}
			if pending, err := journal.PendingCarry(dir); err != nil || pending != "A" {
				t.Fatalf("pending lineage %q, %v; want A", pending, err)
			}
			archived, err := os.ReadFile(filepath.Join(dir, "archive", "A.jsonl"))
			if err != nil || !bytes.Equal(original, archived) {
				t.Fatalf("original archive changed: %v", err)
			}
			final := newJournal(t, dir, "C", 4, &context)
			final.add(&journal.SessionCarried{Sources: got.Sources, Marks: true, Carried: got.Cores})
			final.close()
			if carry := prepare(t, dir, []defect.Entry{}); carry != nil {
				t.Fatalf("recorded carry reapplied: %+v", carry)
			}
		})
	}
}
