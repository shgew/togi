package session

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"slices"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

type windowCrashTrials struct {
	machine.Trials
	advance func(time.Duration)
}

func (t windowCrashTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	r, err := t.Trials.Start(ctx, spec)
	return windowCrashRunning{Running: r, advance: t.advance}, err
}

type windowCrashRunning struct {
	machine.Running
	advance func(time.Duration)
}

func (r windowCrashRunning) Wait(context.Context, machine.Reporter) (machine.Result, error) {
	r.advance(2 * time.Second)
	return machine.Result{}, machine.ErrCrashed
}

func crashWindowTrial(t *testing.T, r *runner, advance func(time.Duration)) {
	t.Helper()
	r.in.Machine.Trials = windowCrashTrials{Trials: r.in.Machine.Trials, advance: advance}
	a := tuner.Action{Trial: tuner.Trial{Core: 0, Offset: -1, Regime: machine.R1, Condition: machine.Isolated, DurationS: 10}}
	if err := r.trial(context.Background(), a); !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("trial: %v, want crash", err)
	}
}

func TestRecoveryDoesNotClaimRecordedPreWindowMCE(t *testing.T) {
	r, k, advance := boundaryRunner(t)
	advance(10 * time.Second)
	crashWindowTrial(t, r, advance)
	oldBoot := r.in.Boot
	r.in.Machine.Clock.(interface{ Reboot() }).Reboot()
	r.in.Boot, _ = r.in.Machine.Host.BootID()
	r = reopenBoundaryRunner(t, r, k)
	var m journal.MCE
	data := fmt.Sprintf(`{"cpu":0,"core":0,"bank_type":"load_store","corrected":true,"from_boot":%q,"monotonic_ns":1000000000,"lines":["before trial"]}`, oldBoot)
	if err := json.Unmarshal([]byte(data), &m); err != nil {
		t.Fatal(err)
	}
	e, err := r.append(&m)
	if err != nil {
		t.Fatal(err)
	}
	r = reopenBoundaryRunner(t, r, k)
	if err := r.recoverCrashes(context.Background()); err != nil {
		t.Fatal(err)
	}
	var end *journal.TrialEnd
	for _, event := range r.in.Journal.Events() {
		if slices.Contains(event.Cause, e.Seq) {
			t.Fatalf("pre-window MCE #%d became %s #%d cause", e.Seq, event.Kind, event.Seq)
		}
		if p, ok := event.Data.(*journal.TrialEnd); ok {
			end = p
		}
	}
	if end == nil {
		t.Fatal("missing recovered trial end")
	}
	if diff := cmp.Diff(machine.Crash, end.Signal); diff != "" {
		t.Fatal(diff)
	}
	before := r.in.Journal.Events()
	r = reopenBoundaryRunner(t, r, k)
	if err := r.recoverCrashes(context.Background()); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(before, r.in.Journal.Events()); diff != "" {
		t.Fatalf("recovery changed on reread (-before +after):\n%s", diff)
	}
	t.Logf("core-local MCE at 1s, window starts at 10s, crash at 12s: MCE recorded once, no trial/crash cause; recovered signal=%s; second recovery unchanged", end.Signal)
}

func TestKernelMCEWindowBoundaries(t *testing.T) {
	r, k, advance := boundaryRunner(t)
	advance(20 * time.Second)
	since := 10 * time.Second
	for _, tc := range []struct {
		name string
		mono time.Duration
	}{
		{"zero", 0},
		{"before", since - 1},
		{"start", since},
		{"inside", 15 * time.Second},
		{"teardown", 20 * time.Second},
		{"future", 20*time.Second + 1},
	} {
		k.add(r.in.Boot, tc.mono, tc.name)
	}
	if _, err := r.kernelBoundary("window", since, false); err != nil {
		t.Fatal(err)
	}
	got := map[string]bool{}
	for _, e := range r.in.Journal.Events() {
		if p, ok := e.Data.(*journal.MCE); ok {
			got[p.Lines[0]] = p.Trial == "window" && !p.BetweenTrials
			if p.MonotonicNS == nil {
				t.Fatal("missing monotonic time")
			}
			if p.Lines[0] == "zero" && *p.MonotonicNS != 0 {
				t.Fatalf("zero time became %d", *p.MonotonicNS)
			}
		}
	}
	want := map[string]bool{"zero": false, "before": false, "start": true, "inside": true, "teardown": true, "future": false}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatal(diff)
	}
	before := r.in.Journal.Events()
	if _, err := r.kernelBoundary("window", since, false); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(before, r.in.Journal.Events()); diff != "" {
		t.Fatalf("duplicate read changed events:\n%s", diff)
	}
}

func TestMCEReplayKeepsBootLocalAndLegacyEvidence(t *testing.T) {
	for _, tc := range []struct {
		name      string
		fromBoot  string
		eventBoot string
		mono      *int64
		corrected bool
		between   bool
		want      bool
	}{
		{name: "legacy live", eventBoot: "old", want: true},
		{name: "legacy recovered", fromBoot: "old", want: true},
		{name: "legacy retained", fromBoot: "new", want: true},
		{name: "before window", fromBoot: "old", mono: new(int64(10*time.Second - 1))},
		{name: "window start", fromBoot: "old", mono: new(int64(10 * time.Second)), want: true},
		{name: "unread crash tail", fromBoot: "old", mono: new(int64(12 * time.Second)), want: true},
		{name: "zero is present", fromBoot: "old", mono: new(int64(0))},
		{name: "retained next boot zero", fromBoot: "new", mono: new(int64(0)), want: true},
		{name: "retained next boot large", fromBoot: "new", mono: new(int64(time.Hour)), want: true},
		{name: "corrected next boot", fromBoot: "new", mono: new(int64(0)), corrected: true},
		{name: "another boot", fromBoot: "other", mono: new(int64(12 * time.Second))},
		{name: "live before window", eventBoot: "old", mono: new(int64(0))},
		{name: "live window start", eventBoot: "old", mono: new(int64(10 * time.Second)), want: true},
		{name: "live different boot", eventBoot: "new", mono: new(int64(12 * time.Second))},
		{name: "post window", eventBoot: "old", mono: new(int64(12 * time.Second)), between: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if tc.eventBoot == "" {
				tc.eventBoot = "new"
			}
			f := newFold()
			f.Fold(journal.Event{Seq: 1, Boot: "old", Mono: 10000, Data: &journal.TrialIntent{Trial: "window"}})
			f.Fold(journal.Event{Seq: 2, Boot: "old", Mono: 11000, Data: &journal.TrialStart{Trial: "window", WindowStartNS: new(int64(10 * time.Second))}})
			f.Fold(journal.Event{Seq: 3, Boot: tc.eventBoot, Data: &journal.MCE{FromBoot: tc.fromBoot, MonotonicNS: tc.mono, Corrected: tc.corrected, BetweenTrials: tc.between, Lines: []string{tc.name}}})
			seqs := append(slices.Clone(f.open.mces), f.recordedFor("old", "new")...)
			var want []int
			if tc.want {
				want = []int{3}
			}
			if diff := cmp.Diff(want, seqs); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestRecoveryRecordsPreWindowMCEWithoutCause(t *testing.T) {
	r, k, advance := boundaryRunner(t)
	advance(10 * time.Second)
	r.in.Machine.SMU = boundarySMU{SMU: r.in.Machine.SMU, write: func(offset int) {
		if offset == -1 {
			k.add(r.in.Boot, time.Second, "pre-window unread tail")
		}
	}}
	crashWindowTrial(t, r, advance)
	r.in.Machine.Clock.(interface{ Reboot() }).Reboot()
	r.in.Boot, _ = r.in.Machine.Host.BootID()
	r = reopenBoundaryRunner(t, r, k)
	if err := r.recoverCrashes(context.Background()); err != nil {
		t.Fatal(err)
	}
	mceSeq := 0
	for _, e := range r.in.Journal.Events() {
		if p, ok := e.Data.(*journal.MCE); ok {
			if !p.BetweenTrials || p.Trial != "" || p.MonotonicNS == nil || *p.MonotonicNS != int64(time.Second) {
				t.Fatalf("pre-window MCE: %+v", p)
			}
			mceSeq = e.Seq
		}
	}
	if mceSeq == 0 {
		t.Fatal("missing pre-window MCE")
	}
	for _, e := range r.in.Journal.Events() {
		if slices.Contains(e.Cause, mceSeq) {
			t.Fatalf("pre-window MCE became %s cause", e.Kind)
		}
	}
	t.Log("unread pre-window core-local MCE remains visible with monotonic_ns=1000000000 and between_trials=true; crash and trial causes exclude it")
}
