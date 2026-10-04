package watch

import (
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"

	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

func TestR7RowsNameTopRequestersAndSelfSufficiency(t *testing.T) {
	workload := machine.Workloads(machine.R7)[0].ID
	s := Snapshot{r7: []tuner.R7CoreStatus{
		{Core: 3, CCD: 0, Workload: workload, TopRequester: true, SelfSufficient: true, Passes: 1},
		{Core: 7, CCD: 0, Workload: workload, OffsetFallback: true},
	}}
	text := strings.Join(s.r7Rows(240, false), "\n")
	for _, want := range []string{"top requesters core 03", "Self-sufficient: core 03", "not yet demonstrated: core 07", "not a guarantee"} {
		if !strings.Contains(text, want) {
			t.Fatalf("missing %q in R7 rows:\n%s", want, text)
		}
	}
	s.r7[0].OffsetFallback = true
	for _, compact := range []bool{false, true} {
		if text := strings.Join(s.r7Rows(240, compact), "\n"); !strings.Contains(text, "(offset") {
			t.Fatalf("fallback must not claim a voltage measurement (compact %t): %s", compact, text)
		}
	}
}

func TestShortDashboardReachesEveryR7Workload(t *testing.T) {
	t.Parallel()
	for _, c := range watchCuts(t) {
		if c.name != "checking" {
			continue
		}
		s, now := Project(c.events), cutTime(c.events)
		frame := ansi.Strip(Render(s, 120, 33, now))
		for _, w := range machine.Workloads(machine.R7) {
			name := strings.Join(strings.Fields(workloadLabel(w.ID))[:2], " ")
			if !strings.Contains(frame, "R7 "+name+": top ") || !strings.Contains(frame, "self-sufficient: ") {
				t.Errorf("120x33 frame omits the R7 %s status:\n%s", w.ID, frame)
			}
		}
	}
}
