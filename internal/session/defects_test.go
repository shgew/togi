package session

import (
	"errors"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func testDefect(direction defect.Direction) defect.Entry {
	return defect.Entry{ID: 2, Title: "test decision defect", PR: 100, Direction: direction, Decisions: []defect.DecisionMatch{{
		Kind: journal.KindTunerDecision, Decision: journal.StepDeeper, Cause: journal.KindTrialEnd,
		Predicate: func(_ defect.Evidence, decision, _ journal.Event) bool {
			return decision.Data.(*journal.TunerDecision).Core == 0
		},
	}}}
}

func TestDefectAnswersAndDisarm(t *testing.T) {
	for _, tc := range []struct {
		name       string
		answer     *bool
		wantResets int
	}{
		{name: "yes resets only affected core", answer: new(true), wantResets: 1},
		{name: "no continues without reset", answer: new(false)},
		{name: "unattended aggressive defect disarms"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			in := simInput(t.TempDir(), newSim(t, small()))
			if stop := simulate(t, in); stop.Reason != StopCycles {
				t.Fatalf("initial run stopped with %s", stop.Reason)
			}
			in.Defects = []defect.Entry{testDefect(defect.TooAggressive)}
			calls := 0
			if tc.answer != nil {
				in.Prompt = func(f defect.Finding) (bool, error) {
					calls++
					if f.Entry.ID != 2 || !slices.Equal(f.Cores, []int{0}) || len(f.Decisions) == 0 {
						t.Fatalf("prompt finding %+v", f)
					}
					return *tc.answer, nil
				}
			}
			stop := simulate(t, in)
			if tc.answer == nil {
				if stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndDefect {
					t.Fatalf("unattended stop %+v", stop)
				}
			} else if stop.Reason != StopCycles || calls != 1 {
				t.Fatalf("answered stop %+v, prompt calls %d", stop, calls)
			}
			events := readEvents(t, in.Dir)
			found, answered, resets, consumed := 0, 0, 0, 0
			resetSeq := 0
			for _, e := range events {
				switch p := e.Data.(type) {
				case *journal.DefectFound:
					if p.ID != 2 || !slices.Equal(p.Cores, []int{0}) || !slices.Equal(e.Cause, p.Decisions) {
						t.Fatalf("found %+v cause %v", p, e.Cause)
					}
					found++
				case *journal.DefectAnswered:
					if tc.answer == nil || p.ID != 2 || !slices.Equal(p.Cores, []int{0}) || p.Answer != map[bool]string{true: "yes", false: "no"}[*tc.answer] {
						t.Fatalf("answer %+v", p)
					}
					answered++
				case *journal.CommandReset:
					if p.Core == nil || *p.Core != 0 {
						t.Fatalf("reset of unrelated core: %+v", p)
					}
					resets++
					resetSeq = e.Seq
				case *journal.CorePhase:
					if p.Core == 0 && resetSeq != 0 && slices.Contains(e.Cause, resetSeq) {
						consumed++
					}
				}
			}
			if found != 1 || resets != tc.wantResets || (tc.answer != nil && answered != 1) || (tc.answer == nil && answered != 0) {
				t.Fatalf("found %d answered %d resets %d", found, answered, resets)
			}
			if tc.wantResets > 0 && consumed == 0 {
				t.Fatal("reset was queued but not consumed in this run")
			}
			if tc.answer != nil {
				_ = simulate(t, in)
				if calls != 1 || len(defect.Unanswered(readEvents(t, in.Dir))) != 0 {
					t.Fatalf("answered defect asked again: %d calls", calls)
				}
			}
		})
	}
}

func TestRealPowerOffDefectFoundOnceOnResume(t *testing.T) {
	in := simInput(t.TempDir(), newSim(t, small()))
	if stop := simulate(t, in); stop.Reason != StopCycles {
		t.Fatalf("initial run stopped with %s", stop.Reason)
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: "historic-poweroff", Now: in.Machine.Now, Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	old := Build()
	old.Fixes = 0
	if _, err := j.Append(&journal.ConfigLoaded{Build: old, Path: in.ConfigPath, Config: configSnapshot(in.Config)}); err != nil {
		t.Fatal(err)
	}
	if _, err := j.Append(&journal.TrialProgress{Trial: "legacy-poweroff", Detail: "core 00 backend exited early: <nil>"}); err != nil {
		t.Fatal(err)
	}
	end, err := j.Append(&journal.TrialEnd{Trial: "legacy-poweroff", Outcome: journal.OutcomeFailure, Signal: machine.UnexpectedExit, DurationS: 136})
	if err != nil {
		t.Fatal(err)
	}
	failure, err := j.Append(&journal.Failure{Signal: machine.UnexpectedExit, Attribution: journal.Attributed, Core: new(0), Offset: new(-10), Trial: "legacy-poweroff"}, end.Seq)
	if err != nil {
		t.Fatal(err)
	}
	decision, err := j.Append(&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -10, ToOffset: -9, FailurePoint: new(-10), Reason: "power-off false failure"}, failure.Seq)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := j.Append(&journal.Shutdown{Reason: journal.ShutdownSignal}); err != nil {
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	if stop := simulate(t, in); stop.Reason != StopCycles {
		t.Fatalf("too-cautious finding stopped tuning: %+v", stop)
	}
	events := readEvents(t, in.Dir)
	found := 0
	for _, e := range events {
		if p, ok := e.Data.(*journal.DefectFound); ok {
			if p.ID != 1 || !slices.Equal(p.Cores, []int{0}) || !slices.Equal(p.Decisions, []int{decision.Seq}) || !slices.Equal(e.Cause, p.Decisions) {
				t.Fatalf("power-off finding %+v, cause %v", p, e.Cause)
			}
			found++
		}
	}
	if found != 1 {
		t.Fatalf("found defect %d times; want once", found)
	}
	found = 0
	_ = simulate(t, in)
	for _, e := range readEvents(t, in.Dir) {
		if p, ok := e.Data.(*journal.DefectFound); ok && p.ID == 1 {
			found++
		}
	}
	if found != 1 {
		t.Fatalf("finding repeated after second resume: %d findings", found)
	}
}

func TestDefectPromptFailureLeavesAnswerUncommitted(t *testing.T) {
	t.Parallel()
	r, _, closeJournal := checkedRunner(t, []int{0, 0})
	defer closeJournal()
	r.in.Defects = []defect.Entry{testDefect(defect.TooAggressive)}
	if _, err := r.append(&journal.DefectFound{ID: 2, Direction: string(defect.TooAggressive), Cores: []int{0}, Decisions: []int{1}}, 1); err != nil {
		t.Fatal(err)
	}
	before := r.in.Journal.Events()
	failure := errors.New("terminal input closed")
	r.in.Prompt = func(defect.Finding) (bool, error) { return false, failure }
	stop, err := r.checkDefects()
	if stop != nil || !errors.Is(err, failure) || !strings.Contains(err.Error(), "ask whether to reset defect 2 cores") {
		t.Fatalf("failed operator prompt: %+v, %v", stop, err)
	}
	if diff := cmp.Diff(before, r.in.Journal.Events()); diff != "" {
		t.Fatalf("prompt failure recorded an answer or reset:\n%s", diff)
	}
}

func TestLegacyDefectAnswerRejectsUnrelatedWarning(t *testing.T) {
	t.Parallel()
	for _, wrongOperation := range []bool{false, true} {
		t.Run(map[bool]string{false: "wrong cause", true: "wrong operation"}[wrongOperation], func(t *testing.T) {
			t.Parallel()
			r, _, closeJournal := checkedRunner(t, []int{0, 0})
			defer closeJournal()
			r.in.Defects = []defect.Entry{testDefect(defect.TooAggressive)}
			found, err := r.append(&journal.DefectFound{ID: 2, Direction: string(defect.TooAggressive), Cores: []int{0, 1}, Decisions: []int{1}}, 1)
			if err != nil {
				t.Fatal(err)
			}
			var reset journal.Event
			for core := range 2 {
				reset, err = r.append(&journal.CommandReset{Core: new(core)}, found.Seq)
				if err != nil {
					t.Fatal(err)
				}
			}
			operation, cause := "write state projection", found.Seq
			if wrongOperation {
				operation, cause = "retain passed trial", reset.Seq
			}
			if _, err := r.append(&journal.SessionWarning{Operation: operation, Error: "unavailable"}, cause); err != nil {
				t.Fatal(err)
			}
			answer, err := r.append(&journal.DefectAnswered{ID: 2, Cores: []int{0, 1}, Answer: "yes"}, found.Seq)
			if err != nil {
				t.Fatal(err)
			}
			if stop, err := r.checkDefects(); err != nil || stop != nil {
				t.Fatalf("resume answer: %+v, %v", stop, err)
			}
			var cores []int
			for _, e := range r.in.Journal.Events() {
				if p, ok := e.Data.(*journal.CommandReset); ok && cmp.Equal([]int{answer.Seq}, e.Cause) {
					cores = append(cores, *p.Core)
				}
			}
			if diff := cmp.Diff([]int{0, 1}, cores); diff != "" {
				t.Fatalf("unrelated warning falsely completed the legacy answer:\n%s", diff)
			}
			before := r.in.Journal.Events()
			if stop, err := r.checkDefects(); err != nil || stop != nil {
				t.Fatalf("repeat resume: %+v, %v", stop, err)
			}
			if diff := cmp.Diff(before, r.in.Journal.Events()); diff != "" {
				t.Fatalf("resumed answer duplicated resets:\n%s", diff)
			}
		})
	}
}
