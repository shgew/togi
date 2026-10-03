package session

import (
	"errors"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/sim"
)

// command runs f on the journal in dir as a separate command process would, one second after the last event.
func command(t *testing.T, dir string, f func(*journal.Journal) error) error {
	t.Helper()
	at := lastEvent(t, dir).Time.Add(time.Second)
	j, err := journal.Open(dir, journal.Options{Boot: "command-boot", Now: func() time.Time { return at }})
	if err != nil {
		t.Fatal(err)
	}
	err = f(j)
	if cerr := j.Close(); cerr != nil {
		t.Fatal(cerr)
	}
	return err
}

func memJournal(in simRun, j *journal.Journal) Journal {
	return wrapFor(in, nil)(j)
}

func resumed(t *testing.T, dir string, cfg sim.Config) *sim.Machine {
	t.Helper()
	cfg, err := sim.Resume(dir, cfg)
	if err != nil {
		t.Fatal(err)
	}
	return newSim(t, cfg)
}

func coreState(t *testing.T, in simRun, core int) journal.CoreState {
	t.Helper()
	st, err := readMemState(in.state)
	if err != nil {
		t.Fatal(err)
	}
	return st.Cores[core]
}

func TestResetCore(t *testing.T) {
	t.Parallel()
	in, ref := referenceRun(t, small())
	dir := in.Dir
	if err := command(t, dir, func(j *journal.Journal) error { return ResetCore(memJournal(in, j), 1) }); err != nil {
		t.Fatal(err)
	}
	m := resumed(t, dir, small())
	in.Machine = m
	if stop := simulate(t, in); stop.Reason != StopLaps {
		t.Fatalf("stopped with %+v", stop)
	}
	events := readEvents(t, dir)
	reset := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindCommandReset })
	if reset < len(ref) || events[reset+1].Kind != journal.KindShutdown {
		t.Fatalf("command.reset at index %d, not after the reference run and followed by shutdown", reset)
	}
	restart := slices.IndexFunc(events, func(e journal.Event) bool {
		p, ok := e.Data.(*journal.CorePhase)
		return ok && p.Core == 1 && p.From == journal.PhaseAtLimit && p.To == journal.PhaseSearch && slices.Equal(e.Cause, []int{events[reset].Seq})
	})
	if restart < 0 {
		t.Fatal("no at its limit -> search for core 1 citing command.reset")
	}
	if c := coreState(t, in, 1); c.Phase != journal.PhaseAtLimit || c.Offset != m.AloneLimit(1) {
		t.Fatalf("core 1 %+v, want at its limit again at its solo limit %d", c, m.AloneLimit(1))
	}
	if st, _ := readMemState(in.state); st.Checking == nil || st.Checking.CleanLaps == 0 {
		t.Fatalf("checking %+v, want a clean lap again", st.Checking)
	}
}

func TestResetAll(t *testing.T) {
	t.Parallel()
	dir, ref := reference(t, small())
	old := ref[0].Data.(*journal.SessionStart).Session
	var path string
	if err := command(t, dir, func(j *journal.Journal) (err error) {
		path, err = ResetAll(j)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	archived, _, err := journal.ReadFile(filepath.Join(dir, path))
	if err != nil {
		t.Fatal(err)
	}
	if p, ok := archived[len(archived)-1].Data.(*journal.SessionArchived); !ok || p.Session != old || path != filepath.Join("archive", old+".jsonl") {
		t.Fatalf("archive %s ends with %s", path, archived[len(archived)-1].Msg)
	}
	if stop := simulate(t, simInput(dir, resumed(t, dir, small()))); stop.Reason != StopLaps {
		t.Fatalf("stopped with %+v", stop)
	}
	if s := readEvents(t, dir)[0].Data.(*journal.SessionStart).Session; s == old {
		t.Fatalf("the next run reused session %s", s)
	}
}

func TestResetAllRefusesAnExistingArchive(t *testing.T) {
	t.Parallel()
	dir, ref := reference(t, small())
	taken := filepath.Join(dir, "archive", ref[0].Data.(*journal.SessionStart).Session+".jsonl")
	if err := os.MkdirAll(filepath.Dir(taken), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(taken, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	err := command(t, dir, func(j *journal.Journal) error {
		_, err := ResetAll(j)
		return err
	})
	if err == nil || !strings.Contains(err.Error(), "already exists") {
		t.Fatalf("ResetAll over an existing archive: %v", err)
	}
	if got := readEvents(t, dir); len(got) != len(ref) {
		t.Fatalf("the refused reset left %d events, want the %d it found", len(got), len(ref))
	}
}

func TestResetRefusalsLeaveJournalUnchanged(t *testing.T) {
	t.Parallel()
	for _, name := range []string{"empty core", "empty all", "incompatible", "unknown core"} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			m := newSim(t, small())
			j, err := journal.Open(t.TempDir(), journal.Options{Boot: "command", Now: m.Now, Build: Build()})
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			if name == "incompatible" || name == "unknown core" {
				build := Build()
				if name == "incompatible" {
					build.Ruleset++
				}
				cores, err := m.Seams().Host.Topology()
				if err != nil {
					t.Fatal(err)
				}
				if _, err := j.Append(&journal.SessionStart{Build: build, Session: "reset", Cores: cores}); err != nil {
					t.Fatal(err)
				}
			}
			before := j.Events()
			if name == "empty all" {
				_, err = ResetAll(j)
			} else {
				err = ResetCore(j, 99)
			}
			switch name {
			case "empty core", "empty all":
				if !errors.Is(err, ErrNoSession) {
					t.Fatalf("reset: %v, want no session", err)
				}
			case "unknown core":
				if !errors.Is(err, ErrNoSuchCore) || !strings.Contains(err.Error(), "reset core 99") {
					t.Fatalf("reset: %v, want identified unknown core", err)
				}
			case "incompatible":
				var incompatible *journal.IncompatibleError
				if !errors.As(err, &incompatible) || incompatible.Field != "ruleset" {
					t.Fatalf("reset: %v, want ruleset refusal", err)
				}
			}
			if diff := cmp.Diff(before, j.Events()); diff != "" {
				t.Fatalf("refused command changed journal (-before +after):\n%s", diff)
			}
		})
	}
}

func TestResetCoreAppendFailurePreservesDurablePrefix(t *testing.T) {
	t.Parallel()
	for _, kind := range []journal.Kind{journal.KindCommandReset, journal.KindShutdown} {
		t.Run(string(kind), func(t *testing.T) {
			t.Parallel()
			r, _, closeJournal := checkedRunner(t, []int{0, 0})
			defer closeJournal()
			before := r.in.Journal.Events()
			j := &failAppendJournal{Journal: r.in.Journal, kind: kind}
			err := ResetCore(j, 1)
			if !errors.Is(err, io.ErrClosedPipe) || !strings.Contains(err.Error(), "queue reset of core 1") {
				t.Fatalf("reset append failure: %v", err)
			}
			events := j.Events()
			if diff := cmp.Diff(before, events[:len(before)]); diff != "" {
				t.Fatal(diff)
			}
			want := 0
			if kind == journal.KindShutdown {
				want = 1
			}
			if len(events)-len(before) != want {
				t.Fatalf("durable suffix has %d events, want %d", len(events)-len(before), want)
			}
			if want == 1 {
				if diff := cmp.Diff(&journal.CommandReset{Core: new(1)}, events[len(before)].Data); diff != "" {
					t.Fatalf("durable reset (-want +got):\n%s", diff)
				}
				if replay, err := replayFor(r.in.Journal); err != nil || replay.state.Cores[1].Queued != "reset" {
					t.Fatalf("durable reset lost on replay: %+v, %v", replay.state.Cores, err)
				}
			}
		})
	}
}
