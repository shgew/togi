package simrun

import (
	"context"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

func TestSixteenCoresReachBronze(t *testing.T) {
	dir := t.TempDir()
	m, err := sim.New(sim.Config{Seed: 1})
	if err != nil {
		t.Fatal(err)
	}
	stop, err := Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1})
	if err != nil {
		t.Fatal(err)
	}
	if stop.Reason != session.StopRotations {
		t.Fatalf("stopped with %+v", stop)
	}
	st, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(st.Cores) != 16 || st.Refine != nil || st.Tier != journal.TierBronze {
		t.Fatalf("state has %d cores, refine %+v, tier %s", len(st.Cores), st.Refine, st.Tier)
	}
	for _, c := range st.Cores {
		if c.Phase != journal.PhaseDone {
			t.Errorf("core %d is %s, want done", c.Core, c.Phase)
		}
	}
	if diff := cmp.Diff([]string(nil), m.Violations()); diff != "" {
		t.Errorf("isolation violations (-want +got):\n%s", diff)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read journal: %v, torn %q", err, torn)
	}
	qualifying := false
	parts := map[int]map[int]int{}
	var marks []journal.MarkJoint
	failed := map[int]int{}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.GuardRotation:
			if p.Event == journal.RotationEnd && p.Clean && p.Qualifying {
				qualifying = true
			}
		case *journal.TrialIntent:
			if p.Regime == machine.R7 && p.Phase == journal.PhaseGuard {
				if parts[p.Rotation] == nil {
					parts[p.Rotation] = map[int]int{}
				}
				parts[p.Rotation][p.DurationS]++
			}
			for core, at := range failed {
				if p.Profile[core] <= at {
					t.Errorf("trial %s reaches failed mark of core %d at %d", p.Trial, core, at)
				}
			}
			for _, mark := range marks {
				reaches := true
				for _, member := range mark.Members {
					if p.Profile[member.Core] > member.Offset {
						reaches = false
						break
					}
				}
				if reaches {
					t.Errorf("trial %s reaches joint mark J%d", p.Trial, mark.Mark)
				}
			}
		case *journal.Failure:
			if p.Attribution == journal.Attributed && p.Core != nil && p.Offset != nil {
				failed[*p.Core] = *p.Offset
			}
		case *journal.MarkJoint:
			marks = append(marks, *p)
		}
	}
	if !qualifying {
		t.Error("no clean qualifying rotation end")
	}
	found := false
	for _, durations := range parts {
		if durations[120] >= 9 && durations[300] >= 6 && durations[600] >= 3 {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("R7 parts did not run three starts of 120s and long 300/300/600s: %v", parts)
	}
}
