package watch

import (
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

// A rerun after a backoff names the step checking resumes at: the step of the next cycle trial the journal runs, which
// skips the steps whose chain endings the tuner records without a trial.
func TestRerunNamesTheStepCheckingResumesAt(t *testing.T) {
	t.Parallel()
	events := simulated(t, sessionJournal)
	intents := map[string]*journal.TrialIntent{}
	checked, skipped := 0, 0
	for i, e := range events {
		if p, ok := e.Data.(*journal.TrialIntent); ok {
			intents[p.Trial] = p
		}
		start, ok := e.Data.(*journal.TrialStart)
		if !ok {
			continue
		}
		in := intents[start.Trial]
		if in == nil || !in.Rerun || in.Phase != journal.PhaseChecking || in.Hunt > 0 {
			continue
		}
		next, chains, clean := nextCycleTrial(events[i+1:])
		if next == nil || !clean {
			continue
		}
		s := Project(events[:i+1])
		if s.cycle == nil {
			t.Fatalf("rerun %s has no cycle", in.Trial)
		}
		if next.Cycle != s.cycle.number {
			// Passing the reruns completes this cycle, so the step belongs to the next one and none is named.
			continue
		}
		checked++
		if chains {
			skipped++
		}
		step := stepOfTrial(t, events, next.Trial)
		if diff := cmp.Diff(step, s.cycle.resumeStep()+1); diff != "" {
			t.Errorf("rerun %s at seq %d: step named after the backoff differs from the next cycle trial %s (-next +named):\n%s", in.Trial, e.Seq, next.Trial, diff)
		}
		text := ansi.Strip(Render(s, 240, 67, e.Time))
		if want := fmt.Sprintf("waits at step %d until the rerun passes", step); !strings.Contains(text, want) {
			t.Errorf("rerun %s at seq %d: narration lacks %q", in.Trial, e.Seq, want)
		}
		if want := fmt.Sprintf("paused at step %d of", step); !strings.Contains(text, want) {
			t.Errorf("rerun %s at seq %d: stage line lacks %q", in.Trial, e.Seq, want)
		}
	}
	if checked == 0 || skipped == 0 {
		t.Fatalf("fixture must hold reruns whose next cycle trial skips steps with chain endings: %d reruns, %d with chain endings", checked, skipped)
	}
}

// stepOfTrial is the checking step a trial runs in, as the dashboard shows it while the trial is in flight; the journal
// leaves the step out of the intents of steps the tuner does not number.
func stepOfTrial(t *testing.T, events []journal.Event, trial string) int {
	t.Helper()
	for i, e := range events {
		if p, ok := e.Data.(*journal.TrialStart); ok && p.Trial == trial {
			if s := Project(events[:i+1]); s.trial != nil && s.trial.step > 0 {
				return s.trial.step
			}
		}
	}
	t.Fatalf("trial %s has no step in the journal", trial)
	return 0
}

// nextCycleTrial finds the first non-rerun checking cycle trial in the events, whether chain endings come before it, and
// whether the reruns before it all passed, which is when the journal agrees with the forecast of passing.
func nextCycleTrial(events []journal.Event) (next *journal.TrialIntent, chains, clean bool) {
	clean = true
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.CheckingChain:
			chains = true
		case *journal.Failure, *journal.DeadEnd, *journal.Shutdown:
			clean = false
		case *journal.TrialEnd:
			clean = clean && p.Outcome == journal.OutcomePass
		case *journal.TrialIntent:
			if p.Cycle > 0 && !p.Rerun && p.Hunt == 0 && p.Phase == journal.PhaseChecking {
				return p, chains, clean
			}
			if !p.Rerun {
				return nil, chains, false
			}
		}
	}
	return nil, chains, false
}

// A crash recovery names the step the crashed trial ran in, also after a backoff has moved the cycle on.
func TestRecoveryNamesTheStepTheCrashedTrialRanIn(t *testing.T) {
	t.Parallel()
	events := simulated(t, sessionJournal)
	intents := map[string]*journal.TrialIntent{}
	started := map[string]int{}
	checked, moved := 0, 0
	for i, e := range events {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			intents[p.Trial] = p
		case *journal.TrialStart:
			started[p.Trial] = i
		case *journal.TrialEnd:
			in := intents[p.Trial]
			if p.Signal != machine.Crash || in == nil || in.Cycle == 0 || in.Hunt > 0 || in.Round > 0 || in.Rerun {
				continue
			}
			ran := Project(events[:started[p.Trial]+1]).trial.step
			// The recovery shows until the next trial starts, after the backoff the crash caused.
			end := i
			for end+1 < len(events) {
				if _, ok := events[end+1].Data.(*journal.TrialIntent); ok {
					break
				}
				end++
			}
			s := Project(events[:end+1])
			if s.recover == nil || s.recover.trial == nil || s.recover.trial.id != p.Trial {
				continue
			}
			checked++
			if s.cycle != nil && s.cycle.current+1 != ran {
				moved++
			}
			if diff := cmp.Diff(ran, s.recover.trial.step); diff != "" {
				t.Errorf("recovery at seq %d: step of crashed trial %s (-ran +named):\n%s", events[end].Seq, p.Trial, diff)
			}
		}
	}
	if checked == 0 || moved == 0 {
		t.Fatalf("fixture must hold a crashed cycle trial whose cycle moved on: %d recoveries, %d moved", checked, moved)
	}
}

// cycleCompletedByRerun is a journal the real tuner writes for a two-core machine on cycle [R1, R6]: R1 passes, R6
// fails naming core 00, and the reruns the backoff owes pass, which completes cycle 1 before cycle 2's first trial.
// It returns the journal as it stands while each rerun runs, after the last rerun's end, and after the cycle's end.
func cycleCompletedByRerun(t *testing.T) (running [][]journal.Event, passed, ended []journal.Event) {
	t.Helper()
	cfg := config.Default()
	cfg.Checking.Cycle = []machine.Regime{machine.R1, machine.R6}
	state := tuner.New()
	var events []journal.Event
	add := func(p journal.Payload, cause ...int) journal.Event {
		e := journal.Event{Seq: len(events) + 1, Time: time.Unix(1000, 0).UTC().Add(time.Duration(len(events)) * time.Second), Boot: "boot", Kind: p.Kind(), Msg: p.Message(), Data: p, Cause: cause}
		events = append(events, e)
		state.Fold(e)
		return e
	}
	infos := []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{0, 2}}, {Core: 1, CCD: 1, CPUs: []int{1, 3}}}
	begin := add(&journal.SessionStart{Schema: journal.Schema, Session: "resume", Cores: infos})
	add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: journal.ConfigSnapshot{
		StartOffsets:        cfg.StartOffsets,
		CandidateSoloLimits: cfg.CandidateSoloLimits,
		Durations:           journal.ConfigDurations(cfg.Durations),
		Evidence:            journal.ConfigEvidence(cfg.Evidence),
		Checking:            journal.ConfigChecking(cfg.Checking),
		DeadEnds:            journal.ConfigDeadEnds(cfg.DeadEnds),
		Backends:            journal.ConfigBackends(cfg.Backends),
		BackendUser:         cfg.BackendUser,
	}})
	for _, info := range infos {
		add(&journal.CorePhase{Core: info.Core, To: journal.PhaseAtLimit, Offset: -20, Pass: new(-20), FailurePoint: new(-21), Reason: "test"}, begin.Seq)
	}
	trials := 0
	run := func(a tuner.Action) (intent, started journal.Event) {
		trials++
		profile := state.Profile()
		p, _, err := a.Trial.Complete(0, profile)
		if err != nil {
			t.Fatal(err)
		}
		p.Trial = fmt.Sprintf("%04d", trials)
		intent = add(p, a.Cause...)
		return intent, add(&journal.TrialStart{Trial: p.Trial})
	}
	failedR6 := false
	for range 200 {
		a := state.Next()
		switch {
		case a.Kind == tuner.Decide:
			e := add(a.Payload, a.Cause...)
			if c, ok := a.Payload.(*journal.CheckingCycle); ok && failedR6 && c.Event == journal.CycleEnd {
				if c.Cycle != 1 {
					t.Fatalf("fixture: cycle %d ended, want cycle 1", c.Cycle)
				}
				ended = slices.Clone(events[:e.Seq])
			}
		case a.Kind != tuner.RunTrial:
			t.Fatalf("fixture: unexpected action %+v", a)
		case a.Trial.Rerun:
			intent, _ := run(a)
			running = append(running, slices.Clone(events))
			add(&journal.TrialEnd{Trial: intent.Data.(*journal.TrialIntent).Trial, Outcome: journal.OutcomePass, DurationS: a.Trial.DurationS}, intent.Seq)
			passed = slices.Clone(events)
		case a.Trial.Cycle == 2:
			if len(running) == 0 || passed == nil || ended == nil {
				t.Fatalf("fixture: cycle 2 started without reruns completing cycle 1: %d reruns", len(running))
			}
			return running, passed, ended
		case a.Trial.Regime == machine.R6 && !failedR6:
			failedR6 = true
			intent, _ := run(a)
			add(&journal.TrialEnd{Trial: intent.Data.(*journal.TrialIntent).Trial, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0), DurationS: 41}, intent.Seq)
		case failedR6:
			t.Fatalf("fixture: cycle 1 ran %+v after its R6 failure instead of completing on the reruns", a.Trial)
		default:
			intent, _ := run(a)
			add(&journal.TrialEnd{Trial: intent.Data.(*journal.TrialIntent).Trial, Outcome: journal.OutcomePass, DurationS: a.Trial.DurationS}, intent.Seq)
		}
	}
	t.Fatal("fixture: the session never reached cycle 2")
	return nil, nil, nil
}

// stageLine is the dashboard's stage line: the first line that names the cycle.
func stageLine(t *testing.T, text string) string {
	t.Helper()
	for line := range strings.SplitSeq(text, "\n") {
		if strings.Contains(line, "CYCLE 1") {
			return strings.TrimSpace(line)
		}
	}
	t.Fatalf("no stage line names cycle 1:\n%s", text)
	return ""
}

// Passing a rerun's checks can complete the cycle, so that the tuner's next cycle trial is cycle 2's first. Cycle 1
// then waits at the step it has not finished, not at cycle 2's step 1, while the reruns run and between trials.
func TestRerunCompletingTheCycleNamesNoStepOfTheNext(t *testing.T) {
	t.Parallel()
	running, passed, ended := cycleCompletedByRerun(t)
	for i, events := range running {
		s := Project(events)
		text := ansi.Strip(Render(s, 240, 67, events[len(events)-1].Time))
		if line := stageLine(t, text); !strings.Contains(line, "paused at step 2 of 2") {
			t.Errorf("rerun %d: the stage line does not hold cycle 1 at its unfinished step 2: %q", i+1, line)
		}
		if !strings.Contains(text, "Cycle 1 waits at step 2 until the rerun passes.") {
			t.Errorf("rerun %d: the narration does not wait at step 2:\n%s", i+1, text)
		}
		if strings.Contains(text, "waits at step 1 ") {
			t.Errorf("rerun %d: the narration borrows cycle 2's step 1:\n%s", i+1, text)
		}
	}
	for name, events := range map[string][]journal.Event{"after the last rerun": passed, "after the cycle's end": ended} {
		text := ansi.Strip(Render(Project(events), 240, 67, events[len(events)-1].Time))
		if line := stageLine(t, text); !strings.Contains(line, "CYCLE 1  step 2 of 2") || strings.Contains(line, "step 1 of") {
			t.Errorf("%s: the stage line shows cycle 1 at another cycle's step: %q", name, line)
		}
	}
}
