package sim

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func sharp(signal machine.Signal) *Model {
	m := DefaultModel()
	m.PastLimitRate = 1e6
	m.Signals = map[machine.Signal]float64{signal: 1}
	return &m
}

func flat(cores, alone, together int) []Limits {
	limits := make([]Limits, cores)
	for c := range limits {
		for i := range limits[c].Alone {
			limits[c].Alone[i] = alone
		}
		for i := range limits[c].Together {
			limits[c].Together[i] = together
		}
	}
	return limits
}

func newMachine(t *testing.T, cfg Config) *Machine {
	t.Helper()
	m, err := New(cfg)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	return m
}

func trial(m *Machine, core, offset int, r machine.Regime, cond machine.Condition, index int) (machine.Result, error) {
	s := m.Seams()
	if err := s.SMU.SetAllOffsets(0); err != nil {
		return machine.Result{}, err
	}
	if err := s.SMU.SetOffset(core, offset); err != nil {
		return machine.Result{}, err
	}
	spec := machine.TrialSpec{
		ID: fmt.Sprintf("%04d", index+1), Regime: r, Workload: machine.PickWorkload(r, index), Condition: cond,
		Cores: []int{core}, CPUs: []int{core}, Duration: 90 * time.Second, Index: index, Seed: 1,
	}
	run, err := s.Trials.Start(context.Background(), spec)
	if err != nil {
		return machine.Result{}, err
	}
	return run.Wait(context.Background(), nil)
}

type progressRecorder struct{ details []string }

func (r *progressRecorder) Progress(detail string)           { r.details = append(r.details, detail) }
func (*progressRecorder) Sample(machine.Sample)              {}
func (*progressRecorder) Signal(int, machine.Signal, string) {}

func TestR6ProgressRespectsTrialDuration(t *testing.T) {
	t.Parallel()
	t.Run("full", func(t *testing.T) {
		m := newMachine(t, Config{Seed: 3, Cores: 16, Limits: flat(16, -10, -10)})
		cores := make([]int, 16)
		for i := range cores {
			cores[i] = i
		}
		spec := machine.TrialSpec{ID: "0001", Regime: machine.R6, Condition: machine.Together, Cores: cores, CPUs: cores, Duration: 90 * time.Second}
		run, err := m.Seams().Trials.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		report := &progressRecorder{}
		res, err := run.Wait(context.Background(), report)
		if err != nil {
			t.Fatal(err)
		}
		want := []string{"first half idle, then 100ms bursts every 2s, one core at a time", "bursts: 23 continues, 39 stops"}
		if !slices.Equal(report.details, want) || res.Stops != 39 || res.Conts != 23 {
			t.Fatalf("R6 progress %v, counts %d/%d; want %v and 39/23", report.details, res.Stops, res.Conts, want)
		}
	})
	t.Run("early exit", func(t *testing.T) {
		m := newMachine(t, Config{Seed: 3, Cores: 16, Limits: flat(16, 0, 0), Model: sharp(machine.UnexpectedExit)})
		if err := m.Seams().SMU.SetOffset(0, -1); err != nil {
			t.Fatal(err)
		}
		spec := machine.TrialSpec{ID: "0002", Regime: machine.R6, Condition: machine.Together, Cores: []int{0}, CPUs: []int{0}, Duration: 90 * time.Second}
		run, err := m.Seams().Trials.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		report := &progressRecorder{}
		res, err := run.Wait(context.Background(), report)
		if err != nil {
			t.Fatal(err)
		}
		if res.Ran >= spec.Duration/2 {
			t.Fatalf("R6 failure at %s was not before the transition", res.Ran)
		}
		if !slices.Equal(report.details, []string{"bursts: 0 continues, 1 stops"}) {
			t.Fatalf("early R6 failure progress %v", report.details)
		}
	})
}

func transcript(t *testing.T, seed uint64) []string {
	t.Helper()
	m := newMachine(t, Config{Seed: seed, Cores: 4})
	s := m.Seams()
	var out []string
	log := func(format string, args ...any) { out = append(out, fmt.Sprintf(format, args...)) }
	boot, _ := s.Host.BootID()
	log("boot %s limits %+v", boot, m.limits)
	for core := range 4 {
		limit := m.AloneLimit(core)
		for _, o := range []int{limit - 2, limit, limit + 1} {
			for _, r := range []machine.Regime{machine.R1, machine.R3, machine.R4} {
				res, err := trial(m, core, machine.ClampOffset(o), r, machine.Alone, 0)
				tctl := -1
				if res.TctlMaxC != nil {
					tctl = *res.TctlMaxC
				}
				res.TctlMaxC = nil
				log("core %d offset %d %s: %+v tctl %d %v at %s", core, o, r, res, tctl, err, m.Now())
				if errors.Is(err, machine.ErrCrashed) {
					m.Reboot()
				}
			}
		}
	}
	first, _ := s.Host.BootID()
	m.Crash()
	m.Reboot()
	second, _ := s.Host.BootID()
	for _, b := range []string{boot, first, second} {
		mces, err := s.Kernel.MCEs(b, 0)
		log("boot %s mces %+v %v", b, mces, err)
	}
	return out
}

func TestSameSeedSameObservations(t *testing.T) {
	a, b := transcript(t, 7), transcript(t, 7)
	if !slices.Equal(a, b) {
		t.Fatalf("seed 7 transcripts differ:\n%v\n%v", a, b)
	}
	if c := transcript(t, 8); a[0] == c[0] {
		t.Fatal("seeds 7 and 8 drew the same limits and boot ID")
	}
}

func TestEachSignal(t *testing.T) {
	t.Parallel()
	for _, signal := range signalOrder {
		t.Run(string(signal), func(t *testing.T) {
			t.Parallel()
			model := sharp(signal)
			model.CrashMCE = 1
			m := newMachine(t, Config{Seed: 1, Cores: 2, BIOS: []int{-3, -3}, Limits: flat(2, -10, -10), Model: model})
			s := m.Seams()
			boot, _ := s.Host.BootID()
			res, err := trial(m, 0, -12, machine.R2, machine.Alone, 0)
			switch signal {
			case machine.ComputationError, machine.Stall, machine.UnexpectedExit:
				if err != nil || res.Signal != signal || res.Core != 0 || res.Ran >= 90*time.Second {
					t.Fatalf("result %+v, %v; want %s on core 0", res, err, signal)
				}
			case machine.CorrectedMCE:
				mces, _ := s.Kernel.MCEs(boot, 0)
				if err != nil || res.Signal != "" || res.Ran != 90*time.Second || len(mces) != 1 || !mces[0].Corrected || mces[0].Core != 0 {
					t.Fatalf("result %+v, %v, mces %+v; want a pass-shaped result and one corrected MCE", res, err, mces)
				}
			case machine.Crash:
				if !errors.Is(err, machine.ErrCrashed) {
					t.Fatalf("error %v, want ErrCrashed", err)
				}
				if _, err := s.Host.BootID(); !errors.Is(err, machine.ErrCrashed) {
					t.Fatalf("BootID after crash: %v", err)
				}
				m.Reboot()
				next, _ := s.Host.BootID()
				mces, _ := s.Kernel.MCEs(next, 0)
				if len(mces) != 1 || mces[0].Corrected || mces[0].Core != 0 || !mces[0].Time.Equal(m.Now()) {
					t.Fatalf("next boot mces %+v, want one uncorrected MCE on core 0 at boot", mces)
				}
			case machine.UncorrectedMCE:
			}
		})
	}
}

func TestNearLimitRate(t *testing.T) {
	t.Parallel()
	for _, rate := range []float64{0, 1e6} {
		model := sharp(machine.ComputationError)
		model.NearLimitRate = rate
		m := newMachine(t, Config{Seed: 2, Cores: 2, Limits: flat(2, -10, -10), Model: model})
		for i := range 100 {
			res, err := trial(m, 1, -10, machine.R1, machine.Alone, i)
			if err != nil || (res.Signal != "") != (rate > 0) {
				t.Fatalf("near-limit rate %g, trial %d: %+v %v", rate, i, res, err)
			}
		}
	}
}

func TestTogetherOnlyLimit(t *testing.T) {
	t.Parallel()
	m := newMachine(t, Config{Seed: 3, Cores: 2, Limits: flat(2, -20, -15), Model: sharp(machine.ComputationError)})
	if res, err := trial(m, 0, -18, machine.R1, machine.Alone, 0); err != nil || res.Signal != "" {
		t.Fatalf("alone at -18: %+v %v, want pass", res, err)
	}
	if err := m.Seams().SMU.SetOffset(1, -1); err != nil {
		t.Fatal(err)
	}
	spec := machine.TrialSpec{ID: "0001", Regime: machine.R1, Condition: machine.Together, Workload: machine.PickWorkload(machine.R1, 0), Cores: []int{0}, CPUs: []int{0}, Duration: 90 * time.Second}
	run, err := m.Seams().Trials.Start(context.Background(), spec)
	if err != nil {
		t.Fatal(err)
	}
	if res, err := run.Wait(context.Background(), nil); err != nil || res.Signal != machine.ComputationError {
		t.Fatalf("together at -18: %+v %v, want failure", res, err)
	}
}

func TestBankTypes(t *testing.T) {
	t.Parallel()
	for _, local := range []float64{1, 0} {
		model := sharp(machine.CorrectedMCE)
		model.CoreLocalBank = local
		m := newMachine(t, Config{Seed: 4, Cores: 2, Limits: flat(2, -10, -10), Model: model})
		for i := range 20 {
			if _, err := trial(m, 0, -15, machine.R2, machine.Alone, i); err != nil {
				t.Fatal(err)
			}
		}
		boot, _ := m.Seams().Host.BootID()
		mces, _ := m.Seams().Kernel.MCEs(boot, 0)
		if len(mces) != 20 {
			t.Fatalf("%d MCEs, want 20", len(mces))
		}
		for _, mce := range mces {
			if mce.BankType.CoreLocal() != (local == 1) || bankTypes[mce.Bank] != mce.BankType {
				t.Fatalf("core-local chance %g: bank %d %s", local, mce.Bank, mce.BankType)
			}
		}
	}
}

func TestFaults(t *testing.T) {
	t.Parallel()
	fresh := func(t *testing.T) (*Machine, machine.Machine) {
		t.Helper()
		m := newMachine(t, Config{Seed: 5, Cores: 2, Limits: flat(2, -10, -10)})
		return m, m.Seams()
	}
	t.Run("corrupt readback", func(t *testing.T) {
		m, s := fresh(t)
		m.CorruptReadback(1)
		if o, _ := s.SMU.Offset(1); o != 0 {
			t.Fatalf("readback before a write: %d", o)
		}
		_ = s.SMU.SetAllOffsets(-7)
		if o, _ := s.SMU.Offset(1); o != -8 {
			t.Fatalf("corrupt readback %d, want -8", o)
		}
		if o, _ := s.SMU.Offset(1); o != -7 {
			t.Fatalf("second readback %d, want -7", o)
		}
		m.CorruptReadback(0)
		_ = s.SMU.SetOffset(0, -50)
		if o, _ := s.SMU.Offset(0); o != -49 {
			t.Fatalf("corrupt readback at the floor %d, want -49", o)
		}
	})
	t.Run("failed write", func(t *testing.T) {
		m, s := fresh(t)
		m.FailWrite()
		if err := s.SMU.SetOffset(0, -5); err == nil {
			t.Fatal("write succeeded")
		}
		if o, _ := s.SMU.Offset(0); o != 0 {
			t.Fatalf("failed write changed the register to %d", o)
		}
		if err := s.SMU.SetOffset(0, -5); err != nil {
			t.Fatalf("second write: %v", err)
		}
		if err := s.SMU.SetOffset(0, 1); err == nil {
			t.Fatal("positive offset accepted")
		}
	})
	t.Run("setup failures", func(t *testing.T) {
		m, _ := fresh(t)
		m.FailSetup(2)
		for i := range 3 {
			_, err := trial(m, 0, 0, machine.R1, machine.Alone, 0)
			if (err != nil) != (i < 2) {
				t.Fatalf("trial %d: %v", i, err)
			}
		}
	})
	t.Run("escape", func(t *testing.T) {
		m, _ := fresh(t)
		m.Escape()
		if res, _ := trial(m, 1, 0, machine.R1, machine.Alone, 0); !slices.Equal(res.Escaped, []int{5}) {
			t.Fatalf("escaped %v, want [5]", res.Escaped)
		}
		if res, _ := trial(m, 1, 0, machine.R1, machine.Alone, 0); res.Escaped != nil {
			t.Fatalf("second trial escaped %v", res.Escaped)
		}
	})
	t.Run("failed check", func(t *testing.T) {
		m, s := fresh(t)
		m.FailCheck("root", "uid 1000")
		checks := s.Host.Preflight()
		if checks[0] != (machine.Check{Name: "root", Detail: "uid 1000"}) || !checks[1].OK {
			t.Fatalf("checks %+v", checks)
		}
		m.FailCheck("root", "")
		if !s.Host.Preflight()[0].OK {
			t.Fatal("cleared check still failing")
		}
	})
	t.Run("BIOS context", func(t *testing.T) {
		m, s := fresh(t)
		changed := defaultBIOSContext
		changed.BIOSVersion = "SIM.2"
		m.SetBIOSContext(changed)
		if c, _ := s.Host.BIOSContext(); c != changed {
			t.Fatalf("BIOS context %+v", c)
		}
	})
	t.Run("crash before apply", func(t *testing.T) {
		m, s := fresh(t)
		m.CrashBeforeApply(1)
		if _, err := s.SMU.Offset(0); err != nil {
			t.Fatalf("read before the first write: %v", err)
		}
		if err := s.SMU.SetAllOffsets(0); !errors.Is(err, machine.ErrCrashed) {
			t.Fatalf("first write: %v, want ErrCrashed", err)
		}
		m.Reboot()
		if err := s.SMU.SetAllOffsets(0); err != nil {
			t.Fatalf("first write of the next boot: %v", err)
		}
	})
	t.Run("violation", func(t *testing.T) {
		m, s := fresh(t)
		_ = s.SMU.SetOffset(1, -4)
		run, err := s.Trials.Start(context.Background(), machine.TrialSpec{ID: "0007", Regime: machine.R1, Condition: machine.Alone, Cores: []int{0}, CPUs: []int{0}, Duration: time.Second})
		if err != nil || run.Started().PID != 1007 {
			t.Fatalf("start: %v", err)
		}
		if v := m.Violations(); len(v) != 1 {
			t.Fatalf("violations %v, want one", v)
		}
	})
}

func TestReboot(t *testing.T) {
	t.Parallel()
	m := newMachine(t, Config{Seed: 6, Cores: 2, BIOS: []int{-4, 0}})
	s := m.Seams()
	before, _ := s.Host.BootID()
	start := m.Now()
	m.CorruptReadback(0)
	_ = s.SMU.SetAllOffsets(-9)
	run, err := s.Trials.Start(context.Background(), machine.TrialSpec{ID: "0001", Regime: machine.R1, Condition: machine.Together, Cores: []int{0}, CPUs: []int{0}, Duration: time.Minute})
	if err != nil {
		t.Fatal(err)
	}
	m.Crash()
	m.Reboot()
	if _, err := run.Wait(context.Background(), nil); !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("Wait on a trial of the crashed boot: %v, want ErrCrashed", err)
	}
	after, _ := s.Host.BootID()
	a, _ := s.SMU.Offset(0)
	b, _ := s.SMU.Offset(1)
	if before == after || a != -4 || b != 0 || m.Now().Sub(start) != 90*time.Second {
		t.Fatalf("after reboot: boot %s (was %s), registers %d %d, clock +%s", after, before, a, b, m.Now().Sub(start))
	}
}

func TestContextDone(t *testing.T) {
	t.Parallel()
	m := newMachine(t, Config{Seed: 6, Cores: 2})
	run, err := m.Seams().Trials.Start(context.Background(), machine.TrialSpec{ID: "0001", Regime: machine.R1, Condition: machine.Alone, Cores: []int{0}, CPUs: []int{0}, Duration: time.Minute})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	start := m.Now()
	if _, err := run.Wait(ctx, nil); !errors.Is(err, context.Canceled) || !m.Now().Equal(start) {
		t.Fatalf("Wait on a done context: %v, clock moved %s", err, m.Now().Sub(start))
	}
}

func TestNewRejects(t *testing.T) {
	t.Parallel()
	for _, cfg := range []Config{
		{Cores: 3},
		{Cores: 2, BIOS: []int{0}},
		{Cores: 2, BIOS: []int{0, 1}},
		{Cores: 2, Limits: flat(1, -10, -10)},
		{Cores: 2, Limits: flat(2, 2, 0)},
	} {
		if _, err := New(cfg); err == nil {
			t.Errorf("New(%+v) accepted", cfg)
		}
	}
}

func TestNewSignalWeights(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		signals map[machine.Signal]float64
		want    machine.Signal
	}{
		{name: "nil"},
		{name: "empty", signals: map[machine.Signal]float64{}},
		{name: "one zero", signals: map[machine.Signal]float64{machine.Crash: 0}},
		{name: "all zero", signals: map[machine.Signal]float64{machine.Crash: 0, machine.Stall: 0}},
		{name: "positive", signals: map[machine.Signal]float64{machine.Crash: 0.25}, want: machine.Crash},
		{name: "zero and positive", signals: map[machine.Signal]float64{machine.Crash: 0, machine.Stall: 0.25}, want: machine.Stall},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			model := DefaultModel()
			model.Signals = tc.signals
			m, err := New(Config{Cores: 2, Model: &model})
			if tc.want == "" {
				if err == nil || !strings.Contains(err.Error(), "model.signals") || !strings.Contains(err.Error(), "positive") {
					t.Fatalf("New error = %v; want model.signals positive-total error", err)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if got := drawSignal(m.signals(machine.R1), 0.5); got != tc.want {
				t.Fatalf("drawSignal = %s; want %s", got, tc.want)
			}
		})
	}
}
