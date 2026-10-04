package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
)

var header = fmt.Sprintf(`{"seq":1,"time":"2026-01-01T00:00:00.000000000Z","boot":"b","kind":"session.start","session":"test","schema":4,"ruleset":%d,"cores":[{"core":0,"ccd":0,"cpus":[0,1]},{"core":8,"ccd":1,"cpus":[2,3]}]}`, tuner.Ruleset) + "\n"

func handwritten(t *testing.T, body string) []journal.Event {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")
	if err := os.WriteFile(path, []byte(header+body), 0600); err != nil {
		t.Fatal(err)
	}
	events, err := journal.ReadHistory(path)
	if err != nil {
		t.Fatal(err)
	}
	return events
}

type finding struct {
	Seq   int
	Check string
}

func findings(issues []violation) []finding {
	var result []finding
	for _, issue := range issues {
		result = append(result, finding{issue.Seq, issue.Check})
	}
	return result
}
func TestAuditInvariants(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, body string
		closed     bool
		want       []finding
	}{
		{"safe point boundary", `{"seq":2,"boot":"b","kind":"core.phase","core":0,"failure_point":-30}
{"seq":3,"boot":"b","kind":"profile.applied","offsets":[-29,0]}
`, false, nil},
		{"point reached", `{"seq":2,"boot":"b","kind":"core.phase","core":0,"failure_point":-30}
{"seq":3,"boot":"b","kind":"profile.applied","offsets":[-30,0]}
`, false, []finding{{3, "avoidance"}}},
		{"point deeper", `{"seq":2,"boot":"b","kind":"failure","core":0,"offset":-30,"attribution":"attributed"}
{"seq":3,"boot":"b","kind":"profile.restored","offsets":[-31,0]}
`, false, []finding{{3, "avoidance"}}},
		{"not yet recorded", `{"seq":2,"boot":"b","kind":"profile.applied","offsets":[-30,0]}
{"seq":3,"boot":"b","kind":"core.phase","core":0,"failure_point":-30}
`, false, nil},
		{"safe combination", `{"seq":2,"boot":"b","kind":"combination","combination":1,"members":[{"core":0,"offset":-30},{"core":8,"offset":-20}]}
{"seq":3,"boot":"b","kind":"profile.applied","offsets":[-31,-19]}
`, false, nil},
		{"combination reached", `{"seq":2,"boot":"b","kind":"combination","combination":1,"members":[{"core":0,"offset":-30},{"core":8,"offset":-20}]}
{"seq":3,"boot":"b","kind":"profile.applied","offsets":[-31,-20]}
`, false, []finding{{3, "avoidance"}}},
		{"SMU safe point", `{"seq":2,"boot":"b","kind":"core.phase","core":0,"failure_point":-30}
{"seq":3,"boot":"b","kind":"smu.intent","op":"set","core":0,"offset":-29}
{"seq":4,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":-29,"cause":[3]}
{"seq":5,"boot":"b","kind":"smu.readback","core":0,"offset":-29,"expected":-29,"cause":[4]}
`, false, nil},
		{"SMU unsafe point", `{"seq":2,"boot":"b","kind":"core.phase","core":0,"failure_point":-30}
{"seq":3,"boot":"b","kind":"smu.intent","op":"set","core":0,"offset":-30}
{"seq":4,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":-30,"cause":[3]}
{"seq":5,"boot":"b","kind":"smu.readback","core":0,"offset":-30,"expected":-30,"cause":[4]}
`, false, []finding{{4, "avoidance"}}},
		{"complete set all", `{"seq":2,"boot":"b","kind":"smu.intent","op":"set_all","offset":0}
{"seq":3,"boot":"b","kind":"smu.write","op":"set_all","offset":0,"cause":[2]}
{"seq":4,"boot":"b","kind":"smu.readback","core":0,"offset":0,"cause":[3]}
{"seq":5,"boot":"b","kind":"smu.readback","core":8,"offset":0,"cause":[3]}
{"seq":6,"boot":"b","kind":"shutdown","reason":"cycles"}
`, true, nil},
		{"missing intent", `{"seq":2,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":0}
{"seq":3,"boot":"b","kind":"smu.readback","core":0,"offset":0,"cause":[2]}
{"seq":4,"boot":"b","kind":"shutdown","reason":"cycles"}
`, true, []finding{{2, "write_protocol"}}},
		{"wrong intent", `{"seq":2,"boot":"b","kind":"smu.intent","op":"set","core":8,"offset":0}
{"seq":3,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":0,"cause":[2]}
{"seq":4,"boot":"b","kind":"smu.readback","core":0,"offset":0,"cause":[3]}
{"seq":5,"boot":"b","kind":"shutdown","reason":"cycles"}
`, true, []finding{{3, "write_protocol"}}},
		{"missing readback core", `{"seq":2,"boot":"b","kind":"smu.intent","op":"set_all","offset":0}
{"seq":3,"boot":"b","kind":"smu.write","op":"set_all","offset":0,"cause":[2]}
{"seq":4,"boot":"b","kind":"smu.readback","core":0,"offset":0,"cause":[3]}
{"seq":5,"boot":"b","kind":"shutdown","reason":"cycles"}
`, true, []finding{{3, "write_protocol"}}},
		{"live write in progress", `{"seq":2,"boot":"b","kind":"smu.intent","op":"set","core":0,"offset":0}
{"seq":3,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":0,"cause":[2]}
`, false, nil},
		{"range boundaries", `{"seq":2,"boot":"b","kind":"profile.applied","offsets":[-50,0]}
`, false, nil},
		{"invalid applied range", `{"seq":2,"boot":"b","kind":"profile.applied","offsets":[-51,1]}
`, false, []finding{{2, "range"}, {2, "range"}}},
		{"invalid written range", `{"seq":2,"boot":"b","kind":"smu.intent","op":"set","core":0,"offset":1}
{"seq":3,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":1,"cause":[2]}
{"seq":4,"boot":"b","kind":"smu.readback","core":0,"offset":1,"cause":[3]}
`, false, []finding{{2, "range"}, {3, "range"}}},
		{"invalid chosen ranges", `{"seq":2,"boot":"b","kind":"tuner.decision","core":0,"from_offset":0,"to_offset":-51}
{"seq":3,"boot":"b","kind":"core.phase","core":0,"offset":1}
{"seq":4,"boot":"b","kind":"trial.intent","trial":"t","offset":-51,"profile":[-51,0]}
{"seq":5,"boot":"b","kind":"profile.change","from":[0,0],"to":[0,1]}
{"seq":6,"boot":"b","kind":"deepening.round","round":1,"event":"start","base":[-10,0],"target":[-51,0],"profile":[-51,0]}
{"seq":7,"boot":"b","kind":"hunt.group","hunt":1,"group":1,"cores":[0],"profile":[1,0]}
{"seq":8,"boot":"b","kind":"checking.step","cycle":1,"step":1,"profile":[0,-51]}
{"seq":9,"boot":"b","kind":"hunt.start","hunt":1,"failing":[0,0],"parked":[-51,0]}
`, false, []finding{{2, "range"}, {3, "range"}, {4, "range"}, {4, "range"}, {5, "range"}, {6, "range"}, {6, "range"}, {7, "range"}, {8, "range"}, {9, "range"}}},
		{"firmware readback", `{"seq":2,"boot":"b","kind":"smu.readback","core":0,"offset":10}
{"seq":3,"boot":"b","kind":"session.baseline","offsets":[10,0],"cause":[2]}
`, false, nil},
		{"previous boot intent", `{"seq":2,"boot":"b","kind":"smu.intent","op":"set","core":0,"offset":0}
{"seq":3,"boot":"c","kind":"smu.write","op":"set","core":0,"offset":0,"cause":[2]}
{"seq":4,"boot":"c","kind":"smu.readback","core":0,"offset":0,"cause":[3]}
{"seq":5,"boot":"c","kind":"shutdown","reason":"cycles"}
`, true, []finding{{3, "write_protocol"}}},
		{"reused intent", `{"seq":2,"boot":"b","kind":"smu.intent","op":"set","core":0,"offset":0}
{"seq":3,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":0,"cause":[2]}
{"seq":4,"boot":"b","kind":"smu.readback","core":0,"offset":0,"cause":[3]}
{"seq":5,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":0,"cause":[2]}
{"seq":6,"boot":"b","kind":"smu.readback","core":0,"offset":0,"cause":[5]}
{"seq":7,"boot":"b","kind":"shutdown","reason":"cycles"}
`, true, []finding{{5, "write_protocol"}}},
		{"write interrupted by reboot", `{"seq":2,"boot":"b","kind":"smu.intent","op":"set","core":0,"offset":-5}
{"seq":3,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":-5,"cause":[2]}
{"seq":4,"boot":"c","kind":"core.phase","core":0}
{"seq":5,"boot":"c","kind":"shutdown","reason":"cycles"}
`, true, nil},
		{"same-boot reconciliation readbacks", `{"seq":2,"boot":"b","kind":"smu.intent","op":"set","core":0,"offset":-5}
{"seq":3,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":-5,"cause":[2]}
{"seq":4,"boot":"b","kind":"smu.readback","core":0,"offset":-5}
{"seq":5,"boot":"b","kind":"smu.readback","core":8,"offset":0}
{"seq":6,"boot":"b","kind":"profile.restored","offsets":[0,0]}
{"seq":7,"boot":"b","kind":"shutdown","reason":"cycles"}
`, true, nil},
		{"earlier cause", `{"seq":2,"boot":"b","kind":"tuner.decision","core":0,"cause":[1]}
`, false, nil},
		{"self and future cause", `{"seq":2,"boot":"b","kind":"tuner.decision","core":0,"cause":[2,3,0]}
{"seq":3,"boot":"b","kind":"core.phase","core":0}
`, false, []finding{{2, "cause"}, {2, "cause"}, {2, "cause"}}},
		{"concluded", `{"seq":2,"boot":"b","kind":"shutdown","reason":"cycles"}
`, true, nil},
		{"dead ended", `{"seq":2,"boot":"b","kind":"deadend","condition":"preflight","detail":"unsupported machine"}
`, true, nil},
		{"archived", `{"seq":2,"boot":"b","kind":"core.phase","core":0}
{"seq":3,"boot":"b","kind":"session.archived","session":"test","path":"archive/test.jsonl"}
{"seq":4,"boot":"b","kind":"session.warning","operation":"write state projection"}
`, true, nil},
		{"reset all", `{"seq":2,"boot":"b","kind":"command.reset","all":true}
`, true, nil},
		{"reset one core does not conclude", `{"seq":2,"boot":"b","kind":"command.reset","core":0}
`, true, []finding{{2, "termination"}}},
		{"no conclusion", `{"seq":2,"boot":"b","kind":"core.phase","core":0}
`, true, []finding{{2, "termination"}}},
		{"no shutdown reason", `{"seq":2,"boot":"b","kind":"shutdown"}
`, true, []finding{{2, "termination"}}},
		{"no dead end reason", `{"seq":2,"boot":"b","kind":"deadend","condition":"smu"}
`, true, []finding{{2, "termination"}}},
		{"no rebuild", `{"seq":2,"boot":"b","kind":"core.phase","core":0}
`, false, nil},
		{"stale rebuilt snapshot", `{"seq":2,"boot":"b","kind":"state.rebuilt","fields":["cores","last_seq"]}
`, false, nil},
		{"current rebuilt snapshot disagrees", `{"seq":2,"boot":"b","kind":"state.rebuilt","fields":["cores"]}
`, false, []finding{{2, "replay"}}},
		{"reset clears marks", `{"seq":2,"boot":"b","kind":"core.phase","core":0,"failure_point":-30}
{"seq":3,"boot":"b","kind":"combination","combination":1,"members":[{"core":0,"offset":-30},{"core":8,"offset":-20}]}
{"seq":4,"boot":"b","kind":"command.reset","core":0}
{"seq":5,"boot":"b","kind":"core.phase","core":0,"to":"search","failure_point":null}
{"seq":6,"boot":"b","kind":"profile.applied","offsets":[-30,-20]}
`, false, nil},
		{"queued reset does not clear", `{"seq":2,"boot":"b","kind":"core.phase","core":0,"failure_point":-30}
{"seq":3,"boot":"b","kind":"command.reset","core":0}
{"seq":4,"boot":"b","kind":"profile.applied","offsets":[-30,0]}
`, false, []finding{{4, "avoidance"}}},
		{"carry point", `{"seq":2,"boot":"b","kind":"session.carried","failure_points":true,"carried":[{"core":8,"failure_point":-10}]}
{"seq":3,"boot":"b","kind":"profile.applied","offsets":[0,-10]}
`, false, []finding{{3, "avoidance"}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got := findings(auditEvents(handwritten(t, tc.body), tc.closed))
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatalf("findings (-want +got):\n%s", diff)
			}
		})
	}
}

func TestProjectedState(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		lastSeq int
		phase   string
		want    []finding
	}{
		{"equal", 1, "checking", nil},
		{"stale disagreement", 0, "search", nil},
		{"different sequence", 2, "search", nil},
		{"current disagreement", 1, "search", []finding{{1, "replay"}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(header), 0600); err != nil {
				t.Fatal(err)
			}
			cached := journal.State{Schema: 4, LastSeq: 1, Phase: "checking", Session: &journal.SessionInfo{ID: "test"}, Cores: []journal.CoreState{{Core: 0, CCD: 0, CPUs: []int{0, 1}}, {Core: 8, CCD: 1, CPUs: []int{2, 3}}}}
			events := handwritten(t, "")
			cached.Session.Start = events[0].Time
			cached.LastSeq, cached.Phase = tc.lastSeq, tc.phase
			data, err := json.Marshal(cached)
			if err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(filepath.Join(dir, "state.json"), data, 0600); err != nil {
				t.Fatal(err)
			}
			issues, count, err := auditDirectory(dir, false)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(1, count); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff(tc.want, findings(issues)); diff != "" {
				t.Fatalf("projection (-want +got):\n%s\n%+v", diff, issues)
			}
		})
	}
}

func writeJournal(t *testing.T, path, header, body string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(header+body), 0600); err != nil {
		t.Fatal(err)
	}
}

func TestDirectoryJournals(t *testing.T) {
	t.Parallel()
	shutdown := `{"seq":2,"boot":"b","kind":"shutdown","reason":"cycles"}
`
	for _, tc := range []struct {
		name, archive string
		simulated     bool
		want          []finding
	}{
		{"glob metacharacters in name", `{"seq":2,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":0}
`, false, []finding{{2, "write_protocol"}}},
		{"transition archive ends anywhere", `{"seq":2,"boot":"b","kind":"smu.intent","op":"set","core":0,"offset":-5}
{"seq":3,"boot":"b","kind":"smu.write","op":"set","core":0,"offset":-5,"cause":[2]}
`, true, nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			dir := filepath.Join(t.TempDir(), "copy[1]")
			writeJournal(t, filepath.Join(dir, "archive", "old.jsonl"), header, tc.archive)
			writeJournal(t, filepath.Join(dir, "events.jsonl"), header, shutdown)
			issues, count, err := auditDirectory(dir, tc.simulated)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(2, count); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff(tc.want, findings(issues)); diff != "" {
				t.Fatalf("findings (-want +got):\n%s", diff)
			}
		})
	}
}

func TestOlderRulesetSnapshot(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	writeJournal(t, filepath.Join(dir, "events.jsonl"), strings.Replace(header, fmt.Sprintf(`"ruleset":%d`, tuner.Ruleset), fmt.Sprintf(`"ruleset":%d`, tuner.Ruleset-1), 1), "")
	if err := os.WriteFile(filepath.Join(dir, "state.json"), []byte(`{"last_seq":1}`), 0600); err != nil {
		t.Fatal(err)
	}
	issues, _, err := auditDirectory(dir, false)
	if err != nil {
		t.Fatal(err)
	}
	if len(issues) > 0 {
		t.Fatalf("older ruleset snapshot reported: %+v", issues)
	}
}

func TestRecordLookupIgnoresPathSpelling(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	root := filepath.Join(base, "bench-1")
	writeJournal(t, filepath.Join(root, "s", "dev-1", "events.jsonl"), header, `{"seq":2,"boot":"b","kind":"core.phase","core":0}
`)
	records := filepath.Join(base, "records.jsonl")
	if err := os.WriteFile(records, []byte(`{"scenario":"s","seed":1,"split":"dev","status":"concluded","wall_s":1}`+"\n"), 0600); err != nil {
		t.Fatal(err)
	}
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	relative, err := filepath.Rel(wd, root)
	if err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if diff := cmp.Diff(1, run([]string{"--records", records, "--root", relative, filepath.Join(root, "s", "dev-1")}, &stdout, &stderr)); diff != "" {
		t.Fatalf("exit (-want +got):\n%s\n%s", diff, stderr.String())
	}
	var issue violation
	if err := json.Unmarshal(stdout.Bytes(), &issue); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(finding{2, "termination"}, finding{issue.Seq, issue.Check}); diff != "" {
		t.Fatalf("finding (-want +got):\n%s", diff)
	}
}
