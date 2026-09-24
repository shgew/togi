package journal

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestInterruptedStateWriteKeepsPrevious(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	j := openTest(t, dir)
	a := State{Schema: Schema, LastSeq: 4, Phase: "per_core", Cores: []CoreState{{Core: 0, CPUs: []int{0, 16}, Offset: -5}}}
	b := State{Schema: Schema, LastSeq: 9, Phase: "guard"}
	if err := j.WriteState(a); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, stateTmpFile), []byte(`{"schema":1,"last_se`), 0o644); err != nil {
		t.Fatal(err)
	}
	got, err := ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if d := DiffFields(got, a); d != nil {
		t.Fatalf("after interrupted write, state differs in %v", d)
	}
	if err := j.WriteState(b); err != nil {
		t.Fatal(err)
	}
	got, err = ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if d := DiffFields(got, b); d != nil {
		t.Fatalf("after second write, state differs in %v", d)
	}
}

func TestDiffFields(t *testing.T) {
	t.Parallel()
	a := State{Schema: Schema, LastSeq: 4, Cores: []CoreState{{Core: 0, Offset: -5}}}
	if d := DiffFields(a, a); d != nil {
		t.Fatalf("equal states differ in %v", d)
	}
	b := a
	b.LastSeq = 5
	b.Cores = []CoreState{{Core: 0, Offset: -6}}
	if d := DiffFields(a, b); !reflect.DeepEqual(d, []string{"cores", "last_seq"}) {
		t.Fatalf("DiffFields = %v, want [cores last_seq]", d)
	}
}

func TestInFlightTracksUnmatchedIntents(t *testing.T) {
	t.Parallel()
	var s State
	events := []Event{
		{Seq: 1, Kind: KindSessionStart, Boot: "a", Data: sessionStart()},
		{Seq: 2, Kind: KindTrialIntent, Boot: "a", Msg: "trial 0001", Data: &TrialIntent{Trial: "0001"}},
		{Seq: 3, Kind: KindSMUIntent, Boot: "a", Msg: "smu", Data: &SMUIntent{Op: SMUSet, Core: new(0), Offset: -5}},
	}
	Replay(events, &s)
	if s.InFlight == nil || s.InFlight.Seq != 3 {
		t.Fatalf("in flight = %+v, want seq 3", s.InFlight)
	}
	s.Fold(Event{Seq: 4, Kind: KindSMUWrite, Boot: "a", Data: &SMUWrite{Op: SMUSet, Core: new(0), Offset: -5}})
	if s.InFlight == nil || s.InFlight.Seq != 2 {
		t.Fatalf("in flight = %+v, want the trial intent", s.InFlight)
	}
	s.Fold(Event{Seq: 5, Kind: KindCrashDetected, Boot: "b", Data: &CrashDetected{PreviousBoot: "a"}})
	if s.InFlight != nil {
		t.Fatalf("in flight = %+v after crash.detected, want nil", s.InFlight)
	}
}
