package session

import (
	"errors"
	"fmt"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

type defectBoundaryJournal struct {
	Journal
	at, seen     int
	after, fired bool
}

func (j *defectBoundaryJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	if p.Kind() != journal.KindDefectAnswered && p.Kind() != journal.KindCommandReset {
		return j.Journal.Append(p, cause...)
	}
	j.seen++
	if !j.fired && j.seen == j.at && !j.after {
		j.fired = true
		return journal.Event{}, errKilled
	}
	e, err := j.Journal.Append(p, cause...)
	if err == nil && !j.fired && j.seen == j.at && j.after {
		j.fired = true
		return e, errKilled
	}
	return e, err
}

func defectRecoveryRunner(j Journal, prompt func(defect.Finding) (bool, error)) *runner {
	r := &runner{in: Input{Journal: j, Prompt: prompt, Defects: []defect.Entry{testDefect(defect.TooAggressive)}}, fold: newFold(), tuner: tuner.New()}
	journal.Replay(j.Events(), r.fold, &r.state, r.tuner)
	r.tuner.Project(&r.state)
	return r
}

func TestDefectAnswerResumesMissingCoreResets(t *testing.T) {
	referenceDir := t.TempDir()
	reference, _ := defectRecoveryFixture(t, referenceDir)
	if stop, err := defectRecoveryRunner(reference, func(defect.Finding) (bool, error) { return true, nil }).checkDefects(); err != nil || stop != nil {
		t.Fatalf("uninterrupted answer: %+v, %v", stop, err)
	}
	if err := reference.Close(); err != nil {
		t.Fatal(err)
	}
	want := defectRecoveryOutcome(t, referenceDir)
	for at := 1; at <= 3; at++ {
		for _, after := range []bool{false, true} {
			t.Run(fmt.Sprintf("append %d after %v", at, after), func(t *testing.T) {
				testDefectAnswerBoundary(t, at, after, want)
			})
		}
	}
}

func defectRecoveryFixture(t *testing.T, dir string) (*journal.Journal, int) {
	t.Helper()
	j, err := journal.Open(dir, journal.Options{Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	start, err := j.Append(&journal.SessionStart{Build: Build(), Session: "defect", Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}})
	if err != nil {
		t.Fatal(err)
	}
	found, err := j.Append(&journal.DefectFound{ID: 2, Title: "test decision defect", Direction: string(defect.TooAggressive), Cores: []int{0, 1}, Decisions: []int{start.Seq}}, start.Seq)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := j.Append(&journal.CommandReset{Core: new(0)}); err != nil {
		t.Fatal(err)
	}
	if _, err := j.Append(&journal.CommandReset{Core: new(1)}, found.Seq); err != nil {
		t.Fatal(err)
	}
	return j, found.Seq
}

func testDefectAnswerBoundary(t *testing.T, at int, after bool, want []journal.CoreState) {
	t.Helper()
	dir := t.TempDir()
	j, foundSeq := defectRecoveryFixture(t, dir)
	calls := 0
	prompt := func(f defect.Finding) (bool, error) {
		calls++
		if !slices.Equal(f.Cores, []int{0, 1}) {
			t.Fatalf("prompt cores %v", f.Cores)
		}
		return true, nil
	}
	cut := &defectBoundaryJournal{Journal: j, at: at, after: after}
	if _, err := defectRecoveryRunner(cut, prompt).checkDefects(); !errors.Is(err, errKilled) || !cut.fired {
		t.Fatalf("interruption: %v, fired %v", err, cut.fired)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(dir, journal.Options{Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	r := defectRecoveryRunner(j, prompt)
	if stop, err := r.checkDefects(); err != nil || stop != nil {
		t.Fatalf("resume: %+v, %v", stop, err)
	}
	wantCalls := 1
	if at == 1 && !after {
		wantCalls = 2
	}
	if calls != wantCalls {
		t.Fatalf("prompted %d times, want %d", calls, wantCalls)
	}
	assertDefectAnswerResets(t, j.Events(), foundSeq)
	seq := len(j.Events())
	if stop, err := r.checkDefects(); err != nil || stop != nil {
		t.Fatalf("second resume: %+v, %v", stop, err)
	}
	if len(j.Events()) != seq || calls != wantCalls {
		t.Fatal("completed answer prompted or reset again")
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(want, defectRecoveryOutcome(t, dir)); diff != "" {
		t.Fatalf("resumed core effects (-uninterrupted +resumed):\n%s", diff)
	}
}

func assertDefectAnswerResets(t *testing.T, events []journal.Event, foundSeq int) {
	t.Helper()
	answerSeq, answers := 0, 0
	for _, e := range events {
		if p, ok := e.Data.(*journal.DefectAnswered); ok {
			answers++
			answerSeq = e.Seq
			if p.Answer != "yes" || !slices.Equal(e.Cause, []int{foundSeq}) {
				t.Fatalf("answer %+v cause %v", p, e.Cause)
			}
		}
	}
	resets := map[int]int{}
	for _, e := range events {
		if p, ok := e.Data.(*journal.CommandReset); ok && slices.Contains(e.Cause, answerSeq) {
			if p.Core == nil || e.Seq <= answerSeq || !slices.Equal(e.Cause, []int{answerSeq}) {
				t.Fatalf("reset #%d %+v cause %v", e.Seq, p, e.Cause)
			}
			resets[*p.Core]++
		}
	}
	if answers != 1 {
		t.Fatalf("answers %d, want 1", answers)
	}
	if diff := cmp.Diff(map[int]int{0: 1, 1: 1}, resets); diff != "" {
		t.Fatalf("answer's core resets (-want +got):\n%s", diff)
	}
}

func defectRecoveryOutcome(t *testing.T, dir string) []journal.CoreState {
	t.Helper()
	in := simInput(dir, newSim(t, small()))
	if stop := simulate(t, in); stop.Reason != StopRotations {
		t.Fatalf("after reset: %+v", stop)
	}
	var state journal.State
	engine := tuner.New()
	journal.Replay(readEvents(t, dir), &state, engine)
	engine.Project(&state)
	for i := range state.Cores {
		state.Cores[i].LastDecision = nil
		if state.Cores[i].Queued != "" {
			t.Fatalf("core %d still has queued reset", state.Cores[i].Core)
		}
	}
	return state.Cores
}

func TestLegacyDefectAnswerPreservesLaterCoreProgress(t *testing.T) {
	for warnings := range 4 {
		t.Run(fmt.Sprintf("projection warnings %d", warnings), func(t *testing.T) {
			j, foundSeq := defectRecoveryFixture(t, t.TempDir())
			defer j.Close()
			for _, core := range []int{0, 1} {
				reset, err := j.Append(&journal.CommandReset{Core: new(core)}, foundSeq)
				if err != nil {
					t.Fatal(err)
				}
				if warnings&(1<<core) != 0 {
					if _, err := j.Append(&journal.SessionWarning{Operation: "write state projection", Error: "disk full"}, reset.Seq); err != nil {
						t.Fatal(err)
					}
				}
			}
			if _, err := j.Append(&journal.DefectAnswered{ID: 2, Cores: []int{0, 1}, Answer: "yes"}, foundSeq); err != nil {
				t.Fatal(err)
			}
			for _, core := range []int{0, 1} {
				if _, err := j.Append(&journal.CorePhase{Core: core, To: journal.PhaseDone, Offset: -30, FailedMark: new(-31)}); err != nil {
					t.Fatal(err)
				}
			}
			r := defectRecoveryRunner(j, func(defect.Finding) (bool, error) {
				t.Fatal("completed legacy answer prompted again")
				return false, nil
			})
			want := slices.Clone(r.state.Cores)
			if stop, err := r.checkDefects(); err != nil || stop != nil {
				t.Fatalf("legacy answer resume: %+v, %v", stop, err)
			}
			if diff := cmp.Diff(want, r.state.Cores); diff != "" {
				t.Fatalf("legacy answer queued resets over later progress (-want +got):\n%s", diff)
			}
		})
	}
}

func TestDefectAnswerDoesNotReuseConsumedFindingResets(t *testing.T) {
	j, foundSeq := defectRecoveryFixture(t, t.TempDir())
	defer j.Close()
	for _, core := range []int{0, 1} {
		if _, err := j.Append(&journal.CommandReset{Core: new(core)}, foundSeq); err != nil {
			t.Fatal(err)
		}
	}
	for _, core := range []int{0, 1} {
		if _, err := j.Append(&journal.CorePhase{Core: core, To: journal.PhaseDone, Offset: -30, FailedMark: new(-31)}); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := j.Append(&journal.DefectAnswered{ID: 2, Cores: []int{0, 1}, Answer: "yes"}, foundSeq); err != nil {
		t.Fatal(err)
	}
	r := defectRecoveryRunner(j, nil)
	if stop, err := r.checkDefects(); err != nil || stop != nil {
		t.Fatalf("unfinished answer resume: %+v, %v", stop, err)
	}
	for _, core := range r.state.Cores {
		if core.Queued != "reset" {
			t.Fatalf("core %d reused an already consumed finding reset: %+v", core.Core, core)
		}
	}
	assertDefectAnswerResets(t, j.Events(), foundSeq)
}
