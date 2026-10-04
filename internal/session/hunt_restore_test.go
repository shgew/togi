package session

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

type stopAfterHuntCommit struct {
	Journal
	cancel              context.CancelFunc
	combinationRecorded bool
	committed           bool
}

func (j *stopAfterHuntCommit) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil {
		if p.Kind() == journal.KindCombination {
			j.combinationRecorded = true
		}
		if j.combinationRecorded && p.Kind() == journal.KindProfileChange {
			j.committed = true
			j.cancel()
		}
	}
	return e, err
}

func TestRestoreAfterCombinationHuntCommitmentNeverReachesCombination(t *testing.T) {
	t.Parallel()
	cfg := sim.Config{Seed: 4, Cores: 4, BIOS: []int{-40, -40, 0, 0}, Limits: make([]sim.Limits, 4), Ranking: []int{4, 3, 2, 1}, Joints: []sim.Joint{{Members: map[int]int{0: -30, 1: -30}, Regimes: []machine.Regime{machine.R7}, Rate: 1e6, Signal: machine.Crash}}}
	for core := range cfg.Limits {
		for i := range cfg.Limits[core].Alone {
			cfg.Limits[core].Alone[i] = -31
		}
		for i := range cfg.Limits[core].Together {
			cfg.Limits[core].Together[i] = -31
		}
	}
	model := sim.DefaultModel()
	model.CrashMCE = 0
	cfg.Model = &model
	in := simInput(t.TempDir(), newSim(t, cfg))
	in.Config.CandidateSoloLimits = map[int]int{0: -30, 1: -30, 2: -30, 3: -30}
	in.Config.Durations.ShortTrialS = 1
	in.Config.Durations.CheckingTrialS = 1
	in.Config.Durations.CheckingAllCoreS = 4
	in.Config.Evidence.Rate = 0.95
	in.Config.Evidence.Miss = 0.2
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	committed := false
	var stop Stop
	for range maxSimulatedBoots {
		result, err := simulateBoot(ctx, in, func(j *journal.Journal) Journal {
			return &stopAfterHuntCommit{Journal: wrapFor(in, nil)(j), cancel: cancel}
		})
		if errors.Is(err, machine.ErrCrashed) {
			in.Machine.Reboot()
			continue
		}
		if err != nil {
			t.Fatalf("hunt run: %v", err)
		}
		stop = result
		break
	}
	events := readEvents(t, in.Dir)
	var combination *journal.Combination
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.Combination:
			combination = p
		case *journal.ProfileChange:
			if combination != nil {
				committed = true
			}
		}
	}
	if combination == nil || !committed || stop.Reason != StopSignal {
		t.Fatalf("combination %+v, committed %v, stop %+v", combination, committed, stop)
	}
	if !slices.Equal(combination.Members, []journal.CombinationMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}) {
		t.Fatalf("combination members %+v", combination.Members)
	}
	regs := slices.Clone(cfg.BIOS)
	sawCombination := false
	for _, e := range events {
		if e.Kind == journal.KindCombination {
			sawCombination = true
		}
		p, ok := e.Data.(*journal.SMUReadback)
		if !ok {
			continue
		}
		if p.Core < 0 {
			for i := range regs {
				regs[i] = p.Offset
			}
		} else {
			regs[p.Core] = p.Offset
		}
		if sawCombination && regs[0] <= -30 && regs[1] <= -30 {
			t.Fatalf("readback after combination reaches combination: %v (event %d)", regs, e.Seq)
		}
	}
	if want := []int{-30, -29, 0, 0}; !slices.Equal(regs, want) {
		t.Fatalf("registers after stop %v, want each core's baseline or its shallower profile offset %v", regs, want)
	}
	for core := range cfg.BIOS {
		got, err := in.Machine.Seams().SMU.Offset(core)
		if err != nil || got != regs[core] {
			t.Fatalf("core %d %d (%v), journal readback %d", core, got, err, regs[core])
		}
	}
}
