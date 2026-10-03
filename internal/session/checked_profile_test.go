package session

import (
	"errors"
	"fmt"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/tuner"
)

func checkedRunner(t *testing.T, baseline []int) (*runner, *sim.Machine, func()) {
	t.Helper()
	cfg := sim.Config{Seed: 3, Cores: len(baseline), BIOS: slices.Clone(baseline)}
	m := newSim(t, cfg)
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
	in.Machine = m
	r := &runner{in: Input{Journal: wrapFor(in, nil)(j), Machine: seams, Config: in.Config, Boot: boot}, fold: newFold(), tuner: tuner.New()}
	r.cores, err = seams.Host.Topology()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := r.append(&journal.SessionStart{Build: Build(), Session: "test", Cores: r.cores}); err != nil {
		t.Fatal(err)
	}
	if _, err := r.append(&journal.SessionBaseline{Offsets: slices.Clone(baseline)}); err != nil {
		t.Fatal(err)
	}
	return r, m, func() {
		if err := j.Close(); err != nil {
			t.Fatal(err)
		}
	}
}

func TestApplyWritesShallowBeforeDeepInCoreOrder(t *testing.T) {
	t.Parallel()
	r, m, closeJournal := checkedRunner(t, []int{0, 0, 0, 0})
	defer closeJournal()
	for core, offset := range []int{-30, -20, -10, -30} {
		if err := m.Seams().SMU.SetOffset(core, offset); err != nil {
			t.Fatal(err)
		}
	}
	r.applied = []int{-30, -20, -10, -30}
	if err := r.apply([]int{-31, -19, -11, -29}, &journal.ProfileApplied{Offsets: []int{-31, -19, -11, -29}, Condition: machine.Together}, 0); err != nil {
		t.Fatal(err)
	}
	var writes [][2]int
	for _, e := range r.in.Journal.Events() {
		if p, ok := e.Data.(*journal.SMUIntent); ok && p.Core != nil {
			writes = append(writes, [2]int{*p.Core, p.Offset})
		}
	}
	if diff := cmp.Diff([][2]int{{1, -19}, {3, -29}, {0, -31}, {2, -11}}, writes); diff != "" {
		t.Fatalf("write ordering (-want +got):\n%s", diff)
	}
}

func TestApplyRefusesCombinationBeforeSMUWrite(t *testing.T) {
	t.Parallel()
	r, _, closeJournal := checkedRunner(t, []int{-30, 0})
	defer closeJournal()
	r.applied = []int{-30, 0}
	if _, err := r.append(&journal.Combination{Combination: 1, Members: []journal.CombinationMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}}); err != nil {
		t.Fatal(err)
	}
	before := r.in.Journal.Events()
	smu := &recordedSMU{SMU: r.in.Machine.SMU}
	r.in.Machine.SMU = smu
	if err := r.apply([]int{-30, -30}, &journal.ProfileApplied{Offsets: []int{-30, -30}, Condition: machine.Together}, 0); err == nil {
		t.Fatal("profile reaching the combination was not refused")
	}
	if diff := cmp.Diff(before, r.in.Journal.Events()); diff != "" {
		t.Fatalf("refused profile recorded a write (-before +after):\n%s", diff)
	}
	if diff := cmp.Diff([][2]int(nil), smu.writes); diff != "" {
		t.Fatalf("refused profile reached the SMU (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]int{-30, 0}, r.applied); diff != "" {
		t.Fatalf("refused profile changed applied offsets (-want +got):\n%s", diff)
	}
}

type recordedSMU struct {
	machine.SMU
	writes [][2]int
}

func (s *recordedSMU) SetOffset(core, offset int) error {
	s.writes = append(s.writes, [2]int{core, offset})
	return s.SMU.SetOffset(core, offset)
}

func TestApplyRefusesCombinationAndPartialWrite(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		failAt  int
		offsets []int
	}{
		{"combination", 0, []int{-30, 0, 0, 0}},
		{"partial SMU write", 3, []int{-30, 0, 0, 0}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			r, m, closeJournal := checkedRunner(t, []int{0, 0, 0, 0})
			defer closeJournal()
			smu := &recordedSMU{SMU: r.in.Machine.SMU}
			r.in.Machine.SMU = smu
			if tc.failAt == 0 {
				if _, err := r.append(&journal.Combination{Combination: 1, Members: []journal.CombinationMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}}); err != nil {
					t.Fatal(err)
				}
			} else {
				m.FailWriteAt(tc.failAt)
			}
			err := r.apply([]int{-30, -30, 0, 0}, &journal.ProfileApplied{Offsets: []int{-30, -30, 0, 0}, Condition: machine.Together}, 0)
			if err == nil || (tc.failAt != 0 && !errors.Is(err, errDeadEndEvidence)) {
				t.Fatalf("apply did not refuse the incomplete write: %v", err)
			}
			if diff := cmp.Diff(tc.offsets, r.applied); diff != "" {
				t.Fatalf("applied (-want +got):\n%s", diff)
			}
			for core, want := range tc.offsets {
				got, readErr := m.Seams().SMU.Offset(core)
				if readErr != nil || got != want {
					t.Errorf("core %d: %d, %v; want %d", core, got, readErr, want)
				}
			}
			events := r.in.Journal.Events()
			if slices.ContainsFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindProfileApplied }) {
				t.Fatal("profile.applied recorded after incomplete write")
			}
			wantWrites := [][2]int{{0, -30}}
			if tc.failAt != 0 {
				wantWrites = append(wantWrites, [2]int{1, -30})
			}
			if diff := cmp.Diff(wantWrites, smu.writes); diff != "" {
				t.Fatalf("actual writes (-want +got):\n%s", diff)
			}
			var intents []*journal.SMUIntent
			var written []*journal.SMUWrite
			var failure *journal.SMUError
			for _, e := range events {
				switch p := e.Data.(type) {
				case *journal.SMUIntent:
					if p.Core != nil {
						intents = append(intents, p)
					}
				case *journal.SMUWrite:
					if p.Core != nil {
						written = append(written, p)
					}
				case *journal.SMUError:
					if diff := cmp.Diff(journal.KindSMUError, e.Kind); diff != "" {
						t.Fatal(diff)
					}
					failure = p
				}
			}
			wantIntents := []*journal.SMUIntent{{Op: journal.SMUSet, Core: new(0), Offset: -30}}
			if tc.failAt != 0 {
				wantIntents = append(wantIntents, &journal.SMUIntent{Op: journal.SMUSet, Core: new(1), Offset: -30})
				if failure == nil || failure.Error == "" {
					t.Fatal("missing SMU failure evidence")
				}
				got := *failure
				got.Error = ""
				if diff := cmp.Diff(journal.SMUError{Op: journal.SMUSet, Core: new(1), Offset: -30}, got); diff != "" {
					t.Fatalf("failed write payload (-want +got):\n%s", diff)
				}
			} else if failure != nil {
				t.Fatal("combination refusal reported an SMU failure")
			}
			if diff := cmp.Diff(wantIntents, intents); diff != "" {
				t.Fatalf("write intents (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff([]*journal.SMUWrite{{Op: journal.SMUSet, Core: new(0), Offset: -30}}, written); diff != "" {
				t.Fatalf("completed writes (-want +got):\n%s", diff)
			}
		})
	}
}

func TestRestoreNeverReachesCombinationWithNonzeroBaseline(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name                    string
		baseline, current, want []int
	}{
		{"combination", []int{-30, -30, 0, 0}, []int{-30, -29, 0, 0}, []int{-30, -29, 0, 0}},
		{"baseline shallower than combination", []int{-40, -40, 0, 0}, []int{-30, -29, 0, 0}, []int{-30, -29, 0, 0}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			r, m, closeJournal := checkedRunner(t, tc.baseline)
			defer closeJournal()
			if _, err := r.append(&journal.Combination{Combination: 1, Members: []journal.CombinationMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}}); err != nil {
				t.Fatal(err)
			}
			for core, offset := range tc.current {
				if err := m.Seams().SMU.SetOffset(core, offset); err != nil {
					t.Fatal(err)
				}
			}
			r.applied = slices.Clone(tc.current)
			r.state.Cores = make([]journal.CoreState, len(tc.current))
			for core, offset := range tc.current {
				r.state.Cores[core] = journal.CoreState{Core: core, Offset: offset}
			}
			if err := r.restore(); err != nil {
				t.Fatal(err)
			}
			var actual []int
			for core := range tc.current {
				offset, err := m.Seams().SMU.Offset(core)
				if err != nil {
					t.Fatal(err)
				}
				actual = append(actual, offset)
			}
			if diff := cmp.Diff(tc.want, actual); diff != "" {
				t.Fatalf("restored (-want +got):\n%s", diff)
			}
			for _, e := range r.in.Journal.Events() {
				if p, ok := e.Data.(*journal.SMUIntent); ok && p.Core != nil && p.Offset <= -30 && *p.Core == 1 {
					t.Fatalf("unsafe restore write: %s", e.Msg)
				}
			}
		})
	}
}

func TestApplyRefusesFailurePoint(t *testing.T) {
	t.Parallel()
	r, _, closeJournal := checkedRunner(t, []int{0, 0, 0, 0})
	defer closeJournal()
	if _, err := r.append(&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -29, FailurePoint: new(-30)}); err != nil {
		t.Fatal(err)
	}
	err := r.apply([]int{0, 0, -30, 0}, &journal.ProfileApplied{Offsets: []int{0, 0, -30, 0}, Condition: machine.Together}, 0)
	want := fmt.Sprintf("refusing to write core %02d to %d: the profile would reach failure point -30 of core 02", 2, -30)
	if err == nil || err.Error() != want {
		t.Fatalf("apply: %v; want %s", err, want)
	}
	if got := r.applied[2]; got != 0 {
		t.Fatalf("applied core 2 %d, want 0", got)
	}
	if errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("unexpected crash: %v", err)
	}
}

func TestFoldTracksRegistersBySparseCoreID(t *testing.T) {
	t.Parallel()
	f := newFold()
	for i, p := range []journal.Payload{
		&journal.SessionStart{Session: "test", Cores: []machine.CoreInfo{{Core: 0, CPUs: []int{0}}, {Core: 8, CPUs: []int{8}}}},
		&journal.SessionBaseline{Offsets: []int{0, 0}},
		&journal.SMUIntent{Op: journal.SMUSet, Core: new(8), Offset: -12},
	} {
		f.Fold(journal.Event{Seq: i + 1, Kind: p.Kind(), Boot: "b", Data: p})
	}
	if diff := cmp.Diff([]bool{false, true}, f.uncertain["b"]); diff != "" {
		t.Fatalf("uncertain after intent (-want +got):\n%s", diff)
	}
	f.Fold(journal.Event{Seq: 4, Kind: journal.KindSMUReadback, Boot: "b", Data: &journal.SMUReadback{Core: 8, Offset: -12, Expected: new(-12)}})
	if diff := cmp.Diff([]int{0, -12}, f.registers["b"]); diff != "" {
		t.Fatalf("registers (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]bool{false, false}, f.uncertain["b"]); diff != "" {
		t.Fatalf("uncertain after readback (-want +got):\n%s", diff)
	}
}

func TestIdleEvidenceWindowStartsAtApplicationsFirstWrite(t *testing.T) {
	t.Parallel()
	f := newFold()
	var windows []int64
	for i, e := range []struct {
		p    journal.Payload
		mono int64
	}{
		{&journal.SessionStart{Session: "test", Cores: []machine.CoreInfo{{Core: 0, CPUs: []int{0}}, {Core: 8, CPUs: []int{8}}}}, 0},
		{&journal.SessionBaseline{Offsets: []int{0, 0}}, 0},
		{&journal.SMUIntent{Op: journal.SMUSet, Core: new(0), Offset: -10}, 1000},
		{&journal.SMUIntent{Op: journal.SMUSet, Core: new(8), Offset: -12}, 2000},
		{&journal.ProfileApplied{Condition: machine.Together}, 3000},
		{&journal.SMUIntent{Op: journal.SMUSet, Core: new(8), Offset: -11}, 5000},
		{&journal.ProfileApplied{Condition: machine.Together}, 6000},
	} {
		f.Fold(journal.Event{Seq: i + 1, Kind: e.p.Kind(), Boot: "b", Mono: e.mono, Data: e.p})
		if _, ok := e.p.(*journal.ProfileApplied); ok {
			windows = append(windows, f.appliedMono["b"])
		}
	}
	if diff := cmp.Diff([]int64{1000, 5000}, windows); diff != "" {
		t.Fatalf("idle evidence window starts (-want +got):\n%s", diff)
	}
}

func TestFoldConsumesAttributedIdleFailure(t *testing.T) {
	t.Parallel()
	f := newFold()
	crash := &journal.CrashDetected{PreviousBoot: "previous", Condition: machine.Together}
	f.Fold(journal.Event{Seq: 1, Kind: crash.Kind(), Boot: "current", Data: crash})
	if diff := cmp.Diff([]int{1}, f.pendingIdle); diff != "" {
		t.Fatalf("pending idle crash (-want +got):\n%s", diff)
	}
	failure := &journal.Failure{Signal: machine.Crash, Attribution: journal.Attributed, Core: new(0), Offset: new(-12)}
	f.Fold(journal.Event{Seq: 2, Kind: failure.Kind(), Boot: "current", Cause: []int{1}, Data: failure})
	if diff := cmp.Diff([]int{}, f.pendingIdle); diff != "" {
		t.Fatalf("pending idle crashes after attributed failure (-want +got):\n%s", diff)
	}
}
