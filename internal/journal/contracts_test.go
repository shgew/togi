package journal

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestEveryKindDecodesItsEnvelope(t *testing.T) {
	t.Parallel()
	at := time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC)
	for kind, typ := range payloadTypes {
		want := Event{Seq: 7, Time: at, Mono: 3, Boot: "b", Kind: kind, Msg: "m", Cause: []int{1}, Data: typ.new()}
		line, err := encode(want)
		if err != nil {
			t.Fatalf("%s: %v", kind, err)
		}
		want.Raw = line
		got, err := decode(line)
		if err != nil {
			t.Fatalf("%s: %v", kind, err)
		}
		if diff := cmp.Diff(want, got); diff != "" {
			t.Errorf("%s (-want +got):\n%s", kind, diff)
		}
	}
}

func TestPayloadStyles(t *testing.T) {
	t.Parallel()
	tests := []struct {
		p     Payload
		style Style
	}{
		{&HostRanking{Ranking: []int{3, 11}}, Plain},
		{&HostRanking{Detail: "missing"}, Plain},
		{&HuntStart{Hunt: 4, Regime: machine.R7, Trial: "0007", Candidates: []int{3, 11}, Starts: 5, StartS: 120}, Yellow},
		{&HuntGroup{Hunt: 4, Group: 1, Cores: []int{3, 11}, Skipped: true, Reason: "already checked"}, Plain},
		{&HuntGroup{Hunt: 4, Group: 2, Cores: []int{3}, Inferred: "pass", Reason: "complement failed"}, Plain},
		{&HuntGroup{Hunt: 4, Group: 3, Cores: []int{11}, Inferred: "failure", Reason: "complement passed"}, Plain},
		{&HuntGroup{Hunt: 4, Group: 4, Cores: []int{3, 11}, DurationS: 120}, Plain},
		{&HuntGroup{Hunt: 4, Group: 5, Cores: []int{3}, Probe: &CombinationMember{Core: 11, Offset: -22}, Held: []CombinationMember{{Core: 3, Offset: -40}}, DurationS: 120}, Plain},
		{&HuntSkipped{Failure: 904, Reason: "failure point already known"}, Plain},
		{&HuntEnd{Hunt: 3, Result: "culprit", Cores: []int{13}, Groups: 4}, Green},
		{&Combination{Combination: 2, Members: []CombinationMember{{Core: 3, Offset: -40}, {Core: 11, Offset: -30}}, Hunt: 4}, Yellow},
		{&DeepeningRound{Round: 2, Event: LapEnd, Passed: true}, GreenBold},
		{&TunerWarning{Warning: "monotonicity", Trial: "0520", Passes: []int{1, 2}}, Yellow},
		{&BackendRetry{Backend: "mprime", Attempt: 2, WaitS: 300, Reason: "setup failed"}, Dim},
		{&CheckingLap{Event: LapEnd, Passed: true}, Plain},
		{&Failure{Signal: machine.Crash, Attribution: Attributed, Core: new(1), Offset: new(-38), Trial: "0385", Regime: machine.R7, Condition: machine.Parked}, Red},
		{&Failure{Signal: machine.Crash, Attribution: Attributed, Core: new(2), Offset: new(-12), Regime: machine.R6, Condition: machine.Together}, Red},
		{&Failure{Signal: machine.Crash, Attribution: Unattributed, Trial: "0310", Regime: machine.R7, Condition: machine.Parked}, Red},
	}
	for _, tt := range tests {
		t.Run(string(tt.p.Kind()), func(t *testing.T) {
			if d := cmp.Diff(tt.style, StyleOf(Event{Data: tt.p})); d != "" {
				t.Errorf("style (-want +got): %s", d)
			}
		})
	}
}

func TestRecoveredTrialMessagesIncludeConditions(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		signal  machine.Signal
		outcome Outcome
	}{
		{"crash", machine.Crash, OutcomeFailure},
		{"computation error", machine.ComputationError, OutcomeFailure},
		{"corrected MCE", machine.CorrectedMCE, OutcomeFailure},
		{"uncorrected MCE", machine.UncorrectedMCE, OutcomeFailure},
		{"thermal trip", "", OutcomeInconclusive},
		{"power loss", "", OutcomeInconclusive},
	} {
		t.Run(tc.name, func(t *testing.T) {
			p := &TrialEnd{
				Trial: "0001", Signal: tc.signal, Outcome: tc.outcome, Reason: tc.name, DurationS: 990,
				LastSampleS: new(987), LastSampleTctlC: new(73),
				LastSampleMinMHz: new(4321), LastSampleMaxMHz: new(5432),
			}
			duration := " after 990s"
			if tc.signal == machine.Crash {
				duration = ", last evidence 990s after start"
			}
			want := fmt.Sprintf("trial 0001 FAIL %s%s, last sample 987s: Tctl 73°C, 4321-5432 MHz", tc.signal, duration)
			if tc.outcome == OutcomeInconclusive {
				want = fmt.Sprintf("trial 0001 INCONCLUSIVE after 990s, last sample 987s: Tctl 73°C, 4321-5432 MHz: %s", tc.name)
			}
			if diff := cmp.Diff(want, p.Message()); diff != "" {
				t.Errorf("recovered trial narrative (-want +got): %s", diff)
			}
		})
	}
}

func TestMonotonicStamp(t *testing.T) {
	t.Parallel()
	for _, tt := range []struct {
		name  string
		clock func() time.Duration
		want  *int64
	}{{"absent", nil, nil}, {"boot origin", func() time.Duration { return 0 }, new(int64(0))}, {"elapsed", func() time.Duration { return 1729 * time.Millisecond }, new(int64(1729))}} {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			j, err := Open(dir, Options{Boot: "boot", Now: func() time.Time { return time.Unix(0, 0) }, Monotonic: tt.clock})
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			e, err := j.Append(sessionStart())
			if err != nil {
				t.Fatal(err)
			}
			var fields map[string]json.RawMessage
			if err := json.Unmarshal(e.Raw, &fields); err != nil {
				t.Fatal(err)
			}
			var got *int64
			if raw, ok := fields["mono_ms"]; ok {
				got = new(int64)
				if err := json.Unmarshal(raw, got); err != nil {
					t.Fatal(err)
				}
			}
			if d := cmp.Diff(tt.want, got); d != "" {
				t.Errorf("mono_ms (-want +got): %s", d)
			}
			events, _, err := Read(dir)
			if err != nil {
				t.Fatal(err)
			}
			if d := cmp.Diff(e.Mono, events[0].Mono); d != "" {
				t.Errorf("replay mono (-want +got): %s", d)
			}
		})
	}
}

func TestNewKindsRoundTripAndCoreFilter(t *testing.T) {
	t.Parallel()
	payloads := []Payload{&HuntStart{Hunt: 1, Failure: 2, Candidates: []int{3}}, &HuntGroup{Hunt: 1, Group: 1, Cores: []int{3}}, &Combination{Combination: 1, Members: []CombinationMember{{Core: 3, Offset: -30}}}, &DeepeningRound{Round: 1, Event: LapStart}, &HostRanking{Ranking: []int{3}}, &HuntEnd{Hunt: 1, Result: "direct"}, &HuntSkipped{Failure: 1}, &TunerWarning{Warning: "monotonicity"}, &BackendRetry{Backend: "mprime"}}
	for _, p := range payloads {
		t.Run(string(p.Kind()), func(t *testing.T) {
			e := Event{Seq: 1, Time: time.Unix(0, 0), Boot: "boot", Kind: p.Kind(), Msg: p.Message(), Data: p}
			raw, err := encode(e)
			if err != nil {
				t.Fatal(err)
			}
			got, err := decode(raw)
			if err != nil {
				t.Fatal(err)
			}
			if d := cmp.Diff(p, got.Data); d != "" {
				t.Errorf("payload (-want +got): %s", d)
			}
			if p.Kind() == KindHuntStart || p.Kind() == KindCombination {
				if !(Filter{Core: new(3)}).Match(got) {
					t.Errorf("core 3 not matched")
				}
				if (Filter{Core: new(4)}).Match(got) {
					t.Errorf("core 4 matched")
				}
			}
		})
	}
}

func TestRecordedContext(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	got, err := RecordedContext(dir)
	if err != nil || got != nil {
		t.Fatalf("absent context: %v, %v", got, err)
	}
	path := filepath.Join(dir, "events.jsonl")
	lines := "{\"kind\":\"session.start\"}\n{\"kind\":\"future.event\"}\n{\"kind\":\"session.context\",\"bios_version\":\"first\"}\n{\"kind\":\"session.context\",\"bios_version\":\"second\"}\n"
	if err := os.WriteFile(path, []byte(lines), 0600); err != nil {
		t.Fatal(err)
	}
	got, err = RecordedContext(dir)
	if err != nil {
		t.Fatal(err)
	}
	want := &machine.BIOSContext{BIOSVersion: "first"}
	if d := cmp.Diff(want, got); d != "" {
		t.Errorf("first context (-want +got): %s", d)
	}
}

func TestRecordedContextRejectsDamage(t *testing.T) {
	for _, tc := range []struct{ data, diagnostic string }{
		{"{\n", "line 1"},
		{"{}\n", "event has no kind"},
		{`{"kind":"shutdown"}` + "\n", "first event is shutdown"},
		{`{"kind":"session.start"}` + "\n" + `{"kind":"session.context","bios_version":17}` + "\n", "decode session.context"},
	} {
		dir := t.TempDir()
		if err := os.WriteFile(filepath.Join(dir, eventsFile), []byte(tc.data), 0600); err != nil {
			t.Fatal(err)
		}
		if _, err := RecordedContext(dir); err == nil || !strings.Contains(err.Error(), tc.diagnostic) {
			t.Fatalf("context error = %v, want %q", err, tc.diagnostic)
		}
	}
	for _, data := range []string{"", `{"kind":"session.start"}` + "\n" + `{"kind":"session.context"`} {
		dir := t.TempDir()
		if err := os.WriteFile(filepath.Join(dir, eventsFile), []byte(data), 0600); err != nil {
			t.Fatal(err)
		}
		got, err := RecordedContext(dir)
		if err != nil || got != nil {
			t.Fatalf("torn context invented: %+v, %v", got, err)
		}
	}
}
