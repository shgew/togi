package session

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/shgew/shycler/internal/journal"
	"github.com/shgew/shycler/internal/sim"
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

func memJournal(dir string, j *journal.Journal) Journal {
	return wrapFor(simRun{Dir: dir}, nil)(j)
}

func resumed(t *testing.T, dir string, cfg sim.Config) *sim.Machine {
	t.Helper()
	cfg, err := sim.Resume(dir, cfg)
	if err != nil {
		t.Fatal(err)
	}
	return newSim(t, cfg)
}

func coreState(t *testing.T, dir string, core int) journal.CoreState {
	t.Helper()
	st, err := readMemState(stateOf(dir))
	if err != nil {
		t.Fatal(err)
	}
	return st.Cores[core]
}

func TestResetCore(t *testing.T) {
	t.Parallel()
	dir, ref := reference(t, small())
	if err := command(t, dir, func(j *journal.Journal) error { return ResetCore(memJournal(dir, j), 1) }); err != nil {
		t.Fatal(err)
	}
	m := resumed(t, dir, small())
	if stop := simulate(t, simInput(dir, m)); stop.Reason != StopRotations {
		t.Fatalf("stopped with %+v", stop)
	}
	events := readEvents(t, dir)
	reset := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindCommandReset })
	if reset < len(ref) || events[reset+1].Kind != journal.KindShutdown {
		t.Fatalf("command.reset at index %d, not after the reference run and followed by shutdown", reset)
	}
	restart := slices.IndexFunc(events, func(e journal.Event) bool {
		p, ok := e.Data.(*journal.CorePhase)
		return ok && p.Core == 1 && p.From == journal.PhaseConfirmed && p.To == journal.PhaseSearch && slices.Equal(e.Cause, []int{events[reset].Seq})
	})
	if restart < 0 {
		t.Fatal("no confirmed -> search for core 1 citing command.reset")
	}
	if c := coreState(t, dir, 1); c.Phase != journal.PhaseConfirmed || c.Offset != m.IsolatedEdge(1) {
		t.Fatalf("core 1 %+v, want confirmed again at its edge %d", c, m.IsolatedEdge(1))
	}
	if st, _ := readMemState(stateOf(dir)); st.Tier != journal.TierBronze {
		t.Fatalf("tier %s, want bronze again", st.Tier)
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
	states.Delete(dir)
	if stop := simulate(t, simInput(dir, resumed(t, dir, small()))); stop.Reason != StopRotations {
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
