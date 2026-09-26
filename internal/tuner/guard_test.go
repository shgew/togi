package tuner

import (
	"fmt"
	"math/rand/v2"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/shycler/internal/config"
	"github.com/shgew/shycler/internal/journal"
	"github.com/shgew/shycler/internal/machine"
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
	if core == allCores {
		var ids []int
		for _, c := range h.s.byID() {
			ids = append(ids, c.id)
		}
		return h.intentOn(r, ids...)
	}
	h.trials++
	p := &journal.TrialIntent{
		Trial: fmt.Sprintf("%04d", h.trials), Regime: r, Workload: "w", DurationS: 120,
		Condition: machine.Resident, Phase: journal.PhaseGuard, Rotation: h.s.guard.rotation,
		Core: new(core), Offset: new(h.s.core(core).offset),
	}
	return h.add(p, h.s.guard.lastSeq)
}

// intentOn records a resident guard trial loading cores.
func (h *harness) intentOn(r machine.Regime, cores ...int) journal.Event {
	h.trials++
	p := &journal.TrialIntent{
		Trial: fmt.Sprintf("%04d", h.trials), Regime: r, Workload: "w", DurationS: 120,
		Condition: machine.Resident, Phase: journal.PhaseGuard, Rotation: h.s.guard.rotation, Cores: cores,
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

func (h *harness) describe(a Action) string {
	if a.Kind == RunTrial {
		t := a.Trial
		target := fmt.Sprintf("c%d", t.Core)
		switch {
		case len(t.Cores) == len(h.s.cores):
			target = "all"
		case len(t.Cores) > 0:
			target = "cores " + strings.ReplaceAll(strings.Trim(fmt.Sprint(t.Cores), "[]"), " ", ",")
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
		switch p.Decision {
		case journal.SuspectBackoff:
			return fmt.Sprintf("suspect %d %d>%d u%d", p.Core, p.FromOffset, p.ToOffset, p.UnprovenDepth)
		case journal.Regain:
			return fmt.Sprintf("regain %d %d>%d u%d", p.Core, p.FromOffset, p.ToOffset, p.UnprovenDepth)
		case journal.StepDeeper, journal.Backoff:
		}
		return fmt.Sprintf("backoff %d %d>%d mark %s u%d", p.Core, p.FromOffset, p.ToOffset, ptr(p.FailedMark), p.UnprovenDepth)
	case *journal.DeadEnd:
		if p.Core != nil {
			return fmt.Sprintf("dead end core %d", *p.Core)
		}
		return "dead end"
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
		got = append(got, h.describe(a))
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
		"trial R4 c0", "trial R4 c1", "trial R7 cores 0", "trial R7 cores 1", "trial R7 all", "trial R5 c0", "trial R5 c1",
		"trial R6 all",
	}
	clean := map[machine.Regime]int{}
	for i, w := range schedule {
		if got := h.describe(a); got != w {
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
		t.Fatalf("after rotation 1: %s then %v", h.describe(a), got)
	}
	if h.describe(a) != "end clean" {
		t.Fatalf("after the last step: %s, want end clean", h.describe(a))
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

// passUntil passes every trial and takes every decision until the next trial of regime r.
func (h *harness) passUntil(a Action, r machine.Regime) Action {
	h.t.Helper()
	for range 1000 {
		switch {
		case a.Kind == RunTrial && a.Trial.Regime == r:
			return a
		case a.Kind == RunTrial:
			h.trial(a, passed)
		default:
			h.decide(a)
		}
		a = h.s.Next()
	}
	h.t.Fatalf("no %s trial after 1000 actions", r)
	return Action{}
}

func TestGuardR7Trials(t *testing.T) {
	t.Parallel()
	wantR7 := func(t *testing.T, a Action, retry bool, workload int, cores ...int) {
		t.Helper()
		w := machine.PickWorkload(machine.R7, workload).ID
		if a.Kind != RunTrial || a.Trial.Regime != machine.R7 || !slices.Equal(a.Trial.Cores, cores) || a.Trial.Workload != w || a.Trial.Retry != retry {
			t.Fatalf("action %+v, want R7 on cores %v with workload %s, retry %v", a, cores, w, retry)
		}
	}
	t.Run("two CCDs", func(t *testing.T) {
		t.Parallel()
		h, a := newGuardHarness(t, []int{-10, -12, -11, -13}, []*int{new(-11), new(-13), new(-12), new(-14)})
		a = h.passUntil(a, machine.R7)
		wantR7(t, a, false, 0, 0, 1)
		h.trial(a, passed)
		a = h.s.Next()
		wantR7(t, a, false, 0, 2, 3)
		h.trial(a, unsure)
		a = h.s.Next()
		wantR7(t, a, true, 0, 2, 3)
		replayed := New()
		for _, e := range h.events {
			replayed.Fold(e)
		}
		if diff := cmp.Diff(a, replayed.Next()); diff != "" {
			t.Fatalf("replayed mid-step mismatch (-live +replayed):\n%s", diff)
		}
		h.trial(a, passed)
		a = h.s.Next()
		wantR7(t, a, false, 0, 0, 1, 2, 3)
		h.trial(a, passed)
		if a = h.s.Next(); a.Kind != RunTrial || a.Trial.Regime != machine.R5 {
			t.Fatalf("after the R7 step: %+v, want R5", a)
		}

		a = h.passUntil(a, machine.R7)
		if a.Trial.Rotation != 2 {
			t.Fatalf("R7 in rotation %d, want 2", a.Trial.Rotation)
		}
		wantR7(t, a, false, 1, 0, 1)
		h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash})
		a = h.passUntil(h.s.Next(), machine.R7)
		wantR7(t, a, false, 2, 0, 1)
	})
	t.Run("one CCD", func(t *testing.T) {
		t.Parallel()
		h, a := newGuardHarness(t, []int{-10}, []*int{new(-11)})
		a = h.passUntil(a, machine.R7)
		wantR7(t, a, false, 0, 0)
		h.trial(a, passed)
		if a = h.s.Next(); a.Kind != RunTrial || a.Trial.Regime != machine.R5 {
			t.Fatalf("after the R7 step: %+v, want R5", a)
		}
	})
}

func TestGuardBlameByLoad(t *testing.T) {
	t.Parallel()
	crash := journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, Reason: "machine crashed during the trial"}
	signal := func(core int) journal.TrialEnd {
		return journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(core), DurationS: 30}
	}
	corrected := journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.CorrectedMCE, DurationS: 120}
	restart := []string{"end unclean", "profile", "start 2", "trial R1 c0"}
	suspects := func(lines ...string) []string {
		return append(append([]string{"unattributed"}, lines...), restart...)
	}
	every := []string{"suspect 0 -10>-9 u1", "suspect 2 -11>-10 u1", "suspect 1 -12>-11 u1", "suspect 3 -13>-12 u1"}
	tests := []struct {
		name    string
		offsets []int
		events  func(h *harness)
		want    []string
		reason  string
	}{
		{"crashed per-core trial backs off its core", nil, func(h *harness) { h.end(h.intent(0, machine.R1), crash) },
			suspects("suspect 0 -10>-9 u1"), "the only loaded core"},
		{"crashed per-core trial on a core at 0", []int{0, -12, -11, -13}, func(h *harness) { h.end(h.intent(0, machine.R1), crash) },
			suspects("suspect 2 -11>-10 u1", "suspect 1 -12>-11 u1", "suspect 3 -13>-12 u1"), "the only loaded core 00 is at CO 0: every core backs off"},
		{"crashed single-CCD R7 trial backs off that CCD", nil, func(h *harness) { h.end(h.intentOn(machine.R7, 2, 3), crash) },
			suspects("suspect 2 -11>-10 u1", "suspect 3 -13>-12 u1"), "R7 loaded only CCD 1: its cores back off"},
		{"crashed single-CCD R7 trial with its cores at 0", []int{-10, -12, 0, 0}, func(h *harness) { h.end(h.intentOn(machine.R7, 2, 3), crash) },
			suspects("suspect 0 -10>-9 u1", "suspect 1 -12>-11 u1"), "every core R7 loaded on CCD 1 is at CO 0: every core backs off"},
		{"crashed all-core R7 trial", nil, func(h *harness) { h.end(h.intent(allCores, machine.R7), crash) }, suspects(every...), "R7 loaded every core"},
		{"crashed R6 trial", nil, func(h *harness) { h.end(h.intent(allCores, machine.R6), crash) }, suspects(every...), "R6 loaded every core"},
		{"idle crash", nil, func(h *harness) {
			h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R6, Condition: machine.Resident})
		}, append(slices.Clone(every), restart...), "no trial was in flight: every core backs off"},
		{"core-local MCE outside the loaded CCD is attributed", nil, func(h *harness) {
			i := h.intentOn(machine.R7, 2, 3)
			m := h.mce(1, machine.LoadStore, "")
			h.end(i, corrected, m.Seq)
		}, append([]string{"attributed 1 at -12", "backoff 1 -12>-11 mark -12 u0"}, restart...), "proven backoff"},
		{"every core at 0", []int{0, 0, 0, 0}, func(h *harness) { h.end(h.intent(allCores, machine.R6), crash) },
			[]string{"unattributed", "dead end"}, ""},
		{"a second per-core crash backs off only its core", nil, func(h *harness) {
			h.end(h.intent(0, machine.R1), crash)
			h.decideUntilTrial()
			h.end(h.intent(2, machine.R1), crash)
		}, []string{"unattributed", "suspect 2 -11>-10 u1", "end unclean", "profile", "start 3", "trial R1 c0"}, "the only loaded core"},
		{"backend signal names its instance's core", nil, func(h *harness) { h.end(h.intent(allCores, machine.R7), signal(1)) },
			append([]string{"attributed 1 at -12", "backoff 1 -12>-11 mark -12 u0"}, restart...), ""},
		{"crash with an uncorrected core-local MCE from the next boot", nil, func(h *harness) {
			i := h.intent(allCores, machine.R7)
			m := h.mce(0, machine.ExecutionUnit, "b2")
			detected := h.add(&journal.CrashDetected{PreviousBoot: "b", InFlight: new(i.Seq), Condition: machine.Resident}, m.Seq)
			h.end(i, crash, detected.Seq, m.Seq)
		}, append([]string{"attributed 0 at -10", "backoff 0 -10>-9 mark -10 u0"}, restart...), ""},
		{"inconclusive retries the trial", nil, func(h *harness) { h.trial(h.s.Next(), unsure) }, []string{"trial R1 c0 retry"}, ""},
		{"proven backoff cancels unproven depth", nil, func(h *harness) {
			h.end(h.intent(0, machine.R1), crash)
			h.decideUntilTrial()
			h.end(h.intent(0, machine.R1), signal(0))
		}, []string{"attributed 0 at -9", "backoff 0 -9>-8 mark -9 u0", "end unclean", "profile", "start 3", "trial R1 c0"}, "the failed mark cancels unproven depth 1"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			offsets := tt.offsets
			if offsets == nil {
				offsets = []int{-10, -12, -11, -13}
			}
			marks := make([]*int, len(offsets))
			for i, o := range offsets {
				marks[i] = new(o - 1)
			}
			h, _ := newGuardHarness(t, offsets, marks)
			tt.events(h)
			from := len(h.events)
			if got := h.until(); !slices.Equal(got, tt.want) {
				t.Fatalf("got  %v\nwant %v", got, tt.want)
			}
			if tt.reason == "" {
				return
			}
			decided := false
			for _, e := range h.events[from:] {
				if d, ok := e.Data.(*journal.TunerDecision); ok {
					decided = true
					if !strings.Contains(d.Reason, tt.reason) {
						t.Fatalf("reason %q, want it to contain %q", d.Reason, tt.reason)
					}
				}
			}
			if !decided {
				t.Fatal("no tuner.decision")
			}
		})
	}
}

func TestGuardInvariants(t *testing.T) {
	t.Parallel()
	regains := 0
	for seed := uint64(1); seed <= 300; seed++ {
		rng := rand.New(rand.NewPCG(seed, 2))
		offsets := make([]int, propertyCores)
		marks := make([]*int, propertyCores)
		for c := range offsets {
			offsets[c] = -rng.IntN(31)
			marks[c] = new(offsets[c] - 1)
		}
		h, a := newGuardHarness(t, offsets, marks)
		cleanEnd := 0
		perEnd := map[[2]int]bool{}
		regainedTo := map[[2]int]bool{}
		last := map[int]journal.Decision{}
		check := func(a Action) {
			switch p := a.Payload.(type) {
			case *journal.TunerDecision:
				switch p.Decision {
				case journal.Backoff, journal.SuspectBackoff:
					if p.ToOffset != p.FromOffset+1 || p.ToOffset > machine.MaxOffset {
						t.Fatalf("seed %d: guard decision %s", seed, h.describe(a))
					}
				case journal.Regain:
					regains++
					if p.ToOffset != p.FromOffset-1 {
						t.Fatalf("seed %d: regain %s", seed, h.describe(a))
					}
					if !slices.Equal(a.Cause, []int{cleanEnd}) {
						t.Fatalf("seed %d: regain %s cites %v, want the latest clean end %d", seed, h.describe(a), a.Cause, cleanEnd)
					}
					if perEnd[[2]int{p.Core, cleanEnd}] {
						t.Fatalf("seed %d: core %d regained twice for clean end %d", seed, p.Core, cleanEnd)
					}
					if regainedTo[[2]int{p.Core, p.ToOffset}] {
						t.Fatalf("seed %d: core %d regained to %d twice", seed, p.Core, p.ToOffset)
					}
					perEnd[[2]int{p.Core, cleanEnd}], regainedTo[[2]int{p.Core, p.ToOffset}] = true, true
				case journal.StepDeeper:
					t.Fatalf("seed %d: guard decision %s", seed, h.describe(a))
				}
			case *journal.ProfileChange:
				for i := range p.To {
					if p.From != nil && p.To[i] < p.From[i] && last[i] != journal.Regain {
						t.Fatalf("seed %d: profile %v -> %v deepens core %d, last decided %s", seed, p.From, p.To, i, last[i])
					}
				}
			}
			if a.Kind != RunTrial || len(a.Trial.Cores) > 0 {
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
				e := h.decide(a)
				switch p := a.Payload.(type) {
				case *journal.DeadEnd:
					break outer
				case *journal.GuardRotation:
					if p.Event == journal.RotationEnd && p.Clean {
						cleanEnd = e.Seq
					}
				case *journal.TunerDecision:
					last[p.Core] = p.Decision
				case *journal.ProfileChange:
					if g := projected(h).Guard; g.CleanS != 0 {
						t.Fatalf("seed %d: clean %d s right after a profile change", seed, g.CleanS)
					}
				}
				a = h.s.Next()
				continue
			}
			target := a.Trial.Core
			if len(a.Trial.Cores) > 0 {
				target = a.Trial.Cores[rng.IntN(len(a.Trial.Cores))]
			}
			pass := 0.6
			if seed%2 == 0 {
				pass = 0.97
			}
			x := rng.Float64()
			if x < pass {
				h.trial(a, passed)
				a = h.s.Next()
				continue
			}
			switch y := (x - pass) / (1 - pass); {
			case y < 0.375:
				h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Stall, Core: new(target), DurationS: 40})
			case y < 0.625:
				i := h.start(a)
				m := h.mce(target, machine.DataFabric, "")
				h.end(i, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.CorrectedMCE, DurationS: 120}, m.Seq)
			case y < 0.75:
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
	if regains == 0 {
		t.Fatal("no seed regained any depth")
	}
}
