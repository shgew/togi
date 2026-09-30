package session

import (
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/tuner"
)

type blockedProjection struct {
	*journal.Journal
	dir      string
	kind     journal.Kind
	blocked  bool
	previous []byte
	failure  error
}

func (j *blockedProjection) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err != nil || j.blocked || p.Kind() != j.kind {
		return e, err
	}
	j.previous, err = os.ReadFile(filepath.Join(j.dir, "state.json"))
	if err != nil {
		return e, err
	}
	if err := os.Remove(filepath.Join(j.dir, ".state.json.tmp")); err != nil && !errors.Is(err, os.ErrNotExist) {
		return e, err
	}
	err = os.Mkdir(filepath.Join(j.dir, ".state.json.tmp"), 0o755)
	j.blocked = err == nil
	return e, err
}

func (j *blockedProjection) WriteState(s journal.State) error {
	err := j.Journal.WriteState(s)
	if err != nil && j.failure == nil {
		j.failure = err
	}
	return err
}

func stateBoot(in simRun, wrap func(*journal.Journal) Journal) (stop Stop, err error) {
	seams := in.Machine.Seams()
	boot, err := seams.Host.BootID()
	if err != nil {
		return Stop{}, err
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Monotonic: seams.Clock.Monotonic, Sync: true, Build: Build()})
	if err != nil {
		return Stop{}, err
	}
	defer func() { err = errors.Join(err, j.Close()) }()
	return Run(context.Background(), Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: wrap(j), Machine: seams, Rotations: in.Rotations, Stderr: in.Stderr})
}

func TestStateWriteFailureWarnsContinuesAndRebuilds(t *testing.T) {
	t.Parallel()
	cfg := small()
	cfg.Model = &sim.Model{}
	m := newSim(t, cfg)
	dir := t.TempDir()
	in := simInput(dir, m)
	in.Config.CandidateEdges = map[int]int{0: -50, 1: -50}
	var stderr bytes.Buffer
	in.Stderr = &stderr
	var blocked *blockedProjection
	stop, err := stateBoot(in, func(j *journal.Journal) Journal {
		blocked = &blockedProjection{Journal: j, dir: dir, kind: journal.KindTrialEnd}
		return blocked
	})
	if err != nil || stop.Reason != StopRotations {
		t.Fatalf("projection failure stopped tuning: stop %+v, error %v", stop, err)
	}
	if !blocked.blocked || blocked.failure == nil {
		t.Fatal("state write failure was not exercised")
	}
	if got := stderr.String(); got != "" {
		t.Fatalf("projection failure invoked emergency cleanup: %s", got)
	}
	for core, want := range cfg.BIOS {
		got, err := m.Seams().SMU.Offset(core)
		if err != nil || got != want {
			t.Fatalf("core %d reads %d (%v), want ordinary restore %d", core, got, err, want)
		}
	}
	saved, err := os.ReadFile(filepath.Join(dir, "state.json"))
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(blocked.previous), string(saved)); diff != "" {
		t.Fatalf("failed atomic writes changed the visible state (-want +got):\n%s", diff)
	}
	events := readEvents(t, dir)
	failed := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd })
	if failed < 0 || events[failed].Data.(*journal.TrialEnd).Outcome != journal.OutcomePass {
		t.Fatal("authoritative passing trial was not durable")
	}
	warnings := 0
	for i := failed; i < len(events); i += 2 {
		if events[i].Kind == journal.KindSessionWarning || i+1 >= len(events) {
			t.Fatalf("event #%d does not have exactly one projection warning", events[i].Seq)
		}
		warning := events[i+1]
		if diff := cmp.Diff(&journal.SessionWarning{Operation: "write state projection", Error: blocked.failure.Error()}, warning.Data); diff != "" {
			t.Fatalf("event #%d projection warning (-want +got):\n%s", events[i].Seq, diff)
		}
		if diff := cmp.Diff([]int{events[i].Seq}, warning.Cause); diff != "" {
			t.Fatalf("warning did not cite its durable event: %s", diff)
		}
		warnings++
	}
	if events[len(events)-2].Kind != journal.KindShutdown {
		t.Fatal("projection failures did not continue through clean shutdown")
	}
	if err := os.Remove(filepath.Join(dir, ".state.json.tmp")); err != nil {
		t.Fatal(err)
	}
	m.Reboot()
	stop, err = stateBoot(in, func(j *journal.Journal) Journal { return j })
	if err != nil || stop.Reason != StopRotations {
		t.Fatalf("next start: stop %+v, error %v", stop, err)
	}
	resumed := readEvents(t, dir)
	rebuilt := resumed[len(events)]
	if rebuilt.Kind != journal.KindStateRebuilt || !slices.Contains(rebuilt.Data.(*journal.StateRebuilt).Fields, "last_seq") {
		t.Fatalf("next start did not record the stale projection rebuild: %+v", rebuilt)
	}
	for _, e := range resumed[len(events):] {
		if e.Kind == journal.KindCrashDetected {
			t.Fatal("warning after durable shutdown turned a clean boot into a crash")
		}
	}
	state, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	var replay journal.State
	decisions := tuner.New()
	journal.Replay(resumed, &replay, decisions)
	decisions.Project(&replay)
	if diff := journal.DiffFields(replay, state); len(diff) != 0 {
		t.Fatalf("rebuilt state differs from authoritative replay in %v", diff)
	}
	t.Logf("durable trial #%d; %d projection warnings; continued to %s without emergency zeroing; next start recorded %s and matched replay", events[failed].Seq, warnings, stop.Reason, rebuilt.Kind)
}

func TestProjectionWarningAppendFailureIsFatal(t *testing.T) {
	for _, startup := range []bool{false, true} {
		name := "after trial start"
		if startup {
			name = "startup rebuild"
		}
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			cfg := small()
			cfg.Model = &sim.Model{}
			m := newSim(t, cfg)
			in := simInput(t.TempDir(), m)
			in.Config.CandidateEdges = map[int]int{0: -50, 1: -50}
			var before []journal.Event
			if startup {
				simulate(t, in)
				before = readEvents(t, in.Dir)
				m.Reboot()
			}
			var stderr bytes.Buffer
			in.Stderr = &stderr
			var faulty *failAppendJournal
			stop, err := stateBoot(in, func(j *journal.Journal) Journal {
				var projection Journal = &blockedProjection{Journal: j, dir: in.Dir, kind: journal.KindTrialStart}
				if startup {
					projection = unwritableState{j}
				}
				faulty = &failAppendJournal{Journal: projection, kind: journal.KindSessionWarning}
				return faulty
			})
			if !faulty.failed || !errors.Is(err, io.ErrClosedPipe) || stop.Reason != "" {
				t.Fatalf("warning append failure was not fatal: stop %+v, error %v", stop, err)
			}
			want := "togi: journal write failed: io: read/write on closed pipe; every core set to CO 0 without an intent (readback all 0)\n"
			if diff := cmp.Diff(want, stderr.String()); diff != "" {
				t.Fatalf("fatal journal failure report (-want +got):\n%s", diff)
			}
			for core := range cfg.BIOS {
				offset, readErr := m.Seams().SMU.Offset(core)
				if readErr != nil || offset != 0 {
					t.Fatalf("core %d reads %d (%v), want emergency zero", core, offset, readErr)
				}
			}
			events := readEvents(t, in.Dir)
			if startup {
				if diff := cmp.Diff(before, events); diff != "" {
					t.Fatalf("event appended after journal failure: %s", diff)
				}
			} else if events[len(events)-1].Kind != journal.KindTrialStart {
				t.Fatalf("authoritative start lost or event appended after journal failure: %s", events[len(events)-1].Kind)
			}
		})
	}
}

func TestStateRebuildWriteFailureWarnsAndContinues(t *testing.T) {
	t.Parallel()
	cfg := small()
	cfg.Model = &sim.Model{}
	m := newSim(t, cfg)
	in := simInput(t.TempDir(), m)
	in.Config.CandidateEdges = map[int]int{0: -50, 1: -50}
	stop, err := stateBoot(in, func(j *journal.Journal) Journal { return j })
	if err != nil || stop.Reason != StopRotations {
		t.Fatalf("initial run: stop %+v, error %v", stop, err)
	}
	before := readEvents(t, in.Dir)
	if err := os.Remove(filepath.Join(in.Dir, "state.json")); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(filepath.Join(in.Dir, ".state.json.tmp"), 0o755); err != nil {
		t.Fatal(err)
	}
	m.Reboot()
	var stderr bytes.Buffer
	in.Stderr = &stderr
	var blocked *blockedProjection
	stop, err = stateBoot(in, func(j *journal.Journal) Journal {
		blocked = &blockedProjection{Journal: j, dir: in.Dir, blocked: true}
		return blocked
	})
	if err != nil || stop.Reason != StopRotations {
		t.Fatalf("startup projection failure stopped tuning: stop %+v, error %v", stop, err)
	}
	resumed := readEvents(t, in.Dir)[len(before):]
	if diff := cmp.Diff(&journal.SessionWarning{Operation: "write state projection", Error: blocked.failure.Error()}, resumed[0].Data); diff != "" {
		t.Fatalf("startup rebuild did not warn: %s", diff)
	}
	for _, e := range resumed {
		if e.Kind == journal.KindStateRebuilt || e.Kind == journal.KindCrashDetected {
			t.Fatalf("failed rebuild or clean boot misreported as %s", e.Kind)
		}
	}
	if got := stderr.String(); got != "" {
		t.Fatalf("startup projection failure invoked emergency cleanup: %s", got)
	}
	t.Log("failed startup rebuild warned and continued through clean shutdown")
}
