package journal

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestTranslateLegacy(t *testing.T) {
	for _, tc := range []struct{ name, old, want string }{
		{"lap", `{"kind":"guard.rotation","rotation":3,"clean":true,"qualifying":true}`, `{"kind":"checking.lap","lap":3,"passed":true,"full":true}`},
		{"step", `{"kind":"guard.step","rotation":3}`, `{"kind":"checking.step","lap":3}`},
		{"member probe", `{"kind":"hunt.mask","mask":4,"edge":{"core":5,"offset":-30},"stage":"edge"}`, `{"kind":"hunt.group","group":4,"probe":{"core":5,"offset":-30},"stage":"probe"}`},
		{"combination", `{"kind":"mark.joint","mark":3}`, `{"kind":"combination","combination":3}`},
		{"deepening", `{"kind":"refine.round","anchor":[0,-20],"anchor_seq":8}`, `{"kind":"deepening.round","base":[0,-20],"base_seq":8}`},
		{"hunt start", `{"kind":"hunt.start","anchor":[0,-20],"anchor_seq":8}`, `{"kind":"hunt.start","parked":[0,-20],"parked_seq":8}`},
		{"hunt end", `{"kind":"hunt.end","masks":4,"result":"joint"}`, `{"kind":"hunt.end","groups":4,"result":"combination"}`},
		{"carried values", `{"kind":"session.carried","marks":true,"carried":[{"core":0,"edge":-30,"edge_session":"old","edge_seq":4,"failed_mark":-31,"mark_session":"older","mark_seq":9,"mark_signal":"crash"}]}`, `{"kind":"session.carried","failure_points":true,"carried":[{"core":0,"candidate_solo_limit":-30,"candidate_solo_limit_session":"old","candidate_solo_limit_seq":4,"failure_point":-31,"failure_point_session":"older","failure_point_seq":9,"failure_point_signal":"crash"}]}`},
		{"phase", `{"kind":"core.phase","from":"resident","to":"done","failed_mark":-31,"check_edge":true,"cleared_joint":[3]}`, `{"kind":"core.phase","from":"has_room","to":"at_limit","failure_point":-31,"check_solo_limit":true,"cleared_combination":[3]}`},
		{"decision", `{"kind":"tuner.decision","phase":"refine","decision":"check_edge","failed_mark":-31}`, `{"kind":"tuner.decision","phase":"deepening","decision":"check_solo_limit","failure_point":-31}`},
		{"trial", `{"kind":"trial.intent","phase":"guard","condition":"resident","rotation":3,"mask":4,"seed":18446744073709551615}`, `{"kind":"trial.intent","phase":"checking","condition":"together","lap":3,"group":4,"seed":18446744073709551615}`},
		{"alone", `{"kind":"trial.intent","condition":"isolated"}`, `{"kind":"trial.intent","condition":"alone"}`},
		{"parked", `{"kind":"failure","condition":"masked"}`, `{"kind":"failure","condition":"parked"}`},
		{"shutdown", `{"kind":"shutdown","reason":"rotations","rotations":3}`, `{"kind":"shutdown","reason":"laps","laps":3}`},
		{"configuration", `{"kind":"config.loaded","schema":2,"config":{"candidate_edges":{"0":-30},"guard":{"rotation":["R1"]},"durations":{"guard_trial_s":120,"guard_idle_s":900,"guard_all_core_s":1200}}}`, `{"kind":"config.loaded","schema":2,"config":{"candidate_solo_limits":{"0":-30},"checking":{"lap":["R1"]},"durations":{"checking_trial_s":120,"checking_idle_s":900,"checking_all_core_s":1200}}}`},
		{"unrelated values", `{"kind":"future.fact","detail":"resident","result":"joint","stage":"edge","reason":"rotations","mark":1,"edge":2}`, `{"kind":"future.fact","detail":"resident","result":"joint","stage":"edge","reason":"rotations","mark":1,"edge":2}`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := translateLegacy([]byte(tc.old))
			if err != nil {
				t.Fatal(err)
			}
			var actual, expected map[string]json.RawMessage
			if err := json.Unmarshal(got, &actual); err != nil {
				t.Fatal(err)
			}
			if err := json.Unmarshal([]byte(tc.want), &expected); err != nil {
				t.Fatal(err)
			}
			canonical := func(value map[string]json.RawMessage) string {
				var body any
				data, err := json.Marshal(value)
				if err != nil {
					t.Fatal(err)
				}
				decoder := json.NewDecoder(bytes.NewReader(data))
				decoder.UseNumber()
				if err := decoder.Decode(&body); err != nil {
					t.Fatal(err)
				}
				data, err = json.Marshal(body)
				if err != nil {
					t.Fatal(err)
				}
				return string(data)
			}
			if diff := cmp.Diff(canonical(expected), canonical(actual)); diff != "" {
				t.Fatalf("translated line (-want +got):\n%s", diff)
			}
		})
	}
}

func TestLegacyReadersPreserveRecordedLines(t *testing.T) {
	for _, schema := range []int{1, 2, 3} {
		t.Run(fmt.Sprint(schema), func(t *testing.T) {
			first := fmt.Sprintf(`{"seq":1,"kind":"session.start","schema":%d,"session":"old"}`, schema)
			second := `{"seq":2,"kind":"trial.intent","msg":"isolated trial under guard: raw diagnostic \u001b[31mfailed\u001b[0m\n","trial":"0001","condition":"isolated","phase":"guard","rotation":3,"mask":4}`
			if schema == 3 {
				second = `{"seq":2,"kind":"trial.intent","msg":"isolated trial under guard: raw diagnostic \u001b[31mfailed\u001b[0m\n","trial":"0001","condition":"alone","phase":"checking","lap":3,"group":4}`
			}
			path := filepath.Join(t.TempDir(), "events.jsonl")
			if err := os.WriteFile(path, []byte(first+"\n"+second+"\n"), 0o644); err != nil {
				t.Fatal(err)
			}
			readReplay := func(path string) ([]Event, error) {
				events, _, err := ReadReplay(filepath.Dir(path), 1)
				return events, err
			}
			for _, read := range []func(string) ([]Event, error){ReadHistory, ReadForCarry, readReplay} {
				events, err := read(path)
				if err != nil {
					t.Fatal(err)
				}
				want := &TrialIntent{Trial: "0001", Condition: machine.Alone, Phase: PhaseChecking, Cycle: 3, Group: 4}
				if diff := cmp.Diff(want, events[1].Data); diff != "" {
					t.Fatalf("payload (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff(second, string(events[1].Raw)); diff != "" {
					t.Fatalf("recorded line (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff("isolated trial under guard: raw diagnostic \x1b[31mfailed\x1b[0m\n", events[1].Msg); diff != "" {
					t.Fatalf("recorded message (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff(schema, events[0].Data.(*SessionStart).Schema); diff != "" {
					t.Fatalf("provenance schema (-want +got):\n%s", diff)
				}
			}
			if _, _, err := ReadFile(path); err == nil {
				t.Fatal("live reader accepted an older schema")
			}
		})
	}
}

func TestTranslateSchemaFourVocabulary(t *testing.T) {
	for _, tc := range []struct{ name, old, want string }{
		{"cycle start", `{"kind":"checking.lap","lap":3,"event":"start","full":true,"msg":"lap 3 start"}`, `{"kind":"checking.cycle","cycle":3,"event":"start","full":true,"msg":"lap 3 start"}`},
		{"cycle end", `{"kind":"checking.lap","lap":3,"event":"end","passed":true}`, `{"kind":"checking.cycle","cycle":3,"event":"end","passed":true}`},
		{"checking step", `{"kind":"checking.step","lap":3,"step":4,"partials":[{"ccd":1,"cores":[3]}]}`, `{"kind":"checking.step","cycle":3,"step":4,"partials":[{"ccd":1,"cores":[3]}]}`},
		{"exposure state", `{"kind":"future.state","checking":{"lap":3,"lap_open":true,"clean_laps":2,"last_clean_lap":1,"exposure":[{"regime":"R1","starts":5}]}}`, `{"kind":"future.state","checking":{"cycle":3,"cycle_open":true,"clean_cycles":2,"last_clean_cycle":1,"exposure":[{"regime":"R1","trials":5}]}}`},
		{"trial intent", `{"kind":"trial.intent","lap":3,"trial":"0001","seed":18446744073709551615}`, `{"kind":"trial.intent","cycle":3,"trial":"0001","seed":18446744073709551615}`},
		{"hunt", `{"kind":"hunt.start","starts":5,"start_s":120,"parked_seq":8}`, `{"kind":"hunt.start","trials":5,"trial_s":120,"parked_seq":8}`},
		{"deepening", `{"kind":"deepening.round","starts":5,"start_s":120,"event":"start","base_seq":8}`, `{"kind":"deepening.round","trials":5,"trial_s":120,"event":"start","base_seq":8}`},
		{"configuration", `{"kind":"config.loaded","schema":3,"config":{"checking":{"lap":["R1"]},"durations":{"start_s":7},"start_offsets":{"0":-30}}}`, `{"kind":"config.loaded","schema":3,"config":{"checking":{"cycle":["R1"]},"durations":{"short_trial_s":7},"start_offsets":{"0":-30}}}`},
		{"shutdown", `{"kind":"shutdown","reason":"laps","laps":3}`, `{"kind":"shutdown","reason":"cycles","cycles":3}`},
		{"provenance", `{"kind":"trial.carried","source":{"schema":3,"evidence":1,"trial":"0001","seq":9},"class":{"duration_s":120}}`, `{"kind":"trial.carried","source":{"schema":3,"evidence":1,"trial":"0001","seq":9},"class":{"duration_s":120}}`},
		{"unrelated start", `{"kind":"trial.start","window_start_ns":9007199254740993,"start_s":7,"reason":"laps","detail":"starts"}`, `{"kind":"trial.start","window_start_ns":9007199254740993,"start_s":7,"reason":"laps","detail":"starts"}`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := translateSchema([]byte(tc.old), 3)
			if err != nil {
				t.Fatal(err)
			}
			canonical := func(data []byte) string {
				t.Helper()
				var body any
				decoder := json.NewDecoder(bytes.NewReader(data))
				decoder.UseNumber()
				if err := decoder.Decode(&body); err != nil {
					t.Fatal(err)
				}
				encoded, err := json.Marshal(body)
				if err != nil {
					t.Fatal(err)
				}
				return string(encoded)
			}
			if diff := cmp.Diff(canonical([]byte(tc.want)), canonical(got)); diff != "" {
				t.Fatalf("schema four vocabulary (-want +got):\n%s", diff)
			}
		})
	}
}

func TestTranslateSchemaChainsLegacySteps(t *testing.T) {
	for _, schema := range []int{1, 2, 3, Schema} {
		t.Run(fmt.Sprint(schema), func(t *testing.T) {
			line := `{"kind":"checking.cycle","cycle":3,"event":"end","passed":true,"full":true,"schema":4}`
			if schema == 3 {
				line = `{"kind":"checking.lap","lap":3,"event":"end","passed":true,"full":true,"schema":3}`
			} else if schema < 3 {
				line = fmt.Sprintf(`{"kind":"guard.rotation","rotation":3,"event":"end","clean":true,"qualifying":true,"schema":%d}`, schema)
			}
			got, err := translateSchema([]byte(line), schema)
			if err != nil {
				t.Fatal(err)
			}
			event, err := decode(got)
			if err != nil {
				t.Fatal(err)
			}
			want := &CheckingCycle{Cycle: 3, Event: CycleEnd, Passed: true, Full: true}
			if diff := cmp.Diff(want, event.Data); diff != "" {
				t.Fatalf("chained translation (-want +got):\n%s", diff)
			}
			var stamp Build
			if err := json.Unmarshal(got, &stamp); err != nil {
				t.Fatal(err)
			}
			if stamp.Schema != schema {
				t.Fatalf("recorded schema changed to %d, want %d", stamp.Schema, schema)
			}
			if schema == Schema && !bytes.Equal(got, []byte(line)) {
				t.Fatal("current schema was rewritten")
			}
		})
	}
	for _, schema := range []int{1, 2, 3} {
		for _, line := range []string{"{", `{"kind":"shutdown"} {}`} {
			if _, err := translateSchema([]byte(line), schema); err == nil {
				t.Fatalf("schema %d accepted malformed event %q", schema, line)
			}
		}
	}
}

func TestVocabularyReadersPreserveArchivedEvidence(t *testing.T) {
	for _, schema := range []int{1, 2, 3, Schema} {
		t.Run(fmt.Sprint(schema), func(t *testing.T) {
			cycleKind, cycleField, shutdownReason := "checking.cycle", "cycle", "cycles"
			trialsField, durationField := "trials", "trial_s"
			if schema < Schema {
				cycleKind, cycleField, shutdownReason = "checking.lap", "lap", "laps"
				trialsField, durationField = "starts", "start_s"
			}
			if schema < 3 {
				cycleKind, cycleField, shutdownReason = "guard.rotation", "rotation", "rotations"
			}
			lines := []string{
				fmt.Sprintf(`{"seq":1,"kind":"session.start","schema":%d,"ruleset":8,"evidence":1,"session":"source"}`, schema),
				fmt.Sprintf(`{"seq":2,"kind":%q,%q:3,"event":"start","msg":"recorded lap start","steps":["R1"]}`, cycleKind, cycleField),
				fmt.Sprintf(`{"seq":3,"kind":"hunt.start","hunt":1,%q:5,%q:7,"parked_seq":2}`, trialsField, durationField),
				fmt.Sprintf(`{"seq":4,"kind":"deepening.round","round":1,"event":"start",%q:5,%q:7,"base_seq":2}`, trialsField, durationField),
				fmt.Sprintf(`{"seq":5,"kind":"shutdown","reason":%q,%q:3}`, shutdownReason, shutdownReason),
			}
			data := []byte(strings.Join(lines, "\n") + "\n")
			dir := t.TempDir()
			path := filepath.Join(dir, eventsFile)
			if err := os.WriteFile(path, data, 0600); err != nil {
				t.Fatal(err)
			}
			readReplay := func(path string) ([]Event, error) {
				events, _, err := ReadReplay(filepath.Dir(path), 8)
				return events, err
			}
			for index, reader := range []func(string) ([]Event, error){ReadHistory, ReadForCarry, readReplay} {
				events, err := reader(path)
				if err != nil {
					t.Fatal(err)
				}
				wantCount := len(lines)
				if index == 1 {
					wantCount = 3 // Carry reads only the session stamp, hunt and shutdown.
				}
				if len(events) != wantCount {
					t.Fatalf("reader %d returned %d events, want %d", index, len(events), wantCount)
				}
				for _, event := range events {
					if !bytes.Equal(event.Raw, []byte(lines[event.Seq-1])) {
						t.Fatalf("source bytes changed at #%d", event.Seq)
					}
					switch p := event.Data.(type) {
					case *SessionStart:
						if p.Schema != schema || p.Ruleset != 8 || p.Epoch() != 1 {
							t.Fatalf("source compatibility stamps changed: %+v", p)
						}
					case *CheckingCycle:
						if p.Cycle != 3 || p.Event != CycleStart || event.Msg != "recorded lap start" {
							t.Fatalf("cycle start not translated: %+v", event)
						}
					case *HuntStart:
						if p.Trials != 5 || p.TrialS != 7 || p.ParkedSeq != 2 {
							t.Fatalf("hunt evidence changed: %+v", p)
						}
					case *DeepeningRound:
						if p.Trials != 5 || p.TrialS != 7 || p.Event != CycleStart || p.BaseSeq != 2 {
							t.Fatalf("deepening evidence changed: %+v", p)
						}
					case *Shutdown:
						if p.Reason != ShutdownCycles || p.Cycles != 3 {
							t.Fatalf("shutdown enum not translated: %+v", p)
						}
					}
				}
			}
			recorded, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.Equal(recorded, data) {
				t.Fatal("reading modified the archived journal")
			}
		})
	}
}

func TestCycleTrialStateWireVocabulary(t *testing.T) {
	state := State{
		Schema: Schema,
		Checking: &CheckingState{
			Cycle: 3, CycleOpen: true, CleanCycles: 2, LastCleanCycle: 1,
			Exposure: []ExposureRow{{Regime: machine.R1, Workload: "workload", Trials: 5}},
		},
	}
	raw, err := marshal(state)
	if err != nil {
		t.Fatal(err)
	}
	for _, field := range []string{`"cycle":3`, `"cycle_open":true`, `"clean_cycles":2`, `"last_clean_cycle":1`, `"trials":5`} {
		if !bytes.Contains(raw, []byte(field)) {
			t.Fatalf("state missing %s: %s", field, raw)
		}
	}
	for _, field := range []string{`"lap"`, `"lap_open"`, `"clean_laps"`, `"last_clean_lap"`, `"starts"`} {
		if bytes.Contains(raw, []byte(field)) {
			t.Fatalf("state retains old field %s: %s", field, raw)
		}
	}
	var decoded State
	if err := json.Unmarshal(raw, &decoded); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(state, decoded, cmp.AllowUnexported(State{})); diff != "" {
		t.Fatalf("state round trip (-want +got):\n%s", diff)
	}
}
