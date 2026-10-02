package journal

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
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
	if diff := cmp.Diff([]string{"cores", "last_seq"}, DiffFields(a, b)); diff != "" {
		t.Fatalf("DiffFields mismatch (-want +got):\n%s", diff)
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

func TestReadStateDamageAndMissing(t *testing.T) {
	dir := t.TempDir()
	if _, err := ReadState(dir); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("missing state error: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, stateFile), []byte(`{"last_seq":`), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := ReadState(dir); err == nil || !strings.Contains(err.Error(), "read state") {
		t.Fatalf("damaged state accepted: %v", err)
	}
}

func TestStateDecisionsAndDeadEndResume(t *testing.T) {
	var s State
	s.Fold(Event{Seq: 1, Kind: KindSessionStart, Data: sessionStart()})
	s.Fold(Event{Seq: 2, Kind: KindTunerDecision, Msg: "failed", Data: &TunerDecision{Core: 7, Phase: PhaseSearch, Decision: Backoff, FromOffset: -1, ToOffset: 0, FailedMark: new(-1), Reason: "failure"}})
	if diff := cmp.Diff(&DecisionRef{Seq: 2, Msg: "failed"}, s.Cores[1].LastDecision); diff != "" {
		t.Fatal(diff)
	}
	s.Fold(Event{Seq: 3, Kind: KindDeadEnd, Msg: "failed at zero", Data: &DeadEnd{Core: new(7), Condition: DeadEndFailureAtZero, Detail: "core 07 failed at CO 0", Action: ActionExit}})
	if diff := cmp.Diff(&DecisionRef{Seq: 3, Msg: "failed at zero"}, s.Cores[1].LastDecision); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(&DeadEndRef{Condition: DeadEndFailureAtZero, Seq: 3}, s.DeadEnd); diff != "" {
		t.Fatal(diff)
	}
	s.Fold(Event{Seq: 4, Kind: KindConfigLoaded, Data: &ConfigLoaded{Config: sampleConfig()}})
	if s.DeadEnd != nil || s.LastSeq != 4 {
		t.Fatalf("resume retained dead end: %+v", s)
	}
}

func TestInvalidStateTimestampKeepsPreviousProjection(t *testing.T) {
	dir := t.TempDir()
	j := openTest(t, dir)
	appendAll(t, j, []Payload{sessionStart()})
	var previous State
	Replay(j.Events(), &previous)
	if err := j.WriteState(previous); err != nil {
		t.Fatal(err)
	}
	bad := previous
	session := *previous.Session
	session.Start = time.Date(10000, 1, 1, 0, 0, 0, 0, time.UTC)
	bad.Session = &session
	if err := j.WriteState(bad); err == nil {
		t.Fatal("invalid projection timestamp accepted")
	}
	got, err := ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(previous.LastSeq, got.LastSeq); diff != "" {
		t.Fatalf("failed encoding replaced previous sequence: %s", diff)
	}
	if diff := cmp.Diff(previous.Session, got.Session); diff != "" {
		t.Fatalf("failed encoding replaced previous session: %s", diff)
	}
}

func TestStateCloseFailureKeepsPreviousProjection(t *testing.T) {
	dir := t.TempDir()
	j := openTest(t, dir)
	appendAll(t, j, []Payload{sessionStart()})
	var previous State
	Replay(j.Events(), &previous)
	if err := j.WriteState(previous); err != nil {
		t.Fatal(err)
	}
	appendAll(t, j, []Payload{&SessionBaseline{Offsets: []int{-5, 0}}})
	var canonical State
	Replay(j.Events(), &canonical)
	j.fs = failingJournalFilesystem{journalFilesystem: j.fs, closeErr: errJournalFilesystem}
	if err := j.WriteState(canonical); !errors.Is(err, errJournalFilesystem) {
		t.Fatalf("state close failure: %v", err)
	}
	stored, err := ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(previous.LastSeq, stored.LastSeq); diff != "" {
		t.Fatalf("unconfirmed temp file replaced sequence: %s", diff)
	}
	if diff := cmp.Diff(previous.Session, stored.Session); diff != "" {
		t.Fatalf("unconfirmed temp file replaced session: %s", diff)
	}
}
