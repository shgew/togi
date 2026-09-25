package session

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"slices"
	"strings"
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
	return SimInput{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1}
}

// memState stands in for state.json: rewriting a file after every event dominates these tests on
// copy-on-write filesystems, and the file itself is covered by the journal package and TestSixteenCoresSurviveARotation.
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
	Unproven   int
}

func (c coreSummary) String() string {
	ptr := func(p *int) string {
		if p == nil {
			return "none"
		}
		return fmt.Sprint(*p)
	}
	return fmt.Sprintf("%s at %d (pass %s, failed mark %s, unproven %d)", c.Phase, c.Offset, ptr(c.Pass), ptr(c.FailedMark), c.Unproven)
}

func summary(t *testing.T, dir string) string {
	t.Helper()
	st, err := readMemState(stateOf(dir))
	if err != nil {
		t.Fatal(err)
	}
	out := fmt.Sprintf("phase %s, dead end %v:", st.Phase, st.DeadEnd)
	for _, c := range st.Cores {
		out += fmt.Sprintf(" core %d %s;", c.Core, coreSummary{c.Phase, c.Offset, c.Pass, c.FailedMark, c.UnprovenDepth})
	}
	if g := st.Guard; g != nil {
		out += fmt.Sprintf(" guard rotation %d, clean rotations %d, clean %d s, window open %v", g.Rotation, g.CleanRotations, g.CleanS, g.EscalationWindow)
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
	if stop := simulate(t, simInput(dir, newSim(t, cfg))); stop.Reason != StopRotations {
		t.Fatalf("reference run stopped with %+v", stop)
	}
	return dir, readEvents(t, dir)
}

func TestSixteenCoresSurviveARotation(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m := newSim(t, sim.Config{Seed: 1})
	began := time.Now()
	stop, err := Simulate(context.Background(), simInput(dir, m))
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("16-core session took %s", time.Since(began))
	if stop.Reason != StopRotations {
		t.Fatalf("stopped with %+v", stop)
	}
	st, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if st.Phase != "guard" || len(st.Cores) != 16 || st.Guard == nil || st.Guard.CleanRotations != 1 || st.Tier != journal.TierBronze {
		t.Fatalf("state phase %s tier %s with %d cores, guard %+v", st.Phase, st.Tier, len(st.Cores), st.Guard)
	}
	for _, c := range st.Cores {
		if c.Phase != journal.PhaseConfirmed || c.Offset < m.ResidentEdge(c.Core) || c.Offset > 0 {
			t.Errorf("core %d %s at %d, hidden resident edge %d", c.Core, c.Phase, c.Offset, m.ResidentEdge(c.Core))
		}
	}
	if v := m.Violations(); len(v) > 0 {
		t.Errorf("isolation violations: %v", v)
	}
	regimes := map[string]machine.Regime{}
	counted := map[string]bool{}
	progress := map[machine.Regime]map[string]bool{}
	for _, e := range readEvents(t, dir) {
		switch p := e.Data.(type) {
		case *journal.SMUIntent:
			if p.Offset < machine.MinOffset || p.Offset > machine.MaxOffset {
				t.Errorf("seq %d writes %d", e.Seq, p.Offset)
			}
		case *journal.TrialIntent:
			regimes[p.Trial] = p.Regime
		case *journal.TrialProgress:
			r := regimes[p.Trial]
			if progress[r] == nil {
				progress[r] = map[string]bool{}
			}
			progress[r][p.Detail] = true
		case *journal.TrialSignal:
			if p.Schedule == "" {
				counted[p.Trial] = true
			}
		case *journal.TrialEnd:
			r := regimes[p.Trial]
			if (r == machine.R3 || r == machine.R4) && p.Signal != machine.Crash && !p.Interrupted && !counted[p.Trial] {
				t.Errorf("trial %s (%s) ended without its load-step counts", p.Trial, r)
			}
		}
	}
	for _, tc := range []struct {
		regime machine.Regime
		detail string
	}{
		{machine.R6, "first half idle, then 100ms bursts every 2s, one core at a time"},
		{machine.R7, "CCD0 only: stopped cores 08-15"},
		{machine.R7, "CCD1 only: resumed cores 08-15, stopped cores 00-07"},
	} {
		if !progress[tc.regime][tc.detail] {
			t.Errorf("simulated journal lacks %s progress %q", tc.regime, tc.detail)
		}
	}
	foundBursts := false
	for detail := range progress[machine.R6] {
		if strings.HasPrefix(detail, "bursts: ") && strings.Contains(detail, " continues, ") && strings.HasSuffix(detail, " stops") {
			foundBursts = true
		}
	}
	if !foundBursts {
		t.Error("simulated journal lacks R6 burst counts")
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
			if stop := drive(t, simInput(dir, m), killAt(k)); stop.Reason != StopRotations {
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
			if p.Core == nil {
				continue
			}
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
			var started, last time.Time
			for _, e := range events[:k] {
				switch p := e.Data.(type) {
				case *journal.TrialIntent:
					intent, ended, started, last = p, false, time.Time{}, time.Time{}
				case *journal.TrialStart:
					started, last = e.Time, e.Time
				case *journal.TrialProgress, *journal.TrialSignal, *journal.TrialSample:
					if !started.IsZero() {
						last = e.Time
					}
				case *journal.TrialEnd:
					ended = ended || p.Trial == intent.Trial
				}
			}
			if !ended {
				want := int(last.Sub(started).Seconds())
				for _, e := range events[k:] {
					if p, ok := e.Data.(*journal.TrialEnd); ok && p.Trial == intent.Trial {
						if p.DurationS != want || !strings.HasSuffix(e.Msg, fmt.Sprintf("after %ds", want)) {
							t.Fatalf("crash at %d (%s): trial.end %+v %q, want %ds", k, hit.Kind, p, e.Msg, want)
						}
						break
					}
				}
			}
			f := failureCiting(events, crash.Seq)
			switch {
			case f == nil:
				t.Fatalf("no failure cites crash.detected seq %d", crash.Seq)
			case !ended && intent.Condition == machine.Resident && (f.Attribution != journal.Unattributed || f.Trial != intent.Trial || f.Regime != intent.Regime || f.Condition != machine.Resident || f.Signal != machine.Crash):
				t.Fatalf("crash at %d (%s) during resident trial %s: failure %+v", k, hit.Kind, intent.Trial, f)
			case !ended && intent.Condition == machine.Isolated && (f.Attribution != journal.Attributed || *f.Core != *intent.Core || *f.Offset != *intent.Offset || f.Trial != intent.Trial || f.Signal != machine.Crash):
				t.Fatalf("crash at %d (%s) during trial %s: failure %+v", k, hit.Kind, intent.Trial, f)
			case ended && f.Attribution != journal.Unattributed:
				t.Fatalf("crash at %d (%s) after trial %s ended: failure %+v", k, hit.Kind, intent.Trial, f)
			}
			if stop.Reason != StopRotations {
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
			if stop := drive(t, simInput(dir, m), crashAt(k, m)); stop.Reason != StopRotations {
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
				if stop.Reason != StopRotations || strays != 2 {
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
			if stop := simulate(t, in); stop.Reason != StopRotations {
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
				if stop.Reason != StopRotations {
					t.Fatalf("stopped with %+v, want rotations", stop)
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
				for _, e := range readEvents(t, in.Dir)[before:] {
					if e.Kind == journal.KindSMUIntent {
						t.Fatalf("seq %d: %s in the run after the dead end", e.Seq, e.Msg)
					}
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
	if stop := simulate(t, in); stop.Reason != StopRotations {
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

type afterLines struct {
	match string
	n     int
	do    func()
}

func (a *afterLines) Write(p []byte) (int, error) {
	if !bytes.Contains(p, []byte(a.match)) {
		return len(p), nil
	}
	if a.n--; a.n == 0 {
		a.do()
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
	in.Log = &afterLines{n: 50, do: cancel}
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
	if stop := simulate(t, in); stop.Reason != StopRotations {
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

func TestInterruptedTrialRecordsTimeRan(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	seams := in.Machine.Seams()
	seams.Trials = interruptedTrials{Trials: seams.Trials, cancel: cancel}
	boot, _ := seams.Host.BootID()
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now})
	if err != nil {
		t.Fatal(err)
	}
	stop, err := Run(ctx, Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: wrapFor(in, nil)(j), Machine: seams})
	if cerr := j.Close(); err == nil {
		err = cerr
	}
	if err != nil || stop.Reason != StopSignal {
		t.Fatalf("stopped with %+v, %v", stop, err)
	}
	for _, e := range readEvents(t, in.Dir) {
		if p, ok := e.Data.(*journal.TrialEnd); ok {
			if !p.Interrupted || p.DurationS != 85 || e.Msg != "trial 0001 INCONCLUSIVE after 85s: stopped by signal" {
				t.Fatalf("trial.end %+v: %s", p, e.Msg)
			}
			return
		}
	}
	t.Fatal("no trial.end")
}

func TestStopRestoresBaseline(t *testing.T) {
	t.Parallel()
	sharp := sim.DefaultModel()
	sharp.PastEdgeRate, sharp.Signals = 1e6, map[machine.Signal]float64{machine.ComputationError: 1}
	uneven := small()
	uneven.BIOS = []int{-10, -5}
	zeroFails := small()
	zeroFails.Edges[0].Isolated = [5]int{1, 1, 1, 1, 1}
	zeroFails.Model = &sharp
	baselineFails := small()
	baselineFails.Edges[0].Isolated = [5]int{-5, -5, -5, -5, -5}
	baselineFails.Model = &sharp
	interrupt := func(_ *sim.Machine, cancel context.CancelFunc) { cancel() }
	tests := []struct {
		name    string
		cfg     sim.Config
		match   string
		at      int
		do      func(m *sim.Machine, cancel context.CancelFunc)
		want    StopReason
		offsets []int
	}{
		{name: "signal during search", cfg: small(), at: 50, do: interrupt, want: StopSignal, offsets: []int{-10, -10}},
		{name: "signal as a trial fails at the baseline", cfg: baselineFails, match: " FAIL ", at: 1, do: interrupt, want: StopSignal, offsets: []int{-5, -10}},
		{name: "rotations with the profile applied", cfg: uneven, want: StopRotations, offsets: []int{-10, -5}},
		{name: "dead end, failed mark above the baseline", cfg: zeroFails, want: StopDeadEnd, offsets: []int{0, -10}},
		{name: "SMU dead end", cfg: small(), at: 50, do: func(m *sim.Machine, _ context.CancelFunc) { m.CorruptReadback(0) }, want: StopDeadEnd},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, tt.cfg))
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			if tt.do != nil {
				in.Log = &afterLines{match: tt.match, n: tt.at, do: func() { tt.do(in.Machine, cancel) }}
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
	signalled := func(dir string, tr *trigger, m *sim.Machine) {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		in := simInput(dir, m)
		in.Log = &afterLines{n: 50, do: cancel}
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
			if c.Condition != machine.Isolated {
				t.Fatalf("seq %d: %s; want the isolated condition applied before the restore", e.Seq, e.Msg)
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
	t.f.pending = &machine.MCE{CPU: spec.CPUs[0], Core: spec.Cores[0], BankType: machine.LoadStore, Corrected: true, Time: t.f.now(), Lines: []string{"[Hardware Error]: Corrected error (test)"}}
	return nil, errors.New("backend exited during setup")
}

type faultyKernel struct {
	machine.Kernel
	f *runnerFault
}

func (k faultyKernel) MCEs(boot string, since time.Time) ([]machine.MCE, error) {
	found, err := k.Kernel.MCEs(boot, since)
	if err == nil && k.f.pending != nil {
		found = append(found, *k.f.pending)
		k.f.pending = nil
	}
	return found, err
}

func TestRunnerErrorKeepsMachineCheck(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	seams := in.Machine.Seams()
	f := &runnerFault{now: in.Machine.Now}
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
		stop, err = Run(context.Background(), Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: wrapFor(in, nil)(j), Machine: seams, Rotations: in.Rotations})
		if cerr := j.Close(); err == nil {
			err = cerr
		}
	}
	if err != nil || stop.Reason != StopRotations {
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
