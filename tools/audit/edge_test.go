package main

import (
	"encoding/json"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
)

func TestAvoidanceHistory(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, body string
		want       []finding
	}{
		{"SMU combination with sparse ids", `{"seq":2,"boot":"b","kind":"session.baseline","offsets":[0,0]}
{"seq":3,"boot":"b","kind":"combination","combination":1,"members":[{"core":0,"offset":-30},{"core":8,"offset":-20}]}
{"seq":4,"boot":"b","kind":"smu.intent","op":"set","core":0,"offset":-30}
{"seq":5,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":-30,"cause":[4]}
{"seq":6,"boot":"b","kind":"smu.readback","core":0,"offset":-30,"cause":[5]}
{"seq":7,"boot":"b","kind":"smu.intent","op":"set","core":8,"offset":-20}
{"seq":8,"boot":"b","kind":"smu.write","op":"set","core":8,"offset":-20,"cause":[7]}
{"seq":9,"boot":"b","kind":"smu.readback","core":8,"offset":-20,"cause":[8]}
`, []finding{{8, "avoidance"}}},
		{"failure points accumulate despite omitted projection", `{"seq":2,"boot":"b","kind":"failure","attribution":"attributed","core":0,"offset":-20}
{"seq":3,"boot":"b","kind":"tuner.decision","core":0,"failure_point":null}
{"seq":4,"boot":"b","kind":"profile.applied","offsets":[-20,0]}
`, []finding{{4, "avoidance"}}},
		{"shallower point wins", `{"seq":2,"boot":"b","kind":"failure","attribution":"attributed","core":0,"offset":-20}
{"seq":3,"boot":"b","kind":"failure","attribution":"attributed","core":0,"offset":-30}
{"seq":4,"boot":"b","kind":"profile.applied","offsets":[-25,0]}
`, []finding{{4, "avoidance"}}},
		{"hunt culprit point", `{"seq":2,"boot":"b","kind":"hunt.start","hunt":1,"failing":[0,-20]}
{"seq":3,"boot":"b","kind":"hunt.end","hunt":1,"result":"culprit","cores":[8]}
{"seq":4,"boot":"b","kind":"profile.applied","offsets":[0,-20]}
`, []finding{{4, "avoidance"}}},
		{"record only cannot record a point", `{"seq":2,"boot":"b","kind":"trial.intent","trial":"t","record_only":true}
{"seq":3,"boot":"b","kind":"failure","trial":"t","attribution":"attributed","core":0,"offset":-20}
{"seq":4,"boot":"b","kind":"profile.applied","offsets":[-20,0]}
`, nil},
		{"carried known failure despite record-only ID", `{"seq":2,"boot":"b","kind":"trial.intent","trial":"0001","record_only":true}
{"seq":3,"boot":"b","kind":"failure","trial":"0001","attribution":"attributed","core":0,"offset":-20,"known_failure":2}
{"seq":4,"boot":"b","kind":"profile.applied","offsets":[-20,0]}
`, []finding{{4, "avoidance"}}},
		{"BIOS change does not carry points", `{"seq":2,"boot":"b","kind":"session.carried","failure_points":false,"carried":[{"core":8,"failure_point":-20}]}
{"seq":3,"boot":"b","kind":"profile.applied","offsets":[0,-20]}
`, nil},
		{"warning after shutdown", `{"seq":2,"boot":"b","kind":"shutdown","reason":"laps"}
{"seq":3,"boot":"b","kind":"session.warning","operation":"write state projection"}
{"seq":4,"boot":"b","kind":"session.warning","operation":"write state projection"}
`, nil},
		{"warnings alone do not conclude", `{"seq":2,"boot":"b","kind":"session.warning","operation":"write state projection"}
{"seq":3,"boot":"b","kind":"session.warning","operation":"write state projection"}
`, []finding{{3, "termination"}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			closed := tc.name == "warning after shutdown" || tc.name == "warnings alone do not conclude"
			if diff := cmp.Diff(tc.want, findings(auditEvents(handwritten(t, tc.body), closed))); diff != "" {
				t.Fatalf("findings (-want +got):\n%s", diff)
			}
		})
	}
}
func TestMissingCause(t *testing.T) {
	t.Parallel()
	events := handwritten(t, "")
	p := &journal.TunerDecision{Core: 0}
	events = append(events, journal.Event{Seq: 3, Boot: "b", Kind: p.Kind(), Data: p, Cause: []int{2}})
	if diff := cmp.Diff([]finding{{3, "cause"}}, findings(auditEvents(events, false))); diff != "" {
		t.Fatal(diff)
	}
}
func TestProjectionShape(t *testing.T) {
	t.Parallel()
	state := journal.State{Schema: 3}
	data, err := json.Marshal(state)
	if err != nil {
		t.Fatal(err)
	}
	var fields map[string]any
	if err := json.Unmarshal(data, &fields); err != nil {
		t.Fatal(err)
	}
	fields["unknown"] = true
	delete(fields, "session")
	data, err = json.Marshal(fields)
	if err != nil {
		t.Fatal(err)
	}
	got, err := projectionDifferences(state, data)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"session", "unknown"}, got); diff != "" {
		t.Fatal(diff)
	}
}
