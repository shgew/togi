package simrun

import (
	"context"
	"strings"
	"testing"
	"time"

	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
	"code.marleb.org/shgew/shycler/internal/session"
	"code.marleb.org/shgew/shycler/internal/sim"
)

func TestSixteenCoresSurviveARotation(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m, err := sim.New(sim.Config{Seed: 1})
	if err != nil {
		t.Fatal(err)
	}
	began := time.Now()
	stop, err := Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1})
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("16-core session took %s", time.Since(began))
	if stop.Reason != session.StopRotations {
		t.Fatalf("stopped with %+v", stop)
	}
	st, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if st.Phase != "guard" || len(st.Cores) != 16 || st.Guard == nil || st.Guard.CleanRotations != 1 || st.Tier != journal.TierBronze {
		t.Fatalf("state phase %s tier %s with %d cores, guard %+v", st.Phase, st.Tier, len(st.Cores), st.Guard)
	}
	for _, c := range st.Cores {
		if c.Phase != journal.PhaseConfirmed || c.Offset < m.ResidentEdge(c.Core) || c.Offset > 0 {
			t.Errorf("core %d %s at %d, hidden resident edge %d", c.Core, c.Phase, c.Offset, m.ResidentEdge(c.Core))
		}
	}
	if v := m.Violations(); len(v) > 0 {
		t.Errorf("isolation violations: %v", v)
	}
	regimes := map[string]machine.Regime{}
	counted := map[string]bool{}
	progress := map[machine.Regime]map[string]bool{}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read journal: %v, torn %q", err, torn)
	}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.SMUIntent:
			if p.Offset < machine.MinOffset || p.Offset > machine.MaxOffset {
				t.Errorf("seq %d writes %d", e.Seq, p.Offset)
			}
		case *journal.TrialIntent:
			regimes[p.Trial] = p.Regime
		case *journal.TrialProgress:
			r := regimes[p.Trial]
			if progress[r] == nil {
				progress[r] = map[string]bool{}
			}
			progress[r][p.Detail] = true
		case *journal.TrialSignal:
			if p.Schedule == "" {
				counted[p.Trial] = true
			}
		case *journal.TrialEnd:
			r := regimes[p.Trial]
			if (r == machine.R3 || r == machine.R4) && p.Signal != machine.Crash && !p.Interrupted && !counted[p.Trial] {
				t.Errorf("trial %s (%s) ended without its load-step counts", p.Trial, r)
			}
		}
	}
	for _, tc := range []struct {
		regime machine.Regime
		detail string
	}{
		{machine.R6, "first half idle, then 100ms bursts every 2s, one core at a time"},
		{machine.R7, "CCD0 only: stopped cores 08-15"},
		{machine.R7, "CCD1 only: resumed cores 08-15, stopped cores 00-07"},
	} {
		if !progress[tc.regime][tc.detail] {
			t.Errorf("simulated journal lacks %s progress %q", tc.regime, tc.detail)
		}
	}
	foundBursts := false
	for detail := range progress[machine.R6] {
		if strings.HasPrefix(detail, "bursts: ") && strings.Contains(detail, " continues, ") && strings.HasSuffix(detail, " stops") {
			foundBursts = true
		}
	}
	if !foundBursts {
		t.Error("simulated journal lacks R6 burst counts")
	}
}
