package journal

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
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
	for _, schema := range []int{1, 2} {
		t.Run(fmt.Sprint(schema), func(t *testing.T) {
			first := fmt.Sprintf(`{"seq":1,"kind":"session.start","schema":%d,"session":"old"}`, schema)
			second := `{"seq":2,"kind":"trial.intent","trial":"0001","condition":"isolated","phase":"guard","rotation":3,"mask":4}`
			path := filepath.Join(t.TempDir(), "old.jsonl")
			if err := os.WriteFile(path, []byte(first+"\n"+second+"\n"), 0o644); err != nil {
				t.Fatal(err)
			}
			for _, read := range []func(string) ([]Event, error){ReadHistory, ReadForCarry} {
				events, err := read(path)
				if err != nil {
					t.Fatal(err)
				}
				want := &TrialIntent{Trial: "0001", Condition: machine.Alone, Phase: PhaseChecking, Lap: 3, Group: 4}
				if diff := cmp.Diff(want, events[1].Data); diff != "" {
					t.Fatalf("payload (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff(second, string(events[1].Raw)); diff != "" {
					t.Fatalf("recorded line (-want +got):\n%s", diff)
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
