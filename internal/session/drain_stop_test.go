package session

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

type cancelAtDrainBoundary struct {
	Journal
	cancel context.CancelFunc
	match  func(journal.Payload) bool
	seq    *int
}

func (j *cancelAtDrainBoundary) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil && *j.seq == 0 && j.match(p) {
		*j.seq = e.Seq
		j.cancel()
	}
	return e, err
}

func TestStopDrainsDurableFailureBoundaries(t *testing.T) {
	t.Parallel()
	for _, boundary := range []string{"search_trial_end", "search_failure", "together_trial_end", "together_failure", "unattributed_trial_end", "unattributed_failure", "zero_trial_end", "zero_failure", "hunt_end", "combination"} {
		t.Run(boundary, func(t *testing.T) {
			t.Parallel()
			zero := boundary == "zero_trial_end" || boundary == "zero_failure"
			unattributed := boundary == "unattributed_trial_end" || boundary == "unattributed_failure" || zero
			combination := boundary == "hunt_end" || boundary == "combination"
			cfg := sim.Config{Seed: 4, Cores: 4, BIOS: []int{-40, -40, 0, 0}, Limits: make([]sim.Limits, 4), Ranking: []int{4, 3, 2, 1}}
			if zero {
				cfg.BIOS = make([]int, cfg.Cores)
			}
			for core := range cfg.Limits {
				for i := range cfg.Limits[core].Alone {
					cfg.Limits[core].Alone[i] = -31
				}
				for i := range cfg.Limits[core].Together {
					cfg.Limits[core].Together[i] = -31
				}
			}
			if combination || unattributed {
				cfg.Joints = []sim.Joint{{Members: map[int]int{0: -30, 1: -30}, Regimes: []machine.Regime{machine.R6}, Rate: 1e6, Signal: machine.CorrectedMCE}}
				if zero {
					cfg.Joints[0].Members = map[int]int{0: 0, 1: 0}
				}
				model := sim.DefaultModel()
				model.CoreLocalBank = 0
				cfg.Model = &model
			} else if boundary == "together_trial_end" || boundary == "together_failure" {
				for i := range cfg.Limits[0].Together {
					cfg.Limits[0].Together[i] = -20
				}
			}
			in := simInput(t.TempDir(), newSim(t, cfg))
			in.Config.CandidateSoloLimits = map[int]int{0: -30, 1: -30, 2: -30, 3: -30}
			if combination || unattributed {
				in.Config.Checking.Cycle = []machine.Regime{machine.R6}
				if !zero {
					for core := range in.Config.CandidateSoloLimits {
						in.Config.CandidateSoloLimits[core] = -31
					}
				}
			}
			bootloader := &fakeBootloader{}
			if zero {
				in.Bootloader = bootloader
				for core := range in.Config.CandidateSoloLimits {
					in.Config.CandidateSoloLimits[core] = 0
				}
			}
			if boundary == "search_trial_end" || boundary == "search_failure" {
				in.Config.CandidateSoloLimits[0] = -40
			}
			in.Config.Durations.SearchTrialS = 1
			in.Config.Durations.ShortTrialS = 1
			in.Config.Durations.CheckingTrialS = 1
			in.Config.Durations.CheckingAllCoreS = 4
			in.Config.Evidence.Rate = 0.95
			in.Config.Evidence.Miss = 0.2
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			seq := 0
			match := func(p journal.Payload) bool {
				switch p := p.(type) {
				case *journal.TrialEnd:
					return p.Outcome == journal.OutcomeFailure && (boundary == "search_trial_end" || boundary == "together_trial_end" || boundary == "unattributed_trial_end" || boundary == "zero_trial_end")
				case *journal.Failure:
					return (p.Attribution == journal.Attributed && (boundary == "search_failure" || boundary == "together_failure")) || (p.Attribution == journal.Unattributed && (boundary == "unattributed_failure" || boundary == "zero_failure"))
				case *journal.HuntEnd:
					return boundary == "hunt_end" && p.Result == "combination"
				case *journal.Combination:
					return boundary == "combination"
				}
				return false
			}
			var stop Stop
			for range maxSimulatedBoots {
				result, err := simulateBoot(ctx, in, func(j *journal.Journal) Journal {
					return &cancelAtDrainBoundary{Journal: wrapFor(in, nil)(j), cancel: cancel, match: match, seq: &seq}
				})
				if errors.Is(err, machine.ErrCrashed) {
					in.Machine.Reboot()
					continue
				}
				if err != nil {
					t.Fatal(err)
				}
				stop = result
				break
			}
			wantStop := StopSignal
			if zero {
				wantStop = StopDeadEnd
			}
			if seq == 0 || stop.Reason != wantStop {
				t.Fatalf("boundary %d, stop %+v", seq, stop)
			}
			if zero && (stop.DeadEnd == nil || stop.DeadEnd.Condition != journal.DeadEndFailureAtZero || bootloader.calls != 1) {
				t.Fatalf("all-zero failure lost its dead end or boot cleanup: stop %+v, clears %d", stop, bootloader.calls)
			}
			events := readEvents(t, in.Dir)
			var restored []int
			backoffs := 0
			for _, e := range events {
				if e.Seq <= seq {
					continue
				}
				switch p := e.Data.(type) {
				case *journal.TrialIntent, *journal.HuntStart, *journal.HuntGroup, *journal.DeepeningRound, *journal.HostRanking:
					t.Fatalf("new work during drain: %+v", e)
				case *journal.TunerDecision:
					if p.Decision != journal.Backoff && p.Decision != journal.CheckSoloLimit {
						t.Fatalf("new move during drain: %+v", p)
					}
					backoffs++
				case *journal.ProfileRestored:
					restored = p.Offsets
				}
			}
			wantBackoffs := 1
			if unattributed {
				wantBackoffs = 0
			}
			if diff := cmp.Diff(wantBackoffs, backoffs); diff != "" {
				t.Fatalf("drained backoffs (-want +got):\n%s", diff)
			}
			if combination && restored == nil {
				restored = []int{-30, -29, 0, 0}
			}
			if zero && restored == nil {
				restored = make([]int, cfg.Cores)
			}
			if len(restored) != cfg.Cores {
				t.Fatalf("restored profile %v", restored)
			}
			for _, e := range events {
				switch p := e.Data.(type) {
				case *journal.Failure:
					if p.Attribution == journal.Attributed && p.Core != nil && p.Offset != nil && restored[*p.Core] <= *p.Offset {
						t.Fatalf("restore %v reaches failure %+v", restored, p)
					}
				case *journal.Combination:
					reaches := true
					for _, m := range p.Members {
						reaches = reaches && restored[m.Core] <= m.Offset
					}
					if reaches {
						t.Fatalf("restore %v reaches combination %+v", restored, p)
					}
				}
			}
			actual := slices.Clone(restored)
			for core := range actual {
				o, err := in.Machine.Seams().SMU.Offset(core)
				if err != nil {
					t.Fatal(err)
				}
				actual[core] = o
			}
			if diff := cmp.Diff(restored, actual); diff != "" {
				t.Fatalf("restore readback (-want +got):\n%s", diff)
			}
		})
	}
}
