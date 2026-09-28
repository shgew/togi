package journal

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestNewPayloadMessagesAndStyles(t *testing.T) {
	t.Parallel()
	tests := []struct {
		p       Payload
		message string
		style   Style
	}{
		{&HostRanking{Ranking: []int{3, 11}}, "preferred cores 03 11", Plain},
		{&HostRanking{Detail: "missing"}, "preferred-core ranking unavailable (missing); core-id order", Plain},
		{&HuntSkipped{Failure: 904, Reason: "already marked"}, "failure #904 is not hunted: already marked", Plain},
		{&HuntEnd{Hunt: 3, Result: "culprit", Cores: []int{13}, Masks: 4}, "hunt 3 found core 13 after 4 masks", Green},
		{&MarkJoint{Mark: 2, Members: []JointMember{{Core: 3, Offset: -40}, {Core: 11, Offset: -30}}, Hunt: 4}, "joint mark J2: core 03 -40 + core 11 -30, observed in hunt 4", Yellow},
		{&RefineRound{Round: 2, Event: RotationEnd, Passed: true}, "refine round 2 end: passed", GreenBold},
		{&TunerWarning{Warning: "monotonicity", Trial: "0520", Passes: []int{1, 2}}, "monotonicity: trial 0520 failed on a profile at least as shallow as 2 passes in its class", Yellow},
		{&BackendRetry{Backend: "mprime", Attempt: 2, WaitS: 300, Reason: "setup failed"}, "backend mprime: retry 2 of 3 after 300s: setup failed", Dim},
		{&GuardRotation{Event: RotationEnd, Clean: true}, "guard rotation 0 end clean", Plain},
	}
	for _, tt := range tests {
		t.Run(string(tt.p.Kind()), func(t *testing.T) {
			if d := cmp.Diff(tt.message, tt.p.Message()); d != "" {
				t.Errorf("message (-want +got): %s", d)
			}
			if d := cmp.Diff(tt.style, StyleOf(Event{Data: tt.p})); d != "" {
				t.Errorf("style (-want +got): %s", d)
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
	payloads := []Payload{&HuntStart{Hunt: 1, Failure: 2, Candidates: []int{3}}, &HuntMask{Hunt: 1, Mask: 1, Cores: []int{3}}, &MarkJoint{Mark: 1, Members: []JointMember{{Core: 3, Offset: -30}}}, &RefineRound{Round: 1, Event: RotationStart}, &HostRanking{Ranking: []int{3}}, &HuntEnd{Hunt: 1, Result: "direct"}, &HuntSkipped{Failure: 1}, &TunerWarning{Warning: "monotonicity"}, &BackendRetry{Backend: "mprime"}}
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
			if p.Kind() == KindHuntStart || p.Kind() == KindMarkJoint {
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
