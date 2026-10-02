package journal

import (
	"bytes"
	"encoding/hex"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func fixedClock() func() time.Time {
	t := time.Date(2026, 10, 2, 1, 14, 7, 120000000, time.UTC)
	return func() time.Time {
		t = t.Add(time.Second)
		return t
	}
}

func openTest(tb testing.TB, dir string) *Journal {
	tb.Helper()
	j, err := Open(dir, Options{Boot: "e8f9a0b1-0000-0000-0000-000000000000", Now: fixedClock()})
	if err != nil {
		tb.Fatalf("Open: %v", err)
	}
	tb.Cleanup(func() { j.Close() })
	return j
}

func appendAll(tb testing.TB, j *Journal, payloads []Payload) []Event {
	tb.Helper()
	var out []Event
	for _, p := range payloads {
		e, err := j.Append(p)
		if err != nil {
			tb.Fatalf("Append %s: %v", p.Kind(), err)
		}
		out = append(out, e)
	}
	return out
}

func sessionStart() *SessionStart {
	return &SessionStart{Schema: Schema, Session: "20261002T011407Z", Cores: []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{0, 16}}, {Core: 7, CCD: 0, CPUs: []int{7, 23}}}}
}

func TestOpenFinishesRenamedIncompatibleArchive(t *testing.T) {
	dir := t.TempDir()
	id := "20261002T011407Z"
	archive := filepath.Join(dir, archiveDir)
	if err := os.Mkdir(archive, 0o755); err != nil {
		t.Fatal(err)
	}
	data := []byte(`{"seq":1,"kind":"session.start","session":"` + id + `","schema":99}` + "\n")
	path := filepath.Join(archive, id+".jsonl")
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
	marker := filepath.Join(archive, id+"-compat-pending")
	if err := os.WriteFile(marker, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	j, err := Open(dir, Options{Boot: "next", Sync: true})
	if err != nil {
		t.Fatalf("open after archive rename: %v", err)
	}
	if len(j.Events()) != 0 {
		t.Fatalf("new journal contains old session: %d events", len(j.Events()))
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(marker); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("pending marker remains: %v", err)
	}
	after, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(after, data) {
		t.Fatalf("archive changed: %v", err)
	}
}

func samplePayloads() []Payload {
	return []Payload{
		sessionStart(),
		&ConfigLoaded{Path: "/etc/togi/config.toml", Config: sampleConfig()},
		&SMUIntent{Op: SMUSet, Core: new(7), Offset: -32},
		&SMUWrite{Op: SMUSet, Core: new(7), Offset: -32},
		&TrialIntent{Trial: "0001", Regime: machine.R6, Workload: "idle", DurationS: 900, Condition: machine.Resident},
		&TrialEnd{Trial: "0001", Outcome: OutcomePass, DurationS: 900},
		&TunerDecision{Core: 7, Phase: PhaseSearch, Decision: StepDeeper, FromOffset: -32, ToOffset: -37, Reason: "coarse, no failed mark yet"},
		&DeadEnd{Condition: DeadEndSMU, Detail: "SMU command failed: timeout", Action: "exit"},
	}
}

func sampleConfig() ConfigSnapshot {
	return ConfigSnapshot{
		StartOffsets:   map[int]int{},
		CandidateEdges: map[int]int{},
		Durations:      ConfigDurations{SearchTrialS: 90, StartS: 120, GuardTrialS: 120, GuardIdleS: 900, GuardAllCoreS: 1200},
		Evidence:       ConfigEvidence{Miss: 0.05, Rate: 0.5},
		Guard:          ConfigGuard{Rotation: []machine.Regime{machine.R7, machine.R7, machine.R7, machine.R2, machine.R2, machine.R2, machine.R6, machine.R5, machine.R1, machine.R1, machine.R1, machine.R3, machine.R4, machine.R6}},
		DeadEnds:       ConfigDeadEnds{InconclusiveInARow: 3, StrayCrashesInARow: 3},
	}
}

func TestReplayTruncatedAtEveryOffset(t *testing.T) {
	t.Parallel()
	src := t.TempDir()
	j := openTest(t, src)
	appendAll(t, j, samplePayloads())
	j.Close()
	data, err := os.ReadFile(filepath.Join(src, eventsFile))
	if err != nil {
		t.Fatal(err)
	}
	lines := bytes.SplitAfter(data, []byte("\n"))
	lines = lines[:len(lines)-1]

	for cut := 0; cut <= len(data); cut++ {
		dir := t.TempDir()
		if err := os.WriteFile(filepath.Join(dir, eventsFile), data[:cut], 0o644); err != nil {
			t.Fatal(err)
		}
		j, err := Open(dir, Options{Boot: "next", Now: fixedClock()})
		if err != nil {
			t.Fatalf("cut %d: Open: %v", cut, err)
		}
		events := j.Events()
		j.Close()

		complete, start := 0, 0
		for complete < len(lines) && start+len(lines[complete]) <= cut {
			start += len(lines[complete])
			complete++
		}
		torn := data[start:cut]
		want := complete
		if len(torn) > 0 && complete > 0 {
			want++
		}
		if len(events) != want {
			t.Fatalf("cut %d: %d events, want %d", cut, len(events), want)
		}
		for i := range complete {
			if !bytes.Equal(events[i].Raw, bytes.TrimSuffix(lines[i], []byte("\n"))) {
				t.Fatalf("cut %d: event %d differs", cut, i+1)
			}
		}
		after, err := os.ReadFile(filepath.Join(dir, eventsFile))
		if err != nil {
			t.Fatal(err)
		}
		switch {
		case complete == 0:
			if len(after) != 0 {
				t.Fatalf("cut %d: torn first line left %d bytes", cut, len(after))
			}
		case len(torn) == 0:
			if !bytes.Equal(after, data[:cut]) {
				t.Fatalf("cut %d: clean cut changed the file", cut)
			}
		default:
			tornEvent, ok := events[complete].Data.(*JournalTorn)
			if !ok {
				t.Fatalf("cut %d: last event is %s, want journal.torn", cut, events[complete].Kind)
			}
			got, err := hex.DecodeString(tornEvent.BytesHex)
			if err != nil || !bytes.Equal(got, torn) || tornEvent.Offset != int64(start) {
				t.Fatalf("cut %d: journal.torn = %+v, want %d bytes at %d", cut, tornEvent, len(torn), start)
			}
			if !bytes.HasPrefix(after, data[:start]) {
				t.Fatalf("cut %d: complete lines were rewritten", cut)
			}
		}
	}
}

func TestSecondWriterRefused(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	first, err := Open(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := Open(dir, Options{}); !errors.Is(err, ErrLocked) {
		t.Fatalf("second Open error = %v, want ErrLocked", err)
	}
	if err := first.Close(); err != nil {
		t.Fatal(err)
	}
	again, err := Open(dir, Options{})
	if err != nil {
		t.Fatalf("Open after Close: %v", err)
	}
	again.Close()
}

func TestRoundTrip(t *testing.T) {
	t.Parallel()
	cfg := sampleConfig()
	cfg.StartOffsets[3] = -10
	payloads := []Payload{
		sessionStart(),
		&SessionContext{BIOSVersion: "3205", Board: "X870E", CPUModel: "Ryzen 9 9950X", Microcode: "0xb404023", BoostLimitMHz: 5700},
		&SessionBaseline{Offsets: []int{0, -5}},
		&SessionNotice{Notice: NoticeNonzeroBaseline, Cores: []int{7}},
		&SessionArchived{Session: "20261002T011407Z", Path: "archive/20261002T011407Z.jsonl"},
		&SessionCarried{Sources: []CarriedSource{{Session: "20261001T000000Z", Path: "archive/20261001T000000Z.jsonl", Schema: 2, Ruleset: 2}}, Marks: true, Carried: []CarriedCore{{Core: 7, Edge: new(-30), EdgeSession: "20261001T000000Z", EdgeSeq: 12, FailedMark: new(-31), MarkSession: "20261001T000000Z", MarkSeq: 14, MarkSignal: machine.Crash}}},
		&ConfigLoaded{Path: "/etc/togi/config.toml", File: true, Config: cfg},
		&PreflightCheck{Check: "root", Detail: "uid 0", OK: true},
		&SMUIntent{Op: SMUSetAll, Offset: 0},
		&SMUWrite{Op: SMUSet, Core: new(7), Offset: -32},
		&SMUReadback{Core: 7, Offset: -31, Expected: new(-32)},
		&SMUError{Op: SMUSet, Core: new(7), Offset: -32, Error: "timeout"},
		&ProfileApplied{Offsets: []int{0, 0}, Condition: machine.Isolated},
		&ProfileChange{From: []int{0, -5}, To: []int{0, -4}},
		&ProfileChange{To: []int{0, -5}},
		&ProfileRestored{Offsets: []int{0, -5}},
		&TrialIntent{Trial: "0413", Core: new(7), Offset: new(-32), Regime: machine.R2, Workload: "mprime-avx2-36k-248k", DurationS: 90, Condition: machine.Isolated, Phase: PhaseSearch, Retry: true},
		&TrialIntent{Trial: "0414", Cores: []int{0, 7}, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", DurationS: 1200, Condition: machine.Resident, Phase: PhaseGuard, Rotation: 3},
		&TrialStart{Trial: "0413", Scope: "togi-trial-0413", PID: 48211, CPUs: []int{7, 23}, Argv: []string{"mprime", "-t"}},
		&TrialProgress{Trial: "0413", Detail: "FFT 36K done"},
		&TrialSignal{Trial: "0413", Schedule: "random on/off periods", Seed: 814},
		&TrialSignal{Trial: "0413", Stops: 10, Conts: 9},
		&TrialSample{Trial: "0413", Warning: "outside allowed cpus", PID: 48211, TID: 48213, CPU: 8},
		&TrialEnd{Trial: "0413", Outcome: OutcomeFailure, Signal: machine.ComputationError, DurationS: 41, TctlMaxC: new(75), Reason: "r", Interrupted: true, Escaped: []int{8}},
		&TrialEnd{Trial: "0414", Outcome: OutcomeFailure, Signal: machine.Stall, Core: new(7), DurationS: 300},
		&Failure{Signal: machine.Stall, Attribution: Attributed, Core: new(7), Offset: new(-32), Trial: "0413"},
		&Failure{Signal: machine.Stall, Attribution: Attributed, Core: new(7), Offset: new(-32), Trial: "0414", Regime: machine.R7, Condition: machine.Resident},
		&Failure{Signal: machine.Crash, Attribution: Unattributed, Regime: machine.R6, Condition: machine.Resident},
		&MCE{CPU: 7, Core: 7, Bank: 5, BankType: machine.LoadStore, Corrected: true, FromBoot: "e8f9a0b1", Lines: []string{"[Hardware Error]: <x> & y"}},
		&CrashDetected{PreviousBoot: "e8f9a0b1", InFlight: new(812), Stray: true},
		&CrashDetected{PreviousBoot: "e8f9a0b1", Condition: machine.Resident},
		&TunerDecision{Core: 7, Phase: PhaseSearch, Decision: CheckEdge, FromOffset: -33, ToOffset: -33, Pass: new(-30), FailedMark: new(-34), Workloads: []string{"r1", "r2"}, Reason: "candidate edge"},
		&TunerDecision{Core: 7, Phase: PhaseGuard, Decision: Backoff, FromOffset: -32, ToOffset: -31, Pass: new(-33), FailedMark: new(-34), Reason: "r"},
		&TunerDecision{Core: 3, Phase: PhaseRefine, Decision: Yield, FromOffset: -30, ToOffset: -29, Reason: "r"},
		&CorePhase{Core: 7, From: PhaseSearch, To: PhaseResident, Offset: -33, Pass: new(-33), FailedMark: new(-34), CheckEdge: true, Workloads: []string{"r1", "r2"}, Reason: "candidate edge"},
		&GuardRotation{Rotation: 2, Event: RotationStart, Steps: []machine.Regime{machine.R1, machine.R6}},
		&GuardRotation{Rotation: 2, Event: RotationEnd, Clean: true},
		&GuardRotation{Rotation: 2, Event: RotationEnd, Reason: "the profile changed"},
		&CommandReset{Core: new(7)},
		&DeadEnd{Condition: DeadEndFailureAtZero, Core: new(7), Detail: "core 07 failed at CO 0", Action: "exit"},
		&BootSavedEntry{Before: "togi", After: ""},
		&BootSavedEntry{Before: "togi", After: "togi", Error: "grub-editenv: exit status 1"},
		&TrialStart{Trial: "0009", Scope: "togi-trial-0009", PID: 10, CPUs: []int{0, 8}, Argv: []string{"mprime"}, Files: []string{"c00/prime.txt"}, Instances: []TrialInstance{{Core: 0, CPUs: []int{0}, PID: 10, Scope: "togi-trial-0009-c00"}, {Core: 8, CPUs: []int{8}, PID: 11, Scope: "togi-trial-0009-c08"}}},
		&Shutdown{Reason: ShutdownRotations, Rotations: 3},
		&JournalTorn{Offset: 120, BytesHex: "7b22"},
		&StateRebuilt{Fields: []string{"cores", "last_seq"}},
	}
	dir := t.TempDir()
	j := openTest(t, dir)
	written := appendAll(t, j, payloads)
	events, torn, err := Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("Read: %v, torn %q", err, torn)
	}
	for i, e := range events {
		w := written[i]
		if diff := cmp.Diff(w.Data, e.Data); diff != "" {
			t.Errorf("%s: data mismatch (-wrote +read):\n%s", w.Kind, diff)
		}
		if e.Msg != w.Msg || e.Seq != w.Seq || !e.Time.Equal(w.Time) || e.Boot != w.Boot || !bytes.Equal(e.Raw, w.Raw) {
			t.Errorf("%s: envelope differs: %s vs %s", w.Kind, e.Raw, w.Raw)
		}
	}
}

func TestAppendRejectsBadEvents(t *testing.T) {
	t.Parallel()
	j := openTest(t, t.TempDir())
	if _, err := j.Append(&Shutdown{Reason: ShutdownSignal}); err == nil {
		t.Fatal("first event other than session.start accepted")
	}
	appendAll(t, j, []Payload{sessionStart()})
	if _, err := j.Append(&Shutdown{Reason: ShutdownSignal}, 2); err == nil {
		t.Fatal("cause naming the event itself accepted")
	}
}
