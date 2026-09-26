package simrun

import (
	"context"
	"maps"
	"slices"
	"strings"
	"testing"

	"github.com/shgew/shycler/internal/config"
	"github.com/shgew/shycler/internal/journal"
	"github.com/shgew/shycler/internal/machine"
	"github.com/shgew/shycler/internal/session"
	"github.com/shgew/shycler/internal/sim"
)

func TestSixteenCoresSurviveARotation(t *testing.T) {
	t.Parallel()
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
	regainable := slices.ContainsFunc(st.Cores, func(c journal.CoreState) bool { return c.UnprovenDepth-c.SettledDepth > 0 })
	wantTier := journal.TierBronze
	if regainable {
		wantTier = journal.TierNone
	}
	if st.Phase != "guard" || len(st.Cores) != 16 || st.Guard == nil || st.Guard.CleanRotations != 1 || st.Tier != wantTier {
		t.Fatalf("state phase %s tier %s (want %s) with %d cores, guard %+v", st.Phase, st.Tier, wantTier, len(st.Cores), st.Guard)
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
	intents := map[string]*journal.TrialIntent{}
	cleanRotation := 0
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.SMUIntent:
			if p.Offset < machine.MinOffset || p.Offset > machine.MaxOffset {
				t.Errorf("seq %d writes %d", e.Seq, p.Offset)
			}
		case *journal.TrialIntent:
			regimes[p.Trial] = p.Regime
			intents[p.Trial] = p
		case *journal.GuardRotation:
			if p.Event == journal.RotationEnd && p.Clean {
				cleanRotation = p.Rotation
			}
		case *journal.CorePhase:
			if p.To == journal.PhaseConfirmed {
				checkConfirmationSlots(t, events, intents, e)
			}
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
	checkR7(t, st, intents, cleanRotation)
}

// checkConfirmationSlots checks that a confirmation cites one pass for each R1 and R2 workload, R3, R4 and R5.
func checkConfirmationSlots(t *testing.T, events []journal.Event, intents map[string]*journal.TrialIntent, confirmed journal.Event) {
	t.Helper()
	var want, got []string
	for _, r := range []machine.Regime{machine.R1, machine.R2} {
		for _, w := range machine.Workloads(r) {
			want = append(want, string(r)+" "+w.ID)
		}
	}
	want = append(want, "R3", "R4", "R5")
	for _, seq := range confirmed.Cause {
		end, ok := events[seq-1].Data.(*journal.TrialEnd)
		if !ok || end.Outcome != journal.OutcomePass {
			t.Errorf("seq %d cites seq %d, not a passed trial", confirmed.Seq, seq)
			continue
		}
		intent := intents[end.Trial]
		slot := string(intent.Regime)
		if intent.Regime == machine.R1 || intent.Regime == machine.R2 {
			slot += " " + intent.Workload
		}
		got = append(got, slot)
	}
	slices.Sort(want)
	slices.Sort(got)
	if !slices.Equal(got, want) {
		t.Errorf("seq %d confirms on %v, want %v", confirmed.Seq, got, want)
	}
}

// checkR7 checks the clean rotation's R7 step: CCD0 alone, CCD1 alone, then every core, on one workload.
func checkR7(t *testing.T, st journal.State, intents map[string]*journal.TrialIntent, rotation int) {
	t.Helper()
	var ccd0, ccd1, all []int
	for _, c := range st.Cores {
		all = append(all, c.Core)
		if c.CCD == 0 {
			ccd0 = append(ccd0, c.Core)
		} else {
			ccd1 = append(ccd1, c.Core)
		}
	}
	var r7 []*journal.TrialIntent
	for _, id := range slices.Sorted(maps.Keys(intents)) {
		if p := intents[id]; p.Regime == machine.R7 && p.Rotation == rotation && !p.Retry {
			r7 = append(r7, p)
		}
	}
	want := []struct {
		cores    []int
		duration int
	}{{ccd0, 300}, {ccd1, 300}, {all, 600}}
	if len(r7) != len(want) {
		t.Fatalf("rotation %d has %d R7 trials, want %d", rotation, len(r7), len(want))
	}
	for i, w := range want {
		if !slices.Equal(r7[i].Cores, w.cores) || r7[i].DurationS != w.duration || r7[i].Workload != r7[0].Workload {
			t.Errorf("R7 trial %d: cores %v, %d s, workload %s; want cores %v, %d s, workload %s", i, r7[i].Cores, r7[i].DurationS, r7[i].Workload, w.cores, w.duration, r7[0].Workload)
		}
	}
}
