package tuner

import (
	"slices"
	"testing"
	"time"

	"github.com/shgew/togi/internal/carry"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// zeroCarryTransition writes the #343 shape as a ruleset-9 session (a parked hunt trial whose core-local MCE named
// core 1, parked at CO 0, then a failure_at_zero dead end), carries it into this ruleset like a run does, and starts
// the new session with each core in phase at the failing profile. It returns the harness and the carried failure.
func zeroCarryTransition(t *testing.T, profile []int) (*harness, int) {
	t.Helper()
	dir := t.TempDir()
	now := time.Date(2026, 10, 1, 0, 0, 0, 0, time.UTC)
	opts := journal.Options{Boot: "boot", Now: func() time.Time { now = now.Add(time.Second); return now }}
	cores := make([]machine.CoreInfo, 4)
	for i := range cores {
		cores[i] = machine.CoreInfo{Core: i, CCD: i / 2, CPUs: []int{i, i + 4}}
	}
	bios := machine.BIOSContext{BIOSVersion: "3.10", Board: "X870E", CPUModel: "9950X", Microcode: "0xb404032", BoostLimitMHz: 5700}
	class := journal.TrialClass{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1, 2, 3}, DurationS: 120}
	j, err := journal.Open(dir, opts)
	if err != nil {
		t.Fatal(err)
	}
	var end int
	for _, p := range []journal.Payload{
		&journal.SessionStart{Schema: journal.Schema, Ruleset: Ruleset - 1, Session: "X", Cores: cores},
		&journal.SessionContext{BIOSContext: bios},
		&journal.TrialIntent{Trial: "0012", Regime: class.Regime, Workload: class.Workload, Cores: class.Cores, DurationS: class.DurationS, Condition: machine.Parked, Phase: journal.PhaseHunt, Hunt: 1, Group: 1, Profile: profile},
		&journal.TrialEnd{Trial: "0012", Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 40},
		&journal.MCE{Core: 1, BankType: machine.LoadStore, Trial: "0012"},
		&journal.Failure{Signal: machine.Crash, Attribution: journal.Attributed, Core: new(1), Offset: new(0), Trial: "0012", Regime: class.Regime, Condition: machine.Parked, Profile: profile},
		&journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(1), Detail: "core 01 failed at CO 0"},
	} {
		e, err := j.Append(p)
		if err != nil {
			t.Fatal(err)
		}
		if _, ok := p.(*journal.TrialEnd); ok {
			end = e.Seq
		}
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	j, err = journal.Lock(dir, opts)
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	c, err := carry.Prepare(j, journal.Build{Schema: journal.Schema, Ruleset: Ruleset}, []defect.Entry{}, &bios)
	if err != nil || c == nil {
		t.Fatalf("carry %+v: %v", c, err)
	}

	h := &harness{t: t, s: New()}
	begin := h.add(&journal.SessionStart{Schema: journal.Schema, Ruleset: Ruleset, Session: "Y", Cores: cores})
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: snapshotConfig(config.Default())})
	fact := 0
	for _, f := range c.Facts {
		e := h.add(f.Payload())
		if f.Session == "X" && f.Seq == end {
			fact = e.Seq
		}
	}
	if fact == 0 {
		t.Fatalf("the failing trial did not carry: %+v", c.Facts)
	}
	carried := h.add(&journal.SessionCarried{Sources: c.Sources, FailurePoints: true, Carried: c.Cores})
	byCore := map[int]*int{}
	for _, cc := range c.Cores {
		byCore[cc.Core] = cc.FailurePoint
	}
	for i, o := range profile {
		h.add(&journal.CorePhase{Core: i, To: journal.PhaseHasRoom, Offset: o, FailurePoint: byCore[i], Reason: "test"}, begin.Seq, carried.Seq)
	}
	return h, fact
}

func TestCarriedUnconfirmedFailureAtZeroIsHunted(t *testing.T) {
	h, fact := zeroCarryTransition(t, []int{0, 0, -20, -25})
	if fail := h.s.core(1).fail; fail != nil {
		t.Fatalf("core 01 carried failure point %d from an unconfirmed failure at CO 0", *fail)
	}
	tr := Trial{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: h.s.ids(), DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Profile: []int{0, 0, -20, -25}}
	a := h.s.skipKnownFailure(Action{Kind: RunTrial, Trial: tr})
	skip, ok := a.Payload.(*journal.Failure)
	if !ok || skip.KnownFailure != fact || skip.Attribution != journal.Unattributed {
		t.Fatalf("the carried failure no longer constrains its class: %+v", a)
	}
	h.decide(a)
	for range 10 {
		a = h.next()
		switch p := a.Payload.(type) {
		case *journal.DeadEnd:
			t.Fatalf("dead end before any all-zero rerun: %+v", p)
		case *journal.HuntStart:
			if p.Failure != fact || len(p.Candidates) == 0 || slices.ContainsFunc(p.Candidates, func(c int) bool { return c != 2 && c != 3 }) {
				t.Fatalf("hunt %+v, want the carried failure hunted among the cores off CO 0", p)
			}
			return
		}
		if a.Kind != Decide {
			t.Fatalf("action %+v before the hunt", a)
		}
		h.decide(a)
	}
	t.Fatal("the carried failure was not hunted")
}

func TestCarriedFailureAtZeroWithAllZeroProfileDeadEnds(t *testing.T) {
	h, _ := zeroCarryTransition(t, []int{0, 0, 0, 0})
	a := h.next()
	if dead, ok := a.Payload.(*journal.DeadEnd); !ok || dead.Condition != journal.DeadEndFailureAtZero || dead.Core == nil || *dead.Core != 1 {
		t.Fatalf("action %+v, want the failure_at_zero dead end of core 01", a)
	}
}
