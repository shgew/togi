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

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/carry"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

var errKilled = errors.New("killed")

func small() sim.Config {
	limits := make([]sim.Limits, 2)
	limits[0].Alone = [5]int{-12, -12, -12, -12, -12}
	limits[1].Alone = [5]int{-13, -13, -13, -11, -13}
	for c := range limits {
		copy(limits[c].Together[:5], limits[c].Alone[:])
		limits[c].Together[5] = slices.Max(limits[c].Alone[:])
		limits[c].Together[6] = limits[c].Together[5]
	}
	return sim.Config{Seed: 3, Cores: 2, BIOS: []int{-10, -10}, Limits: limits}
}

func newSim(t *testing.T, cfg sim.Config) *sim.Machine {
	t.Helper()
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	return m
}

const maxSimulatedBoots = 1000

type simRun struct {
	Config      config.Config
	ConfigPath  string
	Dir         string
	Machine     *sim.Machine
	Log         io.Writer
	Stderr      io.Writer
	Cycles      int
	Bootloader  Bootloader
	Prompt      func(defect.Finding) (bool, error)
	Defects     []defect.Entry
	Seams       *machine.Machine
	Carry       *carry.Carry
	AfterAppend *appendGate
	state       *memState
	// prefix spares later boots decoding what earlier boots of this run already decoded.
	prefix *journal.Prefix
}

func simInput(dir string, m *sim.Machine) simRun {
	return simRun{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, prefix: &journal.Prefix{}, state: &memState{}}
}

func simulateBoot(ctx context.Context, in simRun, wrap func(*journal.Journal) Journal) (Stop, error) {
	seams := in.Machine.Seams()
	if in.Seams != nil {
		seams = *in.Seams
	}
	boot, err := seams.Host.BootID()
	if err != nil {
		return Stop{}, fmt.Errorf("read boot id: %w", err)
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Monotonic: seams.Clock.Monotonic, Build: Build(), Prefix: in.prefix})
	if err != nil {
		return Stop{}, err
	}
	wrapped := wrap(j)
	if in.AfterAppend != nil {
		wrapped = &interruptedJournal{Journal: wrapped, gate: in.AfterAppend}
	}
	stop, err := Run(ctx, Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: wrapped, Machine: seams, Cycles: in.Cycles, Bootloader: in.Bootloader, Prompt: in.Prompt, Defects: in.Defects, Stderr: in.Stderr, Log: in.Log, SessionID: j.SessionID, Carry: in.Carry})
	if cerr := j.Close(); err == nil && cerr != nil {
		return Stop{}, cerr
	}
	return stop, err
}

// memState stands in for state.json: rewriting a file after every event dominates these tests on
// copy-on-write filesystems, and the file itself is covered by the journal package and simrun's TestInMemoryJournalMatchesFileBacked.
// It keeps the last written state and encodes it when read: a boot ends right after a write or at a trigger
// inside Append, before the runner folds anything into the state it last wrote.
type memState struct {
	mu      sync.Mutex
	data    []byte
	pending *journal.State
}

type appendGate struct {
	match func(journal.Payload, journal.Event) bool
	at    int
	seen  int
	after bool
	fired bool
	do    func(journal.Event) error
}

func (g *appendGate) trip(p journal.Payload, e journal.Event) error {
	if g.fired || !g.match(p, e) {
		return nil
	}
	g.seen++
	if g.seen != max(g.at, 1) {
		return nil
	}
	g.fired = true
	return g.do(e)
}

func eventKind(kind journal.Kind) func(journal.Payload, journal.Event) bool {
	return func(p journal.Payload, _ journal.Event) bool { return p.Kind() == kind }
}

func killAt(k int) *appendGate {
	return &appendGate{after: true, match: func(_ journal.Payload, e journal.Event) bool { return e.Seq == k }, do: func(journal.Event) error { return errKilled }}
}

func crashAt(k int, m *sim.Machine) *appendGate {
	g := killAt(k)
	g.do = func(journal.Event) error {
		m.Crash()
		return machine.ErrCrashed
	}
	return g
}

type interruptedJournal struct {
	Journal
	gate *appendGate
}

func (j *interruptedJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	if !j.gate.after {
		if err := j.gate.trip(p, journal.Event{}); err != nil {
			return journal.Event{}, err
		}
	}
	e, err := j.Journal.Append(p, cause...)
	if err == nil && j.gate.after {
		err = j.gate.trip(p, e)
	}
	return e, err
}

type testJournal struct {
	*journal.Journal
	state *memState
}

func (j *testJournal) WriteState(s journal.State) error {
	j.state.mu.Lock()
	defer j.state.mu.Unlock()
	j.state.pending = &s
	return nil
}

func (j *testJournal) ReadState() (journal.State, error) {
	return readMemState(j.state)
}

func readMemState(m *memState) (journal.State, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	var s journal.State
	if m.pending != nil {
		data, err := json.Marshal(*m.pending)
		if err != nil {
			return s, err
		}
		m.data, m.pending = data, nil
	}
	if m.data == nil {
		return s, fs.ErrNotExist
	}
	err := json.Unmarshal(m.data, &s)
	return s, err
}

func wrapFor(in simRun, tr *appendGate) func(*journal.Journal) Journal {
	return func(j *journal.Journal) Journal {
		var wrapped Journal = &testJournal{Journal: j, state: in.state}
		if tr != nil {
			wrapped = &interruptedJournal{Journal: wrapped, gate: tr}
		}
		return wrapped
	}
}

func runSim(ctx context.Context, in simRun, tr *appendGate) (Stop, error) {
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

func drive(t *testing.T, in simRun, tr *appendGate) Stop {
	t.Helper()
	stop, err := runSim(context.Background(), in, tr)
	if err != nil {
		t.Fatalf("simulate: %v", err)
	}
	return stop
}

func simulate(t *testing.T, in simRun) Stop {
	t.Helper()
	return drive(t, in, nil)
}

type coreSummary struct {
	Phase        journal.Phase
	Offset       int
	Pass         *int
	FailurePoint *int
}

func (c coreSummary) String() string {
	ptr := func(p *int) string {
		if p == nil {
			return "none"
		}
		return fmt.Sprint(*p)
	}
	return fmt.Sprintf("%s at %d (pass %s, failure point %s)", c.Phase, c.Offset, ptr(c.Pass), ptr(c.FailurePoint))
}

func summary(t *testing.T, in simRun) string {
	t.Helper()
	st, err := readMemState(in.state)
	if err != nil {
		t.Fatal(err)
	}
	out := fmt.Sprintf("phase %s, dead end %v:", st.Phase, st.DeadEnd)
	for _, c := range st.Cores {
		out += fmt.Sprintf(" core %d %s;", c.Core, coreSummary{c.Phase, c.Offset, c.Pass, c.FailurePoint})
	}
	if g := st.Checking; g != nil {
		out += fmt.Sprintf(" checking cycle %d, clean cycles %d, last clean cycle %d, exposure %v", g.Cycle, g.CleanCycles, g.LastCleanCycle, g.Exposure)
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

func referenceRun(t *testing.T, cfg sim.Config) (simRun, []journal.Event) {
	t.Helper()
	in := simInput(t.TempDir(), newSim(t, cfg))
	if stop := simulate(t, in); stop.Reason != StopCycles {
		t.Fatalf("reference run stopped with %+v", stop)
	}
	return in, readEvents(t, in.Dir)
}

func reference(t *testing.T, cfg sim.Config) (string, []journal.Event) {
	t.Helper()
	in, events := referenceRun(t, cfg)
	return in.Dir, events
}

func TestKillAtEveryEvent(t *testing.T) {
	t.Parallel()
	ref, events := referenceRun(t, small())
	want := summary(t, ref)
	firstStart := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindTrialStart }) + 1
	for k := 1; k <= len(events); k++ {
		t.Run(fmt.Sprint(k), func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			m := newSim(t, small())
			in := simInput(dir, m)
			if stop := drive(t, in, killAt(k)); stop.Reason != StopCycles {
				t.Fatalf("stopped with %+v", stop)
			}
			if got := summary(t, in); got != want {
				t.Fatalf("killed at %d (%s):\n got %s\nwant %s", k, events[k-1].Kind, got, want)
			}
			if k == firstStart {
				for _, e := range readEvents(t, dir) {
					if p, ok := e.Data.(*journal.TrialEnd); ok && p.Trial == "0001" {
						if !p.Interrupted || p.DurationS != 0 || e.Msg != "trial 0001 INCONCLUSIVE, last evidence 0s after start: togi stopped during the trial" {
							t.Fatalf("trial.end %+v: %s", p, e.Msg)
						}
						return
					}
				}
				t.Fatal("no trial.end for trial 0001")
			}
		})
	}
}

func TestCandidateSoloLimitsStartChecking(t *testing.T) {
	dir := t.TempDir()
	in := simInput(dir, newSim(t, small()))
	in.Config.CandidateSoloLimits = map[int]int{0: -12, 1: -13}
	if stop := simulate(t, in); stop.Reason != StopCycles {
		t.Fatalf("stopped with %+v", stop)
	}
	phases := map[int]*journal.CorePhase{}
	for _, e := range readEvents(t, dir) {
		if p, ok := e.Data.(*journal.CorePhase); ok && p.From == "" {
			if phases[p.Core] != nil {
				t.Fatalf("duplicate initial phase for core %d", p.Core)
			}
			phases[p.Core] = p
		}
	}
	want := map[int]*journal.CorePhase{}
	for core, offset := range in.Config.CandidateSoloLimits {
		want[core] = &journal.CorePhase{
			Core: core, To: journal.PhaseSearch, Offset: offset,
			CheckSoloLimit: true, Reason: "configured candidate solo limit",
			Workloads: []string{machine.Workloads(machine.R1)[0].ID, machine.Workloads(machine.R2)[0].ID},
		}
	}
	if diff := cmp.Diff(want, phases); diff != "" {
		t.Fatalf("initial solo limit checks (-want +got):\n%s", diff)
	}
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
			if stop := drive(t, simInput(dir, m), crashAt(k, m)); stop.Reason != StopCycles {
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
				if stop.Reason != StopCycles || strays != 2 {
					t.Fatalf("stopped with %+v after %d stray crashes", stop, strays)
				}
			}
		})
	}
}

func lastEvent(t *testing.T, dir string) journal.Event {
	t.Helper()
	events := readEvents(t, dir)
	return events[len(events)-1]
}

func TestDeadEnds(t *testing.T) {
	t.Parallel()
	zeroFails := small()
	zeroFails.Limits[0].Alone = [5]int{1, 1, 1, 1, 1}
	sharp := sim.DefaultModel()
	sharp.PastLimitRate = 1e6
	zeroFails.Model = &sharp

	changed := machine.BIOSContext{BIOSVersion: "SIM.2", Board: "togi simulator", CPUModel: "Simulated Zen 5 16-Core Processor", Microcode: "0x0", BoostLimitMHz: 5700}
	tests := []struct {
		name     string
		cfg      sim.Config
		fault    func(*sim.Machine)
		before   func(t *testing.T, in simRun)
		want     journal.DeadEndCondition
		evidence journal.Kind
		killable bool
		sticky   bool
	}{
		{name: "failed write", fault: (*sim.Machine).FailWrite, want: journal.DeadEndSMU, evidence: journal.KindSMUError, killable: true},
		{name: "corrupt readback", fault: func(m *sim.Machine) { m.CorruptReadback(0) }, want: journal.DeadEndSMU, evidence: journal.KindSMUReadback, killable: true},
		{name: "six setup failures after retries", fault: func(m *sim.Machine) { m.FailSetup(6) }, want: journal.DeadEndNoEvidence, evidence: journal.KindTrialEnd, killable: true},
		{name: "two setup failures", fault: func(m *sim.Machine) { m.FailSetup(2) }},
		{name: "escaped thread", fault: (*sim.Machine).Escape, want: journal.DeadEndContainment, evidence: journal.KindTrialEnd, killable: true},
		{name: "failed preflight", fault: func(m *sim.Machine) { m.FailCheck("root", "uid 1000") }, want: journal.DeadEndPreflight, evidence: journal.KindPreflightCheck},
		{name: "changed BIOS context", before: func(t *testing.T, in simRun) {
			t.Helper()
			if stop := simulate(t, in); stop.Reason != StopCycles {
				t.Fatalf("first run stopped with %+v", stop)
			}
			in.Machine.SetBIOSContext(changed)
			in.Machine.Reboot()
		}, want: journal.DeadEndPreflight, evidence: journal.KindPreflightCheck},
		{name: "failure at zero", cfg: zeroFails, want: journal.DeadEndFailureAtZero, evidence: journal.KindFailure, killable: true, sticky: true},
	}
	for _, tt := range tests {
		cfg := tt.cfg
		if cfg.Cores == 0 {
			cfg = small()
		}
		setup := func(t *testing.T) simRun {
			t.Helper()
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
				if stop.Reason != StopCycles {
					t.Fatalf("stopped with %+v, want cycles", stop)
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
				before := len(readEvents(t, in.Dir))
				if again := simulate(t, in); again.Reason != StopDeadEnd || again.DeadEnd.Condition != tt.want {
					t.Fatalf("second run stopped with %+v", again)
				}
				assertOnlyRestorationWrites(t, readEvents(t, in.Dir), before)
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
				assertOnlyRestorationWrites(t, readEvents(t, in.Dir), last.Seq)
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
	if stop := simulate(t, in); stop.Reason != StopCycles {
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

func TestSignalStopsCleanly(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m := newSim(t, small())
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	in := simInput(dir, m)
	in.AfterAppend = &appendGate{after: true, match: eventKind(journal.KindTrialStart), do: func(journal.Event) error { cancel(); return nil }}
	stop, err := runSim(ctx, in, nil)
	if err != nil || stop.Reason != StopSignal {
		t.Fatalf("stopped with %+v, %v", stop, err)
	}
	stopped := lastEvent(t, dir)
	if s, ok := stopped.Data.(*journal.Shutdown); !ok || s.Reason != journal.ShutdownSignal {
		t.Fatal("last event is not shutdown{signal}")
	}
	m.Reboot()
	in.AfterAppend = nil
	if stop := simulate(t, in); stop.Reason != StopCycles {
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

type interruptedTrials struct {
	machine.Trials
	cancel context.CancelFunc
}

func (t interruptedTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	r, err := t.Trials.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	return interruptedRunning{Running: r, cancel: t.cancel}, nil
}

type interruptedRunning struct {
	machine.Running
	cancel context.CancelFunc
}

func (r interruptedRunning) Wait(context.Context, machine.Reporter) (machine.Result, error) {
	r.cancel()
	return machine.Result{Ran: 85 * time.Second}, context.Canceled
}

func (r interruptedRunning) Stop() error {
	r.cancel()
	return nil
}

func TestInterruptedTrialRecordsTimeRan(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	seams := in.Machine.Seams()
	seams.Trials = interruptedTrials{Trials: seams.Trials, cancel: cancel}
	in.Seams = &seams
	stop, err := simulateBoot(ctx, in, wrapFor(in, nil))
	if err != nil || stop.Reason != StopSignal {
		t.Fatalf("stopped with %+v, %v", stop, err)
	}
	for _, e := range readEvents(t, in.Dir) {
		if p, ok := e.Data.(*journal.TrialEnd); ok {
			if !p.Interrupted || p.DurationS != 85 || p.Outcome != journal.OutcomeInconclusive {
				t.Fatalf("trial.end %+v: %s", p, e.Msg)
			}
			return
		}
	}
	t.Fatal("no trial.end")
}

func TestStopRestoresBaseline(t *testing.T) {
	t.Parallel()
	uneven := small()
	uneven.BIOS = []int{-10, -5}
	interrupt := func(_ *sim.Machine, cancel context.CancelFunc) { cancel() }
	tests := []struct {
		name    string
		cfg     sim.Config
		do      func(m *sim.Machine, cancel context.CancelFunc)
		want    StopReason
		offsets []int
	}{
		{name: "signal during search", cfg: small(), do: interrupt, want: StopSignal, offsets: []int{-10, -10}},
		{name: "cycles with the profile applied", cfg: uneven, want: StopCycles, offsets: []int{-10, -5}},
		{name: "SMU dead end", cfg: small(), do: func(m *sim.Machine, _ context.CancelFunc) { m.CorruptReadback(0) }, want: StopDeadEnd},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, tt.cfg))
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			if tt.do != nil {
				in.AfterAppend = &appendGate{after: true, match: eventKind(journal.KindTrialStart), do: func(journal.Event) error { tt.do(in.Machine, cancel); return nil }}
			}
			stop, err := runSim(ctx, in, nil)
			if err != nil || stop.Reason != tt.want {
				t.Fatalf("stopped with %+v, %v; want %s", stop, err, tt.want)
			}
			if tt.offsets == nil && stop.DeadEnd.Condition != journal.DeadEndSMU {
				t.Fatalf("dead end %s, want %s", stop.DeadEnd.Condition, journal.DeadEndSMU)
			}
			events := readEvents(t, in.Dir)
			last := len(events) - 1
			if events[last].Kind != journal.KindShutdown {
				t.Fatalf("last event %s, want shutdown", events[last].Kind)
			}
			restored, lastIntent := -1, -1
			for i, e := range events {
				if e.Kind == journal.KindProfileRestored {
					restored = i
				}
				if e.Kind == journal.KindSMUIntent {
					lastIntent = i
				}
			}
			if tt.offsets == nil {
				if restored >= 0 {
					t.Fatalf("seq %d: %s after the SMU failed", events[restored].Seq, events[restored].Msg)
				}
				return
			}
			if restored < lastIntent || restored != last-1 {
				t.Fatalf("profile.restored at index %d, want after the last smu.intent (%d) and right before shutdown (%d)", restored, lastIntent, last)
			}
			smu := in.Machine.Seams().SMU
			for c, want := range tt.offsets {
				if got, err := smu.Offset(c); err != nil || got != want {
					t.Errorf("core %d at CO %d (%v) after the stop, want %d", c, got, err, want)
				}
			}
		})
	}
}

func TestCrashDuringRestoreKeepsTheAppliedCondition(t *testing.T) {
	t.Parallel()
	signalled := func(dir string, tr *appendGate, m *sim.Machine) {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		in := simInput(dir, m)
		in.AfterAppend = &appendGate{after: true, match: eventKind(journal.KindTrialStart), do: func(journal.Event) error { cancel(); return nil }}
		if _, err := runSim(ctx, in, tr); err != nil {
			t.Fatal(err)
		}
	}
	ref := t.TempDir()
	signalled(ref, nil, newSim(t, small()))
	events := readEvents(t, ref)
	baseline := events[slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindSessionBaseline })].Seq
	restoring := slices.IndexFunc(events, func(e journal.Event) bool {
		return e.Kind == journal.KindSMUIntent && slices.Contains(e.Cause, baseline)
	})
	if restoring < 0 {
		t.Fatal("reference run did not restore the baseline")
	}

	dir := t.TempDir()
	m := newSim(t, small())
	signalled(dir, crashAt(events[restoring].Seq, m), m)
	for _, e := range readEvents(t, dir) {
		if c, ok := e.Data.(*journal.CrashDetected); ok {
			if c.Condition != machine.Alone {
				t.Fatalf("seq %d: %s; want the alone condition applied before the restore", e.Seq, e.Msg)
			}
			return
		}
	}
	t.Fatal("no crash.detected after crashing during the restore")
}

type runnerFault struct {
	fired   bool
	pending *machine.MCE
	now     func() time.Time
	mono    func() time.Duration
}

type faultyTrials struct {
	machine.Trials
	f *runnerFault
}

func (t faultyTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	if t.f.fired {
		return t.Trials.Start(ctx, spec)
	}
	t.f.fired = true
	t.f.pending = &machine.MCE{CPU: spec.CPUs[0], Core: spec.Cores[0], BankType: machine.LoadStore, Corrected: true, Time: t.f.now(), Monotonic: t.f.mono(), Lines: []string{"[Hardware Error]: Corrected error (test)"}}
	return nil, errors.New("backend exited during setup")
}

type faultyKernel struct {
	machine.Kernel
	f *runnerFault
}

func (k faultyKernel) ReadMCEs(boot, cursor string) (machine.KernelRead, error) {
	found, err := k.Kernel.ReadMCEs(boot, cursor)
	if err == nil && k.f.pending != nil {
		found.MCEs = append(found.MCEs, *k.f.pending)
		k.f.pending = nil
	}
	return found, err
}

func TestRunnerErrorKeepsMachineCheck(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	seams := in.Machine.Seams()
	f := &runnerFault{now: in.Machine.Now, mono: in.Machine.Monotonic}
	seams.Trials = faultyTrials{Trials: seams.Trials, f: f}
	seams.Kernel = faultyKernel{Kernel: seams.Kernel, f: f}
	var stop Stop
	err := machine.ErrCrashed
	for boots := 0; errors.Is(err, machine.ErrCrashed) && boots < maxSimulatedBoots; boots++ {
		if boots > 0 {
			in.Machine.Reboot()
		}
		boot, _ := seams.Host.BootID()
		j, oerr := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now})
		if oerr != nil {
			t.Fatal(oerr)
		}
		stop, err = Run(context.Background(), Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: wrapFor(in, nil)(j), Machine: seams, Cycles: in.Cycles})
		if cerr := j.Close(); err == nil {
			err = cerr
		}
	}
	if err != nil || stop.Reason != StopCycles {
		t.Fatalf("run stopped with %+v, %v", stop, err)
	}
	events := readEvents(t, in.Dir)
	var intent *journal.TrialIntent
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			if intent == nil {
				intent = p
			}
		case *journal.TrialEnd:
			if p.Trial != "0001" {
				continue
			}
			if p.Outcome != journal.OutcomeFailure || p.Signal != machine.CorrectedMCE {
				t.Fatalf("trial 0001 ended %s %s, want failure corrected_mce", p.Outcome, p.Signal)
			}
			fail := failureCiting(events, e.Seq)
			if fail == nil || fail.Attribution != journal.Attributed || *fail.Core != *intent.Core || *fail.Offset != *intent.Offset {
				t.Fatalf("failure for trial 0001: %+v, want attributed to core %d at %d", fail, *intent.Core, *intent.Offset)
			}
			return
		}
	}
	t.Fatal("no trial.end for trial 0001")
}
