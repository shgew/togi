package session

import (
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/carry"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/tuner"
)

func carriedFixture(t *testing.T, m *sim.Machine) *carry.Carry {
	t.Helper()
	bios, err := m.Seams().Host.BIOSContext()
	if err != nil {
		t.Fatal(err)
	}
	build := journal.Build{Version: "0.7.0", Rev: "source-rev", Ruleset: 6, Schema: journal.Schema}
	at := time.Date(2026, 10, 1, 0, 0, 0, 0, time.UTC)
	return &carry.Carry{
		Context: &bios,
		Sources: []journal.CarriedSource{{Session: "20261001T000000Z", Ruleset: 6, Schema: journal.Schema}},
		Cores:   []journal.CarriedCore{{Core: 0, CandidateSoloLimit: new(-12), CandidateSoloLimitSession: "20261001T000000Z", CandidateSoloLimitSeq: 10}},
		Facts: []facts.Fact{
			{Kind: facts.TrialFact, Session: "20260930T000000Z", Seq: 20, Time: at.Add(-time.Hour), Build: build, Epoch: tuner.EvidenceEpoch, Trial: "0003", Boot: "original-boot", Class: facts.Class{Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, Cores: []int{0}, DurationS: 90}, Condition: machine.Alone, Phase: journal.PhaseSearch, Profile: []int{-12, 0}, Outcome: journal.OutcomePass, DurationS: 90},
			{Kind: facts.TrialFact, Session: "20261001T000000Z", Seq: 10, Time: at, Build: build, Epoch: tuner.EvidenceEpoch, Trial: "0001", Boot: "source-boot", Class: facts.Class{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 90}, Condition: machine.Together, Phase: journal.PhaseChecking, Profile: []int{-12, -13}, Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 11},
			{Kind: facts.IdleFact, Session: "20261001T000000Z", Seq: 12, Time: at.Add(time.Minute), Build: build, Epoch: 0, Boot: "source-boot", Class: facts.Class{Regime: machine.R6, Cores: []int{0, 1}}, Condition: machine.Together, Profile: []int{-12, -13}, Outcome: journal.OutcomeFailure, Signal: machine.Crash, Idle: &facts.IdleContext{}},
		},
	}
}

func runCarriedSession(t *testing.T, dir string, m *sim.Machine, c *carry.Carry, interruptAfter int) ([]journal.Event, Stop) {
	t.Helper()
	in := simInput(dir, m)
	in.Carry = c
	var gate *appendGate
	if interruptAfter > 0 {
		gate = &appendGate{
			after: true,
			at:    interruptAfter,
			match: func(p journal.Payload, _ journal.Event) bool {
				return p.Kind() == journal.KindTrialCarried || p.Kind() == journal.KindFailureCarried
			},
			do: func(journal.Event) error { return errKilled },
		}
	}
	stop := drive(t, in, gate)
	return readEvents(t, dir), stop
}

func TestCarryFactsResumeWithoutDuplicates(t *testing.T) {
	for _, after := range []int{1, 2, 3} {
		t.Run(string(rune('0'+after)), func(t *testing.T) {
			m := newSim(t, small())
			c := carriedFixture(t, m)
			events, _ := runCarriedSession(t, t.TempDir(), m, c, after)
			var carried []journal.Payload
			commit, phase := 0, 0
			for _, e := range events {
				switch d := e.Data.(type) {
				case *journal.SessionStart:
					if d.Evidence != tuner.EvidenceEpoch {
						t.Fatalf("evidence epoch = %d, want %d", d.Evidence, tuner.EvidenceEpoch)
					}
				case *journal.TrialCarried, *journal.FailureCarried:
					if commit != 0 || phase != 0 {
						t.Fatalf("fact %d recorded after carry commitment or phases", e.Seq)
					}
					carried = append(carried, e.Data)
				case *journal.SessionCarried:
					if commit != 0 {
						t.Fatal("duplicate carry commitment")
					}
					commit = e.Seq
				case *journal.CorePhase:
					if phase == 0 {
						phase = e.Seq
					}
				}
			}
			want := make([]journal.Payload, len(c.Facts))
			for i, f := range c.Facts {
				want[i] = f.Payload()
			}
			if diff := cmp.Diff(want, carried); diff != "" {
				t.Fatalf("carried provenance/order changed (-want +got):\n%s", diff)
			}
			if commit == 0 || phase <= commit {
				t.Fatalf("carry commitment %d must precede first phase %d", commit, phase)
			}
		})
	}
}

func TestCarryFactsRequireSameBIOS(t *testing.T) {
	for _, missing := range []bool{false, true} {
		m := newSim(t, small())
		c := carriedFixture(t, m)
		if missing {
			c.Context = nil
		} else {
			c.Context.BIOSVersion = "different BIOS"
		}
		events, _ := runCarriedSession(t, t.TempDir(), m, c, 0)
		var commitment *journal.SessionCarried
		for _, e := range events {
			switch p := e.Data.(type) {
			case *journal.TrialCarried, *journal.FailureCarried:
				t.Fatal("facts carried without matching BIOS context")
			case *journal.SessionCarried:
				commitment = p
			}
		}
		if commitment == nil || commitment.FailurePoints || len(commitment.Carried) != 1 || commitment.Carried[0].CandidateSoloLimit == nil || *commitment.Carried[0].CandidateSoloLimit != -12 {
			t.Fatalf("BIOS transition did not retain only candidate solo limit: %#v", commitment)
		}
	}
}

func TestCarryFactsWaitForValidatedContext(t *testing.T) {
	for _, prefix := range []string{"initial transition", "after session start", "after context", "after first fact"} {
		t.Run(prefix, func(t *testing.T) {
			dir := t.TempDir()
			m := newSim(t, small())
			seams := m.Seams()
			bios, err := seams.Host.BIOSContext()
			if err != nil {
				t.Fatal(err)
			}
			cores, err := seams.Host.Topology()
			if err != nil {
				t.Fatal(err)
			}
			boot, err := seams.Host.BootID()
			if err != nil {
				t.Fatal(err)
			}
			opts := journal.Options{Boot: boot, Now: m.Now, Monotonic: seams.Clock.Monotonic, Build: Build()}
			old, err := journal.Open(dir, opts)
			if err != nil {
				t.Fatal(err)
			}
			for _, p := range []journal.Payload{
				&journal.SessionStart{Schema: journal.Schema, Ruleset: tuner.Ruleset - 1, Session: "20261001T000000Z", Cores: cores, Evidence: tuner.EvidenceEpoch},
				&journal.SessionContext{BIOSContext: bios},
				&journal.TrialIntent{Trial: "0001", Core: new(0), Offset: new(-12), Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, Condition: machine.Alone, Phase: journal.PhaseSearch, DurationS: 90, Profile: []int{-12, 0}},
				&journal.TrialEnd{Trial: "0001", Outcome: journal.OutcomePass, DurationS: 90},
				&journal.TrialIntent{Trial: "0002", Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 90, Profile: []int{-12, -13}},
				&journal.TrialEnd{Trial: "0002", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 11},
				&journal.Failure{Trial: "0002", Attribution: journal.Unattributed, Signal: machine.ComputationError, Condition: machine.Together, Profile: []int{-12, -13}},
				&journal.Failure{Attribution: journal.Unattributed, Signal: machine.Crash, Condition: machine.Together, Profile: []int{-12, -13}},
			} {
				if _, err := old.Append(p); err != nil {
					t.Fatal(err)
				}
			}
			original := facts.FromEvents(old.Events()).Facts
			if err := old.Close(); err != nil {
				t.Fatal(err)
			}
			j, err := journal.Lock(dir, opts)
			if err != nil {
				t.Fatal(err)
			}
			c, err := carry.Prepare(j, Build(), nil, nil)
			if err != nil {
				t.Fatal(err)
			}
			if prefix != "initial transition" {
				if err := j.Open(); err != nil {
					t.Fatal(err)
				}
				payloads := []journal.Payload{&journal.SessionStart{Build: Build(), Session: "20261002T000000Z", Cores: cores, Evidence: tuner.EvidenceEpoch}}
				if prefix != "after session start" {
					payloads = append(payloads, &journal.SessionContext{BIOSContext: bios})
				}
				if prefix == "after first fact" {
					payloads = append(payloads, &journal.SessionBaseline{Offsets: []int{0, 0}}, original[0].Payload())
				}
				for _, p := range payloads {
					if _, err := j.Append(p); err != nil {
						t.Fatal(err)
					}
				}
				if err := j.Close(); err != nil {
					t.Fatal(err)
				}
				j, err = journal.Lock(dir, opts)
				if err != nil {
					t.Fatal(err)
				}
				c, err = carry.Prepare(j, Build(), nil, nil)
				if err != nil {
					t.Fatal(err)
				}
			}
			if err := j.Close(); err != nil {
				t.Fatal(err)
			}
			events, _ := runCarriedSession(t, dir, m, c, 0)
			var got []journal.Payload
			contextSeq, commitment := 0, 0
			for _, e := range events {
				switch e.Data.(type) {
				case *journal.SessionContext:
					contextSeq = e.Seq
				case *journal.TrialCarried, *journal.FailureCarried:
					if contextSeq == 0 || commitment != 0 {
						t.Fatalf("fact %d must follow context and precede commitment", e.Seq)
					}
					got = append(got, e.Data)
				case *journal.SessionCarried:
					if commitment != 0 {
						t.Fatal("duplicate carry commitment")
					}
					commitment = e.Seq
				}
			}
			want := make([]journal.Payload, len(original))
			for i, f := range original {
				want[i] = f.Payload()
			}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("unavailable early BIOS must not commit partial evidence (-want +got):\n%s", diff)
			}
			if commitment == 0 {
				t.Fatal("validated context must complete the carry")
			}
		})
	}
}
