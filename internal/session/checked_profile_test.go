package session

import (
	"errors"
	"fmt"
	"slices"
	"strings"
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
	if err := r.apply([]int{-31, -19, -11, -29}, &journal.ProfileApplied{Offsets: []int{-31, -19, -11, -29}, Condition: machine.Resident}, 0); err != nil {
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

func TestApplyRefusesJointMarkAndPartialWrite(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		failAt  int
		want    string
		offsets []int
	}{
		{"joint mark", 0, "refusing to write core 01 to -30: the profile would reach joint mark J1", []int{-30, 0, 0, 0}},
		{"partial SMU write", 3, "", []int{-30, 0, 0, 0}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			r, m, closeJournal := checkedRunner(t, []int{0, 0, 0, 0})
			defer closeJournal()
			if tc.failAt == 0 {
				if _, err := r.append(&journal.MarkJoint{Mark: 1, Members: []journal.JointMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}}); err != nil {
					t.Fatal(err)
				}
			} else {
				m.FailWriteAt(tc.failAt)
			}
			err := r.apply([]int{-30, -30, 0, 0}, &journal.ProfileApplied{Offsets: []int{-30, -30, 0, 0}, Condition: machine.Resident}, 0)
			if err == nil || (tc.failAt == 0 && err.Error() != tc.want) || (tc.failAt != 0 && !errors.Is(err, errDeadEndEvidence)) {
				t.Fatalf("apply: %v, want %s", err, tc.want)
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
			if tc.failAt != 0 && !slices.ContainsFunc(events, func(e journal.Event) bool {
				return e.Kind == journal.KindSMUError && strings.Contains(e.Msg, "simulated SMU command failure")
			}) {
				t.Fatal("missing SMU failure evidence")
			}
		})
	}
}

func TestRestoreNeverReachesJointMarkWithNonzeroBaseline(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name                    string
		baseline, current, want []int
	}{
		{"joint mark", []int{-30, -30, 0, 0}, []int{-30, -29, 0, 0}, []int{-30, -29, 0, 0}},
		{"baseline shallower than mark", []int{-40, -40, 0, 0}, []int{-30, -29, 0, 0}, []int{-30, -29, 0, 0}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			r, m, closeJournal := checkedRunner(t, tc.baseline)
			defer closeJournal()
			if _, err := r.append(&journal.MarkJoint{Mark: 1, Members: []journal.JointMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}}); err != nil {
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

func TestApplyRefusesFailedMark(t *testing.T) {
	t.Parallel()
	r, _, closeJournal := checkedRunner(t, []int{0, 0, 0, 0})
	defer closeJournal()
	if _, err := r.append(&journal.CorePhase{Core: 2, To: journal.PhaseDone, Offset: -29, FailedMark: new(-30)}); err != nil {
		t.Fatal(err)
	}
	err := r.apply([]int{0, 0, -30, 0}, &journal.ProfileApplied{Offsets: []int{0, 0, -30, 0}, Condition: machine.Resident}, 0)
	want := fmt.Sprintf("refusing to write core %02d to %d: the profile would reach failed mark -30 of core 02", 2, -30)
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
