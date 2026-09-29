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
	cancel    context.CancelFunc
	marked    bool
	committed bool
}

func (j *stopAfterHuntCommit) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil {
		if p.Kind() == journal.KindMarkJoint {
			j.marked = true
		}
		if j.marked && p.Kind() == journal.KindProfileChange {
			j.committed = true
			j.cancel()
		}
	}
	return e, err
}

func TestRestoreAfterJointHuntCommitmentNeverReachesMark(t *testing.T) {
	t.Parallel()
	cfg := sim.Config{Seed: 4, Cores: 4, BIOS: []int{-40, -40, 0, 0}, Edges: make([]sim.Edges, 4), Ranking: []int{4, 3, 2, 1}, Joints: []sim.Joint{{Members: map[int]int{0: -30, 1: -30}, Regimes: []machine.Regime{machine.R7}, Rate: 1e6, Signal: machine.Crash}}}
	for core := range cfg.Edges {
		for i := range cfg.Edges[core].Isolated {
			cfg.Edges[core].Isolated[i] = -31
		}
		for i := range cfg.Edges[core].Resident {
			cfg.Edges[core].Resident[i] = -31
		}
	}
	model := sim.DefaultModel()
	model.CrashMCE = 0
	cfg.Model = &model
	in := simInput(t.TempDir(), newSim(t, cfg))
	in.Config.CandidateEdges = map[int]int{0: -30, 1: -30, 2: -30, 3: -30}
	in.Config.Durations.StartS = 1
	in.Config.Durations.GuardTrialS = 1
	in.Config.Durations.GuardAllCoreS = 4
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
	var mark *journal.MarkJoint
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.MarkJoint:
			mark = p
		case *journal.ProfileChange:
			if mark != nil {
				committed = true
			}
		}
	}
	if mark == nil || !committed || stop.Reason != StopSignal {
		t.Fatalf("mark %+v, committed %v, stop %+v", mark, committed, stop)
	}
	if !slices.Equal(mark.Members, []journal.JointMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}) {
		t.Fatalf("joint members %+v", mark.Members)
	}
	if !slices.ContainsFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindProfileRestored }) {
		t.Fatal("stop after hunt commitment did not restore the nonzero baseline")
	}
	regs := slices.Clone(cfg.BIOS)
	sawMark := false
	for _, e := range events {
		if e.Kind == journal.KindMarkJoint {
			sawMark = true
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
		if sawMark && regs[0] <= -30 && regs[1] <= -30 {
			t.Fatalf("readback after joint mark reaches mark: %v (event %d)", regs, e.Seq)
		}
	}
	for core := range cfg.BIOS {
		got, err := in.Machine.Seams().SMU.Offset(core)
		if err != nil || got != regs[core] {
			t.Fatalf("core %d %d (%v), journal readback %d", core, got, err, regs[core])
		}
	}
}
