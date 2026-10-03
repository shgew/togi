package trial

import (
	"context"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/backend/ycruncher"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

type outputSessionTrials struct {
	machine.Trials
	t           *testing.T
	runner      *Runner
	capFirst    bool
	configure   func(*running)
	starts      int
	trialEnded  bool
	writesAfter int
}

func (s *outputSessionTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	s.starts++
	started, err := s.runner.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	trial := started.(*running)
	if s.configure != nil {
		s.configure(trial)
		return trial, nil
	}
	trial.backend = ycruncher.New("")
	queueOutputConflict(s.t, trial, "Failed to set core affinity to core: 42", s.capFirst)
	return trial, nil
}

type outputSessionJournal struct {
	*journal.Journal
	trials *outputSessionTrials
}

func (j outputSessionJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	event, err := j.Journal.Append(p, cause...)
	if _, ok := p.(*journal.TrialEnd); ok && err == nil {
		j.trials.trialEnded = true
	}
	return event, err
}

type outputSessionSMU struct {
	machine.SMU
	trials *outputSessionTrials
}

func (s outputSessionSMU) SetOffset(core, offset int) error {
	if s.trials.trialEnded {
		s.trials.writesAfter++
	}
	return s.SMU.SetOffset(core, offset)
}

func (s outputSessionSMU) SetAllOffsets(offset int) error {
	if s.trials.trialEnded {
		s.trials.writesAfter++
	}
	return s.SMU.SetAllOffsets(offset)
}

func TestOutputLimitSessionContainmentDeadEnd(t *testing.T) {
	for _, order := range []string{"cap first", "affinity first"} {
		t.Run(order, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				m, err := sim.New(sim.Config{Seed: 3, Cores: 2})
				if err != nil {
					t.Fatal(err)
				}
				seams := m.Seams()
				o := fakeOptions(t, "sleep")
				r := New(o)
				r.host = &fakeHost{}
				trials := &outputSessionTrials{Trials: seams.Trials, t: t, runner: r, capFirst: order == "cap first"}
				seams.Trials = trials
				seams.SMU = outputSessionSMU{SMU: seams.SMU, trials: trials}
				boot, err := seams.Host.BootID()
				if err != nil {
					t.Fatal(err)
				}
				j, err := journal.Open(t.TempDir(), journal.Options{Boot: boot, Now: m.Now, Monotonic: m.Monotonic, Build: session.Build()})
				if err != nil {
					t.Fatal(err)
				}
				defer j.Close()
				stop, err := session.Run(context.Background(), session.Input{Config: config.Default(), Boot: boot, Journal: outputSessionJournal{Journal: j, trials: trials}, Machine: seams, Laps: 1})
				if err != nil || stop.Reason != session.StopDeadEnd || stop.DeadEnd == nil || stop.DeadEnd.Condition != journal.DeadEndContainment {
					t.Fatalf("escaped output stop reason=%s dead end=%+v err=%v", stop.Reason, stop.DeadEnd, err)
				}
				if trials.starts != 1 || trials.writesAfter != 0 {
					t.Fatalf("continued after escaped output: starts=%d profile writes=%d", trials.starts, trials.writesAfter)
				}
				var ends []*journal.TrialEnd
				for _, event := range j.Events() {
					if end, ok := event.Data.(*journal.TrialEnd); ok {
						ends = append(ends, end)
					}
				}
				if len(ends) != 1 || ends[0].Outcome != journal.OutcomeInconclusive || ends[0].ContainmentError != "" {
					t.Fatalf("escaped trial ends=%+v", ends)
				}
				if diff := cmp.Diff([]int{42}, ends[0].Escaped); diff != "" {
					t.Fatalf("journal escaped CPUs (-want +got):\n%s", diff)
				}
				t.Log("escaped42 retained in trial.end; containment dead end; one trial; zero subsequent profile writes")
			})
		})
	}
}

func TestWatchedBacklogSessionContainmentDeadEnd(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		m, err := sim.New(sim.Config{Seed: 3, Cores: 2})
		if err != nil {
			t.Fatal(err)
		}
		seams := m.Seams()
		o := fakeOptions(t, "watched")
		r := New(o)
		r.host = &outputHost{fakeHost: &fakeHost{}, text: strings.Repeat("ok\n", 1000), watch: true}
		trials := &outputSessionTrials{Trials: seams.Trials, t: t, runner: r, configure: func(trial *running) {
			trial.backend = watchClassifyingBackend{Backend: trial.backend, classify: func(line string) backend.Line {
				time.Sleep(time.Second)
				return classifyHelper(line)
			}}
		}}
		seams.Trials = trials
		seams.SMU = outputSessionSMU{SMU: seams.SMU, trials: trials}
		boot, err := seams.Host.BootID()
		if err != nil {
			t.Fatal(err)
		}
		j, err := journal.Open(t.TempDir(), journal.Options{Boot: boot, Now: m.Now, Monotonic: m.Monotonic, Build: session.Build()})
		if err != nil {
			t.Fatal(err)
		}
		defer j.Close()
		stop, err := session.Run(context.Background(), session.Input{Config: config.Default(), Boot: boot, Journal: outputSessionJournal{Journal: j, trials: trials}, Machine: seams, Laps: 1})
		if err != nil || stop.Reason != session.StopDeadEnd || stop.DeadEnd == nil || stop.DeadEnd.Condition != journal.DeadEndContainment {
			t.Fatalf("undrained watched output: stop=%s dead end=%+v err=%v", stop.Reason, stop.DeadEnd, err)
		}
		if trials.starts != 1 || trials.writesAfter != 0 {
			t.Fatalf("continued after watched drain deadline: starts=%d profile writes=%d", trials.starts, trials.writesAfter)
		}
		var ends []*journal.TrialEnd
		for _, event := range j.Events() {
			if end, ok := event.Data.(*journal.TrialEnd); ok {
				ends = append(ends, end)
			}
		}
		if len(ends) != 1 || ends[0].Outcome != journal.OutcomeInconclusive || ends[0].ContainmentError == "" {
			t.Fatalf("undrained watched output could pass: ends=%+v", ends)
		}
	})
}
