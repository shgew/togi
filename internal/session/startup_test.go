package session

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

func TestUnknownConfiguredCoreRefusesBeforeStartingSession(t *testing.T) {
	t.Parallel()
	for _, candidate := range []bool{false, true} {
		name := "start offset"
		if candidate {
			name = "candidate solo limit"
		}
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			if candidate {
				in.Config.CandidateSoloLimits = map[int]int{99: -10}
			} else {
				in.Config.StartOffsets = map[int]int{99: -10}
			}
			_, err := runSim(context.Background(), in, nil)
			if !errors.Is(err, ErrNoSuchCore) || !strings.Contains(err.Error(), name+" for core 99") {
				t.Fatalf("invalid configured core: %v", err)
			}
			if got := readEvents(t, in.Dir); len(got) != 0 {
				t.Fatalf("invalid configuration started a session: %+v", got)
			}
			if diff := cmp.Diff(small().BIOS, actualOffsets(t, in)); diff != "" {
				t.Fatalf("invalid configuration changed offsets:\n%s", diff)
			}
		})
	}
}

func TestCarriedStartsRespectFailurePointsAndMachineTopology(t *testing.T) {
	t.Parallel()
	for _, changed := range []bool{false, true} {
		t.Run(map[bool]string{false: "same BIOS", true: "changed BIOS"}[changed], func(t *testing.T) {
			t.Parallel()
			r, m, closeJournal := checkedRunner(t, []int{0, 0})
			defer closeJournal()
			c := carriedFixture(t, m)
			c.Facts = nil
			c.Cores = []journal.CarriedCore{
				{Core: 0, SoloLimit: new(-12), SoloLimitSession: "source", FailurePoint: new(-10), FailurePointSession: "source"},
				{Core: 1, FailurePoint: new(-20), FailurePointSession: "source"},
				{Core: 99, SoloLimit: new(-15), SoloLimitSession: "source"},
			}
			if changed {
				c.Context.BIOSVersion = "changed"
			}
			r.in.Carry = c
			r.in.Config.CandidateSoloLimits = map[int]int{0: -12}
			if err := r.startSession(); err != nil {
				t.Fatal(err)
			}
			var carried *journal.SessionCarried
			phases := map[int]*journal.CorePhase{}
			for _, e := range r.in.Journal.Events() {
				switch p := e.Data.(type) {
				case *journal.SessionCarried:
					carried = p
				case *journal.CorePhase:
					phases[p.Core] = p
				}
			}
			want := []journal.CarriedCore{c.Cores[0], c.Cores[1]}
			if changed {
				want = []journal.CarriedCore{{Core: 0, SoloLimit: new(-12), SoloLimitSession: "source"}}
			}
			if carried == nil || carried.FailurePoints == changed {
				t.Fatalf("carry commitment: %+v", carried)
			}
			if diff := cmp.Diff(want, carried.Carried); diff != "" {
				t.Fatalf("eligible carried cores (-want +got):\n%s", diff)
			}
			wantOffset, wantFailurePoint := -9, new(-10)
			if changed {
				wantOffset, wantFailurePoint = -12, nil
			}
			if len(phases) != 2 || phases[0] == nil || phases[1] == nil {
				t.Fatalf("initial phases: %+v", phases)
			}
			if phases[0].Offset != wantOffset || !phases[0].CheckSoloLimit {
				t.Fatalf("configured solo limit crossed carried failure point: %+v", phases[0])
			}
			if diff := cmp.Diff(wantFailurePoint, phases[0].FailurePoint); diff != "" {
				t.Fatal(diff)
			}
			if !changed && !strings.Contains(phases[0].Reason, "one count shallower than the failure point -10 carried from session source") {
				t.Fatalf("failure point clamp unexplained: %+v", phases[0])
			}
			if diff := cmp.Diff(map[bool]*int{false: new(-20), true: nil}[changed], phases[1].FailurePoint); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestResumedBaselineClampsInitialSearch(t *testing.T) {
	t.Parallel()
	m := newSim(t, small())
	in := simInput(t.TempDir(), m)
	seams := m.Seams()
	boot, err := seams.Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: m.Now, Monotonic: m.Monotonic, Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := j.Close(); err != nil {
			t.Fatal(err)
		}
	})
	r := &runner{in: Input{Journal: j, Machine: seams, Config: in.Config, Boot: boot}, fold: newFold(), tuner: tuner.New()}
	r.cores, err = seams.Host.Topology()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := r.append(&journal.SessionStart{Build: Build(), Session: "test", Cores: r.cores}); err != nil {
		t.Fatal(err)
	}
	if _, err := r.append(&journal.SessionBaseline{Offsets: []int{-60, 5}}); err != nil {
		t.Fatal(err)
	}
	if err := r.startSession(); err != nil {
		t.Fatal(err)
	}
	var offsets []int
	var reasons []string
	for _, e := range r.in.Journal.Events() {
		if p, ok := e.Data.(*journal.CorePhase); ok {
			offsets = append(offsets, p.Offset)
			reasons = append(reasons, p.Reason)
		}
	}
	if diff := cmp.Diff([]int{-50, 0}, offsets); diff != "" {
		t.Fatalf("initial offsets (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]string{"baseline -60 clamped to -50", "baseline 5 clamped to 0"}, reasons); diff != "" {
		t.Fatalf("clamp diagnostics (-want +got):\n%s", diff)
	}
}

type readFailureHost struct {
	machine.Host
	topologyErr error
	contextErr  error
}

func (h readFailureHost) Topology() ([]machine.CoreInfo, error) {
	if h.topologyErr != nil {
		return nil, h.topologyErr
	}
	return h.Host.Topology()
}

func (h readFailureHost) BIOSContext() (machine.BIOSContext, error) {
	if h.contextErr != nil {
		return machine.BIOSContext{}, h.contextErr
	}
	return h.Host.BIOSContext()
}

func TestHostReadFailureDoesNotStartTuning(t *testing.T) {
	t.Parallel()
	for _, name := range []string{"topology", "initial BIOS context", "resumed BIOS context"} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			if name == "resumed BIOS context" {
				if stop := simulate(t, in); stop.Reason != StopLaps {
					t.Fatalf("reference: %+v", stop)
				}
				in.Machine.Reboot()
			}
			before := 0
			if name == "resumed BIOS context" {
				before = len(readEvents(t, in.Dir))
			}
			seams := in.Machine.Seams()
			failure := errors.New("hardware identity unavailable")
			host := readFailureHost{Host: seams.Host}
			operation := "read BIOS context"
			if name == "topology" {
				host.topologyErr = failure
				operation = "read topology"
			} else {
				host.contextErr = failure
			}
			seams.Host = host
			_, err := runWithSeams(context.Background(), in, seams)
			if !errors.Is(err, failure) || !strings.Contains(err.Error(), operation) {
				t.Fatalf("host failure: %v", err)
			}
			for _, e := range readEvents(t, in.Dir)[before:] {
				if e.Kind == journal.KindSMUIntent || e.Kind == journal.KindTrialIntent || e.Kind == journal.KindShutdown {
					t.Fatalf("unobserved host allowed tuning or clean shutdown: %+v", e)
				}
			}
			if diff := cmp.Diff(small().BIOS, actualOffsets(t, in)); diff != "" {
				t.Fatalf("host failure changed offsets:\n%s", diff)
			}
		})
	}
}

func TestSessionIDFailureLeavesEmptyJournal(t *testing.T) {
	t.Parallel()
	m := newSim(t, small())
	seams := m.Seams()
	boot, err := seams.Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(t.TempDir(), journal.Options{Boot: boot, Now: m.Now, Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	failure := errors.New("cannot allocate session identity")
	_, err = Run(context.Background(), Input{Config: simInput("", m).Config, Boot: boot, Journal: j, Machine: seams, SessionID: func(time.Time) (string, error) { return "", failure }})
	if !errors.Is(err, failure) {
		t.Fatalf("session identity failure: %v", err)
	}
	if got := j.Events(); len(got) != 0 {
		t.Fatalf("failed identity allocation wrote events: %+v", got)
	}
	for core, want := range small().BIOS {
		if got, err := seams.SMU.Offset(core); err != nil || got != want {
			t.Fatalf("identity failure changed core %d: %d, %v", core, got, err)
		}
	}
}

func TestIncompatibleBootClearFailureRetainsBothErrors(t *testing.T) {
	t.Parallel()
	r, _, closeJournal := checkedRunner(t, []int{0, 0})
	defer closeJournal()
	failure := errors.New("GRUB environment unavailable")
	bootloader := &fakeBootloader{err: failure}
	r.in.Bootloader = bootloader
	build := Build()
	build.Ruleset++
	before := r.in.Journal.Events()
	err := r.checkCompatibility([]journal.Event{{Data: &journal.SessionStart{Build: build}}})
	var incompatible *journal.IncompatibleError
	if !errors.Is(err, failure) || !errors.As(err, &incompatible) || incompatible.Field != "ruleset" || !strings.Contains(err.Error(), "clear GRUB saved entry after incompatible journal") {
		t.Fatalf("compatibility refusal lost an operator diagnostic: %v", err)
	}
	if bootloader.calls != 1 {
		t.Fatalf("saved-entry clears %d, want one", bootloader.calls)
	}
	if diff := cmp.Diff(before, r.in.Journal.Events()); diff != "" {
		t.Fatalf("failed incompatible boot handoff modified journal:\n%s", diff)
	}
}
