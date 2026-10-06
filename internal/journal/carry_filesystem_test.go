package journal_test

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/carry"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

type carryFilesystemFixture struct {
	dir      string
	machine  *sim.Machine
	context  machine.BIOSContext
	original map[string][]byte
}

func carryFilesystemSetup(t *testing.T, incomplete bool) carryFilesystemFixture {
	t.Helper()
	model := sim.DefaultModel()
	m, err := sim.New(sim.Config{Seed: 17, Cores: 2, Model: &model})
	if err != nil {
		t.Fatal(err)
	}
	bios, err := m.Seams().Host.BIOSContext()
	if err != nil {
		t.Fatal(err)
	}
	fixture := carryFilesystemFixture{dir: t.TempDir(), machine: m, context: bios, original: make(map[string][]byte)}
	build := session.Build()
	build.Ruleset -= 2
	fixture.writeSource(t, "A", build, true)
	if incomplete {
		build.Ruleset++
		j := fixture.lock(t, build, false)
		if carried, err := carry.Prepare(j, build, []defect.Entry{}, &bios); err != nil || carried == nil {
			_ = j.Close()
			t.Fatalf("first transition: %+v, %v", carried, err)
		}
		if err := j.Close(); err != nil {
			t.Fatal(err)
		}
		fixture.writeSource(t, "B", build, false)
	}
	return fixture
}

func (f carryFilesystemFixture) lock(t *testing.T, build journal.Build, sync bool) *journal.Journal {
	t.Helper()
	j, err := journal.Lock(f.dir, journal.Options{Boot: "filesystem recovery", Now: f.machine.Now, Build: build, Sync: sync})
	if err != nil {
		t.Fatal(err)
	}
	return j
}

func (f carryFilesystemFixture) writeSource(t *testing.T, id string, build journal.Build, established bool) {
	t.Helper()
	j := f.lock(t, build, false)
	if _, err := j.Open(); err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	cores, err := f.machine.Seams().Host.Topology()
	if err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	payloads := []journal.Payload{&journal.SessionStart{Build: build, Session: id, Cores: cores}}
	if established {
		payloads = append(payloads,
			&journal.SessionContext{BIOSContext: f.context},
			&journal.TrialIntent{Trial: "old", Core: new(0), Offset: new(-35), Regime: machine.R6, Condition: machine.Alone},
			&journal.TrialEnd{Trial: "old", Outcome: journal.OutcomePass},
			&journal.CommandReset{Core: new(0)},
			&journal.TrialIntent{Trial: "pass", Core: new(0), Offset: new(-30), Regime: machine.R6, Condition: machine.Alone},
			&journal.TrialEnd{Trial: "pass", Outcome: journal.OutcomePass},
			&journal.TrialIntent{Trial: "fail", Core: new(1), Offset: new(-31), Regime: machine.R6, Condition: machine.Alone},
			&journal.TrialEnd{Trial: "fail", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(1)},
			&journal.Failure{Trial: "fail", Attribution: journal.Attributed, Signal: machine.ComputationError, Core: new(1), Offset: new(-31), Regime: machine.R6, Condition: machine.Alone},
		)
	} else {
		payloads = append(payloads, &journal.ConfigLoaded{Build: build})
	}
	for _, payload := range payloads {
		if _, err := j.Append(payload); err != nil {
			_ = j.Close()
			t.Fatal(err)
		}
	}
	var state journal.State
	journal.Replay(j.Events(), &state)
	if err := j.WriteState(state); err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(filepath.Join(f.dir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	f.original[id] = data
	if err := os.MkdirAll(filepath.Join(f.dir, "trials"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(f.dir, "trials", "artifact"), []byte(id+": immutable trial log"), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestCarryFilesystemFailureReopenMatrix(t *testing.T) {
	for _, incomplete := range []bool{false, true} {
		t.Run(fmt.Sprintf("incomplete upgrade %v", incomplete), func(t *testing.T) {
			t.Parallel()
			reference := carryFilesystemSetup(t, incomplete)
			j := reference.lock(t, session.Build(), true)
			probe := journal.FailFilesystemForTest(j, 0, false)
			if _, err := carry.Prepare(j, session.Build(), []defect.Entry{}, &reference.context); err != nil {
				_ = j.Close()
				t.Fatal(err)
			}
			steps := probe.Calls()
			if err := j.Close(); err != nil {
				t.Fatal(err)
			}
			want := reference.resume(t)
			for i, step := range steps {
				for _, after := range []bool{false, true} {
					t.Run(fmt.Sprintf("%d %s after %v", i+1, step, after), func(t *testing.T) {
						t.Parallel()
						fixture := carryFilesystemSetup(t, incomplete)
						j := fixture.lock(t, session.Build(), true)
						probe := journal.FailFilesystemForTest(j, i+1, after)
						_, err := carry.Prepare(j, session.Build(), []defect.Entry{}, &fixture.context)
						if err == nil || !probe.Fired() {
							_ = j.Close()
							t.Fatalf("failure not reached: %v, %v", err, probe.Calls())
						}
						if err := j.Close(); err != nil {
							t.Fatal(err)
						}
						got := fixture.resume(t)
						if diff := cmp.Diff(want, got); diff != "" {
							t.Fatalf("carry changed across archive boundary (-reference +resumed):\n%s", diff)
						}
					})
				}
			}
		})
	}
}

func (f carryFilesystemFixture) resume(t *testing.T) *journal.SessionCarried {
	t.Helper()
	for attempt := range 2 {
		f.runCarrySession(t, attempt == 1)
	}
	for id, original := range f.original {
		data, err := os.ReadFile(filepath.Join(f.dir, "archive", id+".jsonl"))
		if err != nil || !bytes.Equal(original, data) {
			t.Fatalf("source %s changed during recovery: %v", id, err)
		}
		artifact, err := os.ReadFile(filepath.Join(f.dir, "archive", id+"-trials", "artifact"))
		if err != nil || string(artifact) != id+": immutable trial log" {
			t.Fatalf("trial archive %s: %q, %v", id, artifact, err)
		}
	}
	events, _, err := journal.Read(f.dir)
	if err != nil {
		t.Fatal(err)
	}
	var result *journal.SessionCarried
	for _, event := range events {
		if payload, ok := event.Data.(*journal.SessionCarried); ok {
			if result != nil {
				t.Fatal("carry recorded twice")
			}
			result = payload
		}
	}
	if result == nil || len(result.Sources) == 0 || result.Sources[0].Session != "A" || len(result.Carried) != 2 {
		t.Fatalf("original lineage lost: %+v", result)
	}
	if core := result.Carried[0]; core.Core != 0 || core.CandidateSoloLimit == nil || *core.CandidateSoloLimit != -30 || core.CandidateSoloLimitSession != "A" || core.CandidateSoloLimitSeq != 7 {
		t.Fatalf("reset epoch or passing candidate solo limit lost: %+v", core)
	}
	if core := result.Carried[1]; core.Core != 1 || core.FailurePoint == nil || *core.FailurePoint != -31 || core.FailurePointSession != "A" || core.FailurePointSeq != 10 {
		t.Fatalf("failure point lost: %+v", core)
	}
	if pending, err := journal.PendingCarry(f.dir); err != nil || pending != "" {
		t.Fatalf("settled marker retained: %q, %v", pending, err)
	}
	return result
}

func (f carryFilesystemFixture) runCarrySession(t *testing.T, settled bool) {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	j := f.lock(t, session.Build(), false)
	carried, err := carry.Prepare(j, session.Build(), []defect.Entry{}, &f.context)
	if err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	if settled && carried != nil {
		_ = j.Close()
		t.Fatalf("settled carry reapplied: %+v", carried)
	}
	if _, err := j.Open(); err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	_, err = session.Run(ctx, session.Input{Config: config.Default(), Boot: "filesystem recovery", Journal: j, Machine: f.machine.Seams(), Carry: carried, Defects: []defect.Entry{}, SessionID: j.SessionID})
	if err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
}

func carriedMarkerFixture(t *testing.T) carryFilesystemFixture {
	t.Helper()
	f := carryFilesystemSetup(t, false)
	j := f.lock(t, session.Build(), false)
	if _, err := carry.Prepare(j, session.Build(), []defect.Entry{}, &f.context); err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	f.runCarrySession(t, false)
	return f
}

func TestCarryMarkerClearFilesystemFailureReopenMatrix(t *testing.T) {
	reference := carriedMarkerFixture(t)
	j := reference.lock(t, session.Build(), true)
	probe := journal.FailFilesystemForTest(j, 0, false)
	if carried, err := carry.Prepare(j, session.Build(), []defect.Entry{}, &reference.context); err != nil || carried != nil {
		_ = j.Close()
		t.Fatalf("settled carry: %+v, %v", carried, err)
	}
	steps := probe.Calls()
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	want := reference.resume(t)
	for i, step := range steps {
		for _, after := range []bool{false, true} {
			t.Run(fmt.Sprintf("%d %s after %v", i+1, step, after), func(t *testing.T) {
				t.Parallel()
				fixture := carriedMarkerFixture(t)
				j := fixture.lock(t, session.Build(), true)
				probe := journal.FailFilesystemForTest(j, i+1, after)
				_, err := carry.Prepare(j, session.Build(), []defect.Entry{}, &fixture.context)
				if err == nil || !probe.Fired() {
					_ = j.Close()
					t.Fatalf("marker clear failure not reached: %v, %v", err, probe.Calls())
				}
				if err := j.Close(); err != nil {
					t.Fatal(err)
				}
				if diff := cmp.Diff(want, fixture.resume(t)); diff != "" {
					t.Fatalf("marker interruption reapplied carry:\n%s", diff)
				}
			})
		}
	}
}
