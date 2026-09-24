package session

import (
	"fmt"
	"reflect"
	"slices"
	"testing"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
	"code.marleb.org/shgew/shycler/internal/sim"
)

func TestResidentInstabilitiesConverge(t *testing.T) {
	t.Parallel()
	const seeds = 12
	type result struct{ suspect, proven int }
	results := make([]result, seeds)
	t.Run("seeds", func(t *testing.T) {
		for seed := 1; seed <= seeds; seed++ {
			t.Run(fmt.Sprint(seed), func(t *testing.T) {
				t.Parallel()
				dir := t.TempDir()
				m := newSim(t, sim.Config{Seed: uint64(seed), Cores: 4})
				in := simInput(dir, m)
				in.Rotations = 20
				if stop := simulate(t, in); stop.Reason != StopRotations {
					t.Fatalf("stopped with %+v", stop)
				}
				st, err := readMemState(stateOf(dir))
				if err != nil {
					t.Fatal(err)
				}
				for _, c := range st.Cores {
					if c.Phase != journal.PhaseConfirmed || c.Offset < m.ResidentEdge(c.Core) {
						t.Errorf("core %d %s at %d, hidden resident edge %d", c.Core, c.Phase, c.Offset, m.ResidentEdge(c.Core))
					}
				}
				for _, e := range readEvents(t, dir) {
					d, ok := e.Data.(*journal.TunerDecision)
					if !ok || d.Phase != journal.PhaseGuard {
						continue
					}
					if d.ToOffset <= d.FromOffset {
						t.Errorf("seq %d deepens: %s", e.Seq, e.Msg)
					}
					switch d.Decision {
					case journal.SuspectBackoff:
						results[seed-1].suspect++
					case journal.Backoff:
						results[seed-1].proven++
					case journal.StepDeeper:
						t.Errorf("seq %d steps deeper in guard: %s", e.Seq, e.Msg)
					}
				}
			})
		}
	})
	var total result
	for _, r := range results {
		total.suspect += r.suspect
		total.proven += r.proven
	}
	if total.suspect == 0 || total.proven == 0 {
		t.Fatalf("seeds exercise no suspect/proven backoff: %+v", total)
	}
}

func TestIdleCrashInGuard(t *testing.T) {
	t.Parallel()
	_, ref := reference(t, small())
	i := slices.IndexFunc(ref, func(e journal.Event) bool {
		p, ok := e.Data.(*journal.ProfileApplied)
		return ok && p.Condition == machine.Resident
	})
	if i < 0 {
		t.Fatal("reference run never applied the resident profile")
	}
	dir := t.TempDir()
	m := newSim(t, small())
	if stop := drive(t, simInput(dir, m), crashAt(ref[i].Seq, m)); stop.Reason != StopRotations {
		t.Fatalf("stopped with %+v", stop)
	}
	events := readEvents(t, dir)
	crash, ok := crashDetectedFor(events, ref[i].Boot)
	if !ok || crash.Data.(*journal.CrashDetected).Condition != machine.Resident {
		t.Fatalf("crash.detected for the crashed boot: %+v", crash.Data)
	}
	want := &journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R6, Condition: machine.Resident}
	f := slices.IndexFunc(events, func(e journal.Event) bool {
		return e.Kind == journal.KindFailure && slices.Contains(e.Cause, crash.Seq)
	})
	if f < 0 {
		t.Fatalf("no failure cites crash.detected seq %d", crash.Seq)
	}
	if !reflect.DeepEqual(events[f].Data, want) {
		t.Fatalf("failure citing seq %d: %+v, want %+v", crash.Seq, events[f].Data, want)
	}
	var backedOff []int
	opened := false
	for _, e := range events[f+1:] {
		switch p := e.Data.(type) {
		case *journal.TunerDecision:
			if p.Decision == journal.SuspectBackoff && slices.Contains(e.Cause, events[f].Seq) {
				backedOff = append(backedOff, p.Core)
			}
		case *journal.EscalationWindow:
			opened = opened || (p.State == journal.WindowOpen && slices.Contains(e.Cause, events[f].Seq))
		}
	}
	if !slices.Equal(backedOff, []int{0, 1}) || !opened {
		t.Fatalf("suspect backoffs of cores %v, window opened %v; want cores [0 1] and the window", backedOff, opened)
	}
}

func TestResumeContinuesBoots(t *testing.T) {
	t.Parallel()
	dir, first := reference(t, small())
	cfg, err := Resume(dir, small())
	if err != nil {
		t.Fatal(err)
	}
	boots := map[string]bool{}
	for _, e := range first {
		boots[e.Boot] = true
	}
	last := first[len(first)-1].Time
	if cfg.Boots != len(boots) || !cfg.Start.After(last) {
		t.Fatalf("Resume: %d boots from %s, want %d after %s", cfg.Boots, cfg.Start, len(boots), last)
	}
	in := simInput(dir, newSim(t, cfg))
	in.Rotations = 2
	if stop := simulate(t, in); stop.Reason != StopRotations {
		t.Fatalf("second run stopped with %+v", stop)
	}
	events := readEvents(t, dir)
	if len(events) == len(first) {
		t.Fatal("second run appended nothing")
	}
	for _, e := range events[len(first):] {
		if boots[e.Boot] {
			t.Fatalf("seq %d reuses boot %s", e.Seq, e.Boot)
		}
		if e.Time.Before(last) {
			t.Fatalf("seq %d at %s, before %s", e.Seq, e.Time, last)
		}
		last = e.Time
	}
}

// A boot's kernel log starts with the MCEs its predecessor's crash left in the banks. Once the predecessor's
// crash.detected cites them, they are no evidence of the later boot's own crash.
func TestCrashEvidenceIsClaimedOnce(t *testing.T) {
	t.Parallel()
	f := newFold()
	for _, e := range []journal.Event{
		{Seq: 1, Boot: "a", Kind: journal.KindSessionStart, Data: &journal.SessionStart{}},
		{Seq: 2, Boot: "b", Kind: journal.KindMCE, Data: &journal.MCE{Core: 3, BankType: machine.LoadStore, FromBoot: "b", Lines: []string{"left by a"}}},
		{Seq: 3, Boot: "b", Kind: journal.KindCrashDetected, Data: &journal.CrashDetected{PreviousBoot: "a"}, Cause: []int{2}},
	} {
		f.Fold(e)
	}
	if got := f.recordedFor("a", "b"); !slices.Equal(got, []int{2}) {
		t.Fatalf("evidence of boot a: %v, want [2]", got)
	}
	if got := f.recordedFor("b", "c"); len(got) != 0 {
		t.Fatalf("evidence of boot b: %v, want none", got)
	}
}
