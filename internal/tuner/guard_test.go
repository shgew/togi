package tuner

import (
	"fmt"
	"math/rand/v2"
	"slices"
	"strings"
	"testing"

	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
	"github.com/google/go-cmp/cmp"
)

const allCores = -1

// newGuardHarness confirms every core at its offset and failed mark, then decides until the first guard trial.
func newGuardHarness(t *testing.T, offsets []int, marks []*int) (*harness, Action) {
	t.Helper()
	cores := make([]coreStart, len(offsets))
	for i, o := range offsets {
		cores[i] = coreStart{phase: journal.PhaseConfirmed, offset: o, pass: new(o), fail: marks[i]}
	}
	h := newHarness(t, cores...)
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: config.Default()})
	return h, h.decideUntilTrial()
}

func (h *harness) decideUntilTrial() Action {
	h.t.Helper()
	for range 1000 {
		a := h.s.Next()
		if a.Kind == RunTrial {
			return a
		}
		h.decide(a)
	}
	h.t.Fatal("no trial after 1000 decisions")
	return Action{}
}

// intent records a resident guard trial on core, or on every core when core is allCores.
func (h *harness) intent(core int, r machine.Regime) journal.Event {
	h.trials++
	p := &journal.TrialIntent{
		Trial: fmt.Sprintf("%04d", h.trials), Regime: r, Workload: "w", DurationS: 120,
		Condition: machine.Resident, Phase: journal.PhaseGuard, Rotation: h.s.guard.rotation,
	}
	if core == allCores {
		for _, c := range h.s.byID() {
			p.Cores = append(p.Cores, c.id)
		}
	} else {
		p.Core, p.Offset = new(core), new(h.s.core(core).offset)
	}
	return h.add(p, h.s.guard.lastSeq)
}

func (h *harness) end(intent journal.Event, end journal.TrialEnd, cause ...int) journal.Event {
	end.Trial = intent.Data.(*journal.TrialIntent).Trial
	return h.add(&end, append([]int{intent.Seq}, cause...)...)
}

func (h *harness) mce(core int, bank machine.BankType, fromBoot string) journal.Event {
	return h.add(&journal.MCE{CPU: core, Core: core, Bank: 3, BankType: bank, Corrected: fromBoot == "", FromBoot: fromBoot, Lines: []string{fmt.Sprintf("mce %d", len(h.events))}})
}

func describe(a Action) string {
	if a.Kind == RunTrial {
		t := a.Trial
		target := fmt.Sprintf("c%d", t.Core)
		if t.AllCores {
			target = "all"
		}
		if t.Retry {
			target += " retry"
		}
		return fmt.Sprintf("trial %s %s", t.Regime, target)
	}
	ptr := func(p *int) string {
		if p == nil {
			return "none"
		}
		return fmt.Sprint(*p)
	}
	switch p := a.Payload.(type) {
	case *journal.Failure:
		if p.Attribution == journal.Attributed {
			return fmt.Sprintf("attributed %d at %d", *p.Core, *p.Offset)
		}
		return "unattributed"
	case *journal.TunerDecision:
		if p.Decision == journal.SuspectBackoff {
			return fmt.Sprintf("suspect %d %d>%d u%d", p.Core, p.FromOffset, p.ToOffset, p.UnprovenDepth)
		}
		return fmt.Sprintf("backoff %d %d>%d mark %s u%d", p.Core, p.FromOffset, p.ToOffset, ptr(p.FailedMark), p.UnprovenDepth)
	case *journal.DeadEnd:
		if p.Core != nil {
			return fmt.Sprintf("dead end core %d", *p.Core)
		}
		return "dead end"
	case *journal.EscalationWindow:
		return "window " + string(p.State)
	case *journal.GuardRotation:
		switch {
		case p.Event == journal.RotationStart:
			return fmt.Sprintf("start %d", p.Rotation)
		case p.Clean:
			return "end clean"
		}
		return "end unclean"
	case *journal.ProfileChange:
		return "profile"
	case *journal.TierChange:
		return "tier " + string(p.To)
	case *journal.CorePhase:
		return fmt.Sprintf("%s->%s %d u%d", p.From, p.To, p.Offset, p.UnprovenDepth)
	}
	return fmt.Sprintf("%T", a.Payload)
}

// until collects every action up to and including the next trial or dead end, deciding each one.
func (h *harness) until() []string {
	h.t.Helper()
	var got []string
	for range 100 {
		a := h.s.Next()
		got = append(got, describe(a))
		if a.Kind == RunTrial {
			return got
		}
		h.decide(a)
		if _, ok := a.Payload.(*journal.DeadEnd); ok {
			return got
		}
	}
	h.t.Fatalf("no trial after 100 actions: %v", got)
	return nil
}

func TestGuardRotationSchedule(t *testing.T) {
	t.Parallel()
	h, a := newGuardHarness(t, []int{-10, -12}, []*int{new(-11), new(-13)})
	var decided []journal.Payload
	for _, e := range h.events {
		switch e.Data.(type) {
		case *journal.ProfileChange, *journal.GuardRotation:
			decided = append(decided, e.Data)
		}
	}
	want := []journal.Payload{
		&journal.ProfileChange{To: []int{-10, -12}},
		&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: config.Default().Guard.Rotation},
	}
	if diff := cmp.Diff(want, decided); diff != "" {
		t.Fatalf("guard entry mismatch (-want +got):\n%s", diff)
	}
	schedule := []string{
		"trial R1 c0", "trial R1 c1", "trial R2 c0", "trial R2 c1", "trial R6 all", "trial R3 c0", "trial R3 c1",
		"trial R4 c0", "trial R4 c1", "trial R7 all", "trial R5 c0", "trial R5 c1", "trial R6 all",
	}
	clean := map[machine.Regime]int{}
	for i, w := range schedule {
		if got := describe(a); got != w {
			t.Fatalf("slot %d: %s, want %s", i, got, w)
		}
		if a.Trial.Condition != machine.Resident || a.Trial.Phase != journal.PhaseGuard || a.Trial.Rotation != 1 {
			t.Fatalf("slot %d: trial %+v, want resident guard rotation 1", i, a.Trial)
		}
		end := journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 100 + i}
		clean[a.Trial.Regime] += end.DurationS
		h.trial(a, end)
		a = h.s.Next()
	}
	h.decide(a)
	if got := h.until(); !slices.Equal(got, []string{"tier bronze", "start 2", "trial R1 c0"}) {
		t.Fatalf("after rotation 1: %s then %v", describe(a), got)
	}
	if describe(a) != "end clean" {
		t.Fatalf("after the last step: %s, want end clean", describe(a))
	}
	g := projected(h).Guard
	if g == nil || g.CleanRotations != 1 || g.Rotation != 2 || !g.RotationOpen {
		t.Fatalf("guard state %+v", g)
	}
	total := 0
	for _, rc := range g.Regimes {
		if rc.CleanS != clean[rc.Regime] {
			t.Errorf("%s clean %d s, want %d", rc.Regime, rc.CleanS, clean[rc.Regime])
		}
		total += rc.CleanS
	}
	if g.CleanS != total {
		t.Errorf("clean %d s, want %d", g.CleanS, total)
	}
}

func TestGuardEscalation(t *testing.T) {
	t.Parallel()
	crash := journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, Reason: "machine crashed during the trial"}
	signal := func(core int) journal.TrialEnd {
		return journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(core), DurationS: 30}
	}
	corrected := journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.CorrectedMCE, DurationS: 120}
	restart := []string{"end unclean", "profile", "start 2", "trial R1 c0"}
	suspectAll := append([]string{"unattributed", "suspect 0 -10>-9 u1", "suspect 1 -12>-11 u1", "window open"}, restart...)
	sharedIn := func(core int, r machine.Regime) func(h *harness) {
		return func(h *harness) {
			i := h.intent(core, r)
			m := h.mce(core, machine.L3Cache, "")
			h.end(i, corrected, m.Seq)
		}
	}
	afterSuspect := func(h *harness) {
		sharedIn(0, machine.R1)(h)
		h.decideUntilTrial()
	}
	tests := []struct {
		name    string
		offsets []int
		events  func(h *harness)
		want    []string
		reason  string
	}{
		{"backend signal names its instance's core", nil, func(h *harness) { h.end(h.intent(allCores, machine.R7), signal(1)) },
			append([]string{"attributed 1 at -12", "backoff 1 -12>-11 mark -12 u0"}, restart...), ""},
		{"backend signal on a core at 0", nil, func(h *harness) { h.end(h.intent(2, machine.R1), signal(2)) },
			[]string{"attributed 2 at 0", "dead end core 2"}, ""},
		{"core-local MCE names another core than the target", nil, func(h *harness) {
			i := h.intent(0, machine.R3)
			m := h.mce(1, machine.LoadStore, "")
			h.end(i, corrected, m.Seq)
		}, append([]string{"attributed 1 at -12", "backoff 1 -12>-11 mark -12 u0"}, restart...), ""},
		{"shared MCE with one loaded core", nil, sharedIn(0, machine.R1),
			append([]string{"unattributed", "suspect 0 -10>-9 u1", "window open"}, restart...), "the only loaded core"},
		{"core-local MCEs on two cores", nil, func(h *harness) {
			i := h.intent(allCores, machine.R7)
			a, b := h.mce(0, machine.FloatingPoint, ""), h.mce(1, machine.L2Cache, "")
			h.end(i, corrected, a.Seq, b.Seq)
		}, suspectAll, "R7 involves every core"},
		{"shared MCE with the loaded core at 0", nil, sharedIn(2, machine.R2), suspectAll, "the only loaded core 02 is at CO 0"},
		{"window already open", nil, func(h *harness) {
			h.add(&journal.EscalationWindow{State: journal.WindowOpen, Reason: "test"})
			sharedIn(1, machine.R4)(h)
		}, append([]string{"unattributed", "suspect 0 -10>-9 u1", "suspect 1 -12>-11 u1"}, restart...), "escalation window open: every core backs off"},
		{"R6 crash", nil, func(h *harness) { h.end(h.intent(allCores, machine.R6), crash) }, suspectAll, "R6 involves every core"},
		{"R7 crash", nil, func(h *harness) { h.end(h.intent(allCores, machine.R7), crash) }, suspectAll, "R7 involves every core"},
		{"idle crash", nil, func(h *harness) {
			h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R6, Condition: machine.Resident})
		}, suspectAll[1:], "with the profile applied and no trial in flight; R6 involves every core"},
		{"every core at 0", []int{0, 0, 0}, func(h *harness) { h.end(h.intent(allCores, machine.R6), crash) },
			[]string{"unattributed", "dead end"}, ""},
		{"crash with an uncorrected core-local MCE from the next boot", nil, func(h *harness) {
			i := h.intent(allCores, machine.R7)
			m := h.mce(0, machine.ExecutionUnit, "b2")
			detected := h.add(&journal.CrashDetected{PreviousBoot: "b", InFlight: new(i.Seq), Condition: machine.Resident}, m.Seq)
			h.end(i, crash, detected.Seq, m.Seq)
		}, append([]string{"attributed 0 at -10", "backoff 0 -10>-9 mark -10 u0"}, restart...), ""},
		{"clean rotation closes the window", nil, func(h *harness) {
			afterSuspect(h)
			for a := h.s.Next(); a.Kind == RunTrial; a = h.s.Next() {
				h.trial(a, passed)
			}
		}, []string{"end clean", "tier bronze", "window close", "start 3", "trial R1 c0"}, ""},
		{"inconclusive retries the trial", nil, func(h *harness) { h.trial(h.s.Next(), unsure) }, []string{"trial R1 c0 retry"}, ""},
		{"proven backoff cancels unproven depth", nil, func(h *harness) {
			afterSuspect(h)
			h.end(h.intent(0, machine.R1), signal(0))
		}, []string{"attributed 0 at -9", "backoff 0 -9>-8 mark -9 u0", "end unclean", "profile", "start 3", "trial R1 c0"}, "the failed mark cancels unproven depth 1"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			offsets := tt.offsets
			if offsets == nil {
				offsets = []int{-10, -12, 0}
			}
			marks := []*int{new(offsets[0] - 1), new(offsets[1] - 1), new(offsets[2] - 1)}
			h, _ := newGuardHarness(t, offsets, marks)
			tt.events(h)
			from := len(h.events)
			if got := h.until(); !slices.Equal(got, tt.want) {
				t.Fatalf("got  %v\nwant %v", got, tt.want)
			}
			if tt.reason == "" {
				return
			}
			for _, e := range h.events[from:] {
				if d, ok := e.Data.(*journal.TunerDecision); ok {
					if !strings.Contains(d.Reason, tt.reason) {
						t.Fatalf("reason %q, want it to contain %q", d.Reason, tt.reason)
					}
					return
				}
			}
			t.Fatal("no tuner.decision")
		})
	}
}

func TestGuardNeverDeepens(t *testing.T) {
	t.Parallel()
	for seed := uint64(1); seed <= 300; seed++ {
		rng := rand.New(rand.NewPCG(seed, 2))
		offsets := make([]int, propertyCores)
		marks := make([]*int, propertyCores)
		for c := range offsets {
			offsets[c] = -rng.IntN(31)
			marks[c] = new(offsets[c] - 1)
		}
		h, a := newGuardHarness(t, offsets, marks)
		check := func(a Action) {
			switch p := a.Payload.(type) {
			case *journal.TunerDecision:
				if p.ToOffset != p.FromOffset+1 || p.ToOffset > machine.MaxOffset {
					t.Fatalf("seed %d: guard decision %s", seed, describe(a))
				}
			case *journal.ProfileChange:
				for i := range p.To {
					if p.From != nil && p.To[i] < p.From[i] {
						t.Fatalf("seed %d: profile %v -> %v deepens core %d", seed, p.From, p.To, i)
					}
				}
			}
			if a.Kind != RunTrial || a.Trial.AllCores {
				return
			}
			if f := h.s.core(a.Trial.Core).fail; f != nil && a.Trial.Offset <= *f {
				t.Fatalf("seed %d: trial on core %d at %d, failed mark %d", seed, a.Trial.Core, a.Trial.Offset, *f)
			}
			if a.Trial.Offset < machine.MinOffset || a.Trial.Offset > machine.MaxOffset {
				t.Fatalf("seed %d: trial at %d", seed, a.Trial.Offset)
			}
		}
	outer:
		for range 3000 {
			check(a)
			if a.Kind == Decide {
				h.decide(a)
				if _, ok := a.Payload.(*journal.DeadEnd); ok {
					break outer
				}
				a = h.s.Next()
				continue
			}
			target := a.Trial.Core
			if a.Trial.AllCores {
				target = rng.IntN(propertyCores)
			}
			switch x := rng.Float64(); {
			case x < 0.6:
				h.trial(a, passed)
			case x < 0.75:
				h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Stall, Core: new(target), DurationS: 40})
			case x < 0.85:
				i := h.start(a)
				m := h.mce(target, machine.DataFabric, "")
				h.end(i, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.CorrectedMCE, DurationS: 120}, m.Seq)
			case x < 0.9:
				h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash})
			default:
				h.trial(a, unsure)
			}
			a = h.s.Next()
		}
		for _, c := range projected(h).Cores {
			if c.Offset < offsets[c.Core] || c.Offset > machine.MaxOffset {
				t.Fatalf("seed %d: core %d at %d, confirmed at %d", seed, c.Core, c.Offset, offsets[c.Core])
			}
		}
	}
}
