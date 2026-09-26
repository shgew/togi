package session

import (
	"context"
	"fmt"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/shycler/internal/journal"
	"github.com/shgew/shycler/internal/machine"
	"github.com/shgew/shycler/internal/sim"
	"github.com/shgew/shycler/internal/tuner"
)

func TestResidentInstabilitiesConverge(t *testing.T) {
	t.Parallel()
	const seeds = 12
	type result struct{ suspect, proven, regain int }
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
				events := readEvents(t, dir)
				perCause := map[[2]int]bool{}
				regainedTo := map[[2]int]bool{}
				for _, e := range events {
					d, ok := e.Data.(*journal.TunerDecision)
					if !ok || d.Phase != journal.PhaseGuard {
						continue
					}
					if d.Decision == journal.Regain {
						results[seed-1].regain++
						if d.ToOffset != d.FromOffset-1 {
							t.Errorf("seq %d regains more than one count: %s", e.Seq, e.Msg)
						}
						if len(e.Cause) != 1 || !cleanEnd(events[e.Cause[0]-1]) {
							t.Errorf("seq %d regains without citing a clean rotation end: %v", e.Seq, e.Cause)
							continue
						}
						if perCause[[2]int{d.Core, e.Cause[0]}] || regainedTo[[2]int{d.Core, d.ToOffset}] {
							t.Errorf("seq %d regains again: %s", e.Seq, e.Msg)
						}
						perCause[[2]int{d.Core, e.Cause[0]}], regainedTo[[2]int{d.Core, d.ToOffset}] = true, true
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
					case journal.Regain:
					}
				}
			})
		}
	})
	var total result
	for _, r := range results {
		total.suspect += r.suspect
		total.proven += r.proven
		total.regain += r.regain
	}
	if total.suspect == 0 || total.proven == 0 || total.regain == 0 {
		t.Fatalf("seeds exercise no suspect/proven backoff or regain: %+v", total)
	}
}

func cleanEnd(e journal.Event) bool {
	p, ok := e.Data.(*journal.GuardRotation)
	return ok && p.Event == journal.RotationEnd && p.Clean
}

func residentApplied(t *testing.T, ref []journal.Event) int {
	t.Helper()
	applied := slices.IndexFunc(ref, func(e journal.Event) bool {
		p, ok := e.Data.(*journal.ProfileApplied)
		return ok && p.Condition == machine.Resident
	})
	if applied < 0 {
		t.Fatal("reference run never applied the resident profile")
	}
	return applied
}

func TestIdleCrashInGuard(t *testing.T) {
	t.Parallel()
	_, ref := reference(t, small())
	applied := residentApplied(t, ref)
	firstWrite := -1
	for j := applied - 1; j >= 0 && isSMU(ref[j]); j-- {
		if p, ok := ref[j].Data.(*journal.SMUIntent); ok && p.Offset != 0 {
			firstWrite = j
		}
	}
	if firstWrite < 0 {
		t.Fatalf("no nonzero smu.intent before the resident profile.applied at seq %d", ref[applied].Seq)
	}
	for _, tc := range []struct {
		name string
		at   journal.Event
	}{
		{"after profile.applied", ref[applied]},
		{"part-way through the application", ref[firstWrite]},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			m := newSim(t, small())
			if stop := drive(t, simInput(dir, m), crashAt(tc.at.Seq, m)); stop.Reason != StopRotations {
				t.Fatalf("stopped with %+v", stop)
			}
			events := readEvents(t, dir)
			crash, ok := crashDetectedFor(events, tc.at.Boot)
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
			if diff := cmp.Diff(want, events[f].Data); diff != "" {
				t.Fatalf("failure citing seq %d mismatch (-want +got):\n%s", crash.Seq, diff)
			}
			var backedOff []int
			for _, e := range events[f+1:] {
				if p, ok := e.Data.(*journal.TunerDecision); ok && p.Decision == journal.SuspectBackoff && slices.Contains(e.Cause, events[f].Seq) {
					backedOff = append(backedOff, p.Core)
				}
			}
			if !slices.Equal(backedOff, []int{0, 1}) {
				t.Fatalf("suspect backoffs of cores %v, want [0 1]", backedOff)
			}
			if len(regainsIn(events)) != 0 {
				t.Fatal("regained with --rotations 1; the run should stop at the clean rotation end")
			}
			if last, ok := events[len(events)-1].Data.(*journal.Shutdown); !ok || last.Reason != journal.ShutdownRotations {
				t.Fatalf("last event %s, want shutdown for rotations", events[len(events)-1].Msg)
			}
		})
	}
}

func regainsIn(events []journal.Event) []journal.Event {
	var out []journal.Event
	for _, e := range events {
		if d, ok := e.Data.(*journal.TunerDecision); ok && d.Decision == journal.Regain {
			out = append(out, e)
		}
	}
	return out
}

func TestRegainAfterCleanRotation(t *testing.T) {
	t.Parallel()
	_, ref := reference(t, small())
	at := ref[residentApplied(t, ref)].Seq
	run := func(t *testing.T, ctx context.Context, tr func(*sim.Machine) *trigger) (Stop, string, []journal.Event) {
		t.Helper()
		dir := t.TempDir()
		m := newSim(t, small())
		in := simInput(dir, m)
		in.Rotations = 2
		stop, err := runSim(ctx, in, tr(m))
		if err != nil {
			t.Fatalf("simulate: %v", err)
		}
		return stop, dir, readEvents(t, dir)
	}
	stop, dir, events := run(t, context.Background(), func(m *sim.Machine) *trigger { return crashAt(at, m) })
	if stop.Reason != StopRotations {
		t.Fatalf("stopped with %+v", stop)
	}
	order := tuner.Order(events[0].Data.(*journal.SessionStart).Cores)
	oneEach := func(t *testing.T, events []journal.Event) journal.Event {
		t.Helper()
		regains := regainsIn(events)
		var cores []int
		for _, e := range regains {
			cores = append(cores, e.Data.(*journal.TunerDecision).Core)
			if len(e.Cause) != 1 || e.Cause[0] != regains[0].Cause[0] {
				t.Fatalf("regain seq %d cites %v, want %v like the first", e.Seq, e.Cause, regains[0].Cause)
			}
		}
		if !slices.Equal(cores, order) {
			t.Fatalf("regained cores %v, want one each in scheduling order %v", cores, order)
		}
		end := events[regains[0].Cause[0]-1]
		if !cleanEnd(end) {
			t.Fatalf("regains cite %s, want a clean rotation end", end.Msg)
		}
		return end
	}
	end := oneEach(t, events)
	last := regainsIn(events)[len(order)-1]
	if next := events[last.Seq]; next.Kind != journal.KindProfileChange {
		t.Fatalf("after the last regain: %s, want profile.change", next.Msg)
	}
	st, err := readMemState(stateOf(dir))
	if err != nil {
		t.Fatal(err)
	}
	var before []int
	for _, e := range events[:at] {
		if p, ok := e.Data.(*journal.ProfileChange); ok {
			before = p.To
		}
	}
	if st.Tier != journal.TierBronze || !slices.Equal(st.Guard.Profile, before) {
		t.Fatalf("tier %s, profile %v; want bronze and the pre-crash profile %v", st.Tier, st.Guard.Profile, before)
	}

	t.Run("kill", func(t *testing.T) {
		t.Parallel()
		first := regainsIn(events)[0].Seq
		_, _, killed := run(t, context.Background(), func(m *sim.Machine) *trigger { return crashAt(at, m).then(killAt(first)) })
		if got := oneEach(t, killed); got.Seq != end.Seq {
			t.Fatalf("regains cite seq %d, want %d", got.Seq, end.Seq)
		}
	})
	t.Run("signal", func(t *testing.T) {
		t.Parallel()
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		stop, _, signalled := run(t, ctx, func(m *sim.Machine) *trigger { return crashAt(at, m).then(cancelAt(end.Seq, cancel)) })
		if stop.Reason != StopSignal || len(regainsIn(signalled)) != 0 {
			t.Fatalf("stopped with %+v after %d regains; want a signal stop before any", stop, len(regainsIn(signalled)))
		}
		if p, ok := signalled[len(signalled)-1].Data.(*journal.Shutdown); !ok || p.Reason != journal.ShutdownSignal {
			t.Fatalf("last event %s, want shutdown for the signal", signalled[len(signalled)-1].Msg)
		}
	})
}

func isSMU(e journal.Event) bool {
	switch e.Data.(type) {
	case *journal.SMUIntent, *journal.SMUWrite, *journal.SMUReadback:
		return true
	}
	return false
}

func TestResumeContinuesBoots(t *testing.T) {
	t.Parallel()
	dir, first := reference(t, small())
	cfg, err := sim.Resume(dir, small())
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
