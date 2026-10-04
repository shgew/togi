package watch

import (
	"strings"
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

func TestCoreRowsNameR7TopRequestersAndSelfSufficiency(t *testing.T) {
	workload := machine.Workloads(machine.R7)[0].ID
	s := Snapshot{r7: []tuner.R7CoreStatus{
		{Core: 3, CCD: 0, Workload: workload, TopRequester: true, SelfSufficient: true, Passes: 1},
		{Core: 7, CCD: 0, Workload: workload, OffsetFallback: true},
	}}
	text := strings.Join(s.coreRows(240), "\n")
	for _, want := range []string{"top requesters core 03", "Self-sufficient: core 03", "not yet demonstrated: core 07", "not a guarantee"} {
		if !strings.Contains(text, want) {
			t.Fatalf("missing %q in R7 rows:\n%s", want, text)
		}
	}
	s.r7[0].OffsetFallback = true
	if text := strings.Join(s.coreRows(240), "\n"); !strings.Contains(text, "offset fallback") {
		t.Fatalf("fallback must not claim a voltage measurement: %s", text)
	}
}

func TestToleratedFailureDoesNotReadAsBackoff(t *testing.T) {
	tag, text, _ := decisionText(&journal.TunerDecision{Core: 3, Decision: journal.Tolerate, FromOffset: -20, ToOffset: -20, Reason: "1 failure in 10 starts"})
	if tag == tagBackoff || !strings.Contains(text, "stays at -20") || !strings.Contains(text, "1 failure in 10 starts") {
		t.Fatalf("tolerance misreported: %s %s", tag, text)
	}
}
