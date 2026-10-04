package journal

import (
	"bufio"
	"compress/gzip"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestConfigLoadedDecodesEveryShippedJournalBody(t *testing.T) {
	for _, tc := range []struct {
		name       string
		schema     int
		candidates map[int]int
		cycle        []machine.Regime
	}{
		{"20260924T204352Z", 1, nil, []machine.Regime{machine.R1, machine.R2, machine.R6, machine.R3, machine.R4, machine.R7, machine.R5, machine.R6}},
		{"20260926T151414Z", 2, map[int]int{0: -36, 1: -38, 2: -38, 3: -36, 4: -36, 5: -39, 6: -46, 7: -46, 8: -50, 9: -50, 10: -50, 11: -47, 12: -50, 13: -49, 14: -50, 15: -50}, []machine.Regime{machine.R1, machine.R2, machine.R6, machine.R3, machine.R4, machine.R7, machine.R5, machine.R6}},
		{"20260927T221954Z", 2, map[int]int{0: -31, 1: -38, 2: -37, 3: -34, 4: -36, 5: -36, 6: -45, 7: -43, 8: -48, 9: -48, 10: -49, 11: -46, 12: -50, 13: -48, 14: -50, 15: -50}, []machine.Regime{machine.R2, machine.R7, machine.R6, machine.R5, machine.R1, machine.R3, machine.R4, machine.R6}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f, err := os.Open(filepath.Join("..", "carry", "testdata", tc.name+".jsonl.gz"))
			if err != nil {
				t.Fatal(err)
			}
			defer f.Close()
			gz, err := gzip.NewReader(f)
			if err != nil {
				t.Fatal(err)
			}
			defer gz.Close()
			want := ConfigSnapshot{
				StartOffsets: map[int]int{}, CandidateSoloLimits: tc.candidates,
				Durations: ConfigDurations{SearchTrialS: 90, CheckingTrialS: 120, CheckingIdleS: 900, CheckingAllCoreS: 1200},
				Checking:  ConfigChecking{Cycle: tc.cycle},
				DeadEnds:  ConfigDeadEnds{InconclusiveInARow: 3, StrayCrashesInARow: 3},
				Backends:  ConfigBackends{Mprime: "/nix/store/64hjzgj1msiyndpdxrk9l3gkjf3sczgj-mprime-31.04b02", Ycruncher: "/nix/store/n5g91xa9pzcfqyyh62xpwz3v53f87y0a-y-cruncher-0.8.7.9547"},
			}
			scan := bufio.NewScanner(gz)
			found := false
			for scan.Scan() {
				var kind struct {
					Kind   Kind `json:"kind"`
					Schema int  `json:"schema"`
				}
				if err := json.Unmarshal(scan.Bytes(), &kind); err != nil {
					t.Fatal(err)
				}
				if kind.Kind == KindSessionStart && kind.Schema != tc.schema {
					t.Fatalf("schema %d, want %d", kind.Schema, tc.schema)
				}
				if kind.Kind != KindConfigLoaded {
					continue
				}
				found = true
				line, err := translateSchema(scan.Bytes(), tc.schema)
				if err != nil {
					t.Fatal(err)
				}
				e, err := decode(line)
				if err != nil {
					t.Fatal(err)
				}
				p := e.Data.(*ConfigLoaded)
				if diff := cmp.Diff(want, p.Config); diff != "" {
					t.Fatalf("config.loaded #%d (-want +got):\n%s", e.Seq, diff)
				}
				if p.Path != "/etc/togi/config.toml" || !p.File {
					t.Fatalf("config source #%d: %+v", e.Seq, p)
				}
			}
			if err := scan.Err(); err != nil {
				t.Fatal(err)
			}
			if !found {
				t.Fatal("fixture has no config.loaded body")
			}
		})
	}
}

func TestReadReplayPreservesConfiguration(t *testing.T) {
	t.Parallel()
	for _, schema := range []int{1, 2, 3, Schema} {
		t.Run(fmt.Sprint(schema), func(t *testing.T) {
			schedule := `"checking":{"cycle":["R2","R7","R1"]}`
			duration := "short_trial_s"
			if schema < Schema {
				schedule = `"checking":{"lap":["R2","R7","R1"]}`
				duration = "start_s"
			}
			if schema < 3 {
				schedule = `"guard":{"rotation":["R2","R7","R1"]}`
			}
			first := fmt.Sprintf(`{"seq":1,"kind":"session.start","schema":%d,"ruleset":8,"session":"replay"}`, schema)
			second := fmt.Sprintf(`{"seq":2,"kind":"config.loaded","msg":"recorded configuration","schema":%d,"ruleset":8,"config":{"durations":{"%s":7},"evidence":{"miss":0.01,"rate":0.2},%s}}`, schema, duration, schedule)
			dir := t.TempDir()
			if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(first+"\n"+second+"\ntorn"), 0o600); err != nil {
				t.Fatal(err)
			}
			events, torn, err := ReadReplay(dir, 8)
			if err != nil {
				t.Fatal(err)
			}
			want := &ConfigLoaded{
				Schema: schema, Ruleset: 8,
				Config: ConfigSnapshot{
					Durations: ConfigDurations{ShortTrialS: 7},
					Evidence:  ConfigEvidence{Miss: 0.01, Rate: 0.2},
					Checking:  ConfigChecking{Cycle: []machine.Regime{machine.R2, machine.R7, machine.R1}},
				},
			}
			if diff := cmp.Diff(want, events[1].Data); diff != "" {
				t.Fatalf("replay configuration (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff([]byte("torn"), torn); diff != "" {
				t.Fatalf("torn tail (-want +got):\n%s", diff)
			}
			if schema == Schema {
				current, currentTorn, err := Read(dir)
				if err != nil {
					t.Fatal(err)
				}
				if diff := cmp.Diff(current, events); diff != "" {
					t.Fatalf("current-schema replay (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff(currentTorn, torn); diff != "" {
					t.Fatalf("current-schema torn tail (-want +got):\n%s", diff)
				}
			}
		})
	}
}
