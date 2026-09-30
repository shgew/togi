package session

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

type boundaryKernel struct {
	machine.Kernel
	logs       map[string][]machine.MCE
	calls      int
	failAt     int
	failure    error
	lastCursor string
	dir        string
	journal    *journal.Journal
}

func (k *boundaryKernel) ReadMCEs(boot, cursor string) (machine.KernelRead, error) {
	k.calls++
	k.lastCursor = cursor
	if k.calls == k.failAt {
		return machine.KernelRead{}, k.failure
	}
	start := 0
	if cursor != "" {
		if !strings.HasPrefix(cursor, boot+"/") {
			return machine.KernelRead{}, machine.ErrCursorMissing
		}
		var err error
		start, err = strconv.Atoi(strings.TrimPrefix(cursor, boot+"/"))
		if err != nil || start > len(k.logs[boot]) {
			return machine.KernelRead{}, machine.ErrCursorMissing
		}
	}
	return machine.KernelRead{MCEs: slices.Clone(k.logs[boot][start:]), Cursor: boot + "/" + strconv.Itoa(len(k.logs[boot]))}, nil
}

func (k *boundaryKernel) MCEs(boot string, since time.Duration) ([]machine.MCE, error) {
	var out []machine.MCE
	for _, m := range k.logs[boot] {
		if m.Monotonic >= since {
			out = append(out, m)
		}
	}
	return out, nil
}

func (k *boundaryKernel) add(boot string, mono time.Duration, label string) {
	k.logs[boot] = append(k.logs[boot], machine.MCE{Core: 0, CPU: 0, Corrected: true, BankType: machine.LoadStore, Monotonic: mono, Lines: []string{label}})
}

type boundarySMU struct {
	machine.SMU
	write func(int)
}

func (s boundarySMU) SetOffset(core, offset int) error {
	if err := s.SMU.SetOffset(core, offset); err != nil {
		return err
	}
	if s.write != nil {
		s.write(offset)
	}
	return nil
}

type boundaryTrials struct {
	machine.Trials
	wait func(machine.TrialSpec) machine.Result
}

func (t boundaryTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	r, err := t.Trials.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	return boundaryRunning{Running: r, spec: spec, wait: t.wait}, nil
}

type boundaryRunning struct {
	machine.Running
	spec machine.TrialSpec
	wait func(machine.TrialSpec) machine.Result
}

func (r boundaryRunning) Wait(context.Context, machine.Reporter) (machine.Result, error) {
	return r.wait(r.spec), nil
}

func boundaryRunner(t *testing.T) (*runner, *boundaryKernel, func(time.Duration)) {
	t.Helper()
	m := newSim(t, small())
	seams := m.Seams()
	boot, _ := seams.Host.BootID()
	k := &boundaryKernel{Kernel: seams.Kernel, logs: map[string][]machine.MCE{}, dir: t.TempDir()}
	seams.Kernel = k
	advance := func(d time.Duration) {
		t.Helper()
		if err := m.Sleep(context.Background(), d); err != nil {
			t.Fatal(err)
		}
	}
	seams.Trials = boundaryTrials{Trials: seams.Trials, wait: func(spec machine.TrialSpec) machine.Result {
		advance(spec.Duration)
		return machine.Result{Ran: spec.Duration}
	}}
	j, err := journal.Open(k.dir, journal.Options{Boot: boot, Now: m.Now, Monotonic: m.Monotonic, Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	k.journal = j
	t.Cleanup(func() {
		if k.journal != nil {
			if err := k.journal.Close(); err != nil {
				t.Error(err)
			}
		}
	})
	cores, _ := seams.Host.Topology()
	r := &runner{in: Input{Boot: boot, Machine: seams, Journal: j, Config: config.Default()}, fold: newFold(), tuner: tuner.New(), cores: cores}
	if _, err := r.append(&journal.SessionStart{Build: Build(), Session: "boundary", Cores: cores}); err != nil {
		t.Fatal(err)
	}
	b, err := r.startupBoundary()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := r.append(&journal.ConfigLoaded{Build: Build(), KernelBoundary: b, Config: r.in.Config}); err != nil {
		t.Fatal(err)
	}
	if err := r.startSession(); err != nil {
		t.Fatal(err)
	}
	if err := r.ensureCondition(tuner.Trial{Condition: machine.Isolated}); err != nil {
		t.Fatal(err)
	}
	return r, k, advance
}

func runBoundaryTrial(t *testing.T, r *runner) *journal.TrialEnd {
	t.Helper()
	a := tuner.Action{Trial: tuner.Trial{Core: 0, Offset: -1, Regime: machine.R1, Condition: machine.Isolated, DurationS: 10}}
	if err := r.trial(context.Background(), a); err != nil {
		t.Fatal(err)
	}
	events := r.in.Journal.Events()
	return events[len(events)-1].Data.(*journal.TrialEnd)
}

func TestKernelBoundariesExcludeProfileWritesAndRetryWaits(t *testing.T) {
	r, k, advance := boundaryRunner(t)
	writes := 0
	r.in.Machine.SMU = boundarySMU{SMU: r.in.Machine.SMU, write: func(offset int) {
		if offset != -1 {
			return
		}
		advance(time.Second)
		writes++
		k.add(r.in.Boot, r.in.Machine.Clock.Monotonic(), fmt.Sprintf("profile write %d", writes))
		advance(time.Second)
	}}
	first := runBoundaryTrial(t, r)
	advance(time.Minute)
	k.add(r.in.Boot, r.in.Machine.Clock.Monotonic(), "retry wait")
	advance(time.Second)
	second := runBoundaryTrial(t, r)
	if diff := cmp.Diff([]journal.Outcome{journal.OutcomePass, journal.OutcomePass}, []journal.Outcome{first.Outcome, second.Outcome}); diff != "" {
		t.Fatal(diff)
	}
	var labels []string
	for _, e := range r.in.Journal.Events() {
		if p, ok := e.Data.(*journal.MCE); ok {
			if !p.BetweenTrials || p.Trial != "" {
				t.Fatalf("outside-window MCE fed a trial: %+v", p)
			}
			labels = append(labels, p.Lines...)
		}
		if _, ok := e.Data.(*journal.Failure); ok {
			t.Fatalf("between-trial evidence made a decision: %+v", e)
		}
	}
	if diff := cmp.Diff([]string{"profile write 1", "retry wait", "profile write 2"}, labels); diff != "" {
		t.Fatal(diff)
	}
	t.Logf("profile-write and retry-wait MCEs: %v; trial outcomes: %s, %s; failures: 0", labels, first.Outcome, second.Outcome)
}

func TestKernelBoundaryLossIsStickyBelowStrongerEvidence(t *testing.T) {
	for _, loss := range []error{errors.New("read failed"), machine.ErrCursorMissing} {
		for _, signal := range []machine.Signal{"", machine.ComputationError, machine.CorrectedMCE} {
			t.Run(loss.Error()+"/"+string(signal), func(t *testing.T) {
				r, k, advance := boundaryRunner(t)
				k.failAt, k.failure = k.calls+1, loss
				r.in.Machine.Trials = boundaryTrials{Trials: r.in.Machine.Trials, wait: func(spec machine.TrialSpec) machine.Result {
					advance(time.Second)
					if signal == machine.CorrectedMCE {
						k.add(r.in.Boot, r.in.Machine.Clock.Monotonic(), "during trial")
					}
					advance(spec.Duration - time.Second)
					backend := signal
					if backend == machine.CorrectedMCE {
						backend = ""
					}
					return machine.Result{Ran: spec.Duration, Signal: backend}
				}}
				end := runBoundaryTrial(t, r)
				want := journal.OutcomeInconclusive
				if signal != "" {
					want = journal.OutcomeFailure
				}
				if diff := cmp.Diff(want, end.Outcome); diff != "" {
					t.Fatal(diff)
				}
				if diff := cmp.Diff(signal, end.Signal); diff != "" {
					t.Fatal(diff)
				}
				if end.KernelError == "" {
					t.Fatal("later successful read erased the unread interval")
				}
				r.in.Machine.Trials = boundaryTrials{Trials: r.in.Machine.Trials, wait: func(spec machine.TrialSpec) machine.Result {
					advance(spec.Duration)
					return machine.Result{Ran: spec.Duration}
				}}
				next := runBoundaryTrial(t, r)
				if diff := cmp.Diff(journal.OutcomePass, next.Outcome); diff != "" {
					t.Fatal(diff)
				}
				t.Logf("lost start boundary: %s %s; next covered trial: %s", end.Outcome, end.Signal, next.Outcome)
			})
		}
	}
}

func TestKernelTeardownMCEIsTrialEvidence(t *testing.T) {
	r, k, _ := boundaryRunner(t)
	r.in.Machine.SMU = boundarySMU{SMU: r.in.Machine.SMU, write: func(offset int) {
		if offset == 0 {
			k.add(r.in.Boot, r.in.Machine.Clock.Monotonic(), "teardown")
		}
	}}
	end := runBoundaryTrial(t, r)
	if diff := cmp.Diff(machine.CorrectedMCE, end.Signal); diff != "" {
		t.Fatal(diff)
	}
	for _, seq := range r.in.Journal.Events()[len(r.in.Journal.Events())-1].Cause {
		if p, ok := r.eventAt(seq).Data.(*journal.MCE); ok && p.Trial == end.Trial && !p.BetweenTrials {
			return
		}
	}
	t.Fatal("trial end did not cite teardown evidence")
}

func TestKernelCursorResumeCoversSameBootAndReboot(t *testing.T) {
	for _, reboot := range []bool{false, true} {
		t.Run(fmt.Sprint(reboot), func(t *testing.T) {
			r, k, advance := boundaryRunner(t)
			first := runBoundaryTrial(t, r)
			advance(time.Second)
			k.add(r.in.Boot, r.in.Machine.Clock.Monotonic(), "after last trial")
			oldBoot := r.in.Boot
			if reboot {
				r.in.Machine.Clock.(interface{ Reboot() }).Reboot()
				r.in.Boot, _ = r.in.Machine.Host.BootID()
				k.add(r.in.Boot, 0, "new boot")
			}
			resumed := reopenBoundaryRunner(t, r, k)
			b, err := resumed.startupBoundary()
			if err != nil {
				t.Fatal(err)
			}
			if reboot {
				if k.lastCursor != "" {
					t.Fatal("reused another boot's cursor")
				}
				own, err := resumed.readMCEs(context.Background(), oldBoot)
				if err != nil {
					t.Fatal(err)
				}
				if diff := cmp.Diff("after last trial", own[len(own)-1].Lines[0]); diff != "" {
					t.Fatal(diff)
				}
			} else if k.lastCursor != first.KernelCursor {
				t.Fatalf("same-boot resume cursor %q, want %q", k.lastCursor, first.KernelCursor)
			}
			last := resumed.in.Journal.Events()[len(resumed.in.Journal.Events())-1].Data.(*journal.MCE)
			if !last.BetweenTrials {
				t.Fatalf("resumed outside-window MCE fed decisions: %+v", last)
			}
			t.Logf("reboot=%t: previous cursor=%s, resumed cursor=%s, between-trial MCE=%s", reboot, first.KernelCursor, b.KernelCursor, last.Lines[0])
		})
	}
}

func TestRebootRecordsBetweenTrialMCEWithoutDecisionEvidence(t *testing.T) {
	r, k, advance := boundaryRunner(t)
	runBoundaryTrial(t, r)
	advance(time.Second)
	k.add(r.in.Boot, r.in.Machine.Clock.Monotonic(), "between trials before reboot")
	r.in.Machine.Clock.(interface{ Reboot() }).Reboot()
	r.in.Boot, _ = r.in.Machine.Host.BootID()
	r = reopenBoundaryRunner(t, r, k)
	b, err := r.startupBoundary()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := r.append(&journal.ConfigLoaded{Build: Build(), KernelBoundary: b, Config: r.in.Config}); err != nil {
		t.Fatal(err)
	}
	if err := r.recoverCrashes(context.Background()); err != nil {
		t.Fatal(err)
	}
	mceSeq := 0
	for _, e := range r.in.Journal.Events() {
		if p, ok := e.Data.(*journal.MCE); ok && slices.Contains(p.Lines, "between trials before reboot") {
			if !p.BetweenTrials || p.Trial != "" {
				t.Fatalf("reboot claimed between-trial MCE: %+v", p)
			}
			mceSeq = e.Seq
		}
	}
	if mceSeq == 0 {
		t.Fatal("reboot omitted between-trial MCE")
	}
	for _, e := range r.in.Journal.Events() {
		if slices.Contains(e.Cause, mceSeq) {
			t.Fatalf("between-trial MCE fed decision %s #%d", e.Kind, e.Seq)
		}
	}
}

func reopenBoundaryRunner(t *testing.T, r *runner, k *boundaryKernel) *runner {
	t.Helper()
	if err := k.journal.Close(); err != nil {
		t.Fatal(err)
	}
	k.journal = nil
	j, err := journal.Open(k.dir, journal.Options{Boot: r.in.Boot, Now: r.in.Machine.Clock.Now, Monotonic: r.in.Machine.Clock.Monotonic, Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	k.journal = j
	in := r.in
	in.Journal = j
	resumed := &runner{in: in, fold: newFold(), tuner: tuner.New(), cores: r.cores}
	journal.Replay(j.Events(), resumed.fold, &resumed.state, resumed.tuner)
	return resumed
}
