package tuner

import (
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestWeightedCycleKeepsRegimeRepeatsByFailures(t *testing.T) {
	old := exp
	exp.Weighted = true
	t.Cleanup(func() { exp = old })
	h := hasRoomHarness(t, -10, -12)
	h.s.steps = []machine.Regime{machine.R7, machine.R1, machine.R2, machine.R1, machine.R6, machine.R1, machine.R7}
	for range 5 {
		h.s.failures = append(h.s.failures, entry{class: trialClass{regime: machine.R1}})
	}
	h.s.failures = append(h.s.failures, entry{class: trialClass{regime: machine.R6}})
	p := h.s.cycleStart()
	want := []machine.Regime{machine.R7, machine.R1, machine.R2, machine.R1, machine.R6, machine.R1}
	if diff := cmp.Diff(want, p.Steps); diff != "" {
		t.Fatalf("steps (-want +got):\n%s", diff)
	}
	if !strings.Contains(p.Reason, "R7 x1 (0 failures), R1 x3 (5 failures)") {
		t.Fatalf("reason: %q", p.Reason)
	}
}
