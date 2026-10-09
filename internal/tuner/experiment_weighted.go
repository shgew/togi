package tuner

import (
	"fmt"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func (s *State) cycleStart() *journal.CheckingCycle {
	p := &journal.CheckingCycle{Cycle: s.checking.cycle + 1, Event: journal.CycleStart, Steps: slices.Clone(s.steps)}
	if !exp.Weighted {
		return p
	}
	configured := map[machine.Regime]int{}
	var order []machine.Regime
	for _, r := range s.steps {
		if configured[r] == 0 {
			order = append(order, r)
		}
		configured[r]++
	}
	failures := map[machine.Regime]int{}
	for _, f := range s.failures {
		failures[f.class.regime]++
	}
	kept := map[machine.Regime]int{}
	p.Steps = p.Steps[:0]
	for _, r := range s.steps {
		if kept[r] < min(configured[r], 1+failures[r]) {
			kept[r]++
			p.Steps = append(p.Steps, r)
		}
	}
	counts := make([]string, len(order))
	for i, r := range order {
		counts[i] = fmt.Sprintf("%s x%d (%d failures)", r, kept[r], failures[r])
	}
	p.Reason = "; weighted cycle (experiment): " + strings.Join(counts, ", ")
	return p
}

func weightedCoverageWant(want map[machine.Regime]int) map[machine.Regime]int {
	if !exp.Weighted {
		return want
	}
	for r := range want {
		want[r] = 1
	}
	return want
}
