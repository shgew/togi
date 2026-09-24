package session

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"slices"
	"sync"
	"testing"
	"time"

	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
	"code.marleb.org/shgew/shycler/internal/sim"
)

var errKilled = errors.New("killed")

func small() sim.Config {
	edges := make([]sim.Edges, 2)
	edges[0].Isolated = [5]int{-12, -12, -12, -12, -12}
	edges[1].Isolated = [5]int{-13, -13, -13, -11, -13}
	for c := range edges {
		copy(edges[c].Resident[:5], edges[c].Isolated[:])
		edges[c].Resident[5] = slices.Max(edges[c].Isolated[:])
		edges[c].Resident[6] = edges[c].Resident[5]
	}
	return sim.Config{Seed: 3, Cores: 2, BIOS: []int{-10, -10}, Edges: edges}
}

func newSim(t *testing.T, cfg sim.Config) *sim.Machine {
	t.Helper()
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	return m
}

func simInput(dir string, m *sim.Machine) SimInput {
	return SimInput{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m}
}

// memState stands in for state.json: rewriting a file after every event dominates these tests on
// copy-on-write filesystems, and the file itself is covered by the journal package and TestSixteenCoresReachGuard.
type memState struct {
	mu   sync.Mutex
	data []byte
}

var states sync.Map

func stateOf(dir string) *memState {
	v, _ := states.LoadOrStore(dir, &memState{})
	return v.(*memState)
}

type trigger struct {
	k     int
	crash *sim.Machine
	fired bool
}

func killAt(k int) *trigger                  { return &trigger{k: k} }
func crashAt(k int, m *sim.Machine) *trigger { return &trigger{k: k, crash: m} }

type testJournal struct {
	*journal.Journal
	state *memState
	t     *trigger
}

func (j *testJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err != nil || j.t == nil || j.t.fired || e.Seq != j.t.k {
		return e, err
	}
	j.t.fired = true
	if j.t.crash != nil {
		j.t.crash.Crash()
		return e, machine.ErrCrashed
	}
	return e, errKilled
}

func (j *testJournal) WriteState(s journal.State) error {
	data, err := json.Marshal(s)
	if err != nil {
		return err
	}
	j.state.mu.Lock()
	defer j.state.mu.Unlock()
	j.state.data = data
	return nil
}

func (j *testJournal) ReadState() (journal.State, error) {
	return readMemState(j.state)
}

func readMemState(m *memState) (journal.State, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	var s journal.State
	if m.data == nil {
		return s, fs.ErrNotExist
	}
	err := json.Unmarshal(m.data, &s)
	return s, err
}

func wrapFor(in SimInput, tr *trigger) func(*journal.Journal) Journal {
	st := stateOf(in.Dir)
	return func(j *journal.Journal) Journal { return &testJournal{Journal: j, state: st, t: tr} }
}

func runSim(ctx context.Context, in SimInput, tr *trigger) (Stop, error) {
	for range maxSimulatedBoots {
		stop, err := simulateBoot(ctx, in, wrapFor(in, tr))
		switch {
		case errors.Is(err, errKilled):
		case errors.Is(err, machine.ErrCrashed):
			in.Machine.Reboot()
		default:
			return stop, err
		}
	}
	return Stop{}, errors.New("too many boots")
}

func drive(t *testing.T, in SimInput, tr *trigger) Stop {
	t.Helper()
	stop, err := runSim(context.Background(), in, tr)
	if err != nil {
		t.Fatalf("simulate: %v", err)
	}
	return stop
}

func simulate(t *testing.T, in SimInput) Stop {
	t.Helper()
	return drive(t, in, nil)
}

type coreSummary struct {
	Phase      journal.Phase
	Offset     int
	Pass       *int
	FailedMark *int
}

func (c coreSummary) String() string {
	ptr := func(p *int) string {
		if p == nil {
			return "none"
		}
		return fmt.Sprint(*p)
	}
	return fmt.Sprintf("%s at %d (pass %s, failed mark %s)", c.Phase, c.Offset, ptr(c.Pass), ptr(c.FailedMark))
}

func summary(t *testing.T, dir string) string {
	t.Helper()
	st, err := readMemState(stateOf(dir))
	if err != nil {
		t.Fatal(err)
	}
	out := fmt.Sprintf("phase %s, dead end %v:", st.Phase, st.DeadEnd)
	for _, c := range st.Cores {
		out += fmt.Sprintf(" core %d %s;", c.Core, coreSummary{c.Phase, c.Offset, c.Pass, c.FailedMark})
	}
	return out
}

func readEvents(t *testing.T, dir string) []journal.Event {
	t.Helper()
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read journal: %v, torn %q", err, torn)
	}
	return events
}

func reference(t *testing.T, cfg sim.Config) (dir string, events []journal.Event) {
	t.Helper()
	dir = t.TempDir()
	if stop := simulate(t, simInput(dir, newSim(t, cfg))); stop.Reason != StopGuard {
		t.Fatalf("reference run stopped with %+v", stop)
	}
	return dir, readEvents(t, dir)
}

func TestSixteenCoresReachGuard(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m := newSim(t, sim.Config{Seed: 1})
	began := time.Now()
	stop, err := Simulate(context.Background(), simInput(dir, m))
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("16-core session took %s", time.Since(began))
	if stop.Reason != StopGuard {
		t.Fatalf("stopped with %+v", stop)
	}
	st, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if st.Phase != "guard" || len(st.Cores) != 16 {
		t.Fatalf("state phase %s with %d cores", st.Phase, len(st.Cores))
	}
	for _, c := range st.Cores {
		if c.Phase != journal.PhaseConfirmed || c.Offset != m.IsolatedEdge(c.Core) {
			t.Errorf("core %d %s at %d, hidden edge %d", c.Core, c.Phase, c.Offset, m.IsolatedEdge(c.Core))
		}
	}
	if v := m.Violations(); len(v) > 0 {
		t.Errorf("isolation violations: %v", v)
	}
	for _, e := range readEvents(t, dir) {
		if p, ok := e.Data.(*journal.SMUIntent); ok && (p.Offset < machine.MinOffset || p.Offset > machine.MaxOffset) {
			t.Errorf("seq %d writes %d", e.Seq, p.Offset)
		}
	}
}

func TestKillAtEveryEvent(t *testing.T) {
	t.Parallel()
	refDir, events := reference(t, small())
	want := summary(t, refDir)
	for k := 1; k <= len(events); k++ {
		t.Run(fmt.Sprint(k), func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			m := newSim(t, small())
			if stop := drive(t, simInput(dir, m), killAt(k)); stop.Reason != StopGuard {
				t.Fatalf("stopped with %+v", stop)
			}
			if got := summary(t, dir); got != want {
				t.Fatalf("killed at %d (%s):\n got %s\nwant %s", k, events[k-1].Kind, got, want)
			}
		})
	}
}

func trialEvents(events []journal.Event) []int {
	var ks []int
	for i, e := range events {
		intent, ok := e.Data.(*journal.TrialIntent)
		if !ok {
			continue
		}
		for _, f := range events[i:] {
			if f.Boot != e.Boot {
				break
			}
			ks = append(ks, f.Seq)
			if end, ok := f.Data.(*journal.TrialEnd); ok && end.Trial == intent.Trial {
				if end.Outcome == journal.OutcomeFailure && f.Seq < len(events) {
					if _, ok := events[f.Seq].Data.(*journal.Failure); ok {
						ks = append(ks, f.Seq+1)
					}
				}
				break
			}
		}
	}
	return ks
}

func failureCiting(events []journal.Event, seq int) *journal.Failure {
	for _, e := range events {
		if f, ok := e.Data.(*journal.Failure); ok && slices.Contains(e.Cause, seq) {
			return f
		}
	}
	return nil
}

func crashDetectedFor(events []journal.Event, boot string) (journal.Event, bool) {
	for _, e := range events {
		if c, ok := e.Data.(*journal.CrashDetected); ok && c.PreviousBoot == boot {
			return e, true
		}
	}
	return journal.Event{}, false
}

func checkNeverAtFailedMark(t *testing.T, events []journal.Event) {
	t.Helper()
	marks := map[int]*int{}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.TunerDecision:
			marks[p.Core] = p.FailedMark
		case *journal.CorePhase:
			marks[p.Core] = p.FailedMark
		case *journal.TrialIntent:
			if mark := marks[*p.Core]; mark != nil && *p.Offset <= *mark {
				t.Fatalf("trial %s on core %d at %d, failed mark %d", p.Trial, *p.Core, *p.Offset, *mark)
			}
		}
	}
}

func TestCrashAtEveryTrialEvent(t *testing.T) {
	t.Parallel()
	_, ref := reference(t, small())
	cfg := small()
	for _, k := range trialEvents(ref) {
		t.Run(fmt.Sprint(k), func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			m := newSim(t, cfg)
			stop := drive(t, simInput(dir, m), crashAt(k, m))
			events := readEvents(t, dir)
			hit := events[k-1]
			crash, ok := crashDetectedFor(events, hit.Boot)
			if !ok {
				t.Fatalf("no crash.detected for the boot of seq %d", k)
			}
			var intent *journal.TrialIntent
			var ended bool
			for _, e := range events[:k] {
				switch p := e.Data.(type) {
				case *journal.TrialIntent:
					intent, ended = p, false
				case *journal.TrialEnd:
					ended = ended || p.Trial == intent.Trial
				}
			}
			f := failureCiting(events, crash.Seq)
			switch {
			case f == nil:
				t.Fatalf("no failure cites crash.detected seq %d", crash.Seq)
			case !ended && (f.Attribution != journal.Attributed || *f.Core != *intent.Core || *f.Offset != *intent.Offset || f.Trial != intent.Trial || f.Signal != machine.Crash):
				t.Fatalf("crash at %d (%s) during trial %s: failure %+v", k, hit.Kind, intent.Trial, f)
			case ended && f.Attribution != journal.Unattributed:
				t.Fatalf("crash at %d (%s) after trial %s ended: failure %+v", k, hit.Kind, intent.Trial, f)
			}
			if stop.Reason != StopGuard {
				t.Fatalf("stopped with %+v", stop)
			}
			st, _ := readMemState(stateOf(dir))
			for _, c := range st.Cores {
				if c.Phase != journal.PhaseConfirmed || c.Offset < m.IsolatedEdge(c.Core) {
					t.Fatalf("core %d %s at %d, hidden edge %d", c.Core, c.Phase, c.Offset, m.IsolatedEdge(c.Core))
				}
			}
			checkNeverAtFailedMark(t, events)
		})
	}
}

func crashingModel() *sim.Model {
	model := sim.DefaultModel()
	model.Signals = map[machine.Signal]float64{machine.Crash: 1}
	return &model
}

func TestCrashDuringRecovery(t *testing.T) {
	t.Parallel()
	cfg := small()
	cfg.Model = crashingModel()
	_, ref := reference(t, cfg)
	first := slices.IndexFunc(ref, func(e journal.Event) bool { return e.Kind == journal.KindCrashDetected })
	if first < 0 {
		t.Fatal("reference run never crashed")
	}
	recoveryBoot := ref[first].Boot
	crashedTrial := ref[*ref[first].Data.(*journal.CrashDetected).InFlight-1].Data.(*journal.TrialIntent).Trial
	for _, e := range ref {
		if e.Boot != recoveryBoot {
			continue
		}
		if e.Kind == journal.KindProfileApplied {
			break
		}
		k := e.Seq
		t.Run(fmt.Sprintf("%d_%s", k, e.Kind), func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			m := newSim(t, cfg)
			if stop := drive(t, simInput(dir, m), crashAt(k, m)); stop.Reason != StopGuard {
				t.Fatalf("stopped with %+v", stop)
			}
			events := readEvents(t, dir)
			failures := 0
			for _, e := range events {
				if f, ok := e.Data.(*journal.Failure); ok && f.Trial == crashedTrial {
					if f.Attribution != journal.Attributed || f.Signal != machine.Crash {
						t.Fatalf("failure for trial %s: %+v", crashedTrial, f)
					}
					failures++
				}
			}
			if failures != 1 {
				t.Fatalf("%d failures for trial %s, want 1", failures, crashedTrial)
			}
			c, ok := crashDetectedFor(events, recoveryBoot)
			if !ok || !c.Data.(*journal.CrashDetected).Stray {
				t.Fatalf("crash.detected for the recovery boot: %+v", c.Data)
			}
		})
	}
}

func TestStrayCrashes(t *testing.T) {
	t.Parallel()
	for _, boots := range []int{3, 2} {
		t.Run(fmt.Sprint(boots), func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			m := newSim(t, small())
			m.CrashBeforeApply(boots)
			stop := simulate(t, simInput(dir, m))
			strays := 0
			for _, e := range readEvents(t, dir) {
				if c, ok := e.Data.(*journal.CrashDetected); ok && c.Stray {
					strays++
				}
			}
			switch boots {
			case 3:
				if stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndBootLoop || strays != 3 || len(stop.Evidence) != 3 {
					t.Fatalf("stopped with %+v after %d stray crashes", stop, strays)
				}
			default:
				if stop.Reason != StopGuard || strays != 2 {
					t.Fatalf("stopped with %+v after %d stray crashes", stop, strays)
				}
			}
		})
	}
}

func lastEvent(t *testing.T, dir string) journal.Event {
	events := readEvents(t, dir)
	return events[len(events)-1]
}

func TestDeadEnds(t *testing.T) {
	t.Parallel()
	zeroFails := small()
	zeroFails.Edges[0].Isolated = [5]int{1, 1, 1, 1, 1}
	sharp := sim.DefaultModel()
	sharp.PastEdgeRate = 1e6
	zeroFails.Model = &sharp

	changed := machine.BIOSContext{BIOSVersion: "SIM.2", Board: "shycler simulator", CPUModel: "Simulated Zen 5 16-Core Processor", Microcode: "0x0", BoostLimitMHz: 5700}
	tests := []struct {
		name     string
		cfg      sim.Config
		fault    func(*sim.Machine)
		before   func(t *testing.T, in SimInput)
		want     journal.DeadEndCondition
		evidence journal.Kind
		killable bool
		sticky   bool
	}{
		{name: "failed write", fault: (*sim.Machine).FailWrite, want: journal.DeadEndSMU, evidence: journal.KindSMUError, killable: true},
		{name: "corrupt readback", fault: func(m *sim.Machine) { m.CorruptReadback(0) }, want: journal.DeadEndSMU, evidence: journal.KindSMUReadback, killable: true},
		{name: "three setup failures", fault: func(m *sim.Machine) { m.FailSetup(3) }, want: journal.DeadEndNoEvidence, evidence: journal.KindTrialEnd, killable: true},
		{name: "two setup failures", fault: func(m *sim.Machine) { m.FailSetup(2) }},
		{name: "escaped thread", fault: (*sim.Machine).Escape, want: journal.DeadEndContainment, evidence: journal.KindTrialEnd, killable: true},
		{name: "failed preflight", fault: func(m *sim.Machine) { m.FailCheck("root", "uid 1000") }, want: journal.DeadEndPreflight, evidence: journal.KindPreflightCheck},
		{name: "changed BIOS context", before: func(t *testing.T, in SimInput) {
			if stop := simulate(t, in); stop.Reason != StopGuard {
				t.Fatalf("first run stopped with %+v", stop)
			}
			in.Machine.SetBIOSContext(changed)
			in.Machine.Reboot()
		}, want: journal.DeadEndPreflight, evidence: journal.KindPreflightCheck},
		{name: "failure at zero", cfg: zeroFails, want: journal.DeadEndFailureAtZero, evidence: journal.KindFailure, sticky: true},
	}
	for _, tt := range tests {
		cfg := tt.cfg
		if cfg.Cores == 0 {
			cfg = small()
		}
		setup := func(t *testing.T) SimInput {
			in := simInput(t.TempDir(), newSim(t, cfg))
			if tt.fault != nil {
				tt.fault(in.Machine)
			}
			if tt.before != nil {
				tt.before(t, in)
			}
			return in
		}
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			in := setup(t)
			stop := simulate(t, in)
			if tt.want == "" {
				if stop.Reason != StopGuard {
					t.Fatalf("stopped with %+v, want guard", stop)
				}
				return
			}
			if stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != tt.want {
				t.Fatalf("stopped with %+v, want dead end %s", stop, tt.want)
			}
			if s, ok := lastEvent(t, in.Dir).Data.(*journal.Shutdown); !ok || s.Reason != journal.ShutdownDeadEnd {
				t.Fatal("last event is not shutdown{dead_end}")
			}
			last := stop.Evidence[len(stop.Evidence)-1]
			if last.Kind != tt.evidence {
				t.Fatalf("evidence ends with %s, want %s", last.Kind, tt.evidence)
			}
			if tt.want == journal.DeadEndPreflight && tt.before != nil && last.Data.(*journal.PreflightCheck).Check != "bios_context" {
				t.Fatalf("failed check %+v, want bios_context", last.Data)
			}
			if tt.sticky {
				if again := simulate(t, in); again.Reason != StopDeadEnd || again.DeadEnd.Condition != tt.want {
					t.Fatalf("second run stopped with %+v", again)
				}
			}
			if !tt.killable {
				return
			}
			t.Run("killed after the evidence", func(t *testing.T) {
				in := setup(t)
				stop := drive(t, in, killAt(last.Seq))
				if stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != tt.want {
					t.Fatalf("resumed run stopped with %+v", stop)
				}
				for _, e := range readEvents(t, in.Dir)[last.Seq:] {
					if e.Kind == journal.KindSMUIntent {
						t.Fatalf("seq %d: %s after the evidence", e.Seq, e.Msg)
					}
				}
			})
		})
	}
}

func TestCrashThenPreflightFailure(t *testing.T) {
	t.Parallel()
	_, ref := reference(t, small())
	k := ref[slices.IndexFunc(ref, func(e journal.Event) bool { return e.Kind == journal.KindTrialStart })].Seq
	dir := t.TempDir()
	m := newSim(t, small())
	in := simInput(dir, m)
	if _, err := simulateBoot(context.Background(), in, wrapFor(in, crashAt(k, m))); !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("crashing run: %v", err)
	}
	m.Reboot()
	m.FailCheck("root", "uid 1000")
	if stop := simulate(t, in); stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndPreflight {
		t.Fatalf("stopped with %+v", stop)
	}
	events := readEvents(t, dir)
	intent := events[slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindTrialIntent })].Data.(*journal.TrialIntent)
	var sawCrash, sawEnd, sawFailure bool
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.CrashDetected:
			sawCrash = true
		case *journal.TrialEnd:
			sawEnd = sawEnd || (p.Trial == "0001" && p.Outcome == journal.OutcomeFailure && p.Signal == machine.Crash)
		case *journal.Failure:
			sawFailure = sawFailure || (p.Trial == "0001" && p.Attribution == journal.Attributed && *p.Core == *intent.Core && *p.Offset == *intent.Offset)
		case *journal.DeadEnd:
			if !sawCrash || !sawEnd || !sawFailure {
				t.Fatalf("dead end before the crash was recorded: crash %v, trial end %v, failure %v", sawCrash, sawEnd, sawFailure)
			}
		}
	}
	m.FailCheck("root", "")
	if stop := simulate(t, in); stop.Reason != StopGuard {
		t.Fatalf("second run stopped with %+v", stop)
	}
	failures := 0
	for _, e := range readEvents(t, dir) {
		if f, ok := e.Data.(*journal.Failure); ok && f.Trial == "0001" {
			failures++
		}
	}
	if failures != 1 {
		t.Fatalf("%d failures for trial 0001, want 1", failures)
	}
}

type cancelAfter struct {
	n      int
	cancel context.CancelFunc
}

func (c *cancelAfter) Write(p []byte) (int, error) {
	if c.n--; c.n == 0 {
		c.cancel()
	}
	return len(p), nil
}

func TestSignalStopsCleanly(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m := newSim(t, small())
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	in := simInput(dir, m)
	in.Log = &cancelAfter{n: 50, cancel: cancel}
	stop, err := runSim(ctx, in, nil)
	if err != nil || stop.Reason != StopSignal {
		t.Fatalf("stopped with %+v, %v", stop, err)
	}
	stopped := lastEvent(t, dir)
	if s, ok := stopped.Data.(*journal.Shutdown); !ok || s.Reason != journal.ShutdownSignal {
		t.Fatal("last event is not shutdown{signal}")
	}
	m.Reboot()
	in.Log = io.Discard
	if stop := simulate(t, in); stop.Reason != StopGuard {
		t.Fatalf("second run stopped with %+v", stop)
	}
	events := readEvents(t, dir)
	for _, e := range events[stopped.Seq:] {
		if c, ok := e.Data.(*journal.CrashDetected); ok && c.PreviousBoot == stopped.Boot {
			t.Fatalf("seq %d: %s after a clean stop", e.Seq, e.Msg)
		}
	}
	if !slices.ContainsFunc(events[stopped.Seq:], func(e journal.Event) bool { return e.Kind == journal.KindTrialIntent && e.Boot != stopped.Boot }) {
		t.Fatal("second run did not resume in the next boot")
	}
}
